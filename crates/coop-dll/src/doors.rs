//! Doors (ObjAct) stay open for both players (2026-10-24, NOT verified live).
//!
//! Live 7.4 (2026-10-24 01:30-02:05): a door one player opened ended up closed for the
//! other, and opening it there did not fix it. Root cause from IDA (copy
//! `F:\Temp\ida_world`, jobs w1-w6):
//! - the player's own ObjAct (`0x6BA7B0` -> `0x6BC1B0` with `info+114 = 1`) does not SET
//!   the door's event flag, it TOGGLES it: state vtable slot 4 (`0x6B6470`, door and
//!   one-way door; `0x6B64A0` one-loop) is `SetFlag(f, GetFlag(f) == 0)`, `f = state+0x10`;
//! - a flag that arrives from the friend (world.rs, SetEventFlag) changes only the flag;
//!   the door object stays closed until the block is loaded again (the ObjAct infos are
//!   set up from the flags at load);
//! - so: the friend opens door D (flag on, sent here, our door still closed); we open it
//!   too (our door opens, the toggle switches the flag OFF, sent to the friend); after
//!   our next load D is closed here, while the friend's door stays open until it reloads.
//!
//! The game's own replay of a friend's ObjAct (type 45, `0x6BE740` -> `0x6BA910(block,
//! index, actor, state)`) sets the flag to the given state (slot 5, no toggle) and plays
//! the object's animation. This module does the same from the flags we already sync:
//! 1. a door flag the friend switched ON whose door is loaded here and still closed is
//!    replayed with `0x6BA910(block, index, -1, 1)`;
//! 2. guard: our own write switching OFF a door flag the friend switched on this session
//!    is put back ON at once without telling the friend (world.rs `write_quiet`);
//! 3. guard (receiver): a friend's OFF for a known door flag that is ON here is ignored
//!    (vanilla doors never close again; world.rs `apply_flag`);
//! 4. after every rest all loaded doors whose flag is on are checked once.
//!
//! "Known door flag": the flag of a flag-bearing ObjAct state found by the walker, or,
//! as a fallback for ObjActs whose state keeps no flag (`SprjObjActOneState`, flag
//! driven by EMEVD instead), a flag the friend switched ON right after its character
//! played an ObjAct player animation (`ObjActParam.playerAnimId`, like ownworld.rs).
//! Guards 2/3 stay off for a while after a rest (rest-reset flags are no toggles).
//!
//! Layout, re-derived 2026-10-24 (copy `F:\Temp\ida_doors`, jobs d1-d7) — [VAR] = read in
//! the IDA output, [ARV] = inferred:
//! - `*(base+0x3D55EC8)` is the `FD4Singleton<SprjWorldObjActMan>` instance: `0x6B8400`
//!   and `0x810BB0` pass it straight to its update `0x6BE1B0` -> `0x6BE740` [VAR];
//! - manager `+0x28` i32 block count, `+0x30` inline array of 224-byte blocks
//!   (`0x6BE740`: `*(m+0x30) + 224*slot`, `slot < *(m+0x28)`) [VAR];
//! - block `+0x00` descriptor (`+0x20` = block slot, `0x6BA910`), `+0x10` u8 loaded
//!   (`0x6BC0A0` sets 1 after the setup `0x6B9FE0`, 0 after the unload `0x6BC8E0`),
//!   `+0x78` infos, `+0x80` i32 count, 304 bytes each (`0x6B9D10`) [VAR];
//! - info (ctor `0x6B02C0`, setup `0x6B2430`): `+0x08` own list index ("MyListIdx", =
//!   array index, `0x6B9FE0` passes `v30+v25`), `+0x50` object handle ("ObjHandle"; the
//!   argument of `0x899310`), `+0x54` run state, `+0x88` state object, `+0xA9` u8 state
//!   type, `+0xAC` i32 sub index, `+0xB0` primary info [VAR]. A type-1 door has FOUR
//!   infos (sub 0..3, same flag; `0x6B9FE0` links them, primary = sub 0) [VAR];
//! - state factory `0x6B64E0(info, type)`: 0 `SprjObjActOneState` (vt `0x298B570`, no
//!   flag; slot 16 is a nullsub), 1 `SprjObjActDoorState` (`0x298B628`), 2
//!   `SprjObjActOneLoopState` (`0x298B6E0`), 4 `SprjObjActOneDirOpenDoorState`
//!   (`0x298B798`); 1/2/4 keep the MSB EventFlagID at `state+0x10` (slot 16 `0x6B6E80` /
//!   `0x6B6E90` stores it) [VAR].
//! The old walker read this correctly but only accepted types 1/4 and every sub info.
//! It found 0 doors in 32/32 live checks, so the doors seen live are most likely
//! `OneState` ObjActs (no flag in the state) [ARV]: the first scan now logs a count per
//! state class to settle it.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::game::{self, read, read_ptr};
use crate::log;

const WORLD_OBJACT_MAN: usize = 0x3D55EC8;
const OBJ_MAN: usize = 0x3D5D600;
const MAN_BLOCK_COUNT: usize = 0x28;
const MAN_BLOCKS: usize = 0x30;
const BLOCK_STRIDE: usize = 224;
const BLOCK_LOADED: usize = 0x10;
const BLOCK_INFOS: usize = 0x78;
const BLOCK_INFO_COUNT: usize = 0x80;
const BLOCK_DESC_SLOT: usize = 0x20;
const INFO_STRIDE: usize = 304;
const INFO_INDEX: usize = 0x08;
const INFO_HANDLE: usize = 0x50;
const INFO_RUN_STATE: usize = 0x54;
const INFO_STATE: usize = 0x88;
const INFO_SUB: usize = 0xAC;
const STATE_FLAG: usize = 0x10;
/// State vtables (RVA) and their class.
const STATE_VTS: [(usize, Class); 4] = [
    (0x298B570, Class::One),
    (0x298B628, Class::Door),
    (0x298B6E0, Class::OneLoop),
    (0x298B798, Class::OneDirDoor),
];
const OBJ_ANIM: usize = 0x138;
const OBJ_ANIM_FLAGS: usize = 0x128;
/// `0x6BA910(block, index, actor handle, state)`: the game's replay of a friend's ObjAct.
const REPLAY_RVA: usize = 0x6BA910;
/// `0x6BCBC0(block, req, -1, slot, index, actor, state, 0)`: request builder (pure).
const REQUEST_RVA: usize = 0x6BCBC0;
/// `0x899310(ObjMan, handle) -> object`.
const OBJ_BY_HANDLE_RVA: usize = 0x899310;
/// Run state "executing" (`0x6B31E0` sets 3).
const RUN_EXECUTING: u32 = 3;
/// A friend door flag is replayed this long after it was applied.
const REPLAY_DELAY: Duration = Duration::from_millis(300);
/// Doors are checked this long after a rest (twice, see restsync.rs).
const AFTER_REST: [Duration; 2] = [Duration::from_millis(2500), Duration::from_millis(7500)];
/// A pending door flag that never finds a loaded closed door is dropped after this.
const PENDING_FOR: Duration = Duration::from_secs(5);
/// Fallback: a friend ON flag counts as a door flag when it arrives this long after the
/// friend's ObjAct animation started (the flag is written during / at the end of it)...
const OBJACT_FLAG_AFTER: Duration = Duration::from_secs(6);
/// ...or at most this long before the animation shows up here (snapshot vs reliable event).
const OBJACT_FLAG_BEFORE: Duration = Duration::from_millis(1500);
/// Guards 2/3 are off this long after any rest (rest-reset flags go off legitimately).
const REST_QUIET: Duration = Duration::from_secs(15);
const MAX_LOG: u32 = 60;
/// Sanity caps for the walk (garbage pointers must not make it loop forever).
const MAX_BLOCKS: u32 = 64;
const MAX_INFOS: i32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    One,
    Door,
    OneLoop,
    OneDirDoor,
}

impl Class {
    fn has_flag(self) -> bool {
        self != Class::One
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Door {
    block: usize,
    slot: i32,
    index: i32,
    flag: u32,
    handle: u32,
    running: bool,
}

/// What one walk saw (diagnostics).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Report {
    man: usize,
    blocks: u32,
    loaded: u32,
    /// Loaded blocks whose descriptor / info array could not be read.
    bad_blocks: u32,
    infos: u32,
    no_state: u32,
    one: u32,
    door: u32,
    one_loop: u32,
    one_dir: u32,
    other: u32,
    /// Flag-bearing infos skipped: sub index != 0 (door sub infos).
    subs: u32,
    /// Flag-bearing primaries skipped: flag 0 / -1 or index mismatch.
    bad: u32,
    /// The step that ended the walk early.
    fail: Option<&'static str>,
}

/// Fault-safe memory reads (the game, or a byte snapshot in the tests).
trait Mem {
    fn bytes(&self, addr: usize, out: &mut [u8]) -> bool;
    fn u8(&self, a: usize) -> Option<u8> {
        let mut b = [0u8; 1];
        self.bytes(a, &mut b).then_some(b[0])
    }
    fn u32(&self, a: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        self.bytes(a, &mut b).then(|| u32::from_le_bytes(b))
    }
    fn i32(&self, a: usize) -> Option<i32> {
        self.u32(a).map(|v| v as i32)
    }
    fn ptr(&self, a: usize) -> Option<usize> {
        let mut b = [0u8; 8];
        self.bytes(a, &mut b).then(|| u64::from_le_bytes(b) as usize).filter(|&p| p >= 0x10000)
    }
}

struct GameMem;

impl Mem for GameMem {
    fn bytes(&self, addr: usize, out: &mut [u8]) -> bool {
        game::read_bytes(addr, out)
    }
}

fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

fn le_ptr(b: &[u8], off: usize) -> usize {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap()) as usize
}

/// Walks WorldObjActMan (`base` = module base): every primary info of a loaded block
/// whose state keeps a flag.
fn scan(mem: &impl Mem, base: usize) -> (Vec<Door>, Report) {
    let mut out = Vec::new();
    let mut r = Report::default();
    let Some(man) = mem.ptr(base + WORLD_OBJACT_MAN) else {
        r.fail = Some("WorldObjActMan pointer (base+0x3D55EC8) is null");
        return (out, r);
    };
    r.man = man;
    let (Some(blocks), Some(count)) = (mem.ptr(man + MAN_BLOCKS), mem.u32(man + MAN_BLOCK_COUNT)) else {
        r.fail = Some("block array (+0x30) / count (+0x28) unreadable");
        return (out, r);
    };
    r.blocks = count;
    if count == 0 || count > MAX_BLOCKS {
        r.fail = Some("block count (+0x28) is 0 or out of range");
        return (out, r);
    }
    let mut info = [0u8; INFO_STRIDE];
    for b in 0..count as usize {
        let block = blocks + BLOCK_STRIDE * b;
        if mem.u8(block + BLOCK_LOADED).unwrap_or(0) == 0 {
            continue;
        }
        r.loaded += 1;
        let slot = mem.ptr(block).and_then(|d| mem.i32(d + BLOCK_DESC_SLOT));
        let infos = mem.ptr(block + BLOCK_INFOS);
        let n = mem.i32(block + BLOCK_INFO_COUNT);
        let (Some(slot), Some(infos), Some(n)) = (slot, infos, n) else {
            r.bad_blocks += 1;
            continue;
        };
        for i in 0..n.clamp(0, MAX_INFOS) as usize {
            let at = infos + INFO_STRIDE * i;
            if !mem.bytes(at, &mut info) {
                r.bad += 1;
                continue;
            }
            r.infos += 1;
            let state = le_ptr(&info, INFO_STATE);
            let class = if state < 0x10000 {
                None
            } else {
                mem.ptr(state).and_then(|vt| STATE_VTS.iter().find(|(rva, _)| base + rva == vt).map(|&(_, c)| c))
            };
            match class {
                None if state < 0x10000 => r.no_state += 1,
                None => r.other += 1,
                Some(Class::One) => r.one += 1,
                Some(Class::Door) => r.door += 1,
                Some(Class::OneLoop) => r.one_loop += 1,
                Some(Class::OneDirDoor) => r.one_dir += 1,
            }
            if !class.is_some_and(Class::has_flag) {
                continue;
            }
            if le_u32(&info, INFO_SUB) != 0 {
                r.subs += 1;
                continue;
            }
            let index = le_u32(&info, INFO_INDEX) as i32;
            let flag = mem.u32(state + STATE_FLAG).unwrap_or(0);
            if flag == u32::MAX || flag == 0 || index != i as i32 {
                r.bad += 1;
                continue;
            }
            out.push(Door {
                block,
                slot,
                index,
                flag,
                handle: le_u32(&info, INFO_HANDLE),
                running: le_u32(&info, INFO_RUN_STATE) == RUN_EXECUTING,
            });
        }
    }
    if r.loaded == 0 {
        r.fail = Some("no loaded block (+0x10)");
    }
    (out, r)
}

// ------------------------------------------------------------------ pure decisions

/// The friend's ObjAct animation (started at `anim`) and its ON flag (arrived at `flag`)
/// belong together.
fn objact_near(anim: Option<Instant>, flag: Instant) -> bool {
    anim.is_some_and(|a| {
        if flag >= a {
            flag.duration_since(a) <= OBJACT_FLAG_AFTER
        } else {
            a.duration_since(flag) <= OBJACT_FLAG_BEFORE
        }
    })
}

/// Guard 2: our own write switched `id` OFF; put it back on?
fn keep_open_decision(friend_on: bool, known_door: bool, rest_quiet: bool) -> bool {
    friend_on && known_door && !rest_quiet
}

/// Guard 3: the friend sent OFF for `id`; ignore it?
fn ignore_off_decision(on: bool, here_on: Option<bool>, known_door: bool, rest_quiet: bool) -> bool {
    !on && here_on == Some(true) && known_door && !rest_quiet
}

/// Door flags are map flags (group 1 / 2) [ARV from the live ids].
fn map_flag(id: u32) -> bool {
    matches!(id / 10_000_000 % 10, 1 | 2)
}

/// An animation is an ObjAct player animation (full id or id within its category, as
/// ownworld.rs `objact_anim`).
fn is_objact_anim(anim: i32, set: &HashSet<u32>) -> bool {
    anim > 0 && (set.contains(&(anim as u32)) || set.contains(&(anim as u32 % 1_000_000)))
}

// ------------------------------------------------------------------ state

#[derive(Default)]
struct State {
    /// Door flags the friend switched ON this session (guard 2).
    friend_on: HashSet<u32>,
    /// Flags of flag-bearing ObjAct states seen by any walk (static game data).
    door_flags: HashSet<u32>,
    /// Fallback door flags: friend ON right after its ObjAct animation.
    candidates: HashSet<u32>,
    /// Friend door flags waiting for the replay (flag, applied at).
    pending: Vec<(u32, Instant)>,
    /// Friend ON map flags of the last seconds (for an ObjAct animation seen late).
    recent_on: Vec<(u32, Instant)>,
    /// Each peer's last seen TimeAct id (slot -> anim).
    peer_anim: std::collections::BTreeMap<u8, i32>,
    friend_objact_at: Option<Instant>,
    /// A new load was seen: walk once on the next tick.
    scan_due: bool,
    last_in_world: Duration,
    rest_seen: Option<Instant>,
    rest_checks: usize,
    replays: u32,
    kept_on: u32,
    ignored_off: u32,
    logged: u32,
    logged_scan: bool,
    logged_doors: bool,
    fails_logged: Vec<&'static str>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(State::default))
}

fn note_log(s: &mut State, line: impl FnOnce() -> String) {
    if s.logged < MAX_LOG {
        s.logged += 1;
        log::line(&line());
    }
}

fn rest_quiet(now: Instant) -> bool {
    crate::restsync::last_rest().is_some_and(|t| now.duration_since(t) < REST_QUIET)
}

fn known_door(s: &State, id: u32) -> bool {
    s.door_flags.contains(&id) || s.candidates.contains(&id)
}

/// `ObjActParam.playerAnimId` of every row (once the params are loaded).
fn objact_anims() -> Option<&'static HashSet<u32>> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{OBJ_ACT_PARAM_ST, ParamDef};
    use sekiro::sprj::SoloParamRepository;
    static ANIMS: OnceLock<HashSet<u32>> = OnceLock::new();
    if let Some(a) = ANIMS.get() {
        return Some(a);
    }
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = repo.params().find(|p| p.struct_name() == OBJ_ACT_PARAM_ST::NAME).map(|p| &*p.data)?;
    let set: HashSet<u32> = unsafe { file.rows::<OBJ_ACT_PARAM_ST>() }
        .map(|(_, r)| r.player_anim_id())
        .filter(|&a| a > 0 && a != u32::MAX)
        .collect();
    if set.is_empty() {
        return None;
    }
    Some(ANIMS.get_or_init(|| set))
}

/// Walks the game's ObjAct infos; remembers the door flags and logs the diagnostics
/// (first scan with a loaded block, first scan with doors, each failure step once).
fn doors() -> Vec<Door> {
    let (all, r) = scan(&GameMem, game::module_base());
    with(|s| {
        s.door_flags.extend(all.iter().map(|d| d.flag));
        if let Some(step) = r.fail {
            if !s.fails_logged.contains(&step) && s.fails_logged.len() < 8 {
                s.fails_logged.push(step);
                log::line(&format!("doors: scan failed: {step} (manager {:#x}, {} blocks, {} loaded)", r.man, r.blocks, r.loaded));
            }
            return;
        }
        if !s.logged_scan || (!s.logged_doors && !all.is_empty()) {
            s.logged_scan = true;
            s.logged_doors = !all.is_empty();
            log::line(&format!(
                "doors: scan: WorldObjActMan {:#x}, {} blocks, {} loaded ({} unreadable), {} ObjAct infos (state: one-state {}, door {}, one-loop {}, one-dir-door {}, none {}, other {}); {} door sub infos skipped, {} bad; {} doors with a flag {:?}",
                r.man,
                r.blocks,
                r.loaded,
                r.bad_blocks,
                r.infos,
                r.one,
                r.door,
                r.one_loop,
                r.one_dir,
                r.no_state,
                r.other,
                r.subs,
                r.bad,
                all.len(),
                all.iter().map(|d| d.flag).take(12).collect::<Vec<_>>()
            ));
        }
    });
    all
}

/// The object of `d` already shows the animation the "opened" execution would play.
fn looks_open(d: &Door) -> Option<bool> {
    let base = game::module_base();
    let mut req = [0u32; 16];
    let build: extern "C" fn(usize, *mut u32, i32, i32, i32, i32, u32, u8) -> u8 = unsafe { std::mem::transmute(base + REQUEST_RVA) };
    if build(d.block, req.as_mut_ptr(), -1, d.slot, d.index, -1, 1, 0) == 0 {
        return None;
    }
    let objs = read_ptr(base + OBJ_MAN)?;
    let get: extern "C" fn(usize, u32) -> usize = unsafe { std::mem::transmute(base + OBJ_BY_HANDLE_RVA) };
    let obj = get(objs, d.handle);
    if obj < 0x10000 {
        return None;
    }
    let anim = read::<u32>(obj + OBJ_ANIM)?;
    let flags = read::<u8>(obj + OBJ_ANIM_FLAGS)?;
    Some(anim == req[6] && flags & 4 != 0)
}

/// Opens `d` like the game replays a friend's ObjAct, unless it is open or running.
fn open_if_closed(s: &mut State, d: &Door, why: &str) -> bool {
    if d.running || !crate::game::event_flag(d.flag).unwrap_or(false) {
        return false;
    }
    match looks_open(d) {
        Some(false) => {}
        Some(true) => return false,
        None => {
            note_log(s, || format!("doors: door flag {} (block slot {}, index {}): object not found here, not replayed", d.flag, d.slot, d.index));
            return false;
        }
    }
    let replay: extern "C" fn(usize, i32, i32, u32) -> u8 = unsafe { std::mem::transmute(game::module_base() + REPLAY_RVA) };
    let ok = replay(d.block, d.index, -1, 1) != 0;
    s.replays += 1;
    let n = s.replays;
    note_log(s, || {
        format!(
            "doors: {why}: door flag {} is on but its door (handle {:#x}, block slot {}, index {}) was closed here: opened (0x6BA910 ok = {ok}, {} so far)",
            d.flag, d.handle, d.slot, d.index, n
        )
    });
    ok
}

fn add_candidate(s: &mut State, id: u32, why: &str) {
    if s.door_flags.contains(&id) || s.candidates.len() >= 512 || !s.candidates.insert(id) {
        return;
    }
    s.friend_on.insert(id);
    note_log(s, || format!("doors: flag {id} counts as a door flag ({why}): never switched off again by a toggle or the friend"));
}

/// world.rs: a friend's flag was applied here.
pub fn friend_flag(id: u32, on: bool) {
    if !on || !map_flag(id) {
        return;
    }
    let now = Instant::now();
    with(|s| {
        if s.door_flags.contains(&id) {
            s.friend_on.insert(id);
        }
        if objact_near(s.friend_objact_at, now) {
            add_candidate(s, id, "the friend set it right after an ObjAct animation");
        }
        s.recent_on.retain(|(_, t)| now.duration_since(*t) <= OBJACT_FLAG_BEFORE);
        if s.recent_on.len() < 64 {
            s.recent_on.push((id, now));
        }
        if s.pending.len() < 256 && !s.pending.iter().any(|(f, _)| *f == id) {
            s.pending.push((id, now));
        }
    });
}

/// world.rs, our own write of `id` switching it OFF while it was on: true = it is a door
/// flag the friend opened this session, and our ObjAct toggle just closed it (guard 2):
/// the caller writes it back on quietly.
pub fn keep_open(id: u32) -> bool {
    let now = Instant::now();
    let quiet = rest_quiet(now);
    with(|s| {
        if !keep_open_decision(s.friend_on.contains(&id), known_door(s, id), quiet) {
            return false;
        }
        s.kept_on += 1;
        let n = s.kept_on;
        note_log(s, || format!("doors: our ObjAct toggled door flag {id} off, but the friend had opened it: kept on ({n} so far)"));
        true
    })
}

/// world.rs `apply_flag`, a friend's value for `id`: true = it is an OFF for a known door
/// flag that is ON here (guard 3): the caller drops it (and tells the friend it is on).
pub fn ignore_friend_off(id: u32, on: bool) -> bool {
    if on || !map_flag(id) {
        return false;
    }
    let now = Instant::now();
    let quiet = rest_quiet(now);
    let known = with(|s| known_door(s, id));
    if !known {
        return false;
    }
    if !ignore_off_decision(on, crate::game::event_flag(id), known, quiet) {
        return false;
    }
    with(|s| {
        s.ignored_off += 1;
        let n = s.ignored_off;
        note_log(s, || format!("doors: the friend switched door flag {id} off (its ObjAct toggle); ignored, it stays on here ({n} so far)"));
    });
    true
}

/// world.rs, a new session: the friend's door flags of the last one no longer count.
pub fn reset() {
    with(|s| {
        s.friend_on.clear();
        s.pending.clear();
        s.recent_on.clear();
        s.friend_objact_at = None;
    });
}

/// world.rs, every frame while connected and settled (game thread; flags readable).
/// `peer_anims`: every other player's newest TimeAct id (slot, anim; net snapshots; the
/// 2-player "friend" generalized: any peer's ObjAct animation marks the flags that arrive
/// with it); `in_world_for`: time since our last load.
pub fn tick(peer_anims: &[(u8, i32)], in_world_for: Duration) {
    let now = Instant::now();
    // 0. The peers' ObjAct animations (fallback door flags) and our loads.
    let started = with(|s| {
        if in_world_for < s.last_in_world {
            s.scan_due = true;
        }
        s.last_in_world = in_world_for;
        let started: Vec<i32> = peer_anims
            .iter()
            .filter(|&&(slot, anim)| s.peer_anim.insert(slot, anim) != Some(anim))
            .map(|&(_, anim)| anim)
            .collect();
        s.peer_anim.retain(|slot, _| peer_anims.iter().any(|(p, _)| p == slot));
        started
    });
    if !started.is_empty() && objact_anims().is_some_and(|set| started.iter().any(|&a| is_objact_anim(a, set))) {
        with(|s| {
            s.friend_objact_at = Some(now);
            let late: Vec<u32> = s.recent_on.iter().filter(|(_, t)| now.duration_since(*t) <= OBJACT_FLAG_BEFORE).map(|(f, _)| *f).collect();
            for f in late {
                add_candidate(s, f, "the friend set it right before its ObjAct animation showed up here");
            }
        });
    }
    if with(|s| std::mem::take(&mut s.scan_due) || !s.logged_scan && s.fails_logged.is_empty()) {
        doors();
    }
    // 1. Friend door flags: replay once the flag has settled.
    let due: Vec<u32> = with(|s| {
        let mut due = Vec::new();
        s.pending.retain(|&(f, t)| {
            if now.duration_since(t) < REPLAY_DELAY {
                return true;
            }
            due.push(f);
            false
        });
        due
    });
    if !due.is_empty() {
        let all = doors();
        with(|s| {
            for f in due {
                let mine: Vec<&Door> = all.iter().filter(|d| d.flag == f).collect();
                if mine.is_empty() {
                    continue; // not a walked door, or its block is not loaded here
                }
                s.friend_on.insert(f);
                for d in mine {
                    open_if_closed(s, d, "the friend opened a door");
                }
            }
            s.pending.retain(|&(_, t)| now.duration_since(t) < PENDING_FOR);
        });
    }
    // 4. After a rest: every loaded door whose flag is on must be open.
    let rest = crate::restsync::last_rest();
    let check = with(|s| {
        if rest != s.rest_seen {
            s.rest_seen = rest;
            s.rest_checks = 0;
        }
        s.rest_checks < AFTER_REST.len() && rest.is_some_and(|t| now.duration_since(t) >= AFTER_REST[s.rest_checks])
    });
    if check {
        let all = doors();
        with(|s| {
            s.rest_checks += 1;
            let on = all.iter().filter(|d| crate::game::event_flag(d.flag) == Some(true)).count();
            let opened = all.iter().filter(|d| open_if_closed(s, d, "after a rest")).count();
            note_log(s, || format!("doors: after the rest: {} doors loaded, {on} with the flag on, {opened} were closed and opened again", all.len()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Sparse little-endian memory: (start, bytes).
    #[derive(Default)]
    struct Snap(BTreeMap<usize, Vec<u8>>);

    impl Snap {
        fn put(&mut self, at: usize, data: Vec<u8>) {
            self.0.insert(at, data);
        }
        fn put_u64(&mut self, at: usize, v: u64) {
            self.put(at, v.to_le_bytes().to_vec());
        }
    }

    impl Mem for Snap {
        fn bytes(&self, addr: usize, out: &mut [u8]) -> bool {
            let Some((&start, data)) = self.0.range(..=addr).next_back() else { return false };
            let off = addr - start;
            if off + out.len() > data.len() {
                return false;
            }
            out.copy_from_slice(&data[off..off + out.len()]);
            true
        }
    }

    const BASE: usize = 0x1_4000_0000;
    const MAN: usize = 0x10_0000;
    const BLOCKS: usize = 0x20_0000;
    const DESC: usize = 0x30_0000;
    const INFOS: usize = 0x40_0000;
    const STATES: usize = 0x50_0000;

    /// (state class vt RVA or 0 = no state, flag, sub index, index override)
    fn world(infos: &[(usize, u32, u32, Option<u32>)], loaded: u8) -> Snap {
        let mut m = Snap::default();
        m.put_u64(BASE + WORLD_OBJACT_MAN, MAN as u64);
        let mut man = vec![0u8; 0x40];
        man[MAN_BLOCK_COUNT..MAN_BLOCK_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
        man[MAN_BLOCKS..MAN_BLOCKS + 8].copy_from_slice(&(BLOCKS as u64).to_le_bytes());
        m.put(MAN, man);
        // Block 0 not loaded (garbage pointers: never read), block 1 loaded.
        let mut blocks = vec![0u8; BLOCK_STRIDE * 2];
        let b1 = BLOCK_STRIDE;
        blocks[b1..b1 + 8].copy_from_slice(&(DESC as u64).to_le_bytes());
        blocks[b1 + BLOCK_LOADED] = loaded;
        blocks[b1 + BLOCK_INFOS..b1 + BLOCK_INFOS + 8].copy_from_slice(&(INFOS as u64).to_le_bytes());
        blocks[b1 + BLOCK_INFO_COUNT..b1 + BLOCK_INFO_COUNT + 4].copy_from_slice(&(infos.len() as u32).to_le_bytes());
        blocks[BLOCK_INFOS..BLOCK_INFOS + 8].copy_from_slice(&0xDEAD_0000u64.to_le_bytes());
        blocks[BLOCK_INFO_COUNT..BLOCK_INFO_COUNT + 4].copy_from_slice(&9999u32.to_le_bytes());
        m.put(BLOCKS, blocks);
        let mut desc = vec![0u8; 0x40];
        desc[BLOCK_DESC_SLOT..BLOCK_DESC_SLOT + 4].copy_from_slice(&7u32.to_le_bytes());
        m.put(DESC, desc);
        let mut arr = vec![0u8; INFO_STRIDE * infos.len()];
        for (i, &(vt, flag, sub, idx)) in infos.iter().enumerate() {
            let at = INFO_STRIDE * i;
            arr[at + INFO_INDEX..at + INFO_INDEX + 4].copy_from_slice(&idx.unwrap_or(i as u32).to_le_bytes());
            arr[at + INFO_HANDLE..at + INFO_HANDLE + 4].copy_from_slice(&(0x100 + i as u32).to_le_bytes());
            arr[at + INFO_SUB..at + INFO_SUB + 4].copy_from_slice(&sub.to_le_bytes());
            if vt != 0 {
                let st = STATES + 0x100 * i;
                arr[at + INFO_STATE..at + INFO_STATE + 8].copy_from_slice(&(st as u64).to_le_bytes());
                let mut s = vec![0u8; 0x18];
                s[0..8].copy_from_slice(&((BASE + vt) as u64).to_le_bytes());
                s[STATE_FLAG..STATE_FLAG + 4].copy_from_slice(&flag.to_le_bytes());
                m.put(st, s);
            }
        }
        m.put(INFOS, arr);
        m
    }

    #[test]
    fn walker_finds_primary_flag_states_only() {
        let m = world(
            &[
                (0x298B570, 0, 0, None),          // one-state: counted, no flag
                (0x298B628, 21005024, 0, None),   // door primary
                (0x298B628, 21005024, 1, None),   // door sub infos
                (0x298B628, 21005024, 2, None),
                (0x298B628, 21005024, 3, None),
                (0x298B6E0, 11000670, 0, None),   // one-loop
                (0x298B798, 11000671, 0, None),   // one-dir door
                (0x298B798, u32::MAX, 0, None),   // no flag
                (0x298B798, 11000672, 0, Some(99)), // index mismatch
                (0, 0, 0, None),                  // no state
                (0x1234, 0, 0, None),             // unknown vtable
            ],
            1,
        );
        let (doors, r) = scan(&m, BASE);
        let flags: Vec<u32> = doors.iter().map(|d| d.flag).collect();
        assert_eq!(flags, vec![21005024, 11000670, 11000671]);
        assert_eq!(doors[0], Door { block: BLOCKS + BLOCK_STRIDE, slot: 7, index: 1, flag: 21005024, handle: 0x101, running: false });
        assert_eq!((r.man, r.blocks, r.loaded, r.infos), (MAN, 2, 1, 11));
        assert_eq!((r.one, r.door, r.one_loop, r.one_dir, r.no_state, r.other), (1, 4, 1, 3, 1, 1));
        assert_eq!((r.subs, r.bad, r.fail), (3, 2, None));
    }

    #[test]
    fn walker_reports_the_failed_step() {
        let m = world(&[(0x298B628, 21005024, 0, None)], 0);
        let (doors, r) = scan(&m, BASE);
        assert!(doors.is_empty());
        assert_eq!(r.fail, Some("no loaded block (+0x10)"));
        let empty = Snap::default();
        assert_eq!(scan(&empty, BASE).1.fail, Some("WorldObjActMan pointer (base+0x3D55EC8) is null"));
        let mut m = world(&[], 1);
        m.0.get_mut(&MAN).unwrap()[MAN_BLOCK_COUNT..MAN_BLOCK_COUNT + 4].copy_from_slice(&500u32.to_le_bytes());
        assert_eq!(scan(&m, BASE).1.fail, Some("block count (+0x28) is 0 or out of range"));
    }

    #[test]
    fn objact_window() {
        let a = Instant::now();
        assert!(objact_near(Some(a), a + Duration::from_secs(5)));
        assert!(!objact_near(Some(a), a + Duration::from_secs(7)));
        assert!(objact_near(Some(a + Duration::from_secs(1)), a));
        assert!(!objact_near(Some(a + Duration::from_secs(2)), a));
        assert!(!objact_near(None, a));
    }

    #[test]
    fn guard_decisions() {
        // Guard 2: only a door flag the friend opened, never right after a rest.
        assert!(keep_open_decision(true, true, false));
        assert!(!keep_open_decision(false, true, false));
        assert!(!keep_open_decision(true, false, false));
        assert!(!keep_open_decision(true, true, true));
        // Guard 3: a friend OFF of a known door flag that is on here.
        assert!(ignore_off_decision(false, Some(true), true, false));
        assert!(!ignore_off_decision(true, Some(true), true, false));
        assert!(!ignore_off_decision(false, Some(false), true, false));
        assert!(!ignore_off_decision(false, None, true, false));
        assert!(!ignore_off_decision(false, Some(true), false, false));
        assert!(!ignore_off_decision(false, Some(true), true, true));
    }

    #[test]
    fn flag_and_anim_filters() {
        assert!(map_flag(21005024) && map_flag(11000670));
        assert!(!map_flag(9900) && !map_flag(71000000) && !map_flag(31000000));
        let set: HashSet<u32> = [700250].into_iter().collect();
        assert!(is_objact_anim(700250, &set));
        assert!(is_objact_anim(100_700_250, &set));
        assert!(!is_objact_anim(700251, &set) && !is_objact_anim(-1, &set));
    }
}
