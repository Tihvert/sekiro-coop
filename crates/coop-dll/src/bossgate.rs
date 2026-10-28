//! Boss start gate (docs/IMPLEMENTATION-NOTES.md §24, NOT verified live). Setting
//! `boss_start_gate` ([GAMEPLAY], default 1).
//!
//! Story bosses start through cutscenes and talk scripts that set map / talk flags. The
//! friend's flags are mirrored here (world.rs), so a fight the friend starts could put our
//! world into "fight running" while we are far away (no fog gate, no cutscene): a
//! soft-lock when we arrive. A fight can still be started alone; only the mirroring waits:
//!
//! - "Away" = the friend is not in our map block or more than 60 m from us.
//! - Gated flags: map flags (group 1 / 2, `1AABBkxxx`), talk flags (group 7) and the NPC
//!   state machines 1000-1999. Everything else (global state, items, deaths) passes.
//! - While away, every gated flag of the friend waits 4 s (`PRE_HOLD`), a flag learned as
//!   a boss start flag waits 90 s (cutscenes). When the friend's `EV_BOSS(1)` arrives, all
//!   waiting flags become the fight's held flags, and gated flags applied in the 15 s before
//!   it (the first fight against a boss, nothing learned yet) are switched back here quietly
//!   (`World::write_quiet`: the friend is not told) and held too. During the fight every
//!   gated flag of the friend is held.
//! - Held flags are applied when we come within 60 m of the friend while the fight runs
//!   ("entered the arena"), or when the boss dies (`EV_BOSS(2)`: the kill reaches our world
//!   with the start flags, all inside ownworld.rs's boss window). The fight ending any other
//!   way (gauge gone, the friend died / rested / loaded, link down) drops them: the friend's
//!   own world resets them on its reload, and those resets are never sent (world.rs ignores
//!   writes in the first 5 s after a load).
//! - Learned flags (`%LOCALAPPDATA%\sekiro-coop\boss_start_flags.txt`, "entity flag flag
//!   ..."): the gated flags that were held / switched back at a friend's fight start, and
//!   our own flags switched on from 10 s before to 3 s after our own boss gauge appeared
//!   (bossrules.rs flag recorder).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::game;
use crate::log;
use crate::peers::PeerId;
use crate::world::{self, World};

/// Settings file `boss_start_gate` ([GAMEPLAY], default 1).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

/// Nearer than this to the friend (same block) = in their fight.
pub const NEAR_M: f32 = 60.0;
const PRE_HOLD: Duration = Duration::from_secs(4);
const PRE_HOLD_LEARNED: Duration = Duration::from_secs(90);
/// Gated flags applied this long before the friend's `EV_BOSS(1)` belong to its start.
const START_WINDOW: Duration = Duration::from_secs(15);
const RECENT_KEEP: Duration = Duration::from_secs(20);
/// Own fight: flags of the recorder from 10 s before to 3 s after our gauge appeared.
const OWN_BEFORE_MS: i64 = 10_000;
const OWN_AFTER_MS: i64 = 3_000;
const MAX_PER_BOSS: usize = 64;
const MAX_BOSSES: usize = 256;
const MAX_HELD: usize = 4096;
const SAVE_AFTER: Duration = Duration::from_secs(5);

/// Group 1 / 2 map flags, group 7 talk flags, the NPC states 1000-1999.
pub fn gated_kind(id: u32) -> bool {
    match id / 10_000_000 % 10 {
        1 | 2 | 7 => id >= 10_000_000,
        0 => (1000..2000).contains(&id),
        _ => false,
    }
}

/// "entity flag flag ..." per line; bad lines and tokens are skipped.
pub fn parse_learned(text: &str) -> HashMap<i32, Vec<u32>> {
    let mut out: HashMap<i32, Vec<u32>> = HashMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        let mut it = line.split_whitespace();
        let Some(entity) = it.next().and_then(|s| s.trim_end_matches(':').parse::<i32>().ok()) else { continue };
        let flags = out.entry(entity).or_default();
        for f in it.filter_map(|s| s.parse::<u32>().ok()).filter(|&f| gated_kind(f)) {
            if !flags.contains(&f) && flags.len() < MAX_PER_BOSS {
                flags.push(f);
            }
        }
        if out.len() > MAX_BOSSES {
            break;
        }
    }
    out.retain(|_, v| !v.is_empty());
    out
}

pub fn format_learned(learned: &HashMap<i32, Vec<u32>>) -> String {
    let mut bosses: Vec<_> = learned.iter().collect();
    bosses.sort_by_key(|(e, _)| **e);
    let mut s = String::from("# Boss start flags learned by the co-op mod (bossgate.rs): boss entity, then flag ids.\n");
    for (e, flags) in bosses {
        let mut f = flags.clone();
        f.sort_unstable();
        s += &format!("{e}: {}\n", f.iter().map(u32::to_string).collect::<Vec<_>>().join(" "));
    }
    s
}

fn learned_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("boss_start_flags.txt")
}

#[derive(Clone, Copy)]
struct Fight {
    entity: i32,
    since: Instant,
}

/// The gate of one peer's fights (PLAN-MULTIPLAYER P4 "bossgate per fight"): its flags
/// (the origin a host event carries, world.rs) wait while we are away from THAT peer, and
/// its `EV_BOSS` starts / ends its fight. With two players there is one: the friend's.
#[derive(Default)]
struct PeerGate {
    /// Context from the last frame (bossrules.rs `tick`).
    ready: bool,
    away: bool,
    epoch: u64,
    fight: Option<Fight>,
    /// Waiting flags of this peer while no fight runs: (id, on, since, learned).
    pre: Vec<(u32, bool, Instant, bool)>,
    /// This peer's fight: flags held until we are near or the boss dies.
    held: Vec<(u32, bool)>,
    /// Gated flags of this peer we applied from `pre` while away: (when, id, value before, value).
    recent: VecDeque<(Instant, u32, bool, bool)>,
    revert_due: bool,
    defeat_due: bool,
    end_due: Option<&'static str>,
    entered_logged: bool,
    /// The previous frame `tick_peer` ran (for the cutscene freeze).
    last_tick: Option<Instant>,
}

impl PeerGate {
    fn hold(&mut self, id: u32, on: bool) {
        self.held.retain(|(i, _)| *i != id);
        if self.held.len() < MAX_HELD {
            self.held.push((id, on));
        }
    }

    /// Drops everything waiting / held; returns how many flags that were.
    fn clear(&mut self, who: PeerId, why: &str) -> u32 {
        let n = self.pre.len() + self.held.len();
        if n > 0 {
            log::line(&format!(
                "bossgate: peer {}: {why}: dropped {} waiting and {} held flags",
                who.slot(),
                self.pre.len(),
                self.held.len()
            ));
        }
        self.pre.clear();
        self.held.clear();
        self.recent.clear();
        self.fight = None;
        self.revert_due = false;
        self.defeat_due = false;
        self.end_due = None;
        n as u32
    }
}

#[derive(Default)]
struct Gate {
    peers: BTreeMap<PeerId, PeerGate>,
    // Learned start flags.
    loaded: bool,
    learned: HashMap<i32, Vec<u32>>,
    learned_all: HashSet<u32>,
    changed_at: Option<Instant>,
    // Stats.
    pre_held: u32,
    fight_held: u32,
    applied_late: u32,
    dropped: u32,
    reverted: u32,
}

static GATE: Mutex<Option<Gate>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Gate) -> R) -> Option<R> {
    let mut g = GATE.lock().ok()?;
    Some(f(g.get_or_insert_with(Gate::default)))
}

impl Gate {
    fn load(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(text) = std::fs::read_to_string(learned_path()) {
            self.learned = parse_learned(&text);
        }
        self.learned_all = self.learned.values().flatten().copied().collect();
        log::line(&format!(
            "bossgate: {} learned start flags of {} bosses ({})",
            self.learned_all.len(),
            self.learned.len(),
            learned_path().display()
        ));
    }

    fn learn(&mut self, entity: i32, ids: impl IntoIterator<Item = u32>) -> usize {
        learn_into(&mut self.learned, &mut self.learned_all, &mut self.changed_at, entity, ids)
    }
}

fn learn_into(
    learned: &mut HashMap<i32, Vec<u32>>,
    learned_all: &mut HashSet<u32>,
    changed_at: &mut Option<Instant>,
    entity: i32,
    ids: impl IntoIterator<Item = u32>,
) -> usize {
    if entity <= 0 || (learned.len() >= MAX_BOSSES && !learned.contains_key(&entity)) {
        return 0;
    }
    let per = world::per_player_flags();
    let list = learned.entry(entity).or_default();
    let mut n = 0;
    for id in ids {
        if gated_kind(id) && !per.is_some_and(|s| s.contains(&id)) && !list.contains(&id) && list.len() < MAX_PER_BOSS {
            list.push(id);
            learned_all.insert(id);
            n += 1;
        }
    }
    if n > 0 {
        *changed_at = Some(Instant::now());
    }
    n
}

/// What the gate does with a gated flag of `pg`'s peer (pure, unit tested): true = held.
fn gate_flag(pg: &mut PeerGate, learned: bool, id: u32, on: bool, now: Instant) -> (bool, bool) {
    if !pg.ready {
        return (false, false);
    }
    if !pg.away {
        // Near that peer: this newer value wins over anything still waiting.
        pg.pre.retain(|e| e.0 != id);
        pg.held.retain(|e| e.0 != id);
        return (false, false);
    }
    if pg.fight.is_some() {
        pg.pre.retain(|e| e.0 != id);
        pg.hold(id, on);
        return (true, true);
    }
    pg.pre.retain(|e| e.0 != id);
    pg.pre.push((id, on, now, learned));
    (true, false)
}

/// world.rs, for every flag change of another player before it is applied; `origin` is the
/// player whose world wrote it. True = held here (the gate applies or drops it later);
/// false = apply now as usual.
pub fn hold_friend_flag(origin: PeerId, id: u32, on: bool) -> bool {
    if !gated_kind(id) {
        return false;
    }
    with(|g| {
        if !ENABLED.load(Ordering::Relaxed) {
            return false;
        }
        let learned = g.learned_all.contains(&id);
        let Some(pg) = g.peers.get_mut(&origin) else { return false };
        let (held, in_fight) = gate_flag(pg, learned, id, on, Instant::now());
        if held {
            if in_fight {
                g.fight_held += 1;
            } else {
                g.pre_held += 1;
            }
        }
        held
    })
    .unwrap_or(false)
}

/// world.rs, for `from`'s `EV_BOSS(a, entity)`: 1 started, 2 fell, 3 gauge gone.
pub fn friend_boss_event(from: PeerId, a: i32, entity: i32) {
    with(|g| {
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Some(pg) = g.peers.get_mut(&from).filter(|p| p.ready) else { return };
        match a {
            1 => {
                let first = pg.fight.is_none();
                pg.fight = Some(Fight { entity, since: Instant::now() });
                pg.entered_logged = false;
                let waiting = std::mem::take(&mut pg.pre);
                let ids: Vec<u32> = waiting.iter().map(|e| e.0).collect();
                for (id, on, ..) in waiting {
                    pg.hold(id, on);
                }
                pg.revert_due = first && pg.away;
                let away = pg.away;
                let learned = g.learn(entity, ids.iter().copied());
                log::line(&format!(
                    "bossgate: peer {}'s boss fight {entity} started while we are {}: {} waiting flags held {ids:?} ({learned} newly learned)",
                    from.slot(),
                    if away { "away" } else { "near" },
                    ids.len()
                ));
            }
            2 if pg.fight.is_some() || !pg.pre.is_empty() => pg.defeat_due = true,
            3 if pg.fight.is_some() => pg.end_due = Some("its boss gauge went away without a kill"),
            _ => {}
        }
    });
}

/// bossrules.rs, at the end of a flag recording of our own gauge: (ms, id, on).
pub fn learn_own(entity: i32, entries: &[(i64, u32, bool)]) {
    with(|g| {
        g.load();
        let ids: Vec<u32> = entries
            .iter()
            .filter(|(ms, id, on)| *on && (-OWN_BEFORE_MS..=OWN_AFTER_MS).contains(ms) && world::should_sync(*id))
            .map(|e| e.1)
            .collect();
        let n = g.learn(entity, ids);
        if n > 0 {
            log::line(&format!("bossgate: learned {n} start flags of our own boss {entity}"));
        }
    });
}

/// Some peer's fight is running here without us (for the announcement).
pub fn holding_friend_fight() -> bool {
    ENABLED.load(Ordering::Relaxed) && with(|g| g.peers.values().any(|p| p.fight.is_some() && p.away)).unwrap_or(false)
}

/// The waiting flags split into due now and still waiting: away, a flag is due after
/// `PRE_HOLD` (a learned start flag after `PRE_HOLD_LEARNED`); near, at once.
fn split_pre(
    pre: Vec<(u32, bool, Instant, bool)>,
    away: bool,
    now: Instant,
) -> (Vec<(u32, bool, Instant, bool)>, Vec<(u32, bool, Instant, bool)>) {
    pre.into_iter()
        .partition(|&(_, _, t, learned)| !away || now.duration_since(t) >= if learned { PRE_HOLD_LEARNED } else { PRE_HOLD })
}

/// A peer's cutscene: the waiting flags get `dt` younger (their start moves forward), so the
/// wait counts the time outside the cutscene only.
fn freeze_pre(pre: &mut [(u32, bool, Instant, bool)], dt: Duration) {
    for e in pre {
        crate::cutscene::pause(&mut e.2, dt);
    }
}

/// One peer as the gate sees it this frame.
pub struct PeerCtx {
    pub peer: PeerId,
    /// Its link epoch (a client's relayed peers share the host link's).
    pub epoch: u64,
    /// Its block and position (latest visible snapshot).
    pub at: Option<(i32, [f32; 3])>,
    /// world.rs still sees its fight (`World::peer_boss`).
    pub fight: bool,
    /// Its latest snapshot says it is in a cutscene (cutscene.rs).
    pub cutscene: bool,
}

/// What bossrules.rs knows this frame.
pub struct Ctx {
    pub connected: bool,
    pub here: Option<i32>,
    pub pos: Option<[f32; 3]>,
    /// Every peer, in id order: one gate each.
    pub peers: Vec<PeerCtx>,
}

/// One peer's gate for this frame: link changes, revert, defeat, end, entering, waiting.
#[allow(clippy::too_many_arguments)]
fn tick_peer(
    g_learned: &mut (HashMap<i32, Vec<u32>>, HashSet<u32>, Option<Instant>),
    stats: &mut [u32; 3],
    id: PeerId,
    pg: &mut PeerGate,
    fm: usize,
    fight_alive: bool,
    cutscene: bool,
    now: Instant,
    apply: &mut Vec<(u32, bool)>,
    quiet: &mut Vec<(u32, bool)>,
    notice: &mut Option<String>,
) {
    // A peer in a cutscene: its waiting flags do not age (the wait would run out inside the
    // cutscene and the flags would be applied to a world that is not at the fight yet).
    let dt = pg.last_tick.replace(now).map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
    if cutscene {
        freeze_pre(&mut pg.pre, dt);
    }
    // First fight against this boss: flags we applied just before the start go back.
    if std::mem::take(&mut pg.revert_due) {
        let entity = pg.fight.map_or(0, |f| f.entity);
        let mut back = Vec::new();
        for &(t, fid, before, value) in &pg.recent {
            if now.duration_since(t) <= START_WINDOW && before != value && world::get_flag(fm, fid) == value {
                back.push((fid, before, value));
            }
        }
        pg.recent.clear();
        for &(fid, before, value) in &back {
            quiet.push((fid, before));
            pg.hold(fid, value);
        }
        let learned = learn_into(&mut g_learned.0, &mut g_learned.1, &mut g_learned.2, entity, back.iter().map(|b| b.0));
        if !back.is_empty() {
            stats[2] += back.len() as u32;
            log::line(&format!(
                "bossgate: peer {}'s boss {entity}: {} of its flags of the last {} s switched back here and held {:?} ({learned} newly learned)",
                id.slot(),
                back.len(),
                START_WINDOW.as_secs(),
                back.iter().map(|b| b.0).collect::<Vec<_>>()
            ));
        }
    }
    if std::mem::take(&mut pg.defeat_due) {
        let n = pg.held.len() + pg.pre.len();
        apply.append(&mut pg.held);
        apply.extend(pg.pre.drain(..).map(|e| (e.0, e.1)));
        stats[0] += n as u32;
        log::line(&format!("bossgate: peer {}'s boss fell: {n} held flags applied with the kill", id.slot()));
        pg.fight = None;
        pg.end_due = None;
    }
    if let Some(f) = pg.fight {
        let why = pg
            .end_due
            .take()
            .or_else(|| (!fight_alive && f.since.elapsed() > Duration::from_secs(1)).then_some("its fight ended (death, rest, load)"));
        if let Some(why) = why {
            log::line(&format!("bossgate: peer {}'s boss {}: {why}: {} held start flags dropped", id.slot(), f.entity, pg.held.len()));
            stats[1] += pg.held.len() as u32;
            pg.held.clear();
            pg.fight = None;
        } else if !pg.away && !pg.held.is_empty() {
            let n = pg.held.len();
            apply.append(&mut pg.held);
            stats[0] += n as u32;
            log::line(&format!(
                "bossgate: peer {}'s boss {}: we came within {NEAR_M} m of its fight: {n} held flags applied",
                id.slot(),
                f.entity
            ));
            if !pg.entered_logged {
                pg.entered_logged = true;
                *notice = Some("Joining your friend's boss fight".to_owned());
            }
        }
    }
    // Waiting flags without a fight: in after their delay (at once when near).
    if pg.fight.is_none() && !pg.pre.is_empty() {
        let away = pg.away;
        let (due, keep) = split_pre(std::mem::take(&mut pg.pre), away, now);
        pg.pre = keep;
        for (fid, on, ..) in due {
            if away {
                pg.recent.push_back((now, fid, world::get_flag(fm, fid), on));
            }
            apply.push((fid, on));
        }
    }
    while pg.recent.front().is_some_and(|e| now.duration_since(e.0) > RECENT_KEEP) || pg.recent.len() > 512 {
        pg.recent.pop_front();
    }
}

/// bossrules.rs, once per frame in the world: decides, applies, reverts, saves.
pub fn tick(ctx: Ctx, w: &mut World) {
    let mut apply: Vec<(u32, bool)> = Vec::new();
    let mut quiet: Vec<(u32, bool)> = Vec::new();
    let mut notice: Option<String> = None;
    let fm = world::real_world_flag_man();
    with(|g| {
        g.load();
        // Link changes per peer: a peer that left (or every peer, link down) or came back
        // on a new link. Flags that only waited out the short delay are ordinary world
        // changes of that peer (doors, talks): they go in. A fight's held start flags are dropped.
        let present: HashMap<PeerId, &PeerCtx> =
            if ctx.connected { ctx.peers.iter().map(|p| (p.peer, p)).collect() } else { HashMap::new() };
        let gone: Vec<PeerId> = g.peers.keys().filter(|id| !present.contains_key(id)).copied().collect();
        for id in gone {
            if let Some(mut pg) = g.peers.remove(&id) {
                let waiting: Vec<(u32, bool)> = pg.pre.drain(..).map(|e| (e.0, e.1)).collect();
                if !waiting.is_empty() {
                    log::line(&format!("bossgate: peer {} left: {} of its waiting flags applied now", id.slot(), waiting.len()));
                }
                apply.extend(waiting);
                g.dropped += pg.clear(id, if ctx.connected { "left the session" } else { "link down" });
            }
        }
        for p in &ctx.peers {
            if !ctx.connected {
                break;
            }
            let pg = g.peers.entry(p.peer).or_insert_with(|| PeerGate { epoch: p.epoch, ..PeerGate::default() });
            if pg.epoch != p.epoch {
                let waiting: Vec<(u32, bool)> = pg.pre.drain(..).map(|e| (e.0, e.1)).collect();
                if !waiting.is_empty() {
                    log::line(&format!("bossgate: peer {}: link changed: {} waiting flags applied now", p.peer.slot(), waiting.len()));
                }
                apply.extend(waiting);
                let n = pg.clear(p.peer, "new session");
                g.dropped += n;
                pg.epoch = p.epoch;
            }
            let near = p.at.zip(ctx.pos).is_some_and(|((block, fp), q)| ctx.here == Some(block) && game::distance(fp, q) <= NEAR_M);
            pg.away = !near;
            pg.ready = true;
        }
        // Flags can only be read and written in the real world (not loading, no Remnant).
        let Some(fm) = fm else { return };
        let now = Instant::now();
        if !ENABLED.load(Ordering::Relaxed) {
            // Switched off: everything waiting goes in now, as without the gate.
            for pg in g.peers.values_mut() {
                apply.extend(pg.pre.drain(..).map(|e| (e.0, e.1)));
                apply.append(&mut pg.held);
                pg.fight = None;
            }
            return;
        }
        let fights: HashMap<PeerId, bool> = ctx.peers.iter().map(|p| (p.peer, p.fight)).collect();
        let cuts: HashMap<PeerId, bool> = ctx.peers.iter().map(|p| (p.peer, p.cutscene)).collect();
        let mut learned = (std::mem::take(&mut g.learned), std::mem::take(&mut g.learned_all), g.changed_at);
        let mut stats = [0u32; 3];
        for (&id, pg) in g.peers.iter_mut() {
            let alive = fights.get(&id).copied().unwrap_or(false);
            let cutscene = cuts.get(&id).copied().unwrap_or(false);
            tick_peer(&mut learned, &mut stats, id, pg, fm, alive, cutscene, now, &mut apply, &mut quiet, &mut notice);
        }
        (g.learned, g.learned_all, g.changed_at) = learned;
        g.applied_late += stats[0];
        g.dropped += stats[1];
        g.reverted += stats[2];
        if g.changed_at.is_some_and(|t| t.elapsed() >= SAVE_AFTER) {
            g.changed_at = None;
            let path = learned_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let ok = std::fs::write(&path, format_learned(&g.learned)).is_ok();
            log::line(&format!("bossgate: saved {} learned start flags of {} bosses (ok = {ok})", g.learned_all.len(), g.learned.len()));
        }
    });
    for (id, on) in quiet {
        w.write_quiet(id, on);
    }
    for (id, on) in apply {
        w.apply_friend_flag(id, on);
    }
    if let Some(text) = notice {
        crate::bossrules::notice(text, 5);
    }
}

/// For the stats line in bossrules.rs (every 60 s).
pub fn stats() -> String {
    with(|g| {
        format!(
            "bossgate: waited {} friend flags, held {} in fights, applied late {}, switched back {}, dropped {}; learned {} flags of {} bosses",
            g.pre_held,
            g.fight_held,
            g.applied_late,
            g.reverted,
            g.dropped,
            g.learned_all.len(),
            g.learned.len()
        )
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_kinds() {
        assert!(gated_kind(11_105_800)); // map flag, group 1
        assert!(gated_kind(25_005_100)); // map flag, group 2
        assert!(gated_kind(71_100_500)); // talk flag (Isshin)
        assert!(gated_kind(1_139)); // NPC state machine
        assert!(!gated_kind(9_301)); // global
        assert!(!gated_kind(9_501)); // death penalty
        assert!(!gated_kind(6_911)); // local
        assert!(!gated_kind(50_000_100)); // group 5
        assert!(!gated_kind(0));
    }

    /// Per fight (P4): peer 2 fights far away, peer 3 stands next to us. Peer 2's flags are
    /// held in its fight, peer 3's apply at once; peer 1 is not ready yet (no gate frame).
    #[test]
    fn each_peer_has_its_own_gate() {
        let now = Instant::now();
        let mut far = PeerGate { ready: true, away: true, fight: Some(Fight { entity: 1110800, since: now }), ..PeerGate::default() };
        let mut near = PeerGate { ready: true, away: false, ..PeerGate::default() };
        let mut new = PeerGate::default();
        assert_eq!(gate_flag(&mut far, false, 11_105_800, true, now), (true, true));
        assert_eq!(far.held, vec![(11_105_800, true)]);
        assert_eq!(gate_flag(&mut near, false, 11_105_800, true, now), (false, false));
        assert_eq!(gate_flag(&mut new, false, 11_105_800, true, now), (false, false));
        // Away from a peer without a fight: a short wait (learned start flags: long).
        let mut idle = PeerGate { ready: true, away: true, ..PeerGate::default() };
        assert_eq!(gate_flag(&mut idle, true, 11_105_801, true, now), (true, false));
        assert_eq!(idle.pre.len(), 1);
        assert!(idle.pre[0].3, "learned");
        // Coming near drops what waited for that flag: the newer value wins.
        idle.away = false;
        assert_eq!(gate_flag(&mut idle, false, 11_105_801, false, now), (false, false));
        assert!(idle.pre.is_empty());
    }


    /// A peer's cutscene stops the clock of its waiting flags: a learned start flag (90 s)
    /// does not come due while the peer is in one, however long, and comes due 90 s after.
    #[test]
    fn waiting_flags_freeze_during_a_peer_cutscene() {
        let t0 = Instant::now();
        let frame = Duration::from_millis(100);
        let mut pre = vec![(11_105_800u32, true, t0, true), (11_105_801, true, t0, false)];
        let mut now = t0;
        // 2 s away from the cutscene first: the 4 s flag is half way, the 90 s one nowhere.
        for _ in 0..20 {
            now += frame;
            let (due, keep) = split_pre(std::mem::take(&mut pre), true, now);
            assert!(due.is_empty());
            pre = keep;
        }
        // 5 minutes in a cutscene: frozen, nothing due.
        for _ in 0..3000 {
            now += frame;
            freeze_pre(&mut pre, frame);
            let (due, keep) = split_pre(std::mem::take(&mut pre), true, now);
            assert!(due.is_empty(), "frozen");
            pre = keep;
        }
        // After it: the short flag comes due 2 s later (4 s in all), the learned one at 90 s.
        let mut short_due_after = None;
        let mut learned_due_after = None;
        let end = now;
        for _ in 0..2000 {
            now += frame;
            let (due, keep) = split_pre(std::mem::take(&mut pre), true, now);
            for e in due {
                let after = now.duration_since(end);
                if e.3 {
                    learned_due_after = Some(after);
                } else {
                    short_due_after = Some(after);
                }
            }
            pre = keep;
        }
        let short = short_due_after.expect("short flag due");
        let learned = learned_due_after.expect("learned flag due");
        assert!(short >= Duration::from_millis(1_900) && short <= Duration::from_millis(2_100), "{short:?}");
        assert!(learned >= Duration::from_millis(87_900) && learned <= Duration::from_millis(88_100), "{learned:?}");
        // Near the peer everything is due at once, cutscene or not.
        let (due, keep) = split_pre(vec![(1, true, now, true)], false, now);
        assert_eq!((due.len(), keep.len()), (1, 0));
    }
    #[test]
    fn learned_file_round_trip() {
        let text = "# comment\n1110800: 11105800 11105801 9301 bad 11105800\nnot a line\n1500800 71100500\n";
        let l = parse_learned(text);
        assert_eq!(l.get(&1110800), Some(&vec![11105800, 11105801]));
        assert_eq!(l.get(&1500800), Some(&vec![71100500]));
        assert_eq!(l.len(), 2);
        assert_eq!(parse_learned(&format_learned(&l)), l);
    }
}
