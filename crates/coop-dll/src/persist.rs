//! Session state that outlives loads until a rest or a wipe (IMPLEMENTATION-NOTES §31,
//! NOT verified live). Owner rule: enemies and bosses reset only on a rest (either player)
//! or a wipe (both dead); a single player's death or a walk out of an area and back does
//! not bring the dead back or heal a boss.
//!
//! - **Dead set** (ordinary enemies, NpcParam deathblow count 1): entity ids that died this
//!   rest cycle, seen dying here or reported by the friend, kept on both machines (bulk
//!   `BULK_PERSIST`). An id that loads alive again is killed (ForceDead without the game's
//!   reward). Live 7.4: killed enemies came back after one player went far away and back
//!   (the other machine never saw the death, or our ForceDead left nothing that keeps the
//!   chr dead across an unload), and the friend's kills made while the native session was
//!   disengaged paid the host nothing.
//! - **Rewarded**: entity ids we were paid for this rest cycle (our own kill: the game set
//!   `chr+0x1A12` bits 1|2; or enemies.rs `loot_step` paid). A kill reported by the friend
//!   pays us once unless the id is in here (livesync.rs, the P3 restore, the dead set).
//! - **Boss records** (`[GAMEPLAY] boss_persist_on_death`, bosses and minibosses: deathblow
//!   count >= 2 or the boss gauge's entity): the last HP / lives seen (here or on the friend),
//!   applied to a fresh copy (full HP and lives) the first time it shows up after a load,
//!   so a lone death reload does not bring the boss back at full health. Removed when the
//!   boss dies (its defeat is kept by the game's own flags).
//!
//! A rest of ours (restsync.rs `last_rest`, own or mirrored) or a wipe (both players at
//! 0 HP within 90 s, or the boss wipe release) clears everything; for 13 s after it the
//! friend's messages are ignored and nothing is recorded here (the respawn may not have
//! happened yet), and after a wipe while we are dead nothing until our reload. Every new
//! death pair is a new wipe (no latch). HP is compared and restored as a share of each
//! copy's max (the two machines' scaling can differ); a boss's get-up after a lost life
//! counts as full HP (live 2026-10-24 fixes, NOT verified live).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::enemies::Enemies;
use crate::game::{self, Chr};
use crate::net::{Dest, Net};
use crate::peers::{EpochTracker, PeerId, PeerSet};
use crate::{livesync, log, nsession, posture};

/// `[GAMEPLAY] boss_persist_on_death` (default on). 0 = vanilla: a death reload resets a boss.
pub static BOSS_PERSIST: AtomicBool = AtomicBool::new(true);

pub const BULK_PERSIST: u8 = 50;
const VERSION: u8 = 1;
const MSG_DEAD: u8 = 1;
const MSG_BOSS: u8 = 2;
const MAX_IDS: usize = 4096;
const MAX_BOSSES: usize = 256;

const SCAN_EVERY: Duration = Duration::from_millis(250);
const SEND_EVERY: Duration = Duration::from_secs(1);
/// After a load the game sets up its own state first.
const SETTLE: Duration = Duration::from_secs(3);
/// Kills on load per scan (ForceDead on many enemies in one frame was once followed by a crash).
const KILLS_PER_SCAN: usize = 4;
const WIPE_WINDOW: Duration = Duration::from_secs(90);
/// Friend messages right after a rest are from before it.
const AFTER_REST_IGNORE: Duration = Duration::from_secs(13);
/// After a boss loses a life its HP is not recorded this long (deathblow, get-up refill).
const GETUP_GRACE: Duration = Duration::from_secs(6);
const MAX_LOGS: u32 = 40;
const STATS_EVERY: Duration = Duration::from_secs(30);

// --- Pure rules (unit tested) ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BossRec {
    pub hp: i32,
    pub max_hp: i32,
    pub lives: i32,
    pub lives0: i32,
}

impl BossRec {
    /// Worse for the boss (fewer lives, then a smaller share of its max HP): the state
    /// both machines keep. HP is compared as a fraction of each record's own max: the two
    /// machines' copies can have different maxes (2-player scaling applied on one only,
    /// live 2026-10-24: 2149 unscaled vs 5373 scaled), and an unscaled full copy must not
    /// look "lower" than a damaged scaled one.
    pub fn lower(a: BossRec, b: BossRec) -> BossRec {
        let part = |r: BossRec, other_max: i32| r.hp as i64 * other_max.max(1) as i64;
        if (b.lives, part(b, a.max_hp)) < (a.lives, part(a, b.max_hp)) { b } else { a }
    }
    /// Damaged: worth restoring onto a fresh copy.
    pub fn damaged(self) -> bool {
        self.hp > 0 && (self.lives < self.lives0 || self.hp < self.max_hp)
    }
}

/// A wipe (PLAN-MULTIPLAYER §2 persist): we died and every connected peer died, all within
/// `WIPE_WINDOW` of each other. Two players: the old "both at 0 HP within 90 s". No peer:
/// never (a lone death is not a wipe).
pub fn is_wipe(ours: Option<Instant>, peers: &[Option<Instant>]) -> bool {
    let Some(ours) = ours else { return false };
    if peers.is_empty() {
        return false;
    }
    let mut lo = ours;
    let mut hi = ours;
    for p in peers {
        let Some(t) = *p else { return false };
        lo = lo.min(t);
        hi = hi.max(t);
    }
    hi - lo <= WIPE_WINDOW
}

/// A copy that just appeared with full HP and every life (the load made it new).
pub fn fresh_copy(hp: i32, max_hp: i32, lives: i32, lives0: i32) -> bool {
    hp > 0 && hp >= max_hp && (lives0 < 2 || lives >= lives0)
}

/// What a fresh copy gets from a record: (lives to write, HP to set).
pub fn restore_plan(copy_max_hp: i32, copy_lives0: i32, rec: BossRec) -> Option<(Option<i32>, i32)> {
    if !rec.damaged() || copy_max_hp <= 0 {
        return None;
    }
    let lives = (copy_lives0 >= 2 && rec.lives < copy_lives0).then_some(rec.lives.max(1));
    // A new life starts at full HP: the HP of the record belongs to its lives. The
    // record's share of its max goes onto the copy's max (scaling can differ).
    let hp = if rec.max_hp > 0 && rec.max_hp != copy_max_hp {
        (rec.hp as i64 * copy_max_hp as i64 / rec.max_hp as i64) as i32
    } else {
        rec.hp
    };
    Some((lives, hp.clamp(1, copy_max_hp)))
}

/// The records to drop when `id` is dead: only its own (a non-final phase), or the whole
/// encounter's once the encounter is defeated (bosses.rs, 2026-10-24).
fn records_to_drop(id: i32, encounter_defeated: bool) -> Vec<i32> {
    match crate::bosses::encounter_of(id) {
        Some(enc) if encounter_defeated => enc.entities.to_vec(),
        _ => vec![id],
    }
}

/// A phase-1 copy (not a final entity) never gets its record back once a final member of
/// its encounter has one: the fight is past that phase (Butterfly 1000800 vs 1000810).
fn phase_one_stale(id: i32, bosses: &HashMap<i32, (BossRec, bool)>) -> bool {
    crate::bosses::encounter_of(id).is_some_and(|enc| {
        !enc.final_entities.contains(&id) && enc.final_entities.iter().any(|f| bosses.contains_key(f))
    })
}

/// The record of one observation. During the get-up after a lost life the copy sits at
/// 1 HP for a moment (the deathblow's own trailing hit, livesync.rs) before the refill:
/// live 2026-10-24 that 1 HP became the record and a death reload restored the
/// miniboss at 1 HP. A new life starts full, so the get-up counts as full.
fn observed(hp: i32, max_hp: i32, lives: i32, lives0: i32, getting_up: bool) -> BossRec {
    BossRec { hp: if getting_up { max_hp.max(hp) } else { hp }, max_hp, lives, lives0 }
}

/// Every player died within `WIPE_WINDOW` (`is_wipe`): the deaths (ours, then each
/// peer's) to clear for, unless this very set was cleared already. A later death makes a
/// new set, so a second wipe soon after a first one clears again (live 2026-10-24: the
/// first clear latched, the records seen after it survived the real wipe and were
/// restored).
fn wipe_deaths(ours: Option<Instant>, peers: &[Option<Instant>], cleared: Option<&[Instant]>) -> Option<Vec<Instant>> {
    if !is_wipe(ours, peers) {
        return None;
    }
    // is_wipe: ours and every peer's death are known.
    let set: Vec<Instant> = std::iter::once(ours?).chain(peers.iter().flatten().copied()).collect();
    (cleared != Some(set.as_slice())).then_some(set)
}

/// `DEATH_COMMITTED` (same address as world.rs): 1 once a death is final (not during a
/// resurrection window).
const DEATH_COMMITTED: usize = 0x3EA0D74;
/// A member of the same encounter first seen this long after its first member is a phase
/// entity appearing mid-fight, not part of the load's initial set.
const SAME_LOAD_SET: Duration = Duration::from_secs(5);

/// Our death became committed this frame (the step to 1; HP 0 alone is not a death).
pub fn commit_edge(last: u32, now: u32) -> bool {
    now == 1 && last != 1
}

/// Boss records are restored only on a load that follows a death of ours, and only for the
/// load's initial set: `first_seen_gap` = time since the first member of its encounter
/// was seen in this load (None = this is the first).
pub fn restore_allowed(reload_after_death: bool, first_seen_gap: Option<Duration>) -> bool {
    reload_after_death && first_seen_gap.is_none_or(|g| g <= SAME_LOAD_SET)
}

fn encode_dead(ids: &[i32]) -> Vec<u8> {
    let ids = &ids[..ids.len().min(MAX_IDS)];
    let mut out = vec![VERSION, MSG_DEAD];
    out.extend((ids.len() as u16).to_le_bytes());
    for id in ids {
        out.extend(id.to_le_bytes());
    }
    out
}

fn encode_bosses(recs: &[(i32, BossRec)]) -> Vec<u8> {
    let recs = &recs[..recs.len().min(MAX_BOSSES)];
    let mut out = vec![VERSION, MSG_BOSS];
    out.extend((recs.len() as u16).to_le_bytes());
    for (id, r) in recs {
        out.extend(id.to_le_bytes());
        out.extend(r.hp.to_le_bytes());
        out.extend(r.max_hp.to_le_bytes());
        out.push(r.lives.clamp(-128, 127) as i8 as u8);
        out.push(r.lives0.clamp(-128, 127) as i8 as u8);
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
enum Msg {
    Dead(Vec<i32>),
    Bosses(Vec<(i32, BossRec)>),
}

fn decode(d: &[u8]) -> Option<Msg> {
    let (&ver, rest) = d.split_first()?;
    if ver != VERSION {
        return None;
    }
    let (&kind, rest) = rest.split_first()?;
    let n = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?) as usize;
    let body = &rest[2..];
    let i32_at = |b: &[u8], at: usize| -> Option<i32> { Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?)) };
    match kind {
        MSG_DEAD if n <= MAX_IDS && body.len() == 4 * n => Some(Msg::Dead((0..n).map(|i| i32_at(body, 4 * i)).collect::<Option<_>>()?)),
        MSG_BOSS if n <= MAX_BOSSES && body.len() == 14 * n => Some(Msg::Bosses(
            (0..n)
                .map(|i| {
                    let b = &body[14 * i..14 * i + 14];
                    Some((
                        i32_at(b, 0)?,
                        BossRec { hp: i32_at(b, 4)?, max_hp: i32_at(b, 8)?, lives: b[12] as i8 as i32, lives0: b[13] as i8 as i32 },
                    ))
                })
                .collect::<Option<_>>()?,
        )),
        _ => None,
    }
}

// --- State ------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Seen {
    chr: usize,
    alive: bool,
    lives0: i32,
    boss: bool,
}

#[derive(Default)]
struct Stats {
    died_here: u32,
    from_friend: u32,
    killed_on_load: u32,
    killed_loot: u32,
    bosses_restored: u32,
}

#[derive(Default)]
struct State {
    dead: HashSet<i32>,
    rewarded: HashSet<i32>,
    bosses: HashMap<i32, (BossRec, bool)>,
    /// This load: last observation per entity, bosses already checked for a restore,
    /// ids we killed on load.
    seen: HashMap<i32, Seen>,
    boss_checked: HashSet<i32>,
    killed_here: HashSet<i32>,
    last_in_world: Duration,
    out_dead: Vec<i32>,
    out_bosses: bool,
    /// Peer links we sent everything on (per link epoch, P4) and the ones still due.
    links: EpochTracker,
    full_due: BTreeSet<PeerId>,
    send_at: Option<Instant>,
    scan_at: Option<Instant>,
    rest_seen: Option<Instant>,
    cleared_at: Option<Instant>,
    our_dead_at: Option<Instant>,
    /// Each connected peer's committed death (EV_DEATH) within the wipe window.
    peer_dead_at: BTreeMap<PeerId, Instant>,
    /// Our `DEATH_COMMITTED` last frame; committed deaths so far, and the count at the last
    /// load: a load after a new commit is a death reload (E, 7.15).
    our_committed: u32,
    death_commits: u32,
    commits_at_load: u32,
    /// This load follows a death of ours: boss records may be restored (briefly).
    reload_after_death: bool,
    /// When that load (one after a death of ours) came (`after_death_guard`).
    death_load_at: Option<Instant>,
    /// This load: when the first member of each encounter (keyed by its first entity) was seen.
    enc_first: HashMap<i32, Instant>,
    /// The deaths (ours first, then each peer's) the last wipe clear was for.
    wipe_cleared: Option<Vec<Instant>>,
    /// A wipe cleared everything while we were dead: the world here is still the old one
    /// until our reload, so nothing is recorded, sent or taken from the peers until then.
    hold_load: bool,
    /// When a boss was last seen losing a life (its get-up HP is not a record), and the
    /// lives it had at our last sight (this load).
    life_lost_at: HashMap<i32, Instant>,
    last_lives: HashMap<i32, i32>,
    stats: Stats,
    stats_at: Option<Instant>,
    logs: u32,
}

impl State {
    fn log(&mut self, msg: String) {
        self.logs += 1;
        if self.logs <= MAX_LOGS || self.logs % 50 == 0 {
            log::line(&msg);
        }
    }

    fn clear(&mut self, why: &str) {
        let (d, r, b) = (self.dead.len(), self.rewarded.len(), self.bosses.len());
        self.dead.clear();
        self.rewarded.clear();
        self.bosses.clear();
        self.out_dead.clear();
        self.out_bosses = false;
        self.killed_here.clear();
        self.boss_checked.clear();
        self.life_lost_at.clear();
        self.last_lives.clear();
        self.cleared_at = Some(Instant::now());
        log::line(&format!("persist: {why}: dead set ({d}), paid kills ({r}) and boss records ({b}) cleared"));
    }

    /// Right after a clear: the friend's messages are from before it, and our own world
    /// may still be the old one (a mirrored rest respawns ~2 s later, a direct fallback
    /// after 4-5 s; live 2026-10-24 the host re-recorded the miniboss's pre-respawn
    /// 1 life / 4743 HP within a second of the rest clear).
    fn ignoring_friend(&self) -> bool {
        self.cleared_at.is_some_and(|t| t.elapsed() < AFTER_REST_IGNORE)
    }

    fn frozen(&self) -> bool {
        self.hold_load || self.ignoring_friend() || wipe_recent(WIPE_STALE)
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

/// audit.rs: the boss record kept for `id` this rest cycle (read only).
pub fn boss_record(id: i32) -> Option<BossRec> {
    with(|s| s.bosses.get(&id).map(|(r, _)| *r)).flatten()
}

/// We were paid for this enemy in this rest cycle.
pub fn rewarded(id: i32) -> bool {
    with(|s| s.rewarded.contains(&id)).unwrap_or(false)
}

/// enemies.rs `loot_step`: our own kill (the game paid) or our loot for a friend's kill.
pub fn mark_rewarded(id: i32) {
    with(|s| s.rewarded.insert(id));
}

/// A wipe seen elsewhere (bossrules.rs release "wipe").
/// Both players reload now (the death screen), so the world stays frozen until ours.
pub fn note_wipe(why: &str) {
    mark_wipe();
    with(|s| {
        if !s.hold_load {
            s.clear(&format!("wipe ({why})"));
            s.hold_load = true;
        }
    });
}

/// A wipe decided after we already reloaded (puppet.rs `death_sync`): the records go and the
/// friend's late ones are ignored for `AFTER_REST_IGNORE` (`clear`), but nothing is held until the
/// next load (that is `note_wipe`, for a wipe whose reload is still ahead of us).
pub fn note_wipe_loaded(why: &str) {
    with(|s| s.clear(&format!("wipe after our reload ({why})")));
}

/// For how long after a wipe the friend's dead lists, boss records, lives and loot are
/// treated as the old world's (v8.25c). Without a wipe epoch on the packets (protocol bump,
/// out of scope) this is a time window: a friend still in the old world for longer gets through.
pub const WIPE_STALE: Duration = Duration::from_secs(90);

/// When the last wipe was decided here (`note_wipe`, the tick's `wipe_deaths`); NOT `note_wipe_loaded`.
static WIPE_AT: Mutex<Option<Instant>> = Mutex::new(None);

fn mark_wipe() {
    if let Ok(mut g) = WIPE_AT.lock() {
        *g = Some(Instant::now());
    }
}

/// `at` is within `within` of `now` (pure, for tests).
fn recent_at(at: Option<Instant>, now: Instant, within: Duration) -> bool {
    at.is_some_and(|t| now.saturating_duration_since(t) < within)
}

/// A wipe was decided less than `within` ago: whatever the friend sends about enemies may
/// predate it (the world was reset), so dead lists / lives / loot from it are dropped.
pub fn wipe_recent(within: Duration) -> bool {
    let at = WIPE_AT.lock().ok().and_then(|g| *g);
    recent_at(at, Instant::now(), within)
}

/// Dead and not reloaded yet, or less than `window` since the load after our death (v8.6).
/// A peer that reloads later still plays the old world for a while, so its lives states
/// are from before the reset (live 2026-10-25, Genichiro: the friend reloaded 5 s before
/// the spectating host, took the host's "1110800 lives 0" onto its fresh boss and got
/// phase 2 with its cutscene at once; that lives 0 then went back onto the host's fresh
/// copy: two bosses). posture.rs and livesync.rs take no lives inside it.
pub fn after_death_guard(window: Duration) -> bool {
    with(|s| s.death_commits != s.commits_at_load || s.death_load_at.is_some_and(|t| t.elapsed() < window)).unwrap_or(false)
}

/// Every frame in the world while connected (puppet.rs, after livesync).
pub struct Ctx<'a> {
    pub net: &'a mut Net,
    /// This frame's other players: each one's latest visible snapshot HP (None when none /
    /// hidden) and its committed death (world.rs EV_DEATH, this frame's: puppet.rs
    /// refreshes it after `World::on_frame`).
    pub peers: &'a PeerSet,
    pub enemies: &'a Enemies,
    pub player: Chr,
    pub in_world_for: Duration,
}

pub fn tick(c: Ctx) {
    let Ok(mut g) = STATE.lock() else { return };
    let st = g.get_or_insert_with(State::default);
    let now = Instant::now();

    // A load: chr pointers and the per-load bookkeeping start over.
    if c.in_world_for < st.last_in_world {
        st.seen.clear();
        st.boss_checked.clear();
        st.killed_here.clear();
        st.last_lives.clear();
        st.enc_first.clear();
        // Boss records come back only after a real death reload of ours (E, 7.15).
        st.reload_after_death = st.death_commits != st.commits_at_load;
        st.commits_at_load = st.death_commits;
        st.death_load_at = st.reload_after_death.then_some(now);
        if std::mem::take(&mut st.hold_load) {
            log::line("persist: reloaded after the wipe: records start over from the fresh world");
        }
    }
    st.last_in_world = c.in_world_for;

    // A rest (ours or mirrored) resets the world for both.
    let rest = crate::restsync::last_rest();
    if rest.is_some() && rest != st.rest_seen {
        st.rest_seen = rest;
        st.clear("rest");
    }
    // A wipe: every player (us and each connected peer) dead within 90 s. Each death is
    // stamped when it happens, so a later death replaces an older one and can make a new
    // wipe (no latch, §35). Only a COMMITTED death counts (7.15): HP 0 during a
    // resurrection window made 3 false wipe clears while both were alive (live 7.13).
    let committed = game::read::<u32>(game::module_base() + DEATH_COMMITTED).unwrap_or(0);
    if commit_edge(st.our_committed, committed) {
        st.our_dead_at = Some(now);
        st.death_commits += 1;
    }
    st.our_committed = committed;
    // Peers that left are no longer part of the wipe.
    st.peer_dead_at.retain(|id, _| c.peers.by_id(*id).is_some());
    for p in c.peers.iter() {
        // Its committed death (EV_DEATH; its snapshot is hidden while dead). Snapshot HP 0
        // alone is no death (7.15).
        if let Some(t) = p.death_at
            && now.duration_since(t) <= WIPE_WINDOW
            && st.peer_dead_at.get(&p.id).is_none_or(|f| t > *f)
        {
            st.peer_dead_at.insert(p.id, t);
        }
    }
    let peer_deaths: Vec<Option<Instant>> = c.peers.iter().map(|p| st.peer_dead_at.get(&p.id).copied()).collect();
    if let Some(set) = wipe_deaths(st.our_dead_at, &peer_deaths, st.wipe_cleared.as_deref()) {
        st.wipe_cleared = Some(set);
        mark_wipe();
        st.clear(if peer_deaths.len() > 1 { "wipe (every player died)" } else { "wipe (both players died)" });
        // Dead now: the bosses here are still the damaged ones until our reload.
        if c.player.hp().is_some_and(|h| h <= 0) {
            st.hold_load = true;
        }
    }
    if st.our_dead_at.is_some_and(|x| now.duration_since(x) > WIPE_WINDOW) {
        st.our_dead_at = None;
    }
    st.peer_dead_at.retain(|_, x| now.duration_since(*x) <= WIPE_WINDOW);

    // The peers' news: dead sets merged (union), boss records at the minimum.
    while let Some((_from, d)) = c.net.take_bulk_from(BULK_PERSIST) {
        if st.frozen() {
            continue;
        }
        match decode(&d) {
            Some(Msg::Dead(ids)) => {
                for id in ids {
                    if id > 0 && st.dead.insert(id) {
                        st.stats.from_friend += 1;
                    }
                }
            }
            Some(Msg::Bosses(recs)) => {
                for (id, r) in recs {
                    if id <= 0 {
                        continue;
                    }
                    let (merged, theirs) = match st.bosses.get(&id) {
                        Some(&(ours, f)) => {
                            let m = BossRec::lower(ours, r);
                            (m, if m == ours { f } else { true })
                        }
                        None => (r, true),
                    };
                    st.bosses.insert(id, (merged, theirs));
                }
            }
            None => log::line(&format!("persist: unreadable message from the friend ({} bytes)", d.len())),
        }
    }

    // Nothing seen right after a clear becomes a record or a dead-set entry: the world
    // here may not have reset yet (frozen()).
    if st.scan_at.is_none_or(|t| now.duration_since(t) >= SCAN_EVERY) {
        st.scan_at = Some(now);
        if !st.frozen() {
            scan(st, c.enemies, c.in_world_for);
        }
    }

    // Our news: everything to a peer on its new link (host: each joiner; a client: the host,
    // whose set already holds the union), then what changed to everyone.
    let joins = st.links.update(c.peers.link_epochs());
    st.full_due.extend(joins.joined);
    if !st.full_due.is_empty() && st.send_at.is_none_or(|t| now.duration_since(t) >= SEND_EVERY) {
        let linked = c.peers.link_epochs().len();
        if st.full_due.len() >= linked {
            // Every link is new (two players: every connection, as before): one send to all.
            st.full_due.clear();
            st.out_dead = st.dead.iter().copied().collect();
            st.out_bosses = !st.bosses.is_empty();
        } else {
            st.send_at = Some(now);
            let recs: Vec<(i32, BossRec)> = st.bosses.iter().map(|(id, (r, _))| (*id, *r)).collect();
            let ids: Vec<i32> = st.dead.iter().copied().collect();
            for id in std::mem::take(&mut st.full_due) {
                for chunk in ids.chunks(MAX_IDS) {
                    c.net.send_bulk_to(Dest::Peer(id), BULK_PERSIST, &encode_dead(chunk));
                }
                if !recs.is_empty() {
                    c.net.send_bulk_to(Dest::Peer(id), BULK_PERSIST, &encode_bosses(&recs));
                }
                log::line(&format!("persist: peer {} joined: sent it our dead set ({}) and boss records ({})", id.slot(), ids.len(), recs.len()));
            }
        }
    }
    if (!st.out_dead.is_empty() || st.out_bosses) && st.send_at.is_none_or(|t| now.duration_since(t) >= SEND_EVERY) {
        st.send_at = Some(now);
        if !st.out_dead.is_empty() {
            let ids = std::mem::take(&mut st.out_dead);
            for chunk in ids.chunks(MAX_IDS) {
                c.net.send_bulk(BULK_PERSIST, &encode_dead(chunk));
            }
        }
        if std::mem::take(&mut st.out_bosses) {
            let recs: Vec<(i32, BossRec)> = st.bosses.iter().map(|(id, (r, _))| (*id, *r)).collect();
            c.net.send_bulk(BULK_PERSIST, &encode_bosses(&recs));
        }
    }

    if st.stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
        if st.stats_at.is_some() {
            let s = std::mem::take(&mut st.stats);
            log::line(&format!(
                "persist (30 s): dead set {} (died here {}, from the friend {}), killed on load {} ({} with our loot), paid kills {}, boss records {} (restored {})",
                st.dead.len(),
                s.died_here,
                s.from_friend,
                s.killed_on_load,
                s.killed_loot,
                st.rewarded.len(),
                st.bosses.len(),
                s.bosses_restored
            ));
        }
        st.stats_at = Some(now);
    }
}

fn scan(st: &mut State, enemies: &Enemies, in_world_for: Duration) {
    let gauge = game::boss_state().map(|b| b.entity);
    let settled = in_world_for >= SETTLE;
    let mut kills = 0;
    for chr in nsession::enemy_chrs() {
        let Some(id) = chr.entity_id().filter(|&e| e > 0) else { continue };
        let Some(hp) = chr.hp() else { continue };
        let prev = st.seen.get(&id).copied().filter(|p| p.chr == chr.0);
        let lives0 = prev.map_or_else(|| livesync::lives0_of(chr), |p| p.lives0);
        let boss = prev.is_some_and(|p| p.boss) || livesync::multi_life(lives0) || gauge == Some(id);
        // A Puppeteer puppet fights for its owner after its deathblow: alive, never killed
        // as an earlier death (livesync.rs `puppet_plan`, Tihvert 2026-10-27).
        // v8.61: walked only where it changes anything (HP 0, or in the dead set): see
        // livesync.rs `puppet_check_needed`.
        let puppet = (hp <= 0 || st.dead.contains(&id)) && crate::statelog::owner_team_state(chr.0).is_some_and(|(_, on)| on);
        let alive = hp > 0 || puppet;
        st.seen.insert(id, Seen { chr: chr.0, alive, lives0, boss });

        if boss {
            if settled {
                boss_step(st, chr, id, alive, lives0);
            }
            continue;
        }
        // Died here (seen alive in this load first).
        if !alive {
            if prev.is_some_and(|p| p.alive) && st.dead.insert(id) {
                st.stats.died_here += 1;
                st.out_dead.push(id);
            }
            continue;
        }
        // Alive, but it died earlier this rest cycle (here or on the friend's side).
        if puppet {
            if st.dead.contains(&id) && st.killed_here.insert(id) {
                st.log(format!("dead-set: {id} died earlier but is a puppet now (stateInfo 302): not killed"));
            }
            continue;
        }
        if settled && kills < KILLS_PER_SCAN && st.dead.contains(&id) && !st.killed_here.contains(&id) {
            st.killed_here.insert(id);
            let loot = !st.rewarded.contains(&id);
            if enemies.force_dead_one(id, loot) {
                kills += 1;
                st.stats.killed_on_load += 1;
                if loot {
                    st.stats.killed_loot += 1;
                }
                st.log(format!(
                    "dead-set: {id} killed on load (died earlier this session; {})",
                    if loot { "not paid here yet: our own loot once" } else { "already paid here: no loot" }
                ));
            }
        }
    }
}

fn boss_step(st: &mut State, chr: Chr, id: i32, alive: bool, lives0: i32) {
    if !alive {
        // The game keeps a boss's defeat in its flags; a dead miniboss stays dead too.
        let defeated = crate::bosses::encounter_of(id).is_some_and(|e| game::event_flag(e.defeat_flag) == Some(true));
        for m in records_to_drop(id, defeated) {
            if st.bosses.remove(&m).is_some() {
                st.out_bosses = true;
            }
        }
        return;
    }
    if !BOSS_PERSIST.load(Ordering::Relaxed) {
        return;
    }
    let Some((data, hp, max_hp, _, lives)) = livesync::read_data(chr) else { return };
    // First sight of this boss in this load: after a death reload of ours a fresh copy gets
    // the record, but not a phase entity that shows up mid-fight (live 7.13: Way of Tomoe
    // got a record at the gauge swap with no reload at all).
    let key = crate::bosses::encounter_of(id).and_then(|e| e.entities.first().copied()).unwrap_or(id);
    let first_seen = st.enc_first.get(&key).map(|t| t.elapsed());
    st.enc_first.entry(key).or_insert_with(Instant::now);
    if st.boss_checked.insert(id)
        && restore_allowed(st.reload_after_death, first_seen)
        && fresh_copy(hp, max_hp, lives, lives0)
        && !phase_one_stale(id, &st.bosses)
        && let Some(&(rec, from_friend)) = st.bosses.get(&id)
        && let Some((new_lives, new_hp)) = restore_plan(max_hp, lives0, rec)
    {
        let lives_ok = new_lives.is_none_or(|l| livesync::set_lives(data, lives, l, "persist restore"));
        let hp_ok = posture::set_hp(chr, new_hp);
        st.stats.bosses_restored += 1;
        st.log(format!(
            "persist: boss {id} restored hp {new_hp} lives {} after a death reload (record {}, ok = {})",
            new_lives.unwrap_or(lives),
            if from_friend { "from the friend" } else { "ours" },
            lives_ok && hp_ok
        ));
        return;
    }
    // Keep the record current; it only goes down within a rest cycle (a boss never heals
    // by the owner's rule; a new life starts full but has one life less).
    let lives_now = if livesync::multi_life(lives0) { lives } else { lives0 };
    // A life lost here (our last sight) or below the record (the friend's record may
    // already have the lower lives when livesync lowers our copy).
    let before = st.last_lives.insert(id, lives_now);
    if before.is_some_and(|b| lives_now < b) || st.bosses.get(&id).is_some_and(|(old, _)| lives_now < old.lives) {
        st.life_lost_at.insert(id, Instant::now());
    }
    let getting_up = st.life_lost_at.get(&id).is_some_and(|t| t.elapsed() < GETUP_GRACE);
    let seen = observed(hp, max_hp, lives_now, lives0, getting_up);
    let (merged, from_friend) = match st.bosses.get(&id) {
        Some(&(old, f)) => {
            let m = BossRec::lower(old, seen);
            (m, f && m == old)
        }
        None => (seen, false),
    };
    if st.bosses.get(&id).is_none_or(|(old, _)| *old != merged) {
        st.bosses.insert(id, (merged, from_friend));
        st.out_bosses = true;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wipe_recent_window() {
        use super::recent_at;
        use std::time::{Duration, Instant};
        let w = Duration::from_secs(90);
        let t0 = Instant::now();
        assert!(!recent_at(None, t0, w));
        assert!(recent_at(Some(t0), t0, w));
        assert!(recent_at(Some(t0), t0 + Duration::from_secs(89), w));
        assert!(!recent_at(Some(t0), t0 + Duration::from_secs(90), w));
        assert!(!recent_at(Some(t0), t0 + Duration::from_secs(500), w));
    }

    use super::*;

    #[test]
    fn messages_roundtrip_and_reject_garbage() {
        let d = encode_dead(&[1100225, 1110200]);
        assert_eq!(decode(&d), Some(Msg::Dead(vec![1100225, 1110200])));
        let r = BossRec { hp: 1200, max_hp: 3000, lives: 1, lives0: 2 };
        let b = encode_bosses(&[(1100800, r)]);
        assert_eq!(decode(&b), Some(Msg::Bosses(vec![(1100800, r)])));
        assert_eq!(decode(&d[..d.len() - 1]), None);
        assert_eq!(decode(&[9, 1, 0, 0]), None);
        assert_eq!(decode(&[]), None);
    }

    /// P4: a wipe is every connected player dead within 90 s (3-6 players); one living peer
    /// keeps the world. Two players: the old pairwise rule.
    #[test]
    fn wipe_needs_every_player_within_the_window() {
        let t = Instant::now();
        let s = |x: u64| Some(t + Duration::from_secs(x));
        assert!(is_wipe(s(0), &[s(90)]), "two players, 90 s apart");
        assert!(!is_wipe(s(0), &[s(91)]));
        assert!(!is_wipe(None, &[s(0)]));
        assert!(!is_wipe(s(0), &[]), "alone");
        for n in 2..=5 {
            let mut deaths: Vec<Option<Instant>> = (0..n).map(|i| s(10 * i as u64)).collect();
            assert!(is_wipe(s(5), &deaths), "{} players all dead", n + 1);
            deaths[n - 1] = None;
            assert!(!is_wipe(s(5), &deaths), "one of {} players alive", n + 1);
            deaths[n - 1] = s(95);
            assert!(!is_wipe(s(0), &deaths), "first and last death 95 s apart");
            deaths[0] = s(10);
            assert!(is_wipe(s(10), &deaths), "85 s from the first to the last");
        }
    }

    #[test]
    fn the_lower_boss_state_wins() {
        let a = BossRec { hp: 500, max_hp: 3000, lives: 2, lives0: 2 };
        let b = BossRec { hp: 2900, max_hp: 3000, lives: 1, lives0: 2 };
        // Fewer lives beats more HP (a new life starts full).
        assert_eq!(BossRec::lower(a, b), b);
        assert_eq!(BossRec::lower(b, a), b);
        let c = BossRec { hp: 400, ..a };
        assert_eq!(BossRec::lower(a, c), c);
        // Live 2026-10-24: one machine's copy unscaled (full 2149), the other's scaled
        // (3000 of 5373, damaged): the damaged one is lower although its HP is larger.
        let unscaled_full = BossRec { hp: 2149, max_hp: 2149, lives: 1, lives0: 2 };
        let scaled_hurt = BossRec { hp: 3000, max_hp: 5373, lives: 1, lives0: 2 };
        assert_eq!(BossRec::lower(unscaled_full, scaled_hurt), scaled_hurt);
        assert_eq!(BossRec::lower(scaled_hurt, unscaled_full), scaled_hurt);
        // The same share: the record we had stays (no resend).
        let half = BossRec { hp: 1000, max_hp: 2000, lives: 1, lives0: 2 };
        let half2 = BossRec { hp: 2000, max_hp: 4000, lives: 1, lives0: 2 };
        assert_eq!(BossRec::lower(half, half2), half);
    }

    #[test]
    fn dead_phases_drop_only_their_record_until_the_encounter_is_defeated() {
        assert_eq!(records_to_drop(1000800, false), vec![1000800]);
        assert_eq!(records_to_drop(1000810, true), vec![1000800, 1000810]);
        assert_eq!(records_to_drop(1000999, true), vec![1000999]);
    }

    #[test]
    fn phase_one_is_not_restored_past_a_later_record() {
        let rec = BossRec { hp: 100, max_hp: 1000, lives: 1, lives0: 1 };
        let mut m = HashMap::new();
        assert!(!phase_one_stale(1000800, &m));
        m.insert(1000810, (rec, false));
        assert!(phase_one_stale(1000800, &m));
        assert!(!phase_one_stale(1000810, &m));
        assert!(!phase_one_stale(1000999, &m));
    }

    #[test]
    fn the_get_up_after_a_deathblow_is_not_a_record() {
        // Live 2026-10-24 (1120300): stealth deathblow, lives 2 -> 1, 1 HP for a moment
        // before the refill; that 1 HP was restored after the friend's death reload.
        let full = BossRec { hp: 5373, max_hp: 5373, lives: 2, lives0: 2 };
        let get_up = observed(1, 5373, 1, 2, true);
        assert_eq!(get_up, BossRec { hp: 5373, max_hp: 5373, lives: 1, lives0: 2 });
        let rec = BossRec::lower(full, get_up);
        assert_eq!(rec, get_up);
        assert!(rec.damaged());
        // Restored: one life less, full HP.
        assert_eq!(restore_plan(5373, 2, rec), Some((Some(1), 5373)));
        // Real damage after the get-up lowers it as before.
        let hurt = observed(1544, 5373, 1, 2, false);
        assert_eq!(BossRec::lower(rec, hurt), hurt);
    }

    #[test]
    fn a_phase_entity_without_a_death_reload_gets_no_record() {
        // Live 7.13: the gauge swap to Way of Tomoe (first sight mid-fight, no reload).
        assert!(!restore_allowed(false, None));
        assert!(!restore_allowed(false, Some(Duration::from_secs(120))));
        // After a death reload: the load's own set is restored ...
        assert!(restore_allowed(true, None));
        assert!(restore_allowed(true, Some(Duration::from_secs(1))));
        // ... a phase entity appearing mid-fight in that same load is not.
        assert!(!restore_allowed(true, Some(Duration::from_secs(120))));
    }

    #[test]
    fn a_resurrection_is_not_a_death_for_the_wipe() {
        // HP dips to 0 while DEATH_COMMITTED stays 0 (resurrection): no edge, no stamp.
        assert!(!commit_edge(0, 0));
        // The committed death: one edge, not repeated while it stays 1.
        assert!(commit_edge(0, 1));
        assert!(!commit_edge(1, 1));
        assert!(!commit_edge(1, 0));
        // Without a stamp for ours there is no wipe, whatever the peers did.
        let t0 = Instant::now();
        assert_eq!(wipe_deaths(None, &[Some(t0)], None), None);
        assert_eq!(wipe_deaths(None, &[Some(t0), Some(t0)], None), None);
    }

    #[test]
    fn every_wipe_pair_clears_once() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        // Live 2026-10-24 host: the friend died at +0, we at +89: a wipe.
        let first = wipe_deaths(Some(s(89)), &[Some(s(0))], None);
        assert_eq!(first, Some(vec![s(89), s(0)]));
        // Same pair again: nothing.
        assert_eq!(wipe_deaths(Some(s(89)), &[Some(s(0))], first.as_deref()), None);
        // The friend dies again at +107 (the real wipe): a new pair, cleared again.
        assert_eq!(wipe_deaths(Some(s(89)), &[Some(s(107))], first.as_deref()), Some(vec![s(89), s(107)]));
        // Far apart or one alive: no wipe.
        assert_eq!(wipe_deaths(Some(s(200)), &[Some(s(0))], None), None);
        assert_eq!(wipe_deaths(None, &[Some(s(0))], None), None);
        // Three players: one alive is no wipe; a new death of any of them is a new set.
        assert_eq!(wipe_deaths(Some(s(10)), &[Some(s(0)), None], None), None);
        let three = wipe_deaths(Some(s(10)), &[Some(s(0)), Some(s(20))], None);
        assert_eq!(three, Some(vec![s(10), s(0), s(20)]));
        assert_eq!(wipe_deaths(Some(s(10)), &[Some(s(0)), Some(s(20))], three.as_deref()), None);
        assert!(wipe_deaths(Some(s(10)), &[Some(s(30)), Some(s(20))], three.as_deref()).is_some());
    }

    #[test]
    fn a_fresh_copy_gets_the_damaged_record_only() {
        // Live owner report: the friend died alone at the boss, it came back at full HP.
        assert!(fresh_copy(3000, 3000, 2, 2));
        assert!(!fresh_copy(2000, 3000, 2, 2));
        assert!(!fresh_copy(3000, 3000, 1, 2));
        let rec = BossRec { hp: 1200, max_hp: 3000, lives: 1, lives0: 2 };
        assert_eq!(restore_plan(3000, 2, rec), Some((Some(1), 1200)));
        // Scaling changed the max: the record's share (40 %) goes onto the copy's max.
        assert_eq!(restore_plan(1000, 2, rec), Some((Some(1), 400)));
        assert_eq!(restore_plan(7500, 2, rec), Some((Some(1), 3000)));
        // Never 0 by rounding.
        assert_eq!(restore_plan(10, 2, BossRec { hp: 1, max_hp: 5373, lives: 1, lives0: 2 }), Some((Some(1), 1)));
        // Undamaged or dead records do nothing.
        assert_eq!(restore_plan(3000, 2, BossRec { hp: 3000, max_hp: 3000, lives: 2, lives0: 2 }), None);
        assert_eq!(restore_plan(3000, 2, BossRec { hp: 0, max_hp: 3000, lives: 0, lives0: 2 }), None);
        // A single-life gauge boss: HP only.
        assert_eq!(restore_plan(5000, 1, BossRec { hp: 2500, max_hp: 5000, lives: 1, lives0: 1 }), Some((None, 2500)));
    }
}
