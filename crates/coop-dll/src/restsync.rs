//! A rest by either player resets the enemies for both (IMPLEMENTATION-NOTES §27).
//!
//! Live 2026-10-23 (build 6.5, custom sync, mode 4): the friend (client) rested, the host
//! mirrored it (`world: friend rested at idol 1101952, resting here too (ok = true)`) and
//! its enemies did come back (host T lines: 1100233 HP 0 -> 151 two seconds later), but
//! the friend's enemies stayed dead (`HP mismatch: dead here / alive on host [...]` with
//! 18 ids). Order of events: the client's own rest respawns its enemies at once; the
//! host's mirrored rest arrives a round trip later and the idol object respawns after
//! its 1.333 s timer (RE-DEATH-RESET §2.1). In between, the host still sends HP 0 for
//! those enemies, and the client's HP authority (enemies.rs `client_frame`) wrote that
//! 0 onto the freshly respawned enemies: killed again. A moment later the host's
//! respawn reported them alive, but a corpse cannot be revived by HP (the corpse rule),
//! so they stayed dead for good. A host rest works because the host's enemies respawn
//! first (its own rest), so the client's later respawn meets live host HPs.
//!
//! This module:
//! - records every rest that respawned our enemies (`note`): our own rest, a mirrored
//!   rest (mailbox), a resync, a direct AllChrRespawn;
//! - verifies the respawn (WCM `+0x67C` AllChrRespawn state machine seen non-zero, and
//!   enemies dead before the rest alive again), and for a mirrored mailbox rest that did
//!   nothing (the idol object not loaded / not updated near us) calls
//!   `0xA4C0F0` + `0x899500` directly (`world::respawn_all`);
//! - opens a 12 s window in which the client does not kill an enemy that is alive here
//!   but still dead on the host (`hold_kills`), so the host's respawn can catch up;
//! - tells enemies.rs (per-load kill bookkeeping) and nsession.rs (the native session
//!   disengages and waits 10 s, so ownership / type-5 HP from before the rest cannot
//!   bring the dead state back) on the frame after each rest;
//! - lets the P3 death restore (puppet.rs `death_sync`) see that a rest came after a
//!   death: a rest resets the world for both, so nothing dead is restored after it.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::enemies::Enemies;
use crate::peers::PeerSet;
use crate::game::{self, read, read_ptr};
use crate::log;

/// The client keeps an enemy alive here while the host still reports it dead.
pub const HOLD_KILLS_FOR: Duration = Duration::from_secs(12);
/// A respawn is judged after this long (the idol's own respawn waits 1.333 s).
const VERIFY_AFTER: Duration = Duration::from_millis(4000);
const WORLD_CHR_MAN: usize = 0x3D7A1E0;
/// AllChrRespawn state (`0xA4C0F0` sets it to 1; the WCM update runs 1 -> 3 -> 4 -> 5 -> 0).
const WCM_RESPAWN_STATE: usize = 0x67C;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// We rested ourselves (the game's own rest respawned our enemies).
    Own,
    /// The friend rested; we wrote both idol mailboxes (`0x3AFEB98` + `0x3AFEBA0`).
    Mirrored,
    /// The friend rested in another block (or no mailbox): `0xA4C0F0` + `0x899500` direct.
    Direct,
    /// Resync (resync.rs) through the mailboxes.
    Resync,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Own => "our own",
            Kind::Mirrored => "mirrored (idol mailbox)",
            Kind::Direct => "mirrored (direct AllChrRespawn)",
            Kind::Resync => "resync",
        }
    }
    /// A mailbox rest may do nothing (idol object not loaded / not updated): fall back.
    fn may_fall_back(self) -> bool {
        matches!(self, Kind::Mirrored | Kind::Resync)
    }
}

struct Verify {
    kind: Kind,
    idol: i32,
    since: Instant,
    dead_before: Option<HashSet<i32>>,
    saw_state: bool,
    fell_back: bool,
}

#[derive(Default)]
struct State {
    generation: u64,
    seen_generation: u64,
    at: Option<Instant>,
    /// Start of the client's no-kill window (cleared by a load).
    hold_from: Option<Instant>,
    verify: Option<Verify>,
    back_sent_at: Option<Instant>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

/// A rest respawned (or is about to respawn) our enemies. Game thread (world.rs).
pub fn note(kind: Kind, idol: i32) {
    crate::invguard::checkpoint(&format!("before {} rest at idol {idol}", kind.label()));
    crate::invguard::note_rest();
    let now = Instant::now();
    with(|s| {
        s.generation += 1;
        s.at = Some(now);
        s.hold_from = Some(now);
        s.verify = Some(Verify { kind, idol, since: now, dead_before: None, saw_state: false, fell_back: false });
    });
    log::line(&format!(
        "rest: {} rest at idol {idol}: enemies reset here; for {} s the friend's dead enemies are not forced on ours",
        kind.label(),
        HOLD_KILLS_FOR.as_secs()
    ));
}

/// When the last rest of ours happened.
pub fn last_rest() -> Option<Instant> {
    with(|s| s.at).flatten()
}

/// A rest of ours came after `t` (a death, a dead-list request).
pub fn rest_after(t: Instant) -> bool {
    last_rest().is_some_and(|r| r > t)
}

/// Client: an enemy alive here but dead on the host is left alive (the host's respawn
/// of the same rest has not reached us yet).
pub fn hold_kills() -> bool {
    with(|s| s.hold_from.is_some_and(|t| t.elapsed() < HOLD_KILLS_FOR)).unwrap_or(false)
}

/// Host: we asked the friend for its dead list (P3, `EV_BACK`).
pub fn mark_back_sent() {
    with(|s| s.back_sent_at = Some(Instant::now()));
}

/// Host: a rest came after our dead-list request, so the list is from before the reset.
pub fn rest_since_back() -> bool {
    with(|s| s.back_sent_at.is_some_and(|b| s.at.is_some_and(|r| r > b))).unwrap_or(false)
}

fn respawn_state() -> Option<i32> {
    read::<i32>(read_ptr(game::module_base() + WORLD_CHR_MAN)? + WCM_RESPAWN_STATE)
}

/// Every frame in the world (puppet.rs, after world.rs): bookkeeping after a rest, the
/// respawn check, the mailbox fallback. `peers` tells whether we host.
pub fn tick(enemies: &mut Enemies, peers: &PeerSet) {
    let is_host = peers.is_host();
    let Some((new_rest, kind)) = with(|s| {
        let fresh = s.generation != s.seen_generation;
        s.seen_generation = s.generation;
        (fresh, s.verify.as_ref().map(|v| v.kind))
    }) else {
        return;
    };
    if new_rest {
        enemies.on_rest();
        crate::nsession::on_rest(kind.map_or("rest", Kind::label));
    }
    let state = respawn_state();
    let loaded = || enemies.loaded_enemies(is_host);
    let mut fallback: Option<(Kind, i32)> = None;
    let mut report: Option<String> = None;
    with(|s| {
        let Some(v) = s.verify.as_mut() else { return };
        if state.is_some_and(|x| x != 0) {
            v.saw_state = true;
        }
        if v.dead_before.is_none() {
            v.dead_before =
                Some(loaded().into_iter().filter(|(_, c)| c.hp().is_some_and(|hp| hp <= 0)).map(|(id, _)| id).collect());
        }
        if v.since.elapsed() < VERIFY_AFTER {
            return;
        }
        let dead = v.dead_before.take().unwrap_or_default();
        let now_alive: HashSet<i32> =
            loaded().into_iter().filter(|(_, c)| c.hp().is_some_and(|hp| hp > 0)).map(|(id, _)| id).collect();
        let back = dead.iter().filter(|id| now_alive.contains(id)).count();
        let ran = v.saw_state || back > 0;
        report = Some(format!(
            "rest: {} rest at idol {}: respawn {} (AllChrRespawn state seen {}, dead before {}, alive again {}, still dead {}){}",
            v.kind.label(),
            v.idol,
            if ran { "verified" } else { "NOT seen" },
            v.saw_state,
            dead.len(),
            back,
            dead.len() - back,
            if v.fell_back { " after the direct fallback" } else { "" }
        ));
        if !ran && v.kind.may_fall_back() && !v.fell_back {
            fallback = Some((v.kind, v.idol));
            v.fell_back = true;
            v.since = Instant::now();
            v.saw_state = false;
            v.dead_before = Some(dead);
        } else {
            s.verify = None;
        }
    });
    if let Some(r) = report {
        log::line(&r);
    }
    if let Some((kind, idol)) = fallback {
        // A direct AllChrRespawn during a boss fight (or while spectating one) would
        // respawn the boss arena under the fight: skip it.
        if crate::game::boss_state().is_some() || crate::bossrules::spectating() {
            log::line("rest: fallback skipped, boss fight");
            return;
        }
        let ok = crate::world::respawn_all();
        // A new window: the host's view of these enemies changes again now.
        with(|s| s.hold_from = Some(Instant::now()));
        log::line(&format!(
            "rest: the {} rest at idol {idol} respawned nothing here (idol object not loaded or not near us?): AllChrRespawn + objects called directly (ok = {ok})",
            kind.label()
        ));
    }
}

/// A load forgets the pending check and the no-kill window (every character is
/// re-created; after our own death reload the host's HP decides again at once). The
/// time of the rest stays for the P3 death rule.
pub fn on_world_reset() {
    with(|s| {
        s.verify = None;
        s.hold_from = None;
    });
}
