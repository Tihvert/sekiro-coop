//! Shared Resurrect unlock (docs/RE-RESURRECT-FRIEND.md, 2026-10-25, NOT verified live).
//!
//! Sekiro offers a resurrection only when (`0xA34B70`, static IDA) the Resurrection skill bit
//! is set, goods 1110 (temporary node) > 0 or goods 1100 (nodes) > 0 without SpEffect 400,
//! AND the "Revive Recast Point" `f32 PlayerGameData+0x150` is 0. Rising sets it to its maximum
//! 100.0 (`0xA36A60`, `dword_143B010A0`); a respawn / map load sets it to 0 (`0xA2EE70`); its
//! time decay `RevivalRecastGain` (`chr+0x2130`) is 0. Every frame `0xA30B50` lowers it by the
//! sum of `SpEffectParam.revivalRecastDamage` (row `+0x418`, `0xBFDCE0`) over the player's
//! active SpEffects (clamp `0xA23730`); the rows with a value are 100 (rest), 3292 and 8100,
//! 99999 each. So a second resurrection in one life needs that clear. In the logs of
//! 2026-10-25 it came after the player's OWN deathblow (host 1792915115 and 1792928032); a
//! deathblow or kill by another player clears nothing on our side, so with 2-6 players the
//! one who lands the deathblows was the only one who could resurrect again.
//!
//! Here we tell everybody (`EV_REVIVE_UNLOCK`, the session relays an event to every peer)
//! when (a) one of our deathblows starts: SpEffect 110030 comes on our player (c0000 TAE
//! type 67 in every deathblow animation: a200-a250 510xxx-516xxx / 600xxx, a00 7000x0), so it
//! works even while our own lock is 0 (we never died); or (b) our own lock drops from > 0 to
//! 0 while we stay alive in the same character (no load between, no rest of ours just now):
//! the game cleared it for something of ours that (a) missed. One event per 3 s. A peer that
//! hears it clears its own lock the same way the game would have, if it is alive and its
//! lock is up. Nothing else changes: no goods, no nodes, no skill; a player with no node left
//! still gets no resurrection. `[GAMEPLAY] share_resurrect_unlock` (on).
//!
//! Logs (`resurrect:` lines): every lock change of ours with the cause, every death of ours
//! with the lock and the node counts (why it was / was not offered), every enemy death seen
//! here with our lock before / after, and every unlock sent, applied or ignored.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, read, read_ptr};
use crate::log;
use crate::net::Net;
use crate::peers::PeerId;

/// Event kind: a = our running unlock count (for the log), b = 0. Free: 1-8, 40-42 are taken
/// (world.rs, npcintro.rs, resync.rs, talklock.rs).
pub const EV_REVIVE_UNLOCK: u8 = 44;

/// `[GAMEPLAY] share_resurrect_unlock`.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
/// `f32 PlayerGameData+0x150` "Revive Recast Point" (debug text in `0x1407C51A0`).
const PGD_RECAST_POINT: usize = 0x150;
/// Goods 1100 (Resurrective Power nodes), 1110 (temporary node), 2000 (Resurrection).
const GOODS_NODES: u32 = 0x4000_044C;
const GOODS_TEMP_NODE: u32 = 0x4000_0456;
const GOODS_RESURRECTION: u32 = 0x4000_07D0;
/// A frame gap above this is a load (the lock is reset by the respawn, not by a kill).
const MAX_GAP: Duration = Duration::from_millis(1500);
/// Our own rest clears the lock with SpEffect 100; the rest is mirrored to everyone anyway.
const REST_WINDOW: Duration = Duration::from_secs(5);
/// How far back "the lock before" an enemy death looks (the death is seen 1 s late).
const HISTORY: Duration = Duration::from_secs(3);
const MAX_KILL_LINES: u32 = 400;
/// SpEffect on our player while one of our deathblow animations runs (TAE type 67).
const SP_DEATHBLOW: i32 = 110030;
/// One unlock event per this (a deathblow and the lock clear it makes are one).
const SEND_GAP: Duration = Duration::from_secs(3);
/// No deathblow counts this soon after a load (the get-up animation).
const SETTLE: Duration = Duration::from_secs(5);

/// One frame's reading of our own player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// ChrIns address: a new one after every load.
    pub chr: usize,
    pub alive: bool,
    pub lock: f32,
    /// SpEffect 110030 on with our own throw running (a deathblow of ours runs).
    pub deathblow: bool,
}

/// What a step of the detector saw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    /// One of our deathblows started (110030 came on while alive): tell everybody.
    OwnDeathblow { lock: f32 },
    /// The game cleared our lock while we lived on (our deathblow / kill): tell everybody.
    ClearedByGame { from: f32 },
    /// Cleared by our own rest (not shared: the rest is).
    ClearedByRest { from: f32 },
    /// Our lock went up: we rose (a resurrection was used).
    Raised { from: f32, to: f32 },
    /// We died (alive -> dead): log why a resurrection is or is not offered.
    Died { lock: f32 },
}

#[derive(Default)]
pub struct Detector {
    last: Option<(Sample, Instant)>,
    /// Since when the current character has been seen without a load.
    chr_since: Option<Instant>,
}

impl Detector {
    /// `rested` = our own rest within `REST_WINDOW`. A load (new chr, a long gap or a dead
    /// frame between) is never a clear; no deathblow counts in the first `SETTLE` of a chr.
    pub fn step(&mut self, s: Sample, now: Instant, rested: bool) -> Option<Change> {
        let prev = self.last.replace((s, now));
        let Some((p, at)) = prev else {
            self.chr_since = Some(now);
            return None;
        };
        if p.chr != s.chr || now.duration_since(at) > MAX_GAP {
            self.chr_since = Some(now);
            return None;
        }
        let settled = self.chr_since.is_some_and(|t| now.duration_since(t) >= SETTLE);
        if p.alive && !s.alive {
            return Some(Change::Died { lock: s.lock });
        }
        if !(p.alive && s.alive) {
            return None;
        }
        if p.lock > 0.0 && s.lock <= 0.0 {
            return Some(if rested { Change::ClearedByRest { from: p.lock } } else { Change::ClearedByGame { from: p.lock } });
        }
        if s.lock > p.lock {
            return Some(Change::Raised { from: p.lock, to: s.lock });
        }
        if s.deathblow && !p.deathblow && settled && !rested {
            return Some(Change::OwnDeathblow { lock: s.lock });
        }
        None
    }

    /// We cleared the lock ourselves (a peer's unlock): the next step must not see a clear.
    pub fn note_applied(&mut self) {
        if let Some((s, _)) = self.last.as_mut() {
            s.lock = 0.0;
        }
    }
}

/// A peer's unlock applies when we are alive and our lock is up.
pub fn should_apply(alive: bool, lock: f32) -> bool {
    alive && lock > 0.0
}

/// Send now? Not within `SEND_GAP` of the last one.
pub fn send_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= SEND_GAP)
}

struct State {
    det: Detector,
    history: VecDeque<(Instant, f32)>,
    unlocks_sent: i32,
    last_sent: Option<Instant>,
    applied: u32,
    kill_lines: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(|| State {
        det: Detector::default(),
        history: VecDeque::new(),
        unlocks_sent: 0,
        last_sent: None,
        applied: 0,
        kill_lines: 0,
    });
    f(s)
}

fn pgd() -> Option<usize> {
    read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))
}

fn read_sample() -> Option<Sample> {
    let chr = game::player()?;
    let lock = read::<f32>(pgd()? + PGD_RECAST_POINT)?;
    // 110030 also comes on in the get-up after a load (live 1792911753, no throw): only with
    // our own throw running (ThrowModule node + timer, as `enemies::throw_active`).
    let deathblow = crate::statelog::sp_ids(chr.0).contains(&SP_DEATHBLOW) && crate::enemies::throw_active(chr);
    Some(Sample { chr: chr.0, alive: chr.hp().is_some_and(|hp| hp > 0), lock, deathblow })
}

/// Sends one unlock (not within `SEND_GAP` of the last); "told every player (#n)" or why not.
fn send_unlock(net: &mut Net, connected: bool, now: Instant) -> String {
    if !connected || !ENABLED.load(Ordering::Relaxed) {
        return "not shared (single player or share_resurrect_unlock = 0)".to_owned();
    }
    let n = with(|st| {
        if !send_due(st.last_sent, now) {
            return None;
        }
        st.last_sent = Some(now);
        st.unlocks_sent += 1;
        Some(st.unlocks_sent)
    });
    match n {
        Some(n) => {
            net.send_event(EV_REVIVE_UNLOCK, n, 0);
            format!("told every player (unlock #{n})")
        }
        None => "already told every player for this deathblow".to_owned(),
    }
}

/// "nodes 1100 = a, temporary 1110 = b, skill 2000 = c" (None when the inventory is unreadable).
fn nodes_text() -> String {
    match crate::audit::read_inventory() {
        Some(inv) => {
            let q = |id: u32| inv.iter().find(|(i, _)| *i == id).map_or(0, |(_, q)| *q);
            format!(
                "nodes (goods 1100) {}, temporary node (1110) {}, Resurrection skill (2000) {}",
                q(GOODS_NODES),
                q(GOODS_TEMP_NODE),
                if q(GOODS_RESURRECTION) > 0 { "yes" } else { "NO" }
            )
        }
        None => "nodes unreadable".to_owned(),
    }
}

/// world.rs, every frame (game thread): watch our lock, send our unlocks.
pub fn tick(net: &mut Net, connected: bool, now: Instant) {
    let Some(s) = read_sample() else { return };
    let rested = crate::restsync::last_rest().is_some_and(|r| now.duration_since(r) < REST_WINDOW);
    let change = with(|st| {
        st.history.push_back((now, s.lock));
        while st.history.front().is_some_and(|(t, _)| now.duration_since(*t) > HISTORY) {
            st.history.pop_front();
        }
        st.det.step(s, now, rested)
    });
    match change {
        Some(Change::OwnDeathblow { lock }) => {
            let sent = send_unlock(net, connected, now);
            log::line(&format!("resurrect: our deathblow started (SpEffect 110030), our lock {lock:.1}; {sent}"));
        }
        Some(Change::ClearedByGame { from }) => {
            let sent = send_unlock(net, connected, now);
            log::line(&format!(
                "resurrect: our lock {from:.1} -> 0 by the game (our deathblow / kill): Resurrect usable again here; {sent}; {}",
                nodes_text()
            ));
        }
        Some(Change::ClearedByRest { from }) => {
            log::line(&format!("resurrect: our lock {from:.1} -> 0 by our rest; {}", nodes_text()));
        }
        Some(Change::Raised { from, to }) => {
            log::line(&format!(
                "resurrect: we rose: our lock {from:.1} -> {to:.1} (the next Resurrect needs a deathblow / kill by any player, or a rest); {}",
                nodes_text()
            ));
        }
        Some(Change::Died { lock }) => {
            log::line(&format!(
                "resurrect: we died: our lock {lock:.1} ({}); {}",
                if lock > 0.0 { "LOCKED: no Resurrect until a deathblow / kill or a rest" } else { "not locked" },
                nodes_text()
            ));
        }
        None => {}
    }
}

/// world.rs: a peer's `EV_REVIVE_UNLOCK` (one of its deathblows / kills cleared its lock).
pub fn on_event(from: PeerId, a: i32) {
    let Some(s) = read_sample() else {
        log::line(&format!("resurrect: unlock #{a} from peer {} ignored: no player (loading)", from.slot()));
        return;
    };
    if !ENABLED.load(Ordering::Relaxed) {
        log::line(&format!("resurrect: unlock #{a} from peer {} ignored (share_resurrect_unlock = 0)", from.slot()));
        return;
    }
    if !should_apply(s.alive, s.lock) {
        log::line(&format!(
            "resurrect: unlock #{a} from peer {}: nothing to do (we are {}, our lock {:.1}); {}",
            from.slot(),
            if s.alive { "alive" } else { "dead" },
            s.lock,
            nodes_text()
        ));
        return;
    }
    let ok = pgd().is_some_and(|p| game::write(p + PGD_RECAST_POINT, 0.0f32));
    let n = with(|st| {
        if ok {
            st.det.note_applied();
            st.applied += 1;
        }
        st.applied
    });
    log::line(&format!(
        "resurrect: unlock #{a} from peer {} (its deathblow / kill): our lock {:.1} -> {} (applied {n} in total); {}",
        from.slot(),
        s.lock,
        if ok { "0, Resurrect usable again here" } else { "WRITE FAILED" },
        nodes_text()
    ));
}

/// enemies.rs `loot_step`: an enemy died in our world (seen 1 s after its death). `ours` =
/// the game paid it to us itself (our own kill).
pub fn note_enemy_death(id: i32, ours: bool) {
    let Some(s) = read_sample() else { return };
    let Some(before) = with(|st| {
        if st.kill_lines >= MAX_KILL_LINES {
            return None;
        }
        st.kill_lines += 1;
        Some(st.history.front().map_or(s.lock, |(_, l)| *l))
    }) else {
        return;
    };
    log::line(&format!(
        "resurrect: enemy {id} died ({}): our lock {before:.1} (3 s ago) -> {:.1} now, we are {}; {}",
        if ours { "our kill" } else { "not our kill" },
        s.lock,
        if s.alive { "alive" } else { "dead" },
        nodes_text()
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(chr: usize, alive: bool, lock: f32) -> Sample {
        Sample { chr, alive, lock, deathblow: false }
    }

    /// Steps `chr` alive with lock 0 once a second for `secs` seconds from `t`.
    fn settle(d: &mut Detector, chr: usize, t: Instant, secs: u64) -> Instant {
        for i in 0..=secs {
            d.step(s(chr, true, 0.0), t + Duration::from_secs(i), false);
        }
        t + Duration::from_secs(secs)
    }

    #[test]
    fn our_deathblow_is_shared_even_with_no_lock_of_ours() {
        let t = Instant::now();
        let mut d = Detector::default();
        let t = settle(&mut d, 1, t, 5);
        let db = Sample { deathblow: true, ..s(1, true, 0.0) };
        assert_eq!(d.step(db, t + Duration::from_millis(16), false), Some(Change::OwnDeathblow { lock: 0.0 }));
        // Still on: one event per deathblow.
        assert_eq!(d.step(db, t + Duration::from_millis(32), false), None);
        // A dead player's 110030 (a deathblow on us, a load) is nothing.
        let mut d = Detector::default();
        d.step(s(1, false, 100.0), t, false);
        assert_eq!(d.step(Sample { deathblow: true, ..s(1, false, 100.0) }, t + Duration::from_millis(16), false), None);
    }

    #[test]
    fn no_deathblow_right_after_a_load_or_a_rest() {
        let t = Instant::now();
        let mut d = Detector::default();
        let t = settle(&mut d, 1, t, 5);
        // A load: a new chr, then the get-up within 5 s.
        let t = settle(&mut d, 2, t + Duration::from_millis(16), 3);
        let db = Sample { deathblow: true, ..s(2, true, 0.0) };
        assert_eq!(d.step(db, t + Duration::from_millis(16), false), None);
        // Our rest just now.
        let mut d = Detector::default();
        let t = settle(&mut d, 1, t, 5);
        assert_eq!(d.step(Sample { deathblow: true, ..s(1, true, 0.0) }, t + Duration::from_millis(16), true), None);
    }

    #[test]
    fn one_unlock_event_per_three_seconds() {
        let t = Instant::now();
        assert!(send_due(None, t));
        assert!(!send_due(Some(t), t + Duration::from_millis(2900)));
        assert!(send_due(Some(t), t + Duration::from_secs(3)));
    }

    #[test]
    fn a_clear_while_alive_is_the_games_and_is_shared() {
        let t = Instant::now();
        let mut d = Detector::default();
        assert_eq!(d.step(s(1, true, 100.0), t, false), None);
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_millis(16), false), Some(Change::ClearedByGame { from: 100.0 }));
        // Stays 0: nothing more.
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_millis(32), false), None);
    }

    #[test]
    fn a_rest_clear_is_not_shared() {
        let t = Instant::now();
        let mut d = Detector::default();
        d.step(s(1, true, 100.0), t, true);
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_millis(16), true), Some(Change::ClearedByRest { from: 100.0 }));
    }

    #[test]
    fn a_load_or_a_death_is_never_a_clear() {
        let t = Instant::now();
        let mut d = Detector::default();
        d.step(s(1, true, 100.0), t, false);
        // Respawn after a death reload: a new ChrIns with the lock reset to 0.
        assert_eq!(d.step(s(2, true, 0.0), t + Duration::from_millis(16), false), None);
        // A long frame gap (loading screen) in the same chr.
        let mut d = Detector::default();
        d.step(s(1, true, 100.0), t, false);
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_secs(5), false), None);
        // Dead frames in between.
        let mut d = Detector::default();
        d.step(s(1, true, 100.0), t, false);
        assert_eq!(d.step(s(1, false, 100.0), t + Duration::from_millis(16), false), Some(Change::Died { lock: 100.0 }));
        assert_eq!(d.step(s(1, false, 0.0), t + Duration::from_millis(32), false), None);
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_millis(48), false), None);
    }

    #[test]
    fn rising_raises_the_lock_and_a_peer_unlock_is_not_echoed() {
        let t = Instant::now();
        let mut d = Detector::default();
        d.step(s(1, true, 0.0), t, false);
        assert_eq!(d.step(s(1, true, 100.0), t + Duration::from_millis(16), false), Some(Change::Raised { from: 0.0, to: 100.0 }));
        // A peer's unlock written by us: the detector must not send it back out.
        d.note_applied();
        assert_eq!(d.step(s(1, true, 0.0), t + Duration::from_millis(32), false), None);
    }

    #[test]
    fn a_peer_unlock_applies_only_to_a_living_locked_player() {
        assert!(should_apply(true, 100.0));
        assert!(!should_apply(true, 0.0));
        assert!(!should_apply(false, 100.0));
    }
}
