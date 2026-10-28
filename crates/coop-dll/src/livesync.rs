//! Lives (deathblows) and death stay the same on both machines while the native session
//! owns the enemies (`[GAMEPLAY] native_lives_sync = 1`, IMPLEMENTATION-NOTES §28, NOT
//! verified live).
//!
//! Why (live 7.2, 2026-10-23, the 2-deathblow miniboss 1100301 netId 0x83f): lives are
//! local data (`data+0x25C`), only the hit path `0xB6E6A0` lowers them (a deathblow =
//! hit type 5), and no native packet carries them. Type 21 carries HP / posture damage
//! only; type 18 (the throw = every deathblow) is applied on the receiver only when that
//! machine owns the defender. The friend's deathblow at 14:28:23 (`tx type 18 ... 3f 08
//! f0 ff`, the friend owned the enemy after its stealth claim) took a life there (2 -> 1);
//! on the host the copy was NetAI, so nothing (still 2). The host's deathblow at 14:29:01
//! took the host's 2 -> 1, and on the friend, who owned it again, the received throw took
//! the last life: dead there ("reward: the game's drop of ... deathblows 2"), alive on the
//! host. posture.rs, which syncs lives in our own sync, is off under the native session
//! (`posture::enabled`). The friend's dead copy kept getting the host's HP (type 5) and
//! behavior (70 / 71) for the living host copy: "the corpse deals damage".
//!
//! What this does, at ~7 Hz while engaged (stage >= 2):
//! - every enemy below its NpcParam deathblow count (`+0x126`) or dead is sent as
//!   `(netId, lives, dead | fresh)` in a Lives message on the nsession lane (changes at
//!   once, all of them every 2 s; a new engagement starts over);
//! - the friend's states are merged by minimum (lives only go down, death is final), and
//!   each copy here follows with the game's own steps: lives written + the life SFX
//!   (`0x9F0410`), HP to max through the setter `0xBD64E0` and posture to full when a life
//!   is left (the vanilla get-up), and ForceDead (`0x751F20(entity, noReward = 1)`) for a
//!   death, with lives 0 first so the "cannot die" floor cannot hold it;
//! - rewards: ForceDead pays nothing itself; a death of an enemy we were not paid for in
//!   this rest cycle (persist.rs `rewarded`) gets our own drops / sen once from enemies.rs
//!   `loot_step` ("died without our kill", the no-reward bit ForceDead set cleared first);
//!   one we were paid for already (our own kill before a death reload) is marked no-loot.
//!   (7.4 used "fresh = seen dying during this engagement": every kill the friend made
//!   while disengaged was unpaid.) Boss / miniboss item lots stay with keyitems.rs (lot
//!   flags, 120 s double guard);
//! - a native throw on that enemy in the last 3 s that this machine applies itself (we sent
//!   it, or we own the defender) holds the merge, so a deathblow is never taken twice; a
//!   death waits 0.4 s for the native kill (type 5) first;
//! - a lives drop with a life left is a get-up, never a death (7.15): the scan that sees
//!   it may also read HP <= 0, so the drop is detected BEFORE the dead test (live 7.13:
//!   the host's own deathblow on a NetAI copy read lives 2 -> 1 and HP 0 in one scan, the
//!   drop was never recorded and the copy stayed down). A NetAI copy is refilled (HP +
//!   posture) at once; an owned copy gets `OWN_GETUP_AFTER` to get up by itself. For
//!   `GETUP_HOLD` (6 s) after any drop HP <= 1 is refilled again (the trailing hit of the
//!   deathblow, the friend's stale ~1 HP type 5 right after a refill); the stale type-5
//!   HP is applied in nsession.rs, so it is countered here by the immediate re-refill;
//! - single-life bosses (gauge entity / `bosses.rs` final entities) are tracked for death
//!   too: a death on either side kills the other copy, a recent throw holds the merge;
//! - lives only go down during an engagement: a write that would raise them is logged and
//!   refused (`set_lives`);
//! - a dead copy here stays dead: HP raised again without a revive (type 5 from a living
//!   host copy) is written back to 0, so it is a corpse for the game's hit checks too.
//!
//! N players (PLAN-MULTIPLAYER §5.14, NOT verified live): every engaged player sends its
//! states to all (a client's go through the host, which passes them on with the origin),
//! and `nsession::take_lives` returns them from every origin; the merge is a minimum over
//! all of them (lives only go down, a death anywhere is final), so the order between
//! players does not matter. "The friend" below = any other engaged player. The engagement
//! key changes on the host whenever one of its clients engages or leaves: the state then
//! starts over and is rebuilt from the 2 s resends.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::enemies::Enemies;
use crate::game::{self, Chr, read, read_bytes, read_ptr};
use crate::nsession::{self, LIFE_DEAD, LIFE_FRESH, LifeEntry};
use crate::peers::PeerSet;
use crate::{log, netai, posture};

/// `[GAMEPLAY] native_lives_sync` (default on).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const SCAN_EVERY: Duration = Duration::from_millis(150);
const RESEND_EVERY: Duration = Duration::from_secs(2);
const THROW_HOLD_MS: u64 = 3000;
const KILL_GRACE: Duration = Duration::from_millis(400);
/// After a lives drop: HP is held at max (re-refilled whenever <= 1) this long.
const GETUP_HOLD: Duration = Duration::from_secs(6);
const STATS_EVERY: Duration = Duration::from_secs(10);
const MAX_LOGS: u32 = 40;
/// An engagement that starts this soon after our rest: the friend's first states (sent
/// within `REST_BASELINE_FOR` of the start) are a baseline, not deathblows (§34).
const REST_GUARD: Duration = Duration::from_secs(30);
const REST_BASELINE_FOR: Duration = Duration::from_millis(3500);
/// The others' lives states are ignored this long after a death reload of ours (v8.6).
const DEATH_GUARD: Duration = Duration::from_secs(45);
/// Our own deathblow took a life: the copy gets this long to get up by itself, and the
/// watch ends after `OWN_DROP_FOR`.
const OWN_GETUP_AFTER: Duration = Duration::from_millis(1500);
const OWN_DROP_FOR: Duration = Duration::from_secs(6);

const CHR_PARAM_REF: usize = 0x2010;
const REF_ROW: usize = 0x08;
/// NpcParam i8 deathblow count (= the lives an enemy starts with).
const NPC_DEATHBLOWS: usize = 0x126;
/// Data module block read in one go: `+0x130` HP .. `+0x25C` lives.
const DATA_BLOCK: usize = 0x130;
const DATA_BLOCK_LEN: usize = 0x25C + 4 - DATA_BLOCK;
pub(crate) const DATA_HP: usize = 0x130;
const DATA_MAX_HP: usize = 0x134;
const DATA_POSTURE: usize = 0x148;
const DATA_MAX_POSTURE: usize = 0x14C;
pub(crate) const DATA_LIVES: usize = 0x25C;

// --- Pure rules (unit tested) ---------------------------------------------------------

/// What to do with our copy of one enemy, given the friend's merged state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Nothing,
    /// A native throw on it is still applying here (or the native kill gets its moment).
    Hold,
    /// The friend's copy lost lives ours still has: down to `to`; `refill` = a life is left
    /// (the vanilla get-up: full HP and posture).
    Lower { to: i32, refill: bool },
    /// The friend's copy died; ours is alive. `loot` = it died during this engagement.
    Kill { loot: bool },
}

/// A single-life boss's remote kill waits at most this long for its deathblow throw (v8.29a).
const BOSS_THROW_WAIT: Duration = Duration::from_secs(12);

/// True while the boss's own deathblow throw still plays and the cap is not reached.
pub fn boss_kill_waits(throw_active: bool, waited: Duration) -> bool {
    throw_active && waited < BOSS_THROW_WAIT
}

pub fn plan(local_lives: i32, local_dead: bool, remote_lives: i32, remote_flags: u8, throw_pending: bool, kill_waited: bool) -> Plan {
    if local_dead {
        return Plan::Nothing;
    }
    if remote_flags & LIFE_DEAD != 0 {
        if throw_pending || !kill_waited {
            return Plan::Hold;
        }
        return Plan::Kill { loot: remote_flags & LIFE_FRESH != 0 };
    }
    if remote_lives >= 0 && remote_lives < local_lives {
        if throw_pending {
            return Plan::Hold;
        }
        return Plan::Lower { to: remote_lives, refill: remote_lives > 0 };
    }
    Plan::Nothing
}

/// The friend's state for one enemy, merged over messages: lives only go down, death and
/// "fresh" are kept once seen. `first_dead` = when we first heard of the death.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remote {
    pub lives: i32,
    pub flags: u8,
}

/// After a rest (§34): the friend's lives from the first states of the engagement are a
/// baseline (live 2026-10-24: 1120300 lost a life to the host's pre-rest state on the
/// friend during the 13 s our own sync ran, and the next engagement took it back onto the
/// host's fresh copy, 2 -> 1). Only fewer lives than that baseline are a deathblow.
pub fn rest_baseline_blocks(baseline: Option<i32>, remote_lives: i32) -> bool {
    baseline.is_some_and(|b| remote_lives >= b)
}

pub fn merge(prev: Option<Remote>, lives: i8, flags: u8) -> Remote {
    match prev {
        None => Remote { lives: lives as i32, flags },
        Some(p) => Remote { lives: p.lives.min(lives as i32), flags: p.flags | flags },
    }
}

/// Our state of one enemy worth telling the friend: dead, or a multi-life enemy (NpcParam
/// deathblow count >= 2: bosses, minibosses) below its starting lives. Live 7.4: the
/// lives field `data+0x25C` reads 0 on ordinary enemies (deathblow count 1), so every one
/// of them counted as "short of lives" (`here 198 enemies short of lives`) and was sent
/// every 2 s; lives only mean something for multi-life enemies.
pub fn entry(net_id: u16, lives0: i32, lives: i32, dead: bool, fresh: bool) -> Option<LifeEntry> {
    if !dead && (!multi_life(lives0) || lives >= lives0) {
        return None;
    }
    let flags = if dead { LIFE_DEAD | if fresh { LIFE_FRESH } else { 0 } } else { 0 };
    Some((net_id, lives.clamp(-128, 127) as i8, flags))
}

/// Lives are tracked only for enemies that start with two or more (bosses, minibosses).
pub fn multi_life(lives0: i32) -> bool {
    lives0 >= 2
}

/// Our own game consumed a life of a multi-life enemy (our deathblow) but the copy did not
/// get up. Live 2026-10-24 (miniboss 1000300, 2 orbs): lives 2 -> 1 here, HP came back
/// through the host's HP sync, posture stayed low (it counts DOWN, full = max, posture.rs).
/// Due `OWN_GETUP_AFTER`..`OWN_DROP_FOR` after the drop, with a life left (`lives <
/// prev_lives`: no revive), no throw on it still applying, and posture neither near max
/// (the get-up writes max) nor risen well above its value at the drop.
/// 7.15: also due when HP is stuck at <= 1 % of max (live 7.13: HP stayed 0-1).
#[allow(clippy::too_many_arguments)]
pub fn own_getup_due(
    prev_lives: i32,
    lives: i32,
    hp: i32,
    max_hp: i32,
    posture: i32,
    max_posture: i32,
    at_drop: i32,
    elapsed: Duration,
    throw_pending: bool,
) -> bool {
    let hp_stuck = max_hp > 0 && hp <= max_hp / 100;
    let posture_stuck = max_posture > 0 && posture < max_posture - max_posture / 20 && posture <= at_drop + max_posture / 4;
    lives < prev_lives && lives > 0 && !throw_pending && elapsed >= OWN_GETUP_AFTER && elapsed < OWN_DROP_FOR && (hp_stuck || posture_stuck)
}

/// A multi-life enemy's lives dropped between two scans with a life left: a deathblow
/// took one (`prev` = lives and dead flag of the last scan). Checked BEFORE the dead test.
pub fn lives_drop(prev: Option<(i32, bool)>, lives_read: i32) -> bool {
    prev.is_some_and(|(l, dead)| !dead && l > lives_read) && lives_read > 0
}

/// HP <= 0 with a life left is the get-up in progress, not a death: on the scan that saw
/// the drop, or inside the get-up window after one. A copy that was dead already is not.
pub fn getting_up(was_dead: bool, lives_read: i32, hp: i32, drop: bool, in_window: bool) -> bool {
    !was_dead && lives_read > 0 && hp <= 0 && (drop || in_window)
}

/// Inside the get-up window HP <= 1 is refilled again (replaces the 2 s REFILL_GUARD).
pub fn getup_guard(elapsed: Duration, hp: i32, max_hp: i32) -> bool {
    elapsed < GETUP_HOLD && max_hp > 1 && hp <= 1
}

/// Lives only go down (rests and wipes revive through the game's own reset, not here).
pub fn lives_write_ok(cur: i32, to: i32) -> bool {
    to <= cur.max(0)
}

/// The boss gauge's entity or a final entity of a `bosses.rs` encounter: tracked for death
/// even with a single life (`lives0 == 1`, e.g. Way of Tomoe 1110801).
pub fn boss_tracked(entity: Option<i32>, gauge: Option<i32>) -> bool {
    entity.is_some_and(|e| e > 0 && (gauge == Some(e) || crate::bosses::encounter_of(e).is_some_and(|enc| enc.final_entities.contains(&e))))
}

/// The one writer of `data+0x25C` (also persist.rs): refuses and logs a raise.
pub(crate) fn set_lives(data: usize, cur: i32, to: i32, who: &str) -> bool {
    if !lives_write_ok(cur, to) {
        log::line(&format!("livesync: {who} would raise lives {cur} -> {to}: refused (lives only go down)"));
        debug_assert!(false, "lives raise {cur} -> {to} by {who}");
        return false;
    }
    game::write(data + DATA_LIVES, to)
}

/// A dead copy whose HP came back without a revive (the revive `0x9F4360` resets lives to
/// the NpcParam count `lives0`, a HP write does not): keep it a corpse. A death with all
/// lives left (a script's kill) cannot be told from a revive, so it is left alone.
/// A puppet (Puppeteer Ninjutsu: `statelog::owner_team_state`) fights for its owner after
/// its deathblow (Tihvert 2026-10-27: it died at once in co-op): the friend's copy dying is
/// not applied to it.
pub fn puppet_plan(puppet: bool, p: Plan) -> Plan {
    if puppet && matches!(p, Plan::Kill { .. }) { Plan::Nothing } else { p }
}

/// v8.61: whether `puppet` can change anything for this enemy this scan: the dead test (HP 0),
/// the corpse rule (dead here before) and `puppet_plan` (only a Kill, only when the friend's
/// copy is dead). Otherwise the SpEffect-list walk (4 ReadProcessMemory per node) is skipped:
/// for all ~170 enemies every 150 ms it was the v8.59 frame-time regression (mod avg 0.73 ->
/// 1.55 ms, ticks with a mod frame over 10 ms 3% -> 58% on the host, 2026-10-28).
pub fn puppet_check_needed(hp: i32, dead_before: bool, remote_dead: bool) -> bool {
    hp <= 0 || dead_before || remote_dead
}

pub fn corpse_raised(was_dead: bool, hp: i32, lives: i32, lives_at_death: i32, lives0: i32) -> bool {
    was_dead && hp > 0 && lives_at_death < lives0 && lives <= lives_at_death
}

// --- State ------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Local {
    chr: usize,
    lives0: i32,
    lives: i32,
    dead: bool,
    /// Seen alive -> dead during this engagement.
    fresh: bool,
    lives_at_death: i32,
}

#[derive(Default)]
struct Stats {
    sent: u32,
    msgs: u32,
    received: u32,
    lowered: u32,
    killed: u32,
    killed_loot: u32,
    held: u32,
    guarded: u32,
    corpses: u32,
    /// Ignored right after our death reload (`DEATH_GUARD`).
    stale: u32,
}

#[derive(Default)]
struct State {
    key: u64,
    local: HashMap<u16, Local>,
    sent: HashMap<u16, LifeEntry>,
    remote: HashMap<u16, (Remote, Instant)>,
    /// Get-up window per enemy: since the lives drop (`GETUP_HOLD`).
    hold: HashMap<u16, Instant>,
    /// Our own game took a life here: (when, lives before, posture at the drop).
    own_drop: HashMap<u16, (Instant, i32, i32)>,
    held_logged: HashMap<u16, Instant>,
    /// Boss kills waiting for our deathblow throw to end: (first deferred, line logged).
    boss_wait: HashMap<u16, (Instant, bool)>,
    /// This engagement started within `REST_GUARD` of our rest: since when; the friend's
    /// baseline lives per netId (`rest_baseline_blocks`), and the netIds logged for it.
    rest_start: Option<Instant>,
    baseline: HashMap<u16, i32>,
    baseline_logged: Vec<u16>,
    death_guard_logged: bool,
    wipe_seen: bool,
    wipe_log_at: Option<Instant>,
    scan_at: Option<Instant>,
    resend_at: Option<Instant>,
    stats_at: Option<Instant>,
    stats: Stats,
    logs: u32,
    /// Puppets logged this engagement.
    puppets: HashSet<u16>,
}

impl State {
    fn log(&mut self, msg: String) {
        self.logs += 1;
        if self.logs <= MAX_LOGS || self.logs % 50 == 0 {
            log::line(&msg);
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub(crate) fn lives0_of(chr: Chr) -> i32 {
    read_ptr(chr.0 + CHR_PARAM_REF)
        .and_then(|r| read_ptr(r + REF_ROW))
        .and_then(|row| read::<i8>(row + NPC_DEATHBLOWS))
        .map_or(1, |n| (n as i32).max(1))
}

/// (data module, HP, max HP, max posture, lives) in one read.
pub(crate) fn read_data(chr: Chr) -> Option<(usize, i32, i32, i32, i32)> {
    let data = posture::data_of(chr)?;
    let mut b = [0u8; DATA_BLOCK_LEN];
    if !read_bytes(data + DATA_BLOCK, &mut b) {
        return None;
    }
    let at = |off: usize| i32::from_le_bytes(b[off - DATA_BLOCK..off - DATA_BLOCK + 4].try_into().unwrap_or([0; 4]));
    Some((data, at(DATA_HP), at(DATA_MAX_HP), at(DATA_MAX_POSTURE), at(DATA_LIVES)))
}

/// Current posture (`data+0x148`); read_data's tuple is shared with other modules.
fn read_posture(data: usize) -> Option<i32> {
    read::<i32>(data + DATA_POSTURE)
}

/// Our copy gets up for its next life (as after a vanilla deathblow with lives left).
fn refill(chr: Chr, data: usize, max_hp: i32, max_posture: i32) {
    if max_hp > 0 {
        posture::set_hp(chr, max_hp);
    }
    if max_posture > 0 {
        posture::add_posture(data, max_posture, false);
    }
}

/// Every frame while connected (puppet.rs, after restsync): does nothing unless the
/// native session owns the enemies. The states of every engaged player come from
/// nsession (one merged stream), so `peers` is not needed here.
pub fn tick(enemies: &Enemies, _peers: &PeerSet) {
    let key = if ENABLED.load(Ordering::Relaxed) { nsession::engaged_key() } else { 0 };
    let Ok(mut g) = STATE.lock() else { return };
    let st = g.get_or_insert_with(State::default);
    let now = Instant::now();
    if key != st.key {
        if st.key != 0 {
            stats_line(st, true);
        }
        let logs = st.logs;
        *st = State { key, logs, ..Default::default() };
        if key != 0 {
            log::line(&format!("livesync: on for this engagement (key {:#x})", key as u32));
            if crate::restsync::last_rest().is_some_and(|t| t.elapsed() < REST_GUARD) {
                st.rest_start = Some(now);
                log::line("livesync: this engagement follows a rest: the friend's first lives states are a baseline, not deathblows");
            }
        }
    }
    if key == 0 {
        // Not engaged: whatever the friend sent is for another engagement.
        let _ = nsession::take_lives();
        return;
    }
    // Right after a death reload of ours the others' states can still be the old world's
    // (a wipe where they reload later): not kept at all, since lives only go down in
    // `merge` and a stale 0 would apply once the guard ends (v8.6, persist.rs).
    let after_death = crate::persist::after_death_guard(DEATH_GUARD);
    if after_death && !st.death_guard_logged {
        st.death_guard_logged = true;
        log::line("livesync: right after our death reload: the others' lives states are ignored for a while (they may still be in the old world)");
    }
    // A wipe within WIPE_STALE: the friend's states are from before the reset (v8.25c). On the
    // rising edge everything kept from before is dropped too.
    let wiped = crate::persist::wipe_recent(crate::persist::WIPE_STALE);
    if wiped && !st.wipe_seen {
        st.remote.clear();
        st.baseline.clear();
        st.baseline_logged.clear();
        st.boss_wait.clear();
        st.sent.clear();
    }
    st.wipe_seen = wiped;
    for (n, lives, flags) in nsession::take_lives() {
        if wiped {
            st.stats.stale += 1;
            if st.wipe_log_at.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10)) {
                st.wipe_log_at = Some(now);
                log::line(&format!("livesync: dropped lives from peer {n:#x}: wipe recent (stale before the wipe)"));
            }
            continue;
        }
        if after_death {
            st.stats.stale += 1;
            continue;
        }
        st.stats.received += 1;
        let prev = st.remote.get(&n).map(|(r, _)| *r);
        let m = merge(prev, lives, flags);
        if st.rest_start.is_some_and(|t| now.duration_since(t) <= REST_BASELINE_FOR) && flags & LIFE_DEAD == 0 {
            let b = st.baseline.entry(n).or_insert(lives as i32);
            *b = (*b).min(lives as i32);
        }
        let at = match st.remote.get(&n) {
            Some((r, at)) if r.flags & LIFE_DEAD != 0 || m.flags & LIFE_DEAD == 0 => *at,
            _ => now,
        };
        st.remote.insert(n, (m, at));
    }
    if st.scan_at.is_some_and(|t| now.duration_since(t) < SCAN_EVERY) {
        return;
    }
    st.scan_at = Some(now);
    let resend = st.resend_at.is_none_or(|t| now.duration_since(t) >= RESEND_EVERY);
    if resend {
        st.resend_at = Some(now);
    }
    let mut out: Vec<LifeEntry> = Vec::new();
    let gauge = game::boss_state().map(|b| b.entity);
    for chr in nsession::enemy_chrs() {
        let Some(n) = netai::net_id(chr) else { continue };
        let Some((data, mut hp, max_hp, max_posture, mut lives)) = read_data(chr) else { continue };
        let prev = st.local.get(&n).copied().filter(|p| p.chr == chr.0);
        let lives0 = prev.map_or_else(|| lives0_of(chr), |p| p.lives0);
        // A puppet is alive for us: no corpse rule, no kill for the friend's copy, never sent
        // as dead (its deathblow reads HP 0 for a moment, and death is final on the friend).
        // v8.61: the SpEffect walk only where it can matter (`puppet_check_needed`): for every
        // enemy on every scan it doubled the mod's frame cost (live 2026-10-28).
        let remote_dead = st.remote.get(&n).is_some_and(|(r, _)| r.flags & LIFE_DEAD != 0);
        let owner_team = if puppet_check_needed(hp, prev.is_some_and(|p| p.dead), remote_dead) {
            crate::statelog::owner_team_state(chr.0)
        } else {
            None
        };
        let puppet = owner_team.is_some_and(|(_, on)| on);
        if let Some((sp, on)) = owner_team
            && st.puppets.insert(n)
        {
            let entity = chr.entity_id().unwrap_or(-1);
            log::line(&format!(
                "livesync: enemy {entity} netId {n:#x}: SpEffect {sp} (stateInfo 302, owner's team) at HP {hp}/{max_hp}: {}",
                if on { "a puppet: kept alive here" } else { "chr type is not 2, the game keeps its team: not a puppet" }
            ));
        }
        // A dead copy here stays a corpse (HP from the owner's type 5 for its living copy).
        if let Some(p) = prev
            && !puppet
            && corpse_raised(p.dead, hp, lives, p.lives_at_death, lives0)
        {
            if game::write(data + DATA_HP, 0i32) {
                st.stats.corpses += 1;
                st.log(format!("livesync: enemy netId {n:#x} is dead here, HP {hp} came back (the friend's copy alive?): kept at 0"));
            }
            hp = 0;
        }
        let lives_read = lives;
        let multi = multi_life(lives0);
        let tracked = multi || boss_tracked(chr.entity_id(), gauge);
        // A lives drop with a life left is a get-up, never a death: look at it BEFORE the
        // dead test (the same scan can read HP 0).
        let drop = multi && lives_drop(prev.map(|p| (p.lives, p.dead)), lives_read);
        let in_window = st.hold.get(&n).is_some_and(|t| now.duration_since(*t) < GETUP_HOLD);
        let up = multi && getting_up(prev.is_some_and(|p| p.dead), lives_read, hp, drop, in_window);
        let mut dead = hp <= 0 && !up && !puppet;
        if dead && let Some((t0, _)) = st.boss_wait.remove(&n) {
            log::line(&format!("livesync: boss netId {n:#x}: died by itself after {} ms (no ForceDead)", now.duration_since(t0).as_millis()));
        }
        // One clear line per boss kill (owner 2026-10-25: the log must show one boss once).
        if tracked && dead && prev.is_some_and(|p| !p.dead) {
            log::line(&format!(
                "boss: DEFEATED entity {} netId {n:#x} (lives {lives_read}/{lives0}, max HP {max_hp}, seen here: {})",
                chr.entity_id().unwrap_or(-1),
                if nsession::owned_here(n) { "owned here" } else { "owned by another player" }
            ));
        }
        let mut refilled_now = false;
        if drop {
            let before = prev.map_or(lives_read, |p| p.lives);
            let at_drop = read_posture(data).unwrap_or(0);
            let owned = nsession::owned_here(n);
            st.own_drop.insert(n, (now, before, at_drop));
            st.hold.insert(n, now);
            let entity = chr.entity_id().unwrap_or(-1);
            st.log(format!(
                "livesync: enemy {entity} netId {n:#x}: lives {before} -> {lives_read} (seen here), getting up{}",
                if owned { "" } else { " (NetAI copy: HP and posture full at once)" }
            ));
            if !owned {
                refill(chr, data, max_hp, max_posture);
                refilled_now = true;
            }
        }
        let mut fresh = prev.is_some_and(|p| p.fresh || (!p.dead && dead));
        // The friend's state.
        if let Some(&(r, since)) = st.remote.get(&n) {
            let throw_pending = nsession::recent_throw(n, THROW_HOLD_MS).is_some_and(|sent| sent || nsession::owned_here(n));
            let waited = now.duration_since(since) >= KILL_GRACE;
            let mut p = puppet_plan(puppet, plan(lives, dead, r.lives, r.flags, throw_pending, waited));
            // Lives of single-life enemies are not tracked (their field reads 0); their
            // death is (Kill / Hold, also for `tracked` single-life bosses).
            if matches!(p, Plan::Lower { .. }) && !multi {
                p = Plan::Nothing;
            }
            if matches!(p, Plan::Lower { .. }) && rest_baseline_blocks(st.baseline.get(&n).copied(), r.lives) {
                p = Plan::Nothing;
                if !st.baseline_logged.contains(&n) {
                    st.baseline_logged.push(n);
                    let entity = chr.entity_id().unwrap_or(-1);
                    st.log(format!(
                        "livesync: enemy {entity} netId {n:#x}: the friend's lives {} in its first state after a rest (ours {lives}): not taken (its copy from before the rest?)",
                        r.lives
                    ));
                }
            }
            match p {
                Plan::Nothing => {}
                Plan::Hold => {
                    if st.held_logged.get(&n).is_none_or(|t| now.duration_since(*t) >= Duration::from_secs(5)) {
                        st.held_logged.insert(n, now);
                        st.stats.held += 1;
                        if throw_pending {
                            st.log(format!(
                                "livesync: enemy netId {n:#x}: the friend has lives {} {}, ours {lives}; a native throw on it applies here first",
                                r.lives,
                                if r.flags & LIFE_DEAD != 0 { "(dead)" } else { "" }
                            ));
                        }
                    }
                }
                Plan::Lower { to, refill: up } => {
                    if set_lives(data, lives, to, "Plan::Lower") {
                        posture::life_sfx(chr);
                        if up {
                            refill(chr, data, max_hp, max_posture);
                            refilled_now = true;
                            st.hold.insert(n, now);
                        }
                        st.stats.lowered += 1;
                        let entity = chr.entity_id().unwrap_or(-1);
                        st.log(format!(
                            "livesync: enemy {entity} netId {n:#x}: lives {lives} -> {to} (the friend's deathblow){}",
                            if up { ", gets up: HP and posture full" } else { "" }
                        ));
                        lives = to;
                    }
                }
                Plan::Kill { .. } if tracked && !multi && {
                    // v8.29a: the friend's deathblow on our boss still plays here: ForceDead
                    // would cut the death animation. Wait (capped); the plan is re-run each scan.
                    let first = *st.boss_wait.entry(n).or_insert_with(|| (now, false));
                    let throw = crate::enemies::throw_active(chr);
                    let waits = boss_kill_waits(throw, now.duration_since(first.0));
                    if waits {
                        if !first.1 {
                            st.boss_wait.insert(n, (first.0, true));
                            log::line(&format!("livesync: boss netId {n:#x}: the friend's copy died, ours still in the deathblow (throw active): ForceDead waits"));
                        }
                        st.stats.held += 1;
                    } else {
                        st.boss_wait.remove(&n);
                        if first.0 != now {
                            log::line(&format!(
                                "livesync: boss netId {n:#x}: throw over / 12 s cap after {} ms: ForceDead",
                                now.duration_since(first.0).as_millis()
                            ));
                        }
                    }
                    waits
                } => {}
                Plan::Kill { loot: fresh_there } => {
                    let entity = chr.entity_id().unwrap_or(-1);
                    // Paid once per enemy per rest cycle (persist.rs): the friend's kill
                    // pays us unless we were already paid for this enemy (our own kill
                    // before a death reload, a restore). Live 7.4: the old rule ("fresh" =
                    // seen dying during this engagement) called every kill the friend made
                    // while we were disengaged "an old death": 28 of 29 kills unpaid.
                    let loot = entity > 0 && !crate::persist::rewarded(entity);
                    set_lives(data, lives, 0, "Plan::Kill");
                    let ok = if entity > 0 { enemies.force_dead_one(entity, loot) } else { posture::set_hp(chr, 0) };
                    st.stats.killed += 1;
                    if loot {
                        st.stats.killed_loot += 1;
                    }
                    st.log(format!(
                        "livesync: enemy {entity} netId {n:#x} died on another player's side{}{}: killed here too ({}, {}, ok = {ok})",
                        if fresh_there { " (during this engagement)" } else { " (earlier)" },
                        if tracked && !multi { " (single-life boss)" } else { "" },
                        if entity > 0 { "ForceDead, no game reward" } else { "no entity id: HP setter 0" },
                        if loot { "our own loot once (not paid here yet)" } else { "already paid here: no loot" }
                    ));
                    lives = 0;
                    // Counted as dead now: the scan sees HP 0 next time; no second kill.
                    dead = true;
                    fresh = false;
                }
            }
        }
        // A lives drop was recorded above (`own_drop`): an owned copy gets `OWN_GETUP_AFTER`
        // to get up by itself, then we refill it (HP stuck <= 1 % or posture not refilled).
        if multi && !dead {
            if let Some(&(t, before, at_drop)) = st.own_drop.get(&n) {
                let elapsed = now.duration_since(t);
                if elapsed >= OWN_DROP_FOR || lives_read >= before {
                    st.own_drop.remove(&n);
                } else if let Some(posture_now) = read_posture(data) {
                    let throw_pending = nsession::recent_throw(n, 1000).is_some();
                    if !refilled_now && own_getup_due(before, lives_read, hp, max_hp, posture_now, max_posture, at_drop, elapsed, throw_pending) {
                        refill(chr, data, max_hp, max_posture);
                        refilled_now = true;
                        st.own_drop.remove(&n);
                        let entity = chr.entity_id().unwrap_or(-1);
                        st.log(format!(
                            "livesync: enemy {entity} netId {n:#x}: our deathblow took a life here but the copy did not get up (HP {hp}/{max_hp}, posture {posture_now}/{max_posture}): HP and posture full"
                        ));
                    }
                }
            }
        } else if dead {
            st.own_drop.remove(&n);
        }
        // Get-up window: HP <= 1 (the deathblow's trailing hit, the friend's stale type-5 HP)
        // is refilled again, on either side. An owned copy is left alone until
        // `OWN_GETUP_AFTER` (the game's own get-up is running).
        if let Some(&t) = st.hold.get(&n) {
            let elapsed = now.duration_since(t);
            if elapsed >= GETUP_HOLD || dead {
                st.hold.remove(&n);
            } else if !refilled_now && lives > 0 && getup_guard(elapsed, hp, max_hp) && (elapsed >= OWN_GETUP_AFTER || !nsession::owned_here(n)) {
                posture::set_hp(chr, max_hp);
                st.stats.guarded += 1;
                st.log(format!("livesync: enemy netId {n:#x} at {hp} HP in its get-up window: HP full again"));
            }
        }
        let lives_at_death = match prev {
            Some(p) if p.dead => p.lives_at_death,
            _ => lives,
        };
        st.local.insert(n, Local { chr: chr.0, lives0, lives, dead, fresh, lives_at_death });
        if let Some(e) = entry(n, lives0, lives, dead, fresh)
            && (resend || st.sent.get(&n) != Some(&e))
        {
            st.sent.insert(n, e);
            out.push(e);
        }
    }
    if !out.is_empty() {
        st.stats.sent += out.len() as u32;
        for chunk in out.chunks(nsession::MAX_LIVES) {
            st.stats.msgs += 1;
            nsession::send_lives(chunk.to_vec());
        }
    }
    if st.stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
        if st.stats_at.is_some() {
            stats_line(st, false);
        }
        st.stats_at = Some(now);
    }
}

fn stats_line(st: &mut State, end: bool) {
    let s = std::mem::take(&mut st.stats);
    let reduced = st.local.values().filter(|l| !l.dead && multi_life(l.lives0) && l.lives < l.lives0).count();
    let dead = st.local.values().filter(|l| l.dead).count();
    log::line(&format!(
        "livesync ({}): sent {} states in {} msgs, received {}, lowered {}, killed {} ({} with loot), held {}, get-up refills guarded {}, corpses kept dead {}, ignored after our death reload {}; here {reduced} enemies short of lives, {dead} dead",
        if end { "engagement over" } else { "10 s" },
        s.sent,
        s.msgs,
        s.received,
        s.lowered,
        s.killed,
        s.killed_loot,
        s.held,
        s.guarded,
        s.corpses,
        s.stale
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_lives_after_a_rest_are_a_baseline() {
        // Live 2026-10-24: the friend's first state after the rest said 1120300 lives 1
        // (its copy had taken the host's pre-rest lives); the host's fresh copy had 2.
        assert!(rest_baseline_blocks(Some(1), 1));
        assert_eq!(plan(2, false, 1, 0, false, true), Plan::Lower { to: 1, refill: true });
        // A deathblow after that baseline still counts.
        assert!(!rest_baseline_blocks(Some(1), 0));
        assert!(!rest_baseline_blocks(Some(2), 1));
        // No baseline (no rest, or the enemy first reported later): as before.
        assert!(!rest_baseline_blocks(None, 1));
    }

    #[test]
    fn friend_deathblow_lowers_and_refills_once() {
        // Live case: 2-deathblow miniboss, the friend took one (2 -> 1), ours still 2.
        assert_eq!(plan(2, false, 1, 0, false, true), Plan::Lower { to: 1, refill: true });
        // Already applied (our own copy is at 1 now): nothing more.
        assert_eq!(plan(1, false, 1, 0, false, true), Plan::Nothing);
        // Ours is lower (our deathblow not on the friend yet): never raised.
        assert_eq!(plan(1, false, 2, 0, false, true), Plan::Nothing);
        // Last life gone there but still alive (same frame as the kill): down to 0, no refill.
        assert_eq!(plan(1, false, 0, 0, false, true), Plan::Lower { to: 0, refill: false });
    }

    #[test]
    fn death_on_either_side_is_applied_once_and_never_revived() {
        // The friend's copy died during this engagement: ours dies, our own loot once.
        assert_eq!(plan(1, false, 0, LIFE_DEAD | LIFE_FRESH, false, true), Plan::Kill { loot: true });
        // An old death (restored after a load): no loot.
        assert_eq!(plan(2, false, 0, LIFE_DEAD, false, true), Plan::Kill { loot: false });
        // The native kill (type 5) gets its 0.4 s first.
        assert_eq!(plan(1, false, 0, LIFE_DEAD | LIFE_FRESH, false, false), Plan::Hold);
        // Dead here: nothing, whatever the friend says.
        assert_eq!(plan(0, true, 2, 0, false, true), Plan::Nothing);
        assert_eq!(plan(0, true, 0, LIFE_DEAD, false, true), Plan::Nothing);
    }

    #[test]
    fn a_native_throw_applying_here_holds_the_merge() {
        // Our deathblow is running (we sent the throw), or the friend's lands on an enemy we
        // own: the native path takes the life here; the merge waits.
        assert_eq!(plan(2, false, 1, 0, true, true), Plan::Hold);
        assert_eq!(plan(1, false, 0, LIFE_DEAD | LIFE_FRESH, true, true), Plan::Hold);
    }

    #[test]
    fn merge_keeps_the_minimum_and_death() {
        let a = merge(None, 2, 0);
        assert_eq!(a, Remote { lives: 2, flags: 0 });
        let b = merge(Some(a), 1, 0);
        assert_eq!(b, Remote { lives: 1, flags: 0 });
        // An older message (resend order) never raises it again.
        assert_eq!(merge(Some(b), 2, 0), Remote { lives: 1, flags: 0 });
        let d = merge(Some(b), 0, LIFE_DEAD | LIFE_FRESH);
        assert_eq!(merge(Some(d), 1, 0), Remote { lives: 0, flags: LIFE_DEAD | LIFE_FRESH });
    }

    #[test]
    fn lives_are_the_minimum_over_every_player_in_any_order() {
        // Three other players' states of one 3-life boss: 2 lives, 1 life, still 3.
        let states: [(i8, u8); 3] = [(2, 0), (1, 0), (3, 0)];
        for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0], [2, 0, 1]] {
            let r = order.iter().fold(None, |acc, &i| Some(merge(acc, states[i].0, states[i].1))).unwrap();
            assert_eq!(r, Remote { lives: 1, flags: 0 }, "{order:?}");
            assert_eq!(plan(3, false, r.lives, r.flags, false, true), Plan::Lower { to: 1, refill: true });
        }
        // One of them saw it die: dead for everyone, whatever came after.
        let dead = [(1i8, 0u8), (0, LIFE_DEAD | LIFE_FRESH), (2, 0)];
        for order in [[0, 1, 2], [2, 1, 0], [1, 0, 2]] {
            let r = order.iter().fold(None, |acc, &i| Some(merge(acc, dead[i].0, dead[i].1))).unwrap();
            assert_eq!(r, Remote { lives: 0, flags: LIFE_DEAD | LIFE_FRESH }, "{order:?}");
            assert_eq!(plan(2, false, r.lives, r.flags, false, true), Plan::Kill { loot: true });
        }
    }

    #[test]
    fn only_reduced_or_dead_enemies_are_sent() {
        assert_eq!(entry(0x83F, 2, 2, false, false), None);
        assert_eq!(entry(0x83F, 2, 1, false, false), Some((0x83F, 1, 0)));
        assert_eq!(entry(0x821, 1, 0, true, true), Some((0x821, 0, LIFE_DEAD | LIFE_FRESH)));
        assert_eq!(entry(0x821, 1, 1, true, false), Some((0x821, 1, LIFE_DEAD)));
        assert_eq!(entry(1, 1, 500, false, false), None);
        assert_eq!(entry(1, 200, -300, false, false), Some((1, -128, 0)));
        // Live 7.4: an ordinary enemy (1 deathblow) reads lives 0 while alive: not news.
        assert_eq!(entry(0x900, 1, 0, false, false), None);
        assert!(!multi_life(1) && multi_life(2));
    }

    #[test]
    fn the_puppet_walk_runs_only_where_it_can_change_the_plan() {
        // Alive here, alive on the friend: no corpse rule, no dead test, no Kill to cancel.
        assert!(!puppet_check_needed(500, false, false));
        assert!(puppet_check_needed(0, false, false), "HP 0: the dead test");
        assert!(puppet_check_needed(500, true, false), "dead before: the corpse rule");
        assert!(puppet_check_needed(500, false, true), "the friend's copy dead: a Kill");
        // Every plan that puppet_plan changes needs the friend's copy dead.
        for (lives, rl, fl) in [(1, 1, 0u8), (2, 1, 0), (1, 0, 0), (1, 1, LIFE_FRESH)] {
            let p = plan(lives, false, rl, fl, false, true);
            assert_eq!(puppet_plan(true, p), p, "no Kill without LIFE_DEAD: {p:?}");
        }
    }

    #[test]
    fn a_puppet_is_not_killed_for_the_friends_dead_copy() {
        let kill = plan(0, false, 0, LIFE_DEAD, false, true);
        assert!(matches!(kill, Plan::Kill { .. }));
        assert_eq!(puppet_plan(true, kill), Plan::Nothing);
        assert_eq!(puppet_plan(false, kill), kill);
        let hold = plan(0, false, 0, LIFE_DEAD, true, true);
        assert_eq!(puppet_plan(true, hold), hold);
    }

    #[test]
    fn a_raised_corpse_is_told_from_a_revive() {
        // Died with 0 lives, HP written back by the owner's type 5: still a corpse.
        assert!(corpse_raised(true, 152, 0, 0, 2));
        // The revive (rest) resets lives to the NpcParam count: alive again.
        assert!(!corpse_raised(true, 152, 2, 0, 2));
        assert!(!corpse_raised(false, 152, 0, 0, 2));
        assert!(!corpse_raised(true, 0, 0, 0, 2));
        // Killed by a script with every life left: a revive looks the same, left alone.
        assert!(!corpse_raised(true, 152, 1, 1, 1));
    }
}

#[cfg(test)]
mod own_getup_tests {
    use super::*;

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn due_after_grace_with_low_posture() {
        // 2 -> 1, posture near 0 (counts down), 2 s later: refill.
        assert!(own_getup_due(2, 1, 500, 1000, 0, 100, 0, MS(2000), false));
    }

    #[test]
    fn due_at_1_5_s_with_hp_stuck_at_1_and_posture_broken() {
        // Live 7.13: HP 1 of 5373, posture broken, lives 2 -> 1: refill at 1.5 s.
        assert!(own_getup_due(2, 1, 1, 5373, 0, 100, 0, MS(1500), false));
        // HP stuck alone is enough (posture already full).
        assert!(own_getup_due(2, 1, 1, 5373, 100, 100, 0, MS(1500), false));
        assert!(!own_getup_due(2, 1, 1, 5373, 0, 100, 0, MS(1499), false));
    }

    #[test]
    fn a_drop_is_recorded_when_the_scan_also_reads_hp_0() {
        // lives 2 -> 1 and HP 0 in the same scan: a drop, a get-up, not dead.
        assert!(lives_drop(Some((2, false)), 1));
        assert!(getting_up(false, 1, 0, true, false));
        // Inside the window a later HP 0 is still the get-up.
        assert!(getting_up(false, 1, 0, false, true));
        // Last life gone (lives 0): a death, not a drop.
        assert!(!lives_drop(Some((1, false)), 0));
        assert!(!getting_up(false, 0, 0, false, true));
        // No drop, no window (a script kill with lives left), or dead before: dead.
        assert!(!getting_up(false, 2, 0, false, false));
        assert!(!getting_up(true, 1, 0, true, true));
        assert!(!lives_drop(None, 1) && !lives_drop(Some((1, false)), 1) && !lives_drop(Some((2, true)), 1));
    }

    #[test]
    fn the_get_up_window_refills_hp_back_at_1_until_6_s() {
        assert!(getup_guard(MS(2500), 1, 5373));
        assert!(getup_guard(MS(2500), 0, 5373));
        assert!(!getup_guard(MS(6100), 1, 5373));
        assert!(!getup_guard(MS(2500), 400, 5373));
    }

    #[test]
    fn lives_are_never_raised() {
        assert!(lives_write_ok(2, 1) && lives_write_ok(1, 1) && lives_write_ok(1, 0) && lives_write_ok(-1, 0));
        assert!(!lives_write_ok(1, 2));
        assert_eq!(plan(1, false, 2, 0, false, true), Plan::Nothing);
        assert_eq!(plan(0, false, 2, 0, false, true), Plan::Nothing);
    }

    #[test]
    fn a_single_life_boss_death_is_applied_and_held_for_a_throw() {
        // Way of Tomoe 1110801: lives0 1, lives read 0 while alive.
        assert!(boss_tracked(Some(1110801), None));
        assert!(boss_tracked(Some(5), Some(5)));
        assert!(!boss_tracked(Some(5), Some(6)) && !boss_tracked(None, None));
        assert_eq!(plan(0, false, 0, LIFE_DEAD | LIFE_FRESH, false, true), Plan::Kill { loot: true });
        assert_eq!(plan(0, false, 0, LIFE_DEAD | LIFE_FRESH, true, true), Plan::Hold);
        assert_eq!(plan(0, false, 0, LIFE_DEAD, false, false), Plan::Hold);
        assert_eq!(entry(0x900, 1, 0, true, true), Some((0x900, 0, LIFE_DEAD | LIFE_FRESH)));
    }

    #[test]
    fn boss_kill_waits_only_during_a_throw_and_under_the_cap() {
        assert!(boss_kill_waits(true, Duration::ZERO));
        assert!(boss_kill_waits(true, Duration::from_millis(11_999)));
        assert!(!boss_kill_waits(true, Duration::from_secs(12)));
        assert!(!boss_kill_waits(false, Duration::ZERO));
        assert!(!boss_kill_waits(false, Duration::from_secs(5)));
    }

    #[test]
    fn not_due_early_late_or_when_it_got_up() {
        assert!(!own_getup_due(2, 1, 500, 1000, 0, 100, 0, MS(1000), false));
        assert!(!own_getup_due(2, 1, 500, 1000, 0, 100, 0, MS(6000), false));
        // The get-up refilled posture to max (or within 5 %).
        assert!(!own_getup_due(2, 1, 500, 1000, 100, 100, 0, MS(2000), false));
        assert!(!own_getup_due(2, 1, 500, 1000, 96, 100, 0, MS(2000), false));
    }

    #[test]
    fn not_due_with_throw_pending_no_life_left_or_revive() {
        assert!(!own_getup_due(2, 1, 500, 1000, 0, 100, 0, MS(2000), true));
        assert!(!own_getup_due(1, 0, 500, 1000, 0, 100, 0, MS(2000), false));
        // Lives back at the earlier count (revive / friend's state): nothing to do.
        assert!(!own_getup_due(2, 2, 500, 1000, 0, 100, 0, MS(2000), false));
        // Posture already recovered well above its value at the drop.
        assert!(!own_getup_due(2, 1, 500, 1000, 80, 100, 0, MS(2000), false));
    }
}
