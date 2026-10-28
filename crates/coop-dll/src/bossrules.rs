//! Boss rules (docs/SPEC-BOSS-RULES.md): the friend's boss fight is announced,
//! a player who dies in it waits on the death screen until it is over, a rest of
//! the friend sends a player in the arena home, and the killer's item lots are
//! given to the friend too.
//!
//! Hooks (entry detours, `hook::install_detour`):
//! - `0xA8F7E0` the reload to the last idol after a death. While `SPEC_WANTED` it is
//!   not run but remembered (state `SPEC_WAITING`, phase A: the death screen stays);
//!   `release` calls it later through the trampoline.
//! - `0xA740D0` "create CSDeadStep now?" (phase B, settings `spectate_camera = 1`):
//!   while `SPEC_B_OK` and the player is dead with no resurrection on offer it
//!   answers no (state `SPEC_SPECTATE`): no death screen, no penalty yet, the world
//!   stays and `spectate_cam` puts the camera on the friend. A release lets the
//!   predicate through again, so the normal death (penalty, screen, reload) follows.
//! - `0x6D7070` the load request every warp / reload ends in: while we wait dead it is
//!   not run (live 7.4: a route around the held reload reloaded the host 4 s into phase B).
//! - `0x689260` system event; `edx == 0xFD2` is HandleBossDefeat.
//! - `0x918810` AwardItemLot; during a fresh boss defeat the event flags it sets are
//!   recorded from the flag ring with its lot id.
//!
//! The detours only touch atomics; everything else runs in `BossRules::on_frame`
//! on the frame task (decisions, network, logging).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use coop_net::Snapshot;

use crate::bosses::{self, Encounter, Sig, Verdict};
use crate::game::{self, Chr, module_base, read_ptr};
use crate::hook::{self, FlagEntry};
use crate::log;
use crate::net::Net;
use crate::peers::{PeerFrame, PeerId, PeerSet};
use crate::spectate_cam;
use crate::tuning;
use crate::world::{self, World};

const RELOAD_RVA: usize = 0xA8F7E0;
// push rbx; push rbp; push rsi; push rdi; push r14; sub rsp, 30h (no RIP-relative code)
const RELOAD_PROLOGUE: [u8; 11] = [0x40, 0x53, 0x55, 0x56, 0x57, 0x41, 0x56, 0x48, 0x83, 0xEC, 0x30];
const DEFEAT_RVA: usize = 0x689260;
// sub rsp, 38h; mov [rsp+20h], r9d
const DEFEAT_PROLOGUE: [u8; 9] = [0x48, 0x83, 0xEC, 0x38, 0x44, 0x89, 0x4C, 0x24, 0x20];
const AWARD_RVA: usize = 0x918810;
// mov rax, rsp; push rbp; push rsi; push rdi; push r12
const AWARD_PROLOGUE: [u8; 8] = [0x48, 0x8B, 0xC4, 0x55, 0x56, 0x57, 0x41, 0x54];
/// `*(base+0x3D6CDC0)` is the manager `AwardItemLot` is called on.
const ITEM_MAN_RVA: usize = 0x3D6CDC0;
/// System event id of HandleBossDefeat in `0x689260`.
const SYS_EVENT_BOSS_DEFEAT: u32 = 0xFD2;
/// `bool 0xA740D0(InGameStep*)`: true = `0xA72A70` creates the CSDeadStep (called
/// once per frame from the InGame step `0xA71190`, its only caller).
const DEAD_STEP_RVA: usize = 0xA740D0;
// push rbx; sub rsp, 20h. The next instruction (cmp cs:[0x3EA0D74], 0) is
// RIP-relative and stays in place: the trampoline jumps back to +6.
const DEAD_STEP_PROLOGUE: [u8; 6] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x20];
/// The predicate reads the main player as `*(*(base+0x3D7A1E0) + 0x88)` and
/// offers resurrection when `f32 +0x212C > 0` (`0xA2B5F0`) or `f32 +0x2138 >= 0` (`0xA2B610`).
/// `u8 0x6D7070()`: the load request (GameMan `+0xB4A` and `+0x10` = 1, IDA v75 o6/o7).
/// Every warp / reload route ends here: the death reload `0xA8F7E0`, EMEVD WarpPlayer
/// (`0x68E7F0`, called twice from the dispatcher `0x904EF0`), the Lua event warps
/// (`0x68E850` from ten `OnEvent_*` handlers, `0x68E870` = our `game::warp_to_idol`) and
/// `0xEBA2C0`. Live 7.4 (host, 1792795992-96): 4 s into phase B the world reloaded
/// without passing the held `0xA8F7E0` (no "a reload came during phase B" line), so the
/// route was one of the others. Held while we spectate dead (phase B safety net 2).
const LOAD_REQ_RVA: usize = 0x6D7070;
// mov [rsp+10h], rbx; push rdi; sub rsp, 20h (no RIP-relative code)
const LOAD_REQ_PROLOGUE: [u8; 10] = [0x48, 0x89, 0x5C, 0x24, 0x10, 0x57, 0x48, 0x83, 0xEC, 0x20];
const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const WCM_MAIN_PLAYER: usize = 0x88;
const REVIVE_A: usize = 0x212C;
const REVIVE_B: usize = 0x2138;

/// Return addresses of the known callers of the reload (SPEC §1.2), as RVAs.
const RET_EMEVD: usize = 0x9076AB;
const RET_ESD: usize = 0x11C8FF7;

const SPEC_OFF: u8 = 0;
/// The death reload was held back; the death screen stays.
const SPEC_WAITING: u8 = 1;
/// We called the original reload (phase A) or let the CSDeadStep through (phase B);
/// the load that follows resets the state.
const SPEC_RELEASING: u8 = 2;
/// Phase B: dead in the world, no CSDeadStep yet, the camera follows the friend.
const SPEC_SPECTATE: u8 = 3;

static SPEC_STATE: AtomicU8 = AtomicU8::new(SPEC_OFF);
/// The frame task's verdict: a reload now should be deferred.
static SPEC_WANTED: AtomicBool = AtomicBool::new(false);
/// The frame task's verdict for phase B: everything in `SPEC_WANTED` except our
/// own death (the predicate sees the death in the same frame the HP drops, the
/// frame task one frame later), and `spectate_camera = 1`.
static SPEC_B_OK: AtomicBool = AtomicBool::new(false);
/// A reload was deferred (OFF -> WAITING); the frame task logs and tells the friend.
static SPEC_STARTED: AtomicBool = AtomicBool::new(false);
/// The CSDeadStep was held back (OFF -> SPECTATE); same for phase B.
static SPEC_B_STARTED: AtomicBool = AtomicBool::new(false);
/// A reload arrived while in phase B (SPECTATE -> WAITING): logged once.
static SPEC_B_TO_A: AtomicBool = AtomicBool::new(false);
/// `reload_detour` returned without running the reload, and nobody has run it since.
/// Whoever ends a wait outside `BossRules::release` (panic reset, warp) checks it.
static HELD_RELOAD: AtomicBool = AtomicBool::new(false);
static DEFER_COUNT: AtomicU32 = AtomicU32::new(0);
static DEFER_RET: AtomicUsize = AtomicUsize::new(0);
static RELEASE_REQ: Mutex<Option<&'static str>> = Mutex::new(None);
static EXPEL_REQ: AtomicBool = AtomicBool::new(false);

static RELOAD_TRAMP: AtomicUsize = AtomicUsize::new(0);
static DEAD_STEP_TRAMP: AtomicUsize = AtomicUsize::new(0);
static LOAD_REQ_TRAMP: AtomicUsize = AtomicUsize::new(0);
/// Load requests (`0x6D7070`) held while we spectate dead; the frame task logs new ones.
static LOAD_HELD: AtomicU32 = AtomicU32::new(0);
/// A load request is held and not yet replayed / dropped; its arguments are in `LOAD_HELD_ARGS`.
static LOAD_HELD_PENDING: AtomicBool = AtomicBool::new(false);
/// The arguments of the last held `0x6D7070` call (replayed by a phase-B release).
static LOAD_HELD_ARGS: [AtomicUsize; 4] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
/// The wait was armed by `load_req_detour` (the death load came as a load request, no 0xA8F7E0
/// call): the held request is the death load itself and a release replays it (v8.22).
static LOAD_DEATH_ARMED: AtomicBool = AtomicBool::new(false);
/// A rest while we are dead and not spectating: the frame task forces the reload now.
static FORCE_REQ: AtomicBool = AtomicBool::new(false);
/// The last held request's call stack (return addresses, up to 6), for the log.
static LOAD_HELD_STACK: [AtomicUsize; 6] =
    [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
static DEFEAT_TRAMP: AtomicUsize = AtomicUsize::new(0);
static AWARD_TRAMP: AtomicUsize = AtomicUsize::new(0);

static DEFEAT_PENDING: AtomicBool = AtomicBool::new(false);
static DEFEAT_ENTITY: AtomicI32 = AtomicI32::new(0);
static DEFEAT_BANNER: AtomicI32 = AtomicI32::new(0);
/// `game::now_ms` of the last HandleBossDefeat, 0 = none.
static DEFEAT_AT_MS: AtomicU64 = AtomicU64::new(0);
/// Our own `AwardItemLot` call (a friend's reward): not recorded again.
static REWARD_APPLYING: AtomicBool = AtomicBool::new(false);
/// Lots the game gave us since the last boss defeat: (lot, flags it set).
static COLLECTED: Mutex<Lots> = Mutex::new(Vec::new());
static NOTICES: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

/// An AwardItemLot this soon after a boss defeat belongs to the boss.
const REWARD_WINDOW_MS: u64 = 15_000;
/// The killer sends its lots this long after the defeat (the fight's awards are done).
const REWARD_SEND_AFTER: Duration = Duration::from_secs(15);
/// The receiver waits this long: its own events may give the items first.
const REWARD_APPLY_AFTER: Duration = Duration::from_secs(10);
/// The check that every unique good of an awarded boss lot arrived runs this long after it.
const REWARD_VERIFY_AFTER: Duration = Duration::from_secs(4);
const MAX_LOTS: usize = 64;
const MAX_LOT_FLAGS: usize = 64;
/// A gauge that goes off this soon after a defeat is the boss falling, not a retreat.
/// A peer that started a boss fight this recently still counts as engaged in ours after a load
/// cleared its live `boss` (the host reloads, the friend dies right after).
const PEER_BOSS_GRACE: Duration = Duration::from_secs(15);
const DEFEAT_WINDOW: Duration = Duration::from_secs(10);
/// Our fight counts as the friend's when they are this close (their `EV_BOSS` missed).
const FRIEND_NEAR_M: f32 = 60.0;
/// Waiting on the death screen longer than this wakes us up.
const SPECTATE_MAX: Duration = Duration::from_secs(5 * 60);
/// The friend must be gone this long before we stop waiting for them.
const GONE_AFTER: Duration = Duration::from_secs(5);
/// The friend's HP must stay at 0 this long (no revive) before it counts as a wipe. Only a
/// fallback for a lost EV_DEATH: Sekiro's Resurrect choice waits without limit (live 2026-10-24:
/// 5 s declared a wipe while the friend was still choosing, the friend lost the resurrection).
const DEAD_AFTER: Duration = Duration::from_secs(90);
/// Dead in `RELEASING` with no load and no death screen this long: force the reload,
/// and again this long after it: force a warp to our idol.
const RELEASING_STEP: Duration = Duration::from_secs(15);
/// After the forced warp, `RELEASING` is given up (the global watchdog takes over).
const RELEASING_GIVE_UP: Duration = Duration::from_secs(30);
/// Dead, not spectating, no load and no death screen this long: force the reload.
const DEAD_IDLE_MAX: Duration = Duration::from_secs(45);
/// A rest forces the reload only after we were dead this long (the death screen has time to come).
const REST_FORCE_AFTER: Duration = Duration::from_secs(3);
/// MenuMan menu state 250 = the death screen (live 2026-10-24); `base+0x3D67408`, states at `+0x20`.
const MENU_MAN_RVA: usize = 0x3D67408;
const MENU_STATES: usize = 0x20;
const MENU_DEATH_SCREEN: usize = 250;
/// No player for this long while waiting = a real load / quit to the menu.
const NO_PLAYER_MAX: Duration = Duration::from_secs(3);
const VK_F8: i32 = 0x77;

/// Flag recorder (diagnostics): 10 s before and 3 s after a boss gauge appears.
const REC_BEFORE: Duration = Duration::from_secs(10);
const REC_AFTER: Duration = Duration::from_secs(3);
const REC_HISTORY: usize = 2048;
const REC_LOG_MAX: usize = 400;

#[link(name = "user32")]
unsafe extern "system" {
    fn GetAsyncKeyState(vk: i32) -> i16;
}

type Fn4 = extern "C" fn(usize, usize, usize, usize) -> usize;
/// Item lots with the event flags each one set: (lot, flags).
type Lots = Vec<(i32, Vec<u32>)>;

fn call_original(tramp: &AtomicUsize, a: usize, b: usize, c: usize, d: usize) -> usize {
    let t = tramp.load(Ordering::Acquire);
    if t == 0 {
        // Hooked but the trampoline address is not stored yet (install window).
        return 0;
    }
    let f: Fn4 = unsafe { std::mem::transmute(t) };
    f(a, b, c, d)
}

/// `0xA8F7E0`: the death reload. Held back while the frame task wants a spectator.
extern "C" fn reload_detour(a: usize, b: usize, c: usize, d: usize) -> usize {
    // The mod gave up (3 panics): the game's own reload, always.
    if !crate::mod_alive() {
        return call_original(&RELOAD_TRAMP, a, b, c, d);
    }
    let ret = hook::detour_caller(RELOAD_TRAMP.load(Ordering::Acquire));
    let state = SPEC_STATE.load(Ordering::Acquire);
    let dead = main_player_hp().is_some_and(|hp| hp <= 0);
    match state {
        SPEC_WAITING => {
            note_call(&RELOAD_DIAG, ret, state, dead, true);
            DEFER_RET.store(ret, Ordering::Relaxed);
            DEFER_COUNT.fetch_add(1, Ordering::Relaxed);
            HELD_RELOAD.store(true, Ordering::Release);
            return 0;
        }
        // Phase B safety net: something reloads without a CSDeadStep (EMEVD / ESD on
        // their own). Hold it like phase A; the predicate then lets the CSDeadStep
        // through (death screen), and the release calls the reload.
        SPEC_SPECTATE
            if SPEC_STATE.compare_exchange(SPEC_SPECTATE, SPEC_WAITING, Ordering::AcqRel, Ordering::Acquire).is_ok() =>
        {
            note_call(&RELOAD_DIAG, ret, state, dead, true);
            DEFER_RET.store(ret, Ordering::Relaxed);
            DEFER_COUNT.fetch_add(1, Ordering::Relaxed);
            HELD_RELOAD.store(true, Ordering::Release);
            SPEC_B_TO_A.store(true, Ordering::Release);
            return 0;
        }
        SPEC_OFF
            if SPEC_WANTED.load(Ordering::Acquire)
                && SPEC_STATE.compare_exchange(SPEC_OFF, SPEC_WAITING, Ordering::AcqRel, Ordering::Acquire).is_ok() =>
        {
            note_call(&RELOAD_DIAG, ret, state, dead, true);
            DEFER_RET.store(ret, Ordering::Relaxed);
            DEFER_COUNT.fetch_add(1, Ordering::Relaxed);
            HELD_RELOAD.store(true, Ordering::Release);
            SPEC_STARTED.store(true, Ordering::Release);
            return 0;
        }
        _ => {}
    }
    note_call(&RELOAD_DIAG, ret, state, dead, false);
    call_original(&RELOAD_TRAMP, a, b, c, d)
}

/// The latest call of a hooked function, for the frame task's log (the detours run on the
/// game's thread and only store atomics). `info` packs: bits 0..32 caller RVA, 32..40 spectate
/// state, bit 40 SPEC_WANTED, bit 41 player dead, bit 42 held.
struct CallDiag {
    count: AtomicU32,
    info: AtomicU64,
}
impl CallDiag {
    const fn new() -> Self {
        Self { count: AtomicU32::new(0), info: AtomicU64::new(0) }
    }
}
static RELOAD_DIAG: CallDiag = CallDiag::new();
static LOADREQ_DIAG: CallDiag = CallDiag::new();
/// v8.20: the first dead frame's verdict, kept for the whole death (see `spec_wanted`).
static SPEC_LATCH: AtomicBool = AtomicBool::new(false);

fn note_call(diag: &CallDiag, ret: usize, state: u8, dead: bool, held: bool) {
    let rva = ret.wrapping_sub(module_base()) as u32 as u64;
    let info = rva
        | u64::from(state) << 32
        | u64::from(SPEC_WANTED.load(Ordering::Acquire)) << 40
        | u64::from(dead) << 41
        | u64::from(held) << 42;
    diag.info.store(info, Ordering::Release);
    diag.count.fetch_add(1, Ordering::AcqRel);
}

/// The wait verdict: dead, and either the live verdict or the first dead frame's latch says wait.
fn spec_wanted(wanted_if_dead: bool, latch: bool, dead: bool) -> bool {
    (wanted_if_dead || latch) && dead
}

/// Frame task: logs the newest recorded call of each hooked function, at most one line per
/// second per function and only while dead or a wait is on.
fn diag_log(_dead: bool) {
    static SEEN: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];
    static LAST_MS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    for (i, (diag, name, addr)) in [(&RELOAD_DIAG, "reload", "0xA8F7E0"), (&LOADREQ_DIAG, "loadreq", "0x6D7070")].into_iter().enumerate() {
        let n = diag.count.load(Ordering::Acquire);
        if n == SEEN[i].load(Ordering::Relaxed) {
            continue;
        }
        let info = diag.info.load(Ordering::Acquire);
        let state = (info >> 32) as u8;
        let call_dead = info >> 41 & 1 == 1;
        let now = mono_ms();
        if !(call_dead || state != SPEC_OFF) || now.saturating_sub(LAST_MS[i].load(Ordering::Relaxed)) < 1000 {
            // Not interesting, or rate limited (kept pending: logged on a later frame if still relevant).
            if !(call_dead || state != SPEC_OFF) {
                SEEN[i].store(n, Ordering::Relaxed);
            }
            continue;
        }
        SEEN[i].store(n, Ordering::Relaxed);
        LAST_MS[i].store(now, Ordering::Relaxed);
        log::line(&format!(
            "{name}: {addr} called #{n} from RVA {:#x}, state {state}, wanted {}, dead {call_dead} -> {}",
            info as u32,
            info >> 40 & 1 == 1,
            if info >> 42 & 1 == 1 { "held" } else { "passed" }
        ));
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn RtlCaptureStackBackTrace(skip: u32, n: u32, out: *mut usize, hash: *mut u32) -> u16;
}

/// The main player's HP (`*(*(base+0x3D7A1E0)+0x88)`), None when unreadable.
fn main_player_hp() -> Option<i32> {
    let p = read_ptr(module_base() + WORLD_CHR_MAN_RVA).and_then(|w| read_ptr(w + WCM_MAIN_PLAYER))?;
    Chr(p).hp()
}

/// `0x6D7070()`: the game's load request. While we wait dead (phase B in the world, or
/// the phase A death screen) any load request that reaches here is a route around our
/// held death reload (live 7.4: the host's world reloaded 4 s into phase B): not run.
/// Our own release sets `RELEASING` (or `OFF`) before it calls the reload, so that one
/// passes; our warps need a living player, so they are never held.
extern "C" fn load_req_detour(a: usize, b: usize, c: usize, d: usize) -> usize {
    let mut state = SPEC_STATE.load(Ordering::Acquire);
    let dead = main_player_hp().is_some_and(|hp| hp <= 0);
    let caller = hook::detour_caller(LOAD_REQ_TRAMP.load(Ordering::Acquire));
    // v8.21: the death load can come as a load request (Lua event warp 0x68E850) without the
    // reload 0xA8F7E0 ever being called (live 2026-10-25). Dead + a wait wanted: arm phase A
    // here; the release runs the reload (no arguments) as phase A does.
    if crate::mod_alive()
        && dead
        && state == SPEC_OFF
        && SPEC_WANTED.load(Ordering::Acquire)
        && SPEC_STATE.compare_exchange(SPEC_OFF, SPEC_WAITING, Ordering::AcqRel, Ordering::Acquire).is_ok()
    {
        DEFER_RET.store(caller, Ordering::Relaxed);
        DEFER_COUNT.fetch_add(1, Ordering::Relaxed);
        HELD_RELOAD.store(true, Ordering::Release);
        LOAD_DEATH_ARMED.store(true, Ordering::Release);
        SPEC_STARTED.store(true, Ordering::Release);
        state = SPEC_WAITING;
    }
    let hold = crate::mod_alive() && matches!(state, SPEC_SPECTATE | SPEC_WAITING) && dead;
    note_call(&LOADREQ_DIAG, caller, state, dead, hold);
    if hold {
        let mut frames = [0usize; 8];
        let n = unsafe { RtlCaptureStackBackTrace(0, 8, frames.as_mut_ptr(), std::ptr::null_mut()) } as usize;
        // Frame 0 is this detour; the game's callers follow.
        let base = module_base();
        let game: Vec<usize> = frames[..n.min(8)].iter().copied().filter(|&f| f > base && f < base + 0x4000000).collect();
        for (i, slot) in LOAD_HELD_STACK.iter().enumerate() {
            slot.store(game.get(i).copied().unwrap_or(0), Ordering::Relaxed);
        }
        if game.is_empty() {
            LOAD_HELD_STACK[0].store(hook::detour_caller(LOAD_REQ_TRAMP.load(Ordering::Acquire)), Ordering::Relaxed);
        }
        for (slot, v) in LOAD_HELD_ARGS.iter().zip([a, b, c, d]) {
            slot.store(v, Ordering::Relaxed);
        }
        LOAD_HELD_PENDING.store(true, Ordering::Release);
        LOAD_HELD.fetch_add(1, Ordering::AcqRel);
        return 0;
    }
    call_original(&LOAD_REQ_TRAMP, a, b, c, d)
}

/// No resurrection on offer for the main player (both windows closed). False when
/// anything is unreadable, so the game's own death runs.
fn no_revive_window() -> bool {
    let Some(p) = read_ptr(module_base() + WORLD_CHR_MAN_RVA).and_then(|w| read_ptr(w + WCM_MAIN_PLAYER)) else {
        return false;
    };
    match (game::read::<f32>(p + REVIVE_A), game::read::<f32>(p + REVIVE_B)) {
        (Some(a), Some(b)) => {
            REVIVE_SEEN_A.store(a.to_bits(), Ordering::Release);
            REVIVE_SEEN_B.store(b.to_bits(), Ordering::Release);
            a <= 0.0 && b < 0.0
        }
        _ => false,
    }
}

/// The two resurrection fields as `no_revive_window` last read them (the dead-step detour
/// only stores atomics); logged at a spectate start (v8.10: a friend died with no Resurrect
/// on offer and the log could not say why, live 2026-10-25).
static REVIVE_SEEN_A: AtomicU32 = AtomicU32::new(0);
static REVIVE_SEEN_B: AtomicU32 = AtomicU32::new(0);

fn revive_fields_now() -> String {
    let now = read_ptr(module_base() + WORLD_CHR_MAN_RVA)
        .and_then(|w| read_ptr(w + WCM_MAIN_PLAYER))
        .map(|p| (game::read::<f32>(p + REVIVE_A), game::read::<f32>(p + REVIVE_B)));
    format!(
        "resurrection fields at the death decision {:.3} / {:.3} (offered when the first is > 0 or the second >= 0), now {now:?}",
        f32::from_bits(REVIVE_SEEN_A.load(Ordering::Acquire)),
        f32::from_bits(REVIVE_SEEN_B.load(Ordering::Acquire)),
    )
}

/// `0xA740D0(step)`: phase B. While spectating, no CSDeadStep is created.
extern "C" fn dead_step_detour(step: usize) -> bool {
    let t = DEAD_STEP_TRAMP.load(Ordering::Acquire);
    if t == 0 {
        return false;
    }
    let original: extern "C" fn(usize) -> bool = unsafe { std::mem::transmute(t) };
    // True only with g_deadLatch == 0, no CSDeadStep yet, and the player dead or
    // offered a resurrection.
    if !original(step) {
        return false;
    }
    // The mod gave up: the game's own death step, always.
    if !crate::mod_alive() {
        return true;
    }
    // v8.15: read the two window fields on every dead frame (atomics only), so the frame task can
    // log them whichever branch below runs (phase B is off, the reads used to happen only there).
    let window_closed = no_revive_window();
    match SPEC_STATE.load(Ordering::Acquire) {
        SPEC_SPECTATE => false,
        SPEC_OFF if SPEC_B_OK.load(Ordering::Acquire) => {
            // v8.47 hybrid: decided from the game's own Resurrect conditions (cached by the
            // frame task, `RES_ELIGIBLE`). The game's 110090 / 110093 signal only comes after the
            // death step exists, so waiting for it deadlocked (host deaths 2026-10-26).
            // v8.49: the inventory conditions also hold in a Terror death, where the game never
            // offers it; the CSDeadStep's own choice reads the two window fields, so with them
            // closed no step is made for up to RES_WINDOW_GRACE_MS (phase B if they stay closed).
            let now = mono_ms();
            let start = match RES_GRACE_START.compare_exchange(0, now, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => now,
                Err(t) => t,
            };
            let waited = now.saturating_sub(start);
            let sp_no = SP_NO_REVIVE.load(Ordering::Acquire);
            let Some(elig) = effective_elig(RES_ELIGIBLE.load(Ordering::Acquire), window_closed, waited) else {
                return false;
            };
            let signal = RES_SIGNAL.load(Ordering::Acquire);
            let d = hybrid_decision(elig, true);
            let (ret, outcome) = match d {
                Hybrid::GameDeath => (true, RES_OUT_GAME),
                Hybrid::Spectate => {
                    // Phase B spectate; if the state moved on meanwhile, the game's own death.
                    if SPEC_STATE.compare_exchange(SPEC_OFF, SPEC_SPECTATE, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                        SPEC_B_STARTED.store(true, Ordering::Release);
                        PHASE_B_DEATH.store(true, Ordering::Release);
                        (false, RES_OUT_SPECTATE)
                    } else {
                        (true, RES_OUT_GAME)
                    }
                }
            };
            if RES_LOGGED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                RES_OUTCOME.store(
                    outcome
                        | (u64::from(signal) << 8)
                        | (u64::from(elig) << 16)
                        | (waited.min(0xFFFF) << 24)
                        | (u64::from(window_closed) << 40)
                        | (u64::from(sp_no) << 41),
                    Ordering::Release,
                );
            }
            ret
        }
        _ => true,
    }
}

/// Hybrid phase B (v8.47): a death inside a wanted wait gets no CSDeadStep (the world stays
/// visible, the camera works) when the game's own Resurrect conditions say none is on offer.
/// Decided at the first dead frame from `resurrect_eligible` (cached while alive, `RES_ELIGIBLE`):
/// the game's SpEffect signal (110090 / 110093) only appears AFTER the death step exists, so
/// waiting for it deadlocked (host deaths 2026-10-26: never offered). The signal is only logged.
/// Eligible: the game's death screen + Resurrect choice. Not eligible: phase B spectate.
/// Unknown (reads failed): the game's death screen, the safe side.
const HYBRID_B_LIVE: bool = true;

const SIG_UNDECIDED: u8 = 0;
const SIG_OFFERED: u8 = 1;
const SIG_NOT_OFFERED: u8 = 2;

const ELIG_UNKNOWN: u8 = 0;
const ELIG_YES: u8 = 1;
const ELIG_NO: u8 = 2;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Hybrid {
    /// The game's own death screen (Resurrect on offer or unknown, or spectating is not wanted).
    GameDeath,
    /// Phase B spectate.
    Spectate,
}

/// The pure decision of the dead-step detour: `elig` (ELIG_*), whether spectating is wanted
/// (SPEC_B_OK). Never waits.
fn hybrid_decision(elig: u8, spec_wanted: bool) -> Hybrid {
    if elig == ELIG_NO && spec_wanted { Hybrid::Spectate } else { Hybrid::GameDeath }
}

/// How long a death with both resurrection window fields closed waits for the game to open
/// one before it counts as not offered (v8.49).
const RES_WINDOW_GRACE_MS: u64 = 1500;
/// SpEffect 8007: in every "not offered" dump of 2026-10-27, but also in an OFFERED one and
/// alternating with 8002 on every dead player. Only logged since v8.57.
const SP_NO_RESURRECT: i32 = 8007;

/// The eligibility the dead-step detour acts on (ELIG_*), None = no death step yet, ask again
/// next frame. The inventory conditions failing: no; holding: yes at once (the game's own death
/// screen decides the offer). Unknown: the window fields open = unknown (the game's death
/// screen), both closed = wait up to RES_WINDOW_GRACE_MS, then no.
/// v8.57: SpEffect 8007 is no "not offered" signal (it alternates with 8002 on every dead
/// player and sat in an OFFERED dump, 2026-10-27), and holding the death step while the window
/// fields were closed cost the host Resurrect in two Divine Dragon deaths with lock 0 and nodes
/// (16:37:25, 16:49:50: "eligible no ... waited 787 ms"). A Terror death now shows the game's
/// death screen without Resurrect (phase A) instead of phase B.
fn effective_elig(elig: u8, window_closed: bool, waited_ms: u64) -> Option<u8> {
    if elig != ELIG_UNKNOWN || !window_closed {
        Some(elig)
    } else if waited_ms < RES_WINDOW_GRACE_MS {
        None
    } else {
        Some(ELIG_NO)
    }
}

/// The game's Resurrect conditions (0xA34B70, docs/RE-RESURRECT-FRIEND.md section 1): the skill is
/// learnt, the lock (PlayerGameData+0x150) is free, and a temporary node (goods 1110) or a node
/// (goods 1100) without SpEffect 400 is in the inventory. v8.61: and the byte
/// PlayerGameData+0x178 is clear (`0xA2CC30` keeps the window shut while it is set, IDA
/// 2026-10-27; live 2026-10-28: set at the first dead frame in all 5 "lock 0 + nodes, NOT
/// offered" deaths, clear in all 15 OFFERED ones).
fn resurrect_eligible(lock: f32, has_skill: bool, goods_1110: i32, goods_1100: i32, sp400: bool, no_revive: bool) -> bool {
    has_skill && lock == 0.0 && !no_revive && (goods_1110 > 0 || (!sp400 && goods_1100 > 0))
}

const GAME_DATA_MAN_RVA: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
const PGD_REVIVE_LOCK: usize = 0x150;
/// PlayerGameData+0x178 (byte): one of `0xA2CC30`'s window blocks (statelog.rs `resurrect_blocks`).
const PGD_NO_REVIVE: usize = 0x178;
const SP_NO_NODES: i32 = 400;

fn pgd_ptr() -> Option<usize> {
    read_ptr(module_base() + GAME_DATA_MAN_RVA).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))
}

/// Reads the inputs of `resurrect_eligible` (None = something unreadable). Reads game memory:
/// frame task only.
fn read_resurrect_eligible() -> Option<bool> {
    let pgd = pgd_ptr()?;
    let lock = game::read::<f32>(pgd + PGD_REVIVE_LOCK)?;
    let no_revive = game::read::<u8>(pgd + PGD_NO_REVIVE).is_some_and(|b| b != 0);
    let inv = crate::audit::read_inventory()?;
    let q = |g: u32| inv.iter().find(|(id, _)| *id == 0x4000_0000 + g).map_or(0, |(_, n)| *n as i32);
    let sp400 = crate::statelog::sp_ids(game::player()?.0).contains(&SP_NO_NODES);
    Some(resurrect_eligible(lock, q(2000) > 0, q(1110), q(1100), sp400, no_revive))
}

/// `[GAMEPLAY] clear_stale_no_revive` (default on): see `clear_stale_no_revive`.
pub static CLEAR_STALE_NO_REVIVE: AtomicBool = AtomicBool::new(true);
/// This death went to phase B (no CSDeadStep); cleared once alive again and checked.
static PHASE_B_DEATH: AtomicBool = AtomicBool::new(false);

/// v8.61: write PGD+0x178 back to 0? Live 2026-10-28: the game sets it when Resurrect is not
/// taken and its own death step clears it again within ~10 s (host 10:40:13 -> 10:40:23,
/// 10:51:20 -> 10:51:33), but phase B makes no death step, so after every phase B death it
/// stayed 1 through the reload and the next death offered no Resurrect with lock 0 and nodes
/// (friend 10:19:24, 10:42:00, 10:53:47; host 10:33:08, 10:44:57). Only once alive after a
/// phase B death.
fn clear_stale_no_revive(enabled: bool, phase_b_death: bool, alive: bool, byte: u8) -> bool {
    enabled && phase_b_death && alive && byte != 0
}

/// Frame task: after a phase B death, once alive (HP > 0) again, PGD+0x178 back to 0.
fn stale_no_revive_step(alive: bool) {
    if !alive || !PHASE_B_DEATH.load(Ordering::Acquire) {
        return;
    }
    if game::player().and_then(|p| p.hp()).is_none_or(|hp| hp <= 0) {
        return;
    }
    PHASE_B_DEATH.store(false, Ordering::Release);
    let Some(pgd) = pgd_ptr() else { return };
    let Some(byte) = game::read::<u8>(pgd + PGD_NO_REVIVE) else { return };
    let enabled = CLEAR_STALE_NO_REVIVE.load(Ordering::Relaxed);
    if clear_stale_no_revive(enabled, true, true, byte) {
        let ok = game::write(pgd + PGD_NO_REVIVE, 0u8);
        log::line(&format!(
            "resurrect: PGD+0x178 = {byte} after a phase B death (no death step cleared it; it shuts Resurrect at the next death): written 0, ok = {ok}"
        ));
        RES_ELIG_AT.store(0, Ordering::Release);
    } else {
        log::line(&format!(
            "resurrect: PGD+0x178 = {byte} after a phase B death{}",
            if byte != 0 { " (clear_stale_no_revive = 0: left set, the next death offers no Resurrect)" } else { ": nothing to clear" }
        ));
    }
}

/// Diagnostic (v8.61): every change of PGD+0x178, read every 250 ms.
fn watch_no_revive(dead: bool) {
    static LAST: AtomicU8 = AtomicU8::new(0xFF);
    static AT: AtomicU64 = AtomicU64::new(0);
    let now = mono_ms();
    if now.saturating_sub(AT.load(Ordering::Relaxed)) < 250 {
        return;
    }
    AT.store(now, Ordering::Relaxed);
    let Some(b) = pgd_ptr().and_then(|g| game::read::<u8>(g + PGD_NO_REVIVE)) else { return };
    let prev = LAST.swap(b, Ordering::Relaxed);
    if prev != 0xFF && prev != b {
        log::line(&format!(
            "resurrect: PGD+0x178 {prev} -> {b} (dead {dead}, spectate state {}, phase B death {})",
            SPEC_STATE.load(Ordering::Acquire),
            PHASE_B_DEATH.load(Ordering::Acquire)
        ));
    }
}

/// The cached `resurrect_eligible` (ELIG_*): refreshed every ~500 ms while alive and once on the
/// first dead frame (a failed read keeps the last value).
static RES_ELIGIBLE: AtomicU8 = AtomicU8::new(ELIG_UNKNOWN);
/// `mono_ms` of the last refresh while alive.
static RES_ELIG_AT: AtomicU64 = AtomicU64::new(0);
/// The first-dead-frame refresh of this death is done (reset when alive).
static RES_ELIG_DEAD_READ: AtomicBool = AtomicBool::new(false);

fn refresh_resurrect_eligible() {
    match read_resurrect_eligible() {
        Some(true) => RES_ELIGIBLE.store(ELIG_YES, Ordering::Release),
        Some(false) => RES_ELIGIBLE.store(ELIG_NO, Ordering::Release),
        None => {}
    }
}

/// `mono_ms` of this death's first undecided dead-step call (0 = none; reset when alive).
static RES_GRACE_START: AtomicU64 = AtomicU64::new(0);
/// SpEffect 8007 is on the dead player (frame task; false while alive).
static SP_NO_REVIVE: AtomicBool = AtomicBool::new(false);

/// The Resurrect signal (SIG_*), updated by the frame task every frame (0 while alive). Only logged.
static RES_SIGNAL: AtomicU8 = AtomicU8::new(SIG_UNDECIDED);
/// This death's decision was made and recorded (reset when alive).
static RES_LOGGED: AtomicBool = AtomicBool::new(false);
/// outcome | signal << 8 | eligibility << 16, for the frame task's one log line (0 = none).
static RES_OUTCOME: AtomicU64 = AtomicU64::new(0);
const RES_OUT_GAME: u64 = 1;
const RES_OUT_SPECTATE: u64 = 2;

/// Frame task, every frame: the Resurrect eligibility cache and the (log-only) signal.
fn update_resurrect_signal(dead: bool) {
    if !dead {
        RES_SIGNAL.store(SIG_UNDECIDED, Ordering::Release);
        RES_LOGGED.store(false, Ordering::Release);
        RES_ELIG_DEAD_READ.store(false, Ordering::Release);
        RES_GRACE_START.store(0, Ordering::Release);
        SP_NO_REVIVE.store(false, Ordering::Release);
        let now = mono_ms();
        if now.saturating_sub(RES_ELIG_AT.load(Ordering::Acquire)) >= 500 {
            RES_ELIG_AT.store(now, Ordering::Release);
            refresh_resurrect_eligible();
        }
        return;
    }
    if !RES_ELIG_DEAD_READ.swap(true, Ordering::AcqRel) {
        refresh_resurrect_eligible();
    }
    if let Some(p) = game::player() {
        SP_NO_REVIVE.store(crate::statelog::sp_ids(p.0).contains(&SP_NO_RESURRECT), Ordering::Release);
    }
    let sig = match crate::statelog::player_resurrect_verdict() {
        Some(true) => SIG_OFFERED,
        Some(false) => SIG_NOT_OFFERED,
        None => SIG_UNDECIDED,
    };
    RES_SIGNAL.store(sig, Ordering::Release);
}

/// Frame task: the one log line per death once the dead-step detour decided.
fn log_resurrect_outcome() {
    let v = RES_OUTCOME.swap(0, Ordering::AcqRel);
    if v == 0 {
        return;
    }
    let sig = match ((v >> 8) & 0xFF) as u8 {
        SIG_OFFERED => "offered",
        SIG_NOT_OFFERED => "not offered",
        _ => "undecided",
    };
    let elig = match ((v >> 16) & 0xFF) as u8 {
        ELIG_YES => "yes",
        ELIG_NO => "no",
        _ => "unknown",
    };
    let what = match v & 0xFF {
        RES_OUT_SPECTATE => "phase B spectate",
        _ => "game death screen",
    };
    log::line(&format!(
        "death: resurrect eligible {elig} -> {what} (game signal so far: {sig}; window fields {}, SpEffect 8007 {}, waited {} ms)",
        if v >> 40 & 1 == 1 { "closed" } else { "open" },
        v >> 41 & 1 == 1,
        (v >> 24) & 0xFFFF
    ));
}

/// Millisecond clock since the first call (the dead-step detour only touches atomics).
fn mono_ms() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// An event flag of the active bank (unreadable = off), for `bosses::classify`.
fn flag_on(id: u32) -> bool {
    game::event_flag(id) == Some(true)
}

/// `0x689260(eventMan, systemEventId, entity, banner)`: records HandleBossDefeat.
extern "C" fn defeat_detour(a: usize, b: usize, c: usize, d: usize) -> usize {
    if b as u32 == SYS_EVENT_BOSS_DEFEAT {
        DEFEAT_ENTITY.store(c as i32, Ordering::Relaxed);
        DEFEAT_BANNER.store(d as i32, Ordering::Relaxed);
        DEFEAT_AT_MS.store(game::now_ms(), Ordering::Release);
        DEFEAT_PENDING.store(true, Ordering::Release);
    }
    call_original(&DEFEAT_TRAMP, a, b, c, d)
}

/// `0x918810(itemMan, lot, 1)`: after a fresh defeat, the flags the call sets are the lot's.
/// Every other call is a script grant (EMEVD / ESD talk, keyitems.rs): its items and lot
/// flags are captured for the friend.
extern "C" fn award_detour(a: usize, b: usize, c: usize, d: usize) -> usize {
    let applying = REWARD_APPLYING.load(Ordering::Acquire);
    // The friend's grant of this lot reached us first: our own script's copy is a double.
    if !applying && crate::keyitems::skip_local_award(b as i32) {
        return 0;
    }
    let at = DEFEAT_AT_MS.load(Ordering::Acquire);
    // Also 15 s after a table boss's defeat flag turned on here (keyitems.rs): a synced 9301
    // can start the reward event before our own HBD (bosses.rs, Gyoubu [ARV]).
    let boss_window = (at != 0 && game::now_ms().saturating_sub(at) < REWARD_WINDOW_MS)
        || crate::keyitems::defeat_flag_window();
    let collect = boss_window && !applying && tuning::boss_reward();
    let mark = hook::flag_ring_count();
    if !applying {
        crate::keyitems::award_begin();
    }
    let r = call_original(&AWARD_TRAMP, a, b, c, d);
    // Auditor (audit.rs): this lot's source, for comparing item-lot flags later.
    crate::audit::note_award(b as i32, boss_window || applying);
    if collect {
        collect_lot(b as i32, mark);
    }
    if !applying {
        crate::keyitems::award_end(b as i32, flags_since(mark), boss_window);
    }
    r
}

/// Flags switched on (real world) between ring position `mark` and now.
fn collect_lot(lot: i32, mark: u32) {
    let flags = flags_since(mark);
    if lot > 0
        && let Ok(mut lots) = COLLECTED.lock()
        && lots.len() < MAX_LOTS
    {
        lots.push((lot, flags));
    }
}

/// Flags switched on (real world) between ring position `mark` and now, each once.
fn flags_since(mark: u32) -> Vec<u32> {
    let count = hook::flag_ring_count();
    let mut n = mark;
    if count.wrapping_sub(n) > hook::FLAG_RING {
        n = count.wrapping_sub(hook::FLAG_RING);
    }
    let mut flags: Vec<u32> = Vec::new();
    while n != count && flags.len() < MAX_LOT_FLAGS {
        if let FlagEntry::Ready(id, true, 0) = hook::flag_ring_entry(n)
            && !flags.contains(&id)
        {
            flags.push(id);
        }
        n = n.wrapping_add(1);
    }
    flags
}

pub fn install() {
    // Script item grants (keyitems.rs): the GrantItems hook reads what each award gives.
    crate::keyitems::install();
    let hooks: [(usize, &[u8], Fn4, &str, &AtomicUsize); 3] = [
        (RELOAD_RVA, &RELOAD_PROLOGUE, reload_detour, "boss reload (0xA8F7E0)", &RELOAD_TRAMP),
        (DEFEAT_RVA, &DEFEAT_PROLOGUE, defeat_detour, "system event (0x689260)", &DEFEAT_TRAMP),
        (AWARD_RVA, &AWARD_PROLOGUE, award_detour, "AwardItemLot (0x918810)", &AWARD_TRAMP),
    ];
    let mut ok = 0;
    for (rva, prologue, detour, name, slot) in hooks {
        if let Some(tramp) = hook::install_detour(rva, prologue, detour as usize, name) {
            slot.store(tramp, Ordering::Release);
            ok += 1;
        }
    }
    // Phase B and the camera: only with the reload hook (the safety net of both).
    let reload_ok = RELOAD_TRAMP.load(Ordering::Acquire) != 0;
    let dead_step: extern "C" fn(usize) -> bool = dead_step_detour;
    let dead_step_ok = reload_ok
        && hook::install_detour(DEAD_STEP_RVA, &DEAD_STEP_PROLOGUE, dead_step as usize, "CSDeadStep predicate (0xA740D0)")
            .inspect(|&t| DEAD_STEP_TRAMP.store(t, Ordering::Release))
            .is_some();
    let cam_ok = reload_ok && spectate_cam::install();
    // Phase B safety net 2: load requests around the held reload (EMEVD / Lua warps).
    let load_ok = dead_step_ok
        && hook::install_detour(LOAD_REQ_RVA, &LOAD_REQ_PROLOGUE, load_req_detour as *const () as usize, "load request (0x6D7070)")
            .inspect(|&t| LOAD_REQ_TRAMP.store(t, Ordering::Release))
            .is_some();
    log::line(&format!(
        "bossrules: {ok}/3 hooks installed, phase B {dead_step_ok}, spectator camera {cam_ok}, load requests held while spectating {load_ok}"
    ));
}

/// We wait for the friend: on the death screen (phase A) or dead in the world (phase B).
pub fn spectating() -> bool {
    matches!(SPEC_STATE.load(Ordering::Acquire), SPEC_WAITING | SPEC_SPECTATE)
}

/// The death screen is up (MenuMan menu state 250 != 0).
fn menu_death_screen() -> bool {
    read_ptr(module_base() + MENU_MAN_RVA)
        .and_then(|m| game::read::<i32>(m + MENU_STATES + 4 * MENU_DEATH_SCREEN))
        .is_some_and(|v| v != 0)
}

/// What to do about a `RELEASING` state with no load following.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Wait,
    ForceReload,
    ForceWarp,
    Off,
}

/// The dead watchdog of `RELEASING`. `since` = time since the release or the last forced
/// step, `tries` = forced steps so far. Alive ends it; a load pending or the death
/// screen up waits (the load's own `on_loading` resets the state).
fn releasing_action(dead: bool, since: Duration, load_pending: bool, menu250: bool, tries: u8) -> Action {
    if !dead {
        return Action::Off;
    }
    if load_pending || menu250 {
        return Action::Wait;
    }
    match tries {
        0 if since >= RELEASING_STEP => Action::ForceReload,
        1 if since >= RELEASING_STEP => Action::ForceWarp,
        2.. if since >= RELEASING_GIVE_UP => Action::Off,
        _ => Action::Wait,
    }
}

/// Global dead watchdog (independent of the spectate state): dead, not waiting for a
/// friend, no load, no death screen for `DEAD_IDLE_MAX`.
fn dead_idle_due(idle: Duration) -> bool {
    idle >= DEAD_IDLE_MAX
}

/// Does a rest release / wake us? Waiting: always. Otherwise only a dead player (a
/// rest does nothing for a dead one, so the reload is forced instead).
fn should_release(state: u8, dead: bool, rest: bool) -> bool {
    rest && (matches!(state, SPEC_WAITING | SPEC_SPECTATE) || (dead && matches!(state, SPEC_OFF | SPEC_RELEASING)))
}

/// The wait is older than `SPECTATE_MAX`.
fn spec_timeout(age: Duration) -> Option<&'static str> {
    (age >= SPECTATE_MAX).then_some("timeout")
}

/// A watched peer died for good: its committed death, or HP 0 without a break for `DEAD_AFTER`.
fn friend_wipe(hp_zero_for: Option<Duration>, ev_death: bool) -> bool {
    ev_death || hp_zero_for.is_some_and(|d| d >= DEAD_AFTER)
}

/// Which call a phase-A release / hand-back / forced reload makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseCall {
    /// Replay the held death load request (`0x6D7070`) with its saved arguments.
    ReplayLoad,
    /// The old path: the game's death reload `0xA8F7E0` with zero arguments.
    OriginalReload,
}

/// Replay only when a request is really held AND it armed the wait (the death came as a
/// load request); otherwise the old reload runs.
fn release_call(load_held_pending: bool, death_armed: bool) -> ReleaseCall {
    if load_held_pending && death_armed { ReleaseCall::ReplayLoad } else { ReleaseCall::OriginalReload }
}

/// Makes the call chosen by `release_call`. State is RELEASING or OFF here, so
/// `load_req_detour` lets a replayed request through.
fn replay_or_reload(load_held_pending: bool, death_armed: bool) -> usize {
    if release_call(load_held_pending, death_armed) == ReleaseCall::ReplayLoad {
        let a: Vec<usize> = LOAD_HELD_ARGS.iter().map(|s| s.load(Ordering::Relaxed)).collect();
        let ret = call_original(&LOAD_REQ_TRAMP, a[0], a[1], a[2], a[3]);
        log::line(&format!(
            "spectate: replayed the held load request (death warp) a={:#x} b={:#x} c={:#x} d={:#x} (returned {ret:#x})",
            a[0], a[1], a[2], a[3]
        ));
        ret
    } else {
        mod_death_reload()
    }
}

/// The mod's own death reload with null arguments (0xA8F7E0). It leaves emblems and sen at 0,
/// so what is carried is snapshotted first and given back by invguard.
fn mod_death_reload() -> usize {
    crate::invguard::snapshot_before_reload();
    call_original(&RELOAD_TRAMP, 0, 0, 0, 0)
}

/// Phase-B release: replay the held vanilla death-warp request, fall back to the destructive
/// null-argument reload (0xA8F7E0), or let the game's death step run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReleaseB {
    /// Replay the held `0x6D7070` request with its saved arguments (keeps emblems / items).
    ReplayHeld,
    /// The old path: `mod_death_reload()` (wipes emblems, Resurrect nodes ...).
    NullReload,
    /// Nothing was held: the game's own death step runs.
    LetDeathStep,
}

fn phase_b_release(held: u32, replay_flag: bool) -> ReleaseB {
    if held == 0 {
        ReleaseB::LetDeathStep
    } else if replay_flag {
        ReleaseB::ReplayHeld
    } else {
        ReleaseB::NullReload
    }
}

/// Replays the held load request from `LOAD_HELD_ARGS`. State is RELEASING here, so
/// `load_req_detour` lets it through.
fn replay_held_request(tag: &str) -> usize {
    let a: Vec<usize> = LOAD_HELD_ARGS.iter().map(|s| s.load(Ordering::Relaxed)).collect();
    crate::invguard::snapshot_before_reload();
    let ret = call_original(&LOAD_REQ_TRAMP, a[0], a[1], a[2], a[3]);
    log::line(&format!(
        "spectate: replayed the held load request ({tag}) a={:#x} b={:#x} c={:#x} d={:#x} (returned {ret:#x})",
        a[0], a[1], a[2], a[3]
    ));
    ret
}

/// A rest (ours or any peer's) arrived. True when it was taken here: we wait (release),
/// or we are dead / releasing (the frame task forces the reload; a mailbox rest does nothing).
pub fn rest_while_dead() -> bool {
    let dead = main_player_hp().is_some_and(|hp| hp <= 0);
    if !should_release(SPEC_STATE.load(Ordering::Acquire), dead, true) {
        return false;
    }
    if spectating() {
        spectate_release("rest");
    } else {
        FORCE_REQ.store(true, Ordering::Release);
    }
    true
}

/// Ask the frame task to stop waiting (it calls the held-back reload). No-op unless waiting.
pub fn spectate_release(reason: &'static str) {
    if spectating()
        && let Ok(mut r) = RELEASE_REQ.lock()
    {
        r.get_or_insert(reason);
    }
}

/// Peers that left our wait since the last frame (world.rs: their death, death screen or
/// gauge gone), with the release reason it would be if they were the last one.
static PEER_OUT: Mutex<Vec<(PeerId, &'static str)>> = Mutex::new(Vec::new());

/// world.rs: peer `id` is out of the fight we may be waiting in (`"wipe"`: it died /
/// waits on its own death screen; `"boss gauge gone"`). With 3+ players the wait goes on
/// while another engaged peer still fights (PLAN-MULTIPLAYER §2 bossrules); with two
/// players this is the old `spectate_release(why)`. No-op unless waiting.
pub fn peer_out(id: PeerId, why: &'static str) {
    if spectating()
        && let Ok(mut q) = PEER_OUT.lock()
        && q.len() < 32
    {
        q.push((id, why));
    }
}

/// Bosses that fell on a peer's side during the wait (world.rs `EV_BOSS(2)`): (peer, boss entity).
static BOSS_DEFEATED: Mutex<Vec<(PeerId, i32)>> = Mutex::new(Vec::new());

/// world.rs: peer `id`'s boss `entity` fell (`EV_BOSS(2)`). Releases our wait only when it is the
/// fight we wait for (a watched peer's, or our wait's boss): with 3+ players another group's
/// kill in the same block must not end it. No-op unless waiting.
pub fn boss_defeated_by(id: PeerId, entity: i32) {
    if spectating()
        && let Ok(mut q) = BOSS_DEFEATED.lock()
        && q.len() < 32
    {
        q.push((id, entity));
    }
}

/// Pure: does peer `from`'s boss `entity` falling end the wait for `watch` / `wait_boss`?
fn defeat_ends_wait(watch: &BTreeMap<PeerId, Watch>, wait_boss: i32, from: PeerId, entity: i32) -> bool {
    watch.contains_key(&from) || (entity > 0 && entity == wait_boss)
}

/// One peer we wait for on the death screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Watch {
    /// Out of the fight, with the reason (sticky for this wait).
    out: Option<&'static str>,
    dead_since: Option<Instant>,
    gone_since: Option<Instant>,
}

/// One peer's view for the "wanted" decision (pure, unit tested).
#[derive(Clone, Copy, Debug)]
struct Engage {
    id: PeerId,
    /// world.rs sees its boss fight (`EV_BOSS(1)`).
    boss: bool,
    /// Within `FRIEND_NEAR_M` of us (latest visible snapshot).
    near: bool,
    /// Alive in our block.
    here: bool,
    /// Down on its death screen, or its death committed and no load since.
    down: bool,
    /// It started a boss fight within `PEER_BOSS_GRACE` (survives its load, world.rs `last_boss`).
    recent_boss: bool,
}

/// The peers engaged in our fight: in its own fight (or near us while we fight), alive in
/// our block, not down. Two players: exactly the old `in_fight && friend_here && !friend_down`.
fn engaged_ids(list: &[Engage], our_fight: bool) -> Vec<PeerId> {
    list.iter().filter(|e| (e.boss || (our_fight && (e.near || e.recent_boss))) && e.here && !e.down).map(|e| e.id).collect()
}

/// One frame of a watched peer during the wait: dead `DEAD_AFTER` (5 s) = "wipe", out of our block 5 s =
/// "friend left the block" (the old single-friend timers, per peer).
fn watch_step(w: &mut Watch, snap: Option<Snapshot>, here: Option<i32>, now: Instant) {
    if w.out.is_some() {
        return;
    }
    if snap.is_some_and(|r| r.hp <= 0) {
        let since = *w.dead_since.get_or_insert(now);
        if friend_wipe(Some(now.duration_since(since)), false) {
            w.out = Some("wipe");
            return;
        }
    } else {
        w.dead_since = None;
    }
    if snap.is_some_and(|r| here == Some(r.block)) {
        w.gone_since = None;
    } else if now.duration_since(*w.gone_since.get_or_insert(now)) >= GONE_AFTER {
        w.out = Some("friend left the block");
    }
}

/// The wait ends once every watched peer is out: "wipe" only when each of them died, else
/// another peer's reason (no wipe: somebody lives).
fn wait_verdict(watch: &BTreeMap<PeerId, Watch>) -> Option<&'static str> {
    if watch.is_empty() || watch.values().any(|w| w.out.is_none()) {
        return None;
    }
    if watch.values().all(|w| w.out == Some("wipe")) {
        return Some("wipe");
    }
    watch.values().filter_map(|w| w.out).find(|&r| r != "wipe")
}

/// Ends any wait without `BossRules` (frame task, game thread): nothing is wanted,
/// the state is OFF (phase B: the predicate lets the game's CSDeadStep through on
/// its next call), and a death reload we held is run now when `run_reload`, else
/// dropped (a load is already under way). True if anything was held.
fn hand_back(why: &str, run_reload: bool) -> bool {
    SPEC_WANTED.store(false, Ordering::Release);
    SPEC_LATCH.store(false, Ordering::Release);
    SPEC_B_OK.store(false, Ordering::Release);
    SPEC_STARTED.store(false, Ordering::Release);
    SPEC_B_STARTED.store(false, Ordering::Release);
    SPEC_B_TO_A.store(false, Ordering::Release);
    *RELEASE_REQ.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    PEER_OUT.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    BOSS_DEFEATED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    let was = SPEC_STATE.swap(SPEC_OFF, Ordering::AcqRel);
    let held = HELD_RELOAD.swap(false, Ordering::AcqRel);
    let held_load = LOAD_HELD_PENDING.swap(false, Ordering::AcqRel);
    let armed = LOAD_DEATH_ARMED.swap(false, Ordering::AcqRel);
    FORCE_REQ.store(false, Ordering::Release);
    if was == SPEC_OFF && !held {
        return false;
    }
    log::line(&format!("spectate: hand back ({why}): state {was} -> OFF, held reload {held}"));
    if held {
        if run_reload {
            let ret = replay_or_reload(held_load, armed);
            log::line(&format!("spectate: held reload called ({why}, returned {ret:#x})"));
        } else {
            log::line(&format!("spectate: held reload dropped ({why})"));
        }
    }
    true
}

/// lib.rs, after a frame-task panic (the puppet and its `BossRules` were dropped)
/// or when the mod disables itself: no static may keep the game waiting. The free
/// cam goes back, phase B lets the death through, a held reload runs now.
pub fn emergency_reset(why: &str) {
    let cam = spectate_cam::restore_now();
    let held = hand_back(why, true);
    if cam || held {
        log::line(&format!("bossrules: emergency reset ({why}): camera restored {cam}, wait ended {held}"));
    }
}

/// The friend rested while we are in a boss fight: go to our own idol.
pub fn request_expel() {
    EXPEL_REQ.store(true, Ordering::Release);
}

/// Overlay text for the frame task to show for `secs` seconds.
pub fn notice(text: String, secs: u64) {
    if let Ok(mut n) = NOTICES.lock()
        && n.len() < 8
    {
        n.push((text, secs));
    }
}

pub fn take_notices() -> Vec<(String, u64)> {
    NOTICES.lock().map(|mut n| std::mem::take(&mut *n)).unwrap_or_default()
}

/// A peer had a loading screen after `t` (as `Net::friend_loaded_since`, per peer).
fn loaded_since(p: &PeerFrame, t: Instant) -> bool {
    p.loaded_at.is_some_and(|l| l > t)
}

/// The name shown for `p` (net.rs' compat name while no peer is in the set, e.g. a link
/// that just dropped, as before), else "Friend".
fn friend_name(p: Option<&PeerFrame>, net: &Option<&mut Net>) -> String {
    p.map(|p| p.name.as_str())
        .or_else(|| net.as_deref().and_then(|n| n.peer_name()))
        .filter(|s| !s.is_empty())
        .unwrap_or("Friend")
        .to_string()
}

fn send(net: &mut Option<&mut Net>, kind: u8, a: i32, b: i32) {
    if let Some(n) = net.as_deref_mut()
        && n.connected()
    {
        n.send_event(kind, a, b);
    }
}

/// `BULK_BOSS_REWARD` body: `{i32 boss, u16 n, n * {i32 lot, u16 m, m * u32 flag}}`.
fn encode_reward(boss: i32, lots: &[(i32, Vec<u32>)]) -> Vec<u8> {
    let mut out = boss.to_le_bytes().to_vec();
    out.extend((lots.len() as u16).to_le_bytes());
    for (lot, flags) in lots {
        out.extend(lot.to_le_bytes());
        out.extend((flags.len() as u16).to_le_bytes());
        for f in flags {
            out.extend(f.to_le_bytes());
        }
    }
    out
}

fn decode_reward(d: &[u8]) -> Option<(i32, Lots)> {
    fn take<'a>(d: &'a [u8], p: &mut usize, n: usize) -> Option<&'a [u8]> {
        let s = d.get(*p..p.checked_add(n)?)?;
        *p += n;
        Some(s)
    }
    let mut p = 0;
    let boss = i32::from_le_bytes(take(d, &mut p, 4)?.try_into().ok()?);
    let n = u16::from_le_bytes(take(d, &mut p, 2)?.try_into().ok()?) as usize;
    if n > MAX_LOTS {
        return None;
    }
    let mut lots = Vec::with_capacity(n);
    for _ in 0..n {
        let lot = i32::from_le_bytes(take(d, &mut p, 4)?.try_into().ok()?);
        let m = u16::from_le_bytes(take(d, &mut p, 2)?.try_into().ok()?) as usize;
        if m > MAX_LOT_FLAGS {
            return None;
        }
        let flags = (0..m)
            .map(|_| Some(u32::from_le_bytes(take(d, &mut p, 4)?.try_into().ok()?)))
            .collect::<Option<Vec<u32>>>()?;
        lots.push((lot, flags));
    }
    (p == d.len()).then_some((boss, lots))
}

/// Everything `BossRules::on_frame` needs from the frame task.
pub struct Frame<'a> {
    pub net: Option<&'a mut Net>,
    pub world: &'a mut World,
    /// This frame's other players. P1c: the rules follow `first()` (the 2-player friend):
    /// its latest visible snapshot, and its `view` (where it is drawn: the native remote's
    /// own physics, else the interpolated snapshot) for the spectator camera.
    pub peers: &'a PeerSet,
    pub player: Chr,
    pub here: Option<i32>,
    /// Time since the player appeared after the last load.
    pub in_world_for: Duration,
    /// The menu button "wake at idol".
    pub wake: bool,
}

struct Recording {
    entity: i32,
    at: Instant,
    /// (ms relative to the gauge, flag id, on)
    entries: Vec<(i64, u32, bool)>,
}

#[derive(Default)]
pub struct BossRules {
    /// Gauge seen last frame: (boss entity, name id).
    gauge: Option<(i32, i32)>,
    /// The gauge state while we were alive: our own death may clear the gauge.
    boss_while_alive: bool,
    /// Since when `on_loading` was called without a frame in the world between.
    no_player_since: Option<Instant>,
    // Waiting on the death screen.
    f8_down: bool,
    spec_since: Option<Instant>,
    defer_logged: u32,
    defer_log_at: Option<Instant>,
    /// The peers this wait holds for (engaged at its start or since), PLAN-MULTIPLAYER §2.
    watch: BTreeMap<PeerId, Watch>,
    /// The peer the spectator camera follows (nearest living watched one, sticky).
    cam_target: Option<PeerId>,
    /// No peer to watch at all: since when (the old "friend left the block" timer).
    friend_gone_since: Option<Instant>,
    link_lost_since: Option<Instant>,
    releasing_since: Option<Instant>,
    /// Forced steps of the `RELEASING` watchdog so far (reload, then warp).
    force_tries: u8,
    /// Since when we are dead, not waiting, with no load and no death screen.
    dead_idle_since: Option<Instant>,
    overlay: Option<String>,
    // Expulsion from the arena by the friend's rest.
    expel: Option<game::WarpWait>,
    // Rewards.
    defeat: Option<(i32, Instant)>,
    defeat_handled_at: Option<Instant>,
    pending_rewards: Vec<(Instant, PeerId, Vec<u8>)>,
    reward_wait_log_at: Option<Instant>,
    /// (boss, lot) already given: a reward arriving twice (both worlds saw the kill) counts once.
    applied: HashSet<(i32, i32)>,
    /// (when, boss, lot) of a lot we just awarded or skipped: a few seconds later every unique
    /// good it surely gives must be owned (AwardItemLot left a skill out, live 2026-10-25).
    reward_verify: Vec<(Instant, i32, i32)>,
    // Spectator camera (phase B, also on during a phase A wait).
    cam: spectate_cam::SpectateCam,
    // Flag recorder.
    flag_cursor: Option<u32>,
    flag_hist: VecDeque<(Instant, u32, bool)>,
    recording: Option<Recording>,
    // Script item grants for the friend and the key-item doctor (keyitems.rs).
    keys: crate::keyitems::KeyItems,
    gate_stats_at: Option<Instant>,
    /// Last frame a boss gauge was up (the game clears it before the death step).
    gauge_seen_at: Option<Instant>,
    last_gauge: i32,
    /// The boss of the fight we wait in (logged at the start).
    wait_boss: i32,
    /// The friend's "boss defeated" came before our world saw the defeat: since when we
    /// wait for it (our HandleBossDefeat, the boss dead here, or 20 s).
    defeat_wait: Option<Instant>,
    /// `LOAD_HELD` already logged.
    load_held_logged: u32,
    /// The held death-load arguments were logged for this wait.
    load_args_logged: bool,
    /// GameMan load request seen during phase B by a route we do not hold (logged once).
    load_pending_logged: bool,
    /// A multi-life enemy (miniboss: no boss gauge) we are engaged with: (entity, last
    /// seen). Owner rule: dying to any enemy with several health orbs spectates the friend.
    mini: Option<(i32, Instant)>,
    mini_scan_at: Option<Instant>,
    /// v8.31b: the nearest multi-life enemy within `MINI_NEAR_M` at the last scan, engaged or not:
    /// (entity, distance, lives0, hp, max hp, when). A death next to it counts as a miniboss fight.
    mini_near: Option<(i32, f32, i32, i32, i32, Instant)>,
    /// Our last position a scan read while alive (the position may be unreadable once dead).
    last_me: Option<[f32; 3]>,
    /// The miniboss entity already logged for this fight.
    mini_logged: i32,
    /// The wait's boss is a miniboss that was loaded at the start: its death releases.
    wait_mini: bool,
    /// The table encounter of the wait's boss (bosses.rs): its defeat, not one phase entity
    /// being dead, releases the wait.
    wait_enc: Option<&'static Encounter>,
    /// A table boss's defeat flag turned on here (entity the defeat is reported with, since
    /// when): the friend's kill arriving by flag sync before our HBD counts as the defeat.
    flag_defeat: Option<(i32, Instant)>,
    /// A spectate release held back because somebody is in a cutscene: (reason, since).
    held_release: Option<(&'static str, Instant)>,
    /// The previous `on_frame`, for pausing the wait clocks during a cutscene.
    last_frame: Option<Instant>,
}

/// An HBD / defeat flag younger than this still counts as "the encounter is defeated".
const ENCOUNTER_DONE_KEEP: Duration = Duration::from_secs(60);

/// How close (m) a multi-life enemy must be to us (or the friend) to count as our fight.
const MINI_NEAR_M: f32 = 25.0;
/// Miniboss scan period.
const MINI_SCAN_EVERY: Duration = Duration::from_millis(250);

/// A loaded enemy is a miniboss fight for us: alive, several lives (NpcParam deathblow
/// count >= 2), within `MINI_NEAR_M` of us and engaged (hurt, a life gone, or the friend is
/// at it too). Dying in such a fight spectates the friend (no boss gauge needed).
pub fn mini_engaged(lives0: i32, hp: i32, max_hp: i32, lives: i32, d_player: f32, d_friend: Option<f32>) -> bool {
    crate::livesync::multi_life(lives0)
        && hp > 0
        && d_player <= MINI_NEAR_M
        && (hp < max_hp || lives < lives0 || d_friend.is_some_and(|d| d <= MINI_NEAR_M))
}

/// A cached near multi-life enemy older than this is not trusted at the death frame.
const MINI_NEAR_KEEP: Duration = Duration::from_secs(3);

/// v8.31b: our fight at the death decision: the boss gauge / miniboss verdicts, or the death
/// happened next to a multi-life enemy (it killed us before it was scratched).
pub fn our_fight_at_death(boss_while_alive: bool, gauge: bool, recent_gauge: bool, mini_recent: bool, near_multi_life: bool) -> bool {
    boss_while_alive || gauge || recent_gauge || mini_recent || near_multi_life
}

/// A death within this long of a boss gauge counts as a death in the fight.
const GAUGE_RECENT: Duration = Duration::from_secs(10);
/// The friend's "boss defeated" waits at most this long for the defeat in our world.
const DEFEAT_WAIT_MAX: Duration = Duration::from_secs(20);

/// A spectate release is held at most this long while a cutscene runs (a stuck flag must not
/// keep us from waking up).
const POSTPONE_MAX: Duration = Duration::from_secs(60);

/// Holds a spectate release back while somebody is in a cutscene (our reload would cut ours
/// off, or pull a peer's world out from under it) and hands it out once none is, or after
/// `POSTPONE_MAX`. Called every frame with this frame's release (if any); a held one is kept
/// when a newer one arrives only if there is no newer one.
fn postpone_release(
    release: Option<&'static str>,
    held: &mut Option<(&'static str, Instant)>,
    cutscene: bool,
    now: Instant,
) -> Option<&'static str> {
    let reason = release.or(held.map(|h| h.0))?;
    if cutscene {
        let since = held.map_or(now, |h| h.1);
        if now.saturating_duration_since(since) < POSTPONE_MAX {
            if held.is_none() {
                log::line(&format!("spectate: release ({reason}) postponed: somebody is in a cutscene"));
            }
            *held = Some((reason, since));
            return None;
        }
        log::line(&format!("spectate: release ({reason}) after {} s of postponing, cutscene or not", POSTPONE_MAX.as_secs()));
    } else if held.is_some() {
        log::line(&format!("spectate: postponed release ({reason}) goes ahead"));
    }
    *held = None;
    Some(reason)
}

/// F8 only counts with the settings file's `debug_hotkeys = 1`: players never wake themselves, the
/// wait ends by the rules.
fn f8_down() -> bool {
    let state = unsafe { GetAsyncKeyState(VK_F8) };
    tuning::debug_hotkeys() && state as u16 & 0x8000 != 0
}

impl BossRules {
    /// The table encounter is defeated here: its defeat flag is on, its final HBD came, or the
    /// flag turned on moments ago (flag reads can fail while the flag manager is not ready).
    fn encounter_defeated(&self, enc: &Encounter) -> bool {
        game::event_flag(enc.defeat_flag) == Some(true)
            || self.defeat.is_some_and(|(e, t)| e == enc.hbd_final && t.elapsed() < ENCOUNTER_DONE_KEEP)
            || self.flag_defeat.is_some_and(|(e, t)| {
                t.elapsed() < ENCOUNTER_DONE_KEEP && bosses::encounter_of(e).is_some_and(|x| bosses::same_encounter(x, enc))
            })
    }

    /// The boss entity is finished for the fight rules. Table bosses: the encounter is defeated
    /// (a dead phase entity is not the end). Others: dead (`gone_is_dead`: or no longer loaded).
    fn entity_done(&self, entity: i32, gone_is_dead: bool) -> bool {
        match bosses::encounter_of(entity) {
            Some(enc) => self.encounter_defeated(enc),
            None => game::chr_by_entity_id(entity)
                .and_then(|c| c.hp())
                .map_or(gone_is_dead, |h| h <= 0),
        }
    }

    /// The text for the overlay while we wait on the death screen.
    pub fn overlay(&self) -> Option<&str> {
        self.overlay.as_deref()
    }

    /// A warp is under way: no decisions, and no reload is held back. Our warps
    /// need a living player (`game::safe_to_warp`), so a wait here means we died
    /// during the warp's fade: the state is ended like a release (a held reload
    /// runs now, as it would without the mod; phase B lets the game's death step
    /// through) instead of leaving `SPEC_STATE` for `on_loading` (review finding 7).
    pub fn hands_off(&mut self) {
        self.cam.release("warp");
        if hand_back("warp under way", true) {
            self.overlay = None;
            self.spec_since = None;
            self.releasing_since = None;
            self.force_tries = 0;
            self.dead_idle_since = None;
            self.held_release = None;
        }
    }

    /// A loading screen: the world we knew is gone.
    pub fn on_loading(&mut self) {
        // We hold the reload back ourselves, so no load can follow while we wait: a
        // player that is gone for a moment is not a load. Dropping the state here
        // would leave the death screen with nobody to run the reload.
        let gone = self.no_player_since.get_or_insert_with(Instant::now).elapsed();
        if spectating() && gone < NO_PLAYER_MAX {
            return;
        }
        self.cam.release("loading");
        // A real load (or the title): a reload we still hold is not run any more.
        if hand_back("loading screen", false) {
            log::line("spectate: loading screen, state reset");
        }
        self.gauge = None;
        self.boss_while_alive = false;
        self.mini = None;
        self.mini_near = None;
        self.mini_logged = 0;
        self.overlay = None;
        self.expel = None;
        self.held_release = None;
        self.spec_since = None;
        self.releasing_since = None;
        self.watch.clear();
        self.cam_target = None;
        self.force_tries = 0;
        self.dead_idle_since = None;
        self.friend_gone_since = None;
        self.link_lost_since = None;
    }

    /// Once per frame in the world. Returns true when it started a warp (the
    /// caller must let go of every character pointer, like after a join warp).
    pub fn on_frame(&mut self, f: Frame) -> bool {
        let Frame { mut net, world, peers, player, here, in_world_for, wake } = f;
        // The 2-player friend is the lowest-id peer; the wait follows every engaged peer.
        let friend = peers.first();
        let now = Instant::now();
        self.no_player_since = None;
        // A cutscene (ours or any peer's) stops the clocks that would end a wait or force a
        // reload: SPECTATE_MAX (`spec_since`) and DEAD_IDLE_MAX (`dead_idle_since`).
        let dt = self.last_frame.replace(now).map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        if crate::cutscene::any_in_cutscene() {
            for t in [self.spec_since.as_mut(), self.dead_idle_since.as_mut()].into_iter().flatten() {
                crate::cutscene::pause(t, dt);
            }
        }
        let connected = net.as_deref().is_some_and(Net::connected);
        let hp = player.hp();
        let alive = hp.is_some_and(|h| h > 0);
        let dead = hp.is_some_and(|h| h <= 0);
        self.pump_flags(now);

        // HandleBossDefeat in our world (SPEC §4.2).
        let mut release: Option<&'static str> = None;
        if DEFEAT_PENDING.swap(false, Ordering::AcqRel) {
            let entity = DEFEAT_ENTITY.load(Ordering::Relaxed);
            if bosses::classify(Sig::Hbd(entity), &flag_on) == Verdict::PhaseEnd {
                // Emma before Isshin Ashina: the fight goes on (bosses.rs).
                log::line(&format!("boss: phase end {entity} (intermediate HBD)"));
                DEFEAT_AT_MS.store(0, Ordering::Release);
            } else {
                log::line(&format!("boss: defeated {entity} (banner {})", DEFEAT_BANNER.load(Ordering::Relaxed)));
                // Own world (ownworld.rs): the flags of this defeat are session progress.
                crate::ownworld::note_defeat("our boss");
                self.defeat = Some((entity, now));
                self.defeat_handled_at = Some(now);
                send(&mut net, world::EV_BOSS, 2, entity);
                release = Some("boss defeated");
            }
        }

        // The gauge appears / goes away (SPEC §0.1, §2).
        let boss = game::boss_state();
        let gauge = boss.map(|b| (b.entity, b.name_id));
        match (self.gauge, gauge) {
            (None, Some((entity, name))) => {
                log::line(&format!("boss: gauge on entity {entity} name {name} block {here:?}"));
                self.arm_recording(entity, now);
                send(&mut net, world::EV_BOSS, 1, entity);
            }
            (Some((entity, _)), None) => {
                let fell = self.defeat_handled_at.is_some_and(|t| t.elapsed() < DEFEAT_WINDOW);
                // Minibosses have a gauge but no HandleBossDefeat: still loaded at 0 HP =
                // killed (not a retreat or a load), so the friend hears "fell" (bossgate.rs
                // then applies the fight's held flags instead of dropping them).
                let dead_here = !fell && game::chr_by_entity_id(entity).and_then(|c| c.hp()).is_some_and(|h| h <= 0);
                // Our own death may clear the gauge: that is not the fight ending. A table
                // boss's phase entity going away is not the kill either (bosses.rs).
                let verdict = (alive && !fell)
                    .then(|| bosses::classify(Sig::GaugeOff { entity, chr_dead_or_gone: dead_here }, &flag_on));
                log::line(&format!(
                    "boss: gauge off{}",
                    if fell {
                        " (boss fell)"
                    } else if !alive {
                        " (we are dead, friend not told)"
                    } else {
                        match verdict {
                            Some(Verdict::MiniDefeated) => " (boss dead without HandleBossDefeat: miniboss, friend told it fell)",
                            Some(Verdict::Defeated(_)) => " (encounter defeated flag on, friend told it fell)",
                            Some(Verdict::PhaseEnd) => " (phase end: the fight goes on)",
                            Some(Verdict::FightGoesOn) => " (final phase dead, HandleBossDefeat follows: the fight goes on)",
                            _ => "",
                        }
                    }
                ));
                match verdict {
                    Some(Verdict::Defeated(_) | Verdict::MiniDefeated) => send(&mut net, world::EV_BOSS, 2, entity),
                    Some(Verdict::Retreat) => send(&mut net, world::EV_BOSS, 3, entity),
                    // The fight is held 10 s (or until the next gauge): gauge_seen_at = now.
                    Some(Verdict::PhaseEnd | Verdict::FightGoesOn) => self.gauge_seen_at = Some(now),
                    _ => {}
                }
            }
            (Some(old), Some(new)) if old != new => {
                let v = bosses::classify(Sig::GaugeSwitch { from: old.0, to: new.0 }, &flag_on);
                log::line(&format!(
                    "boss: gauge now entity {} name {} (was {} / {}){}",
                    new.0,
                    new.1,
                    old.0,
                    old.1,
                    if v == Verdict::FightGoesOn { ", same fight" } else { "" }
                ));
            }
            _ => {}
        }
        self.gauge = gauge;
        if let Some((entity, _)) = gauge {
            self.gauge_seen_at = Some(now);
            self.last_gauge = entity;
        }
        if alive {
            self.boss_while_alive = gauge.is_some();
        }

        // A peer's fight is over for us when it has been through a load since.
        let loaded: Vec<_> = world
            .peer_bosses()
            .filter(|&(id, _, since)| peers.by_id(id).is_some_and(|p| loaded_since(p, since)))
            .map(|(id, ..)| id)
            .collect();
        for id in loaded {
            world.end_peer_boss(id);
        }
        for id in world.take_boss_announces() {
            let who = peers.by_id(id).or(friend);
            self.announce_friend_boss(&friend_name(who, &net), player, who.and_then(|p| p.snapshot), here, alive);
        }

        // Should the death reload be held back? (SPEC §1.2 `spectate_wanted`.) Extra
        // beyond the spec: we must be dead (the reload has other callers), and our own
        // fight only counts for a peer near enough to be in it [LIVE].
        // A gauge up within the last 10 s counts too: the game clears it before the
        // death step, so the frame that decides may already see none.
        // Not after the boss fell (its gauge went because it died).
        let recent_gauge = self.gauge_seen_at.is_some_and(|t| now.duration_since(t) < GAUGE_RECENT)
            && !self.defeat_handled_at.is_some_and(|t| t.elapsed() < DEFEAT_WINDOW)
            && !(self.last_gauge > 0 && self.entity_done(self.last_gauge, false));
        let me_pos = player.position();
        // Minibosses have no gauge: a multi-life enemy we are engaged with counts as well
        // (scanned while alive, kept for GAUGE_RECENT like the gauge; not after it died).
        // v7.10, N peers: "the friend at it" = any peer's latest visible position.
        if alive {
            let others: Vec<[f32; 3]> = peers.iter().filter_map(|p| p.snapshot.map(|r| r.pos)).collect();
            self.mini_scan(player, &others, now);
        }
        let mini_recent = self.mini.is_some_and(|(e, t)| {
            now.duration_since(t) < GAUGE_RECENT
                && !self.defeat_handled_at.is_some_and(|d| d.elapsed() < DEFEAT_WINDOW)
                && !self.entity_done(e, false)
        });
        // v8.31b: dying within MINI_NEAR_M of a multi-life enemy counts too, hurt or not (cached by
        // the scan, since our position may be unreadable once dead).
        let died_near = if dead {
            self.mini_near.filter(|n| now.duration_since(n.5) < MINI_NEAR_KEEP && n.1 <= MINI_NEAR_M)
        } else {
            None
        };
        let our_fight_pre = self.boss_while_alive || gauge.is_some() || recent_gauge || mini_recent;
        let our_fight = our_fight_at_death(self.boss_while_alive, gauge.is_some(), recent_gauge, mini_recent, died_near.is_some());
        let list: Vec<Engage> = peers
            .iter()
            .map(|p| {
                let r = p.snapshot;
                Engage {
                    id: p.id,
                    boss: world.peer_boss(p.id).is_some(),
                    recent_boss: world.peer_recent_boss(p.id, PEER_BOSS_GRACE),
                    near: r.zip(me_pos).is_some_and(|(r, q)| game::distance(r.pos, q) <= FRIEND_NEAR_M),
                    here: r.is_some_and(|r| r.hp > 0 && here == Some(r.block)),
                    // Down on its death screen, or its death committed and no load since (the reload).
                    down: world.peer_down(p.id) || world.peer_death_at(p.id).is_some_and(|t| !loaded_since(p, t)),
                }
            })
            .collect();
        // Hold the reload while at least one living peer is engaged in our fight.
        let engaged = engaged_ids(&list, our_fight);
        // Scripted loss (tutorial Genichiro, v7.12): nothing to spectate (bosses.rs). The
        // fight's boss: our gauge, else an engaged peer's boss, else our recent gauge.
        let fight_boss = gauge
            .map(|g| g.0)
            .or(engaged.iter().find_map(|&id| world.peer_boss(id)).map(|b| b.0))
            .or((self.last_gauge > 0 && self.gauge_seen_at.is_some_and(|t| now.duration_since(t) < GAUGE_RECENT)).then_some(self.last_gauge));
        let scripted = fight_boss.and_then(bosses::encounter_of).is_some_and(|e| e.scripted_loss);
        let wanted_if_dead = tuning::boss_spectate() && connected && !engaged.is_empty() && !scripted && !game::remnant();
        static DEATH_LOGGED: AtomicBool = AtomicBool::new(false);
        // v8.20: the verdict is latched on the first dead frame (the live one can drop between the
        // death and the reload); the latch is cleared when alive again and by hand_back / release.
        static LATCH_SAVED: AtomicBool = AtomicBool::new(false);
        if dead && !DEATH_LOGGED.load(Ordering::Acquire) {
            SPEC_LATCH.store(wanted_if_dead, Ordering::Release);
            LATCH_SAVED.store(false, Ordering::Release);
            if wanted_if_dead {
                log::line("spectate: latch set on the first dead frame (wait wanted)");
            }
        } else if !dead {
            SPEC_LATCH.store(false, Ordering::Release);
        }
        let latch = SPEC_LATCH.load(Ordering::Acquire);
        let wanted_now = spec_wanted(wanted_if_dead, latch, dead);
        SPEC_WANTED.store(wanted_now, Ordering::Release);
        if wanted_now && !wanted_if_dead && !LATCH_SAVED.swap(true, Ordering::AcqRel) {
            log::line("spectate: latch kept the wait wanted (the live verdict had dropped)");
        }
        diag_log(dead);
        let cam_mode = tuning::spectate_camera();
        update_resurrect_signal(dead);
        watch_no_revive(dead);
        stale_no_revive_step(alive);
        SPEC_B_OK.store(wanted_if_dead && HYBRID_B_LIVE && cam_mode == tuning::SPECTATE_CAM_PHASE_B, Ordering::Release);
        // v8.15: once per death, on its first frame: the resurrection fields and why a phase A
        // wait did or did not start (live 2026-10-25: a death next to a miniboss with the host
        // alive started none). `dead` here is the same bit that arms SPEC_WANTED.
        if dead && !DEATH_LOGGED.swap(true, Ordering::AcqRel) {
            log::line(&format!(
                "death: first dead frame; {}; wait wanted {wanted_if_dead} (spectate option {}, connected {connected}, engaged {}, scripted {scripted}, remnant {}), our fight {our_fight} (gauge {}, recent gauge {recent_gauge}, miniboss {mini_recent}), fight boss {fight_boss:?}, spectate state {}",
                revive_fields_now(),
                tuning::boss_spectate(),
                engaged.len(),
                game::remnant(),
                gauge.is_some(),
                SPEC_STATE.load(Ordering::Acquire)
            ));
            let flags: Vec<String> = list
                .iter()
                .map(|e| format!("[{} {} {} {} {} {}]", e.id.slot(), e.boss, e.near, e.here, e.down, e.recent_boss))
                .collect();
            log::line(&format!("death: peers [id boss near here down recent] {}", flags.join(" ")));
            if let Some((ent, d, l0, hp, max, _)) = died_near
                && !our_fight_pre
            {
                log::line(&format!("death: died within {d:.0} m of multi-life enemy {ent} (lives0 {l0}, hp {hp}/{max}): counts as a miniboss fight"));
            }
            if !our_fight {
                self.log_nearby_enemies(me_pos.or(self.last_me));
            }
            for e in &list {
                if engaged.contains(&e.id) && e.recent_boss && !e.boss && !(our_fight && e.near) {
                    if let Some((ent, age)) = world.peer_recent_boss_info(e.id, PEER_BOSS_GRACE) {
                        log::line(&format!("engage: peer {} counted by recent-boss grace (boss {ent}, {:.1}s ago)", e.id.slot(), age.as_secs_f32()));
                    }
                }
            }
            if our_fight && engaged.is_empty() {
                for e in &list {
                    log::line(&format!(
                        "engage: peer {} not counted (boss/near/here/down/recent = {}/{}/{}/{}/{})",
                        e.id.slot(), e.boss, e.near, e.here, e.down, e.recent_boss
                    ));
                }
            }
            crate::statelog::resurrect_dump("first dead frame");
        } else if !dead {
            DEATH_LOGGED.store(false, Ordering::Release);
        }
        log_resurrect_outcome();
        // Phase B holds no reload, so a player who is alive again just stops waiting.
        if alive && release.is_none() && SPEC_STATE.load(Ordering::Acquire) == SPEC_SPECTATE {
            release = Some("alive again");
        }

        self.spectate_step(&mut net, world, peers, &engaged, here, connected, wake, release, now);
        self.dead_watchdog(dead, now);

        // The camera: dead and waiting (either phase), never on a living player. It follows
        // the nearest living peer we wait for and switches when that one dies or leaves
        // (spectate_cam.rs `choose_target`, sticky while its peer is in the fight).
        let cam_wanted = cam_mode != tuning::SPECTATE_CAM_OFF && dead && spectating();
        self.cam.learn_peers(player, alive, peers);
        let watch = &self.watch;
        let in_wait = |p: &PeerFrame| watch.get(&p.id).is_some_and(|w| w.out.is_none());
        // Lock-on candidate number one: the boss we wait for, else our last gauge (§41).
        let boss_hint = (self.wait_boss > 0).then_some(self.wait_boss).or((self.last_gauge > 0).then_some(self.last_gauge));
        self.cam.update_peers(cam_wanted, peers, me_pos, &in_wait, boss_hint);
        self.cam_target = self.cam.target();
        self.reward_step(&mut net, connected, in_world_for);
        self.keys.on_frame(&mut net, peers, connected, in_world_for);
        // Boss start gate: each peer's fight flags wait while we are far from that peer.
        crate::bossgate::tick(
            crate::bossgate::Ctx {
                connected,
                here,
                pos: player.position(),
                peers: peers
                    .iter()
                    .map(|p| crate::bossgate::PeerCtx {
                        peer: p.id,
                        epoch: p.epoch,
                        at: p.snapshot.map(|r| (r.block, r.pos)),
                        fight: world.peer_boss(p.id).is_some(),
                        cutscene: p.cutscene,
                    })
                    .collect(),
            },
            world,
        );
        if self.gate_stats_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            self.gate_stats_at = Some(now);
            if connected {
                log::line(&crate::bossgate::stats());
            }
        }
        self.expel_step(alive, in_world_for)
    }

    /// Looks for a miniboss fight (`mini_engaged`) every `MINI_SCAN_EVERY` while we live.
    /// `others`: every peer's latest visible position (the nearest one counts as "at it").
    fn mini_scan(&mut self, player: Chr, others: &[[f32; 3]], now: Instant) {
        if self.mini_scan_at.is_some_and(|t| now.duration_since(t) < MINI_SCAN_EVERY) {
            return;
        }
        self.mini_scan_at = Some(now);
        if let Some((e, t)) = self.mini
            && (now.duration_since(t) >= GAUGE_RECENT || game::chr_by_entity_id(e).and_then(|c| c.hp()).is_some_and(|h| h <= 0))
        {
            // Out of reach for a while, or killed: the next one is a new fight.
            self.mini = None;
            self.mini_logged = 0;
        }
        let Some(me) = player.position() else { return };
        self.last_me = Some(me);
        let mut near: Option<(i32, f32, i32, i32, i32, Instant)> = None;
        let mut best: Option<(f32, i32, i32)> = None;
        for chr in crate::nsession::enemy_chrs() {
            let Some(pos) = chr.position() else { continue };
            let d = game::distance(pos, me);
            if d > MINI_NEAR_M {
                continue;
            }
            let Some(entity) = chr.entity_id().filter(|&e| e > 0) else { continue };
            let lives0 = crate::livesync::lives0_of(chr);
            if !crate::livesync::multi_life(lives0) {
                continue;
            }
            let Some((_, hp, max_hp, _, lives)) = crate::livesync::read_data(chr) else { continue };
            if hp > 0 && near.is_none_or(|n| d < n.1) {
                near = Some((entity, d, lives0, hp, max_hp, now));
            }
            let d_friend = others.iter().map(|&q| game::distance(pos, q)).reduce(f32::min);
            if mini_engaged(lives0, hp, max_hp, lives, d, d_friend) && best.is_none_or(|b| d < b.0) {
                best = Some((d, entity, lives));
            }
        }
        self.mini_near = near;
        if let Some((_, entity, lives)) = best {
            self.mini = Some((entity, now));
            if self.mini_logged != entity {
                self.mini_logged = entity;
                log::line(&format!("boss: miniboss fight {entity} (lives {lives}) counts as a boss fight"));
            }
        }
    }

    /// v8.31b diagnostics: up to 5 living loaded enemies with lives0 >= 1 within 40 m of `at`.
    fn log_nearby_enemies(&self, at: Option<[f32; 3]>) {
        let Some(me) = at else {
            log::line("death: nearby enemies: no known player position");
            return;
        };
        let mut found: Vec<(f32, String)> = Vec::new();
        for chr in crate::nsession::enemy_chrs() {
            let Some(pos) = chr.position() else { continue };
            let d = game::distance(pos, me);
            if d > 40.0 {
                continue;
            }
            let lives0 = crate::livesync::lives0_of(chr);
            if lives0 < 1 {
                continue;
            }
            let Some((_, hp, max_hp, _, _)) = crate::livesync::read_data(chr) else { continue };
            if hp <= 0 {
                continue;
            }
            let ent = chr.entity_id().unwrap_or(0);
            found.push((d, format!("[{ent} {lives0} {hp}/{max_hp} {d:.0}]")));
        }
        found.sort_by(|a, b| a.0.total_cmp(&b.0));
        let list: Vec<String> = found.into_iter().take(5).map(|f| f.1).collect();
        log::line(&format!("death: nearby enemies [entity lives0 hp/max dist] {}", list.join(" ")));
    }

    /// The state machine of the death-screen wait: start, end conditions, release.
    #[allow(clippy::too_many_arguments)]
    fn spectate_step(
        &mut self,
        net: &mut Option<&mut Net>,
        world: &mut World,
        peers: &PeerSet,
        engaged: &[PeerId],
        here: Option<i32>,
        connected: bool,
        wake: bool,
        mut release: Option<&'static str>,
        now: Instant,
    ) {
        let started_a = SPEC_STARTED.swap(false, Ordering::AcqRel);
        let started_b = SPEC_B_STARTED.swap(false, Ordering::AcqRel);
        let outs = PEER_OUT.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        let defeated = BOSS_DEFEATED.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        if started_a || started_b {
            self.spec_since = Some(now);
            self.held_release = None;
            // Whom this wait holds for: the peers engaged in our fight (two players: the friend).
            self.watch = engaged.iter().map(|&id| (id, Watch::default())).collect();
            if self.watch.is_empty()
                && let Some(p) = peers.first()
            {
                self.watch.insert(p.id, Watch::default());
            }
            self.cam_target = None;
            self.friend_gone_since = None;
            self.link_lost_since = None;
            self.f8_down = f8_down();
            // Phase A: the call that started the wait is #1 (the counter also counts
            // earlier waits). Phase B: no reload yet.
            self.defer_logged = 0;
            DEFER_COUNT.store(u32::from(started_a), Ordering::Relaxed);
            if let Ok(mut r) = RELEASE_REQ.lock() {
                *r = None;
            }
            let mini = self.mini.filter(|&(_, t)| now.duration_since(t) < GAUGE_RECENT).map(|m| m.0);
            let boss = self
                .gauge
                .map(|g| g.0)
                .or(self.watch.keys().find_map(|&id| world.peer_boss(id)).map(|b| b.0))
                .or((self.last_gauge > 0 && self.gauge_seen_at.is_some_and(|t| now.duration_since(t) < GAUGE_RECENT)).then_some(self.last_gauge))
                .or(mini)
                .unwrap_or(-1);
            self.wait_boss = boss;
            self.wait_enc = bosses::encounter_of(boss);
            // A miniboss has no HandleBossDefeat: its death here (loaded at the start, HP 0
            // later) is the defeat that releases the wait.
            self.wait_mini = boss > 0 && mini == Some(boss) && game::chr_by_entity_id(boss).is_some();
            self.defeat_wait = None;
            if LOAD_DEATH_ARMED.load(Ordering::Acquire) {
                // Armed by the load request itself: its held arguments are what a release replays.
                self.load_held_logged = LOAD_HELD.load(Ordering::Acquire).wrapping_sub(1);
            } else {
                self.load_held_logged = LOAD_HELD.load(Ordering::Acquire);
                LOAD_HELD_PENDING.store(false, Ordering::Release);
            }
            self.load_pending_logged = false;
            let hps: Vec<String> = self
                .watch
                .keys()
                .map(|&id| format!("peer {} hp {:?}", id.slot(), peers.by_id(id).and_then(|p| p.snapshot).map(|r| r.hp)))
                .collect();
            log::line(&format!(
                "spectate: start (phase {}) boss {boss}, waiting for [{}]; {}",
                if started_a { "A" } else { "B, no death screen" },
                hps.join(", "),
                revive_fields_now()
            ));
            send(net, world::EV_DOWN, 1, 0);
        }
        if spectating() {
            // A peer that joins our fight during the wait holds it too.
            for &id in engaged {
                if !self.watch.contains_key(&id) {
                    log::line(&format!("spectate: peer {} joined the fight, waiting for it too", id.slot()));
                    self.watch.insert(id, Watch::default());
                }
            }
            for (id, why) in outs {
                if let Some(w) = self.watch.get_mut(&id).filter(|w| w.out.is_none()) {
                    w.out = Some(why);
                    log::line(&format!("spectate: peer {} is out of the fight ({why})", id.slot()));
                }
            }
            // A boss that fell on a peer's side ends the wait only when it is our fight's.
            for (id, entity) in defeated {
                if defeat_ends_wait(&self.watch, self.wait_boss, id, entity) {
                    release.get_or_insert("boss defeated");
                } else {
                    log::line(&format!(
                        "spectate: peer {}'s boss {entity} fell, not the fight we wait for (boss {}): the wait goes on",
                        id.slot(),
                        self.wait_boss
                    ));
                }
            }
        }
        if SPEC_B_TO_A.swap(false, Ordering::AcqRel) {
            log::line("spectate: a reload came during phase B: held, the death screen follows (phase A wait)");
        }
        let held = LOAD_HELD.load(Ordering::Acquire);
        // At most one line per 5 s (a script may ask again every frame).
        if held != self.load_held_logged && self.defer_log_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
            self.defer_log_at = Some(now);
            let n = held.wrapping_sub(self.load_held_logged);
            self.load_held_logged = held;
            let base = module_base();
            let stack: Vec<String> = LOAD_HELD_STACK
                .iter()
                .map(|s| s.load(Ordering::Relaxed))
                .filter(|&a| a != 0)
                .map(|a| format!("{:#x}", a.wrapping_sub(base)))
                .collect();
            log::line(&format!(
                "spectate: {n} load request(s) (0x6D7070) while we wait dead, not run (state {}): callers [{}] (0x68E7F0 = EMEVD WarpPlayer, 0x68E850 = Lua event warp, 0xA8F7E0 = death reload)",
                SPEC_STATE.load(Ordering::Acquire),
                stack.join(", ")
            ));
        }
        if LOAD_HELD_PENDING.load(Ordering::Acquire) && LOAD_DEATH_ARMED.load(Ordering::Acquire) && !self.load_args_logged {
            self.load_args_logged = true;
            let args: Vec<usize> = LOAD_HELD_ARGS.iter().map(|a| a.load(Ordering::Relaxed)).collect();
            let base = module_base();
            log::line(&format!(
                "spectate: held load request args a={:#x} b={:#x} c={:#x} d={:#x} (caller RVA {:#x})",
                args[0],
                args[1],
                args[2],
                args[3],
                LOAD_HELD_STACK[0].load(Ordering::Relaxed).wrapping_sub(base)
            ));
        }
        if LOAD_HELD_PENDING.load(Ordering::Acquire)
            && SPEC_STATE.load(Ordering::Acquire) == SPEC_SPECTATE
            && !LOAD_DEATH_ARMED.load(Ordering::Acquire)
            && !self.load_args_logged
        {
            self.load_args_logged = true;
            let a: Vec<usize> = LOAD_HELD_ARGS.iter().map(|a| a.load(Ordering::Relaxed)).collect();
            let base = module_base();
            let callers: Vec<String> = LOAD_HELD_STACK
                .iter()
                .map(|s| s.load(Ordering::Relaxed))
                .filter(|&x| x != 0)
                .map(|x| format!("{:#x}", x.wrapping_sub(base)))
                .collect();
            log::line(&format!(
                "spectate: held load args (phase B) a={:#x} b={:#x} c={:#x} d={:#x} callers [{}]",
                a[0],
                a[1],
                a[2],
                a[3],
                callers.join(", ")
            ));
        }
        // A load pending in phase B that did not come through 0x6D7070: a route we do not
        // know yet. Logged once (GameMan request bytes) so the next live test shows it.
        if SPEC_STATE.load(Ordering::Acquire) == SPEC_SPECTATE && !self.load_pending_logged && game::load_pending() {
            self.load_pending_logged = true;
            let gm = read_ptr(module_base() + 0x3D5C0D0);
            let b = |off: usize| gm.and_then(|g| game::read::<u8>(g + off));
            log::line(&format!(
                "spectate: a load is pending during phase B by a route we do not hold (GameMan +0x10 {:?}, +0xB4A {:?}, +0xC41 {:?}, +0xB6C {:?}, +0xB6E {:?}, +0xDB1 {:?}); the world will reload",
                b(0x10),
                b(0xB4A),
                b(0xC41),
                b(0xB6C),
                b(0xB6E),
                b(0xDB1)
            ));
        }

        match SPEC_STATE.load(Ordering::Acquire) {
            SPEC_WAITING | SPEC_SPECTATE => {
                let defers = DEFER_COUNT.load(Ordering::Relaxed);
                if defers != 0
                    && defers != self.defer_logged && self.defer_log_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                    let again = defers > 1;
                    self.defer_logged = defers;
                    self.defer_log_at = Some(now);
                    let rva = DEFER_RET.load(Ordering::Relaxed).wrapping_sub(module_base());
                    let who = match rva {
                        RET_EMEVD => " (EMEVD 2003[95])",
                        RET_ESD => " (ESD command 131)",
                        _ => "",
                    };
                    log::line(&format!(
                        "spectate: reload deferred{} from {rva:#x}{who} (call #{defers})",
                        if again { " (again)" } else { "" }
                    ));
                }
                let shown = self
                    .cam_target
                    .or_else(|| self.watch.iter().find(|(_, w)| w.out.is_none()).map(|(id, _)| *id))
                    .and_then(|id| peers.by_id(id))
                    .or(peers.first());
                let name = friend_name(shown, net);
                self.overlay = Some(if tuning::debug_hotkeys() {
                    format!("Spectating {name} - [F8] wake at idol")
                } else {
                    format!("Spectating {name}")
                });

                // Each watched peer: at 0 HP for DEAD_AFTER (90 s fallback, no revive) = dead, out of
                // our block for 5 s = gone (or its committed EV_DEATH / EV_DOWN / gauge-gone,
                // `peer_out`). HP back above 0 (a resurrection) cancels its timer.
                for (id, w) in self.watch.iter_mut() {
                    // A peer in a cutscene reads hidden, which the timers would count as gone.
                    if peers.by_id(*id).is_some_and(|p| p.cutscene) {
                        w.gone_since = None;
                        continue;
                    }
                    let was = w.out;
                    watch_step(w, peers.by_id(*id).and_then(|p| p.snapshot), here, now);
                    if was.is_none()
                        && let Some(why) = w.out
                    {
                        log::line(&format!("spectate: peer {} is out of the fight ({why})", id.slot()));
                    }
                }
                // Hard triggers first (§40): they win over a pending "boss defeated" check
                // below. Any peer's rest arrives through RELEASE_REQ; then the link drop, every
                // watched peer out (a wipe only when each of them died, `wait_verdict`; a
                // watched peer that dropped its link is out after GONE_AFTER and the camera
                // switches to another living one meanwhile), the age limit and our own rest.
                let mut hard: Option<&'static str> = None;
                if let Some(req) = RELEASE_REQ.lock().ok().and_then(|mut r| r.take()) {
                    if matches!(req, "wipe" | "rest") {
                        hard = Some(req);
                    } else if release.is_none() {
                        release = Some(req);
                    }
                }
                if hard.is_none() {
                    hard = self.gone_reason(connected, peers, here, now);
                }
                if hard.is_none() {
                    hard = wait_verdict(&self.watch);
                }
                if hard.is_none() {
                    hard = self.spec_since.and_then(|t| spec_timeout(t.elapsed()));
                }
                if hard.is_none() && self.spec_since.is_some_and(crate::restsync::rest_after) {
                    hard = Some("rest");
                }
                if let Some(h) = hard {
                    self.defeat_wait = None;
                    release = Some(h);
                }
                let f8_edge = {
                    let down = f8_down();
                    let edge = down && !self.f8_down;
                    self.f8_down = down;
                    edge
                };
                if release.is_none() && (wake || f8_edge) {
                    release = Some("user");
                }
                // The friend killed the miniboss we died to (loaded here, now at 0 HP): the
                // same release as a boss defeat, through the check below.
                if release.is_none()
                    && self.wait_mini
                    && game::chr_by_entity_id(self.wait_boss).and_then(|c| c.hp()).is_some_and(|h| h <= 0)
                {
                    log::line(&format!("spectate: miniboss {} is dead here, releasing as boss defeated", self.wait_boss));
                    release = Some("boss defeated");
                }
                // The friend's "boss defeated" before our world saw the defeat: a reload
                // now could bring the boss back here (its defeat flag not written yet).
                // Wait for our HandleBossDefeat, the boss dead here, or 20 s.
                if matches!(release, None | Some("boss defeated")) && (release.is_some() || self.defeat_wait.is_some()) {
                    let ours = self.defeat_handled_at.is_some_and(|t| t.elapsed() < Duration::from_secs(30));
                    // A table boss: its encounter is defeated (a dead phase entity is not the end).
                    let boss_dead = match self.wait_enc {
                        Some(enc) => self.encounter_defeated(enc),
                        None => self.wait_boss > 0 && self.entity_done(self.wait_boss, true),
                    };
                    let since = *self.defeat_wait.get_or_insert(now);
                    let waited = now.duration_since(since);
                    if ours || boss_dead || waited >= DEFEAT_WAIT_MAX {
                        if waited >= Duration::from_millis(100) {
                            log::line(&format!(
                                "spectate: boss {} defeat {} after {} ms, releasing",
                                self.wait_boss,
                                if ours { "seen here" } else if boss_dead { "(boss dead / gone here)" } else { "not seen here (20 s)" },
                                waited.as_millis()
                            ));
                        }
                        self.defeat_wait = None;
                        release = release.or(Some("boss defeated"));
                    } else {
                        if release == Some("boss defeated") {
                            log::line(&format!(
                                "spectate: the friend's boss {} fell; waiting for its defeat in our world before the release (up to 20 s)",
                                self.wait_boss
                            ));
                        }
                        release = None;
                    }
                }
                // A release that would reload us while somebody is in a cutscene waits and
                // is retried every frame (cutscene.rs; at most `POSTPONE_MAX`).
                release = postpone_release(release, &mut self.held_release, crate::cutscene::any_in_cutscene(), now);
                if let Some(reason) = release {
                    self.release(reason, net, world, peers);
                }
            }
            SPEC_RELEASING => {
                self.overlay = None;
                // A BossRules that did not start the release (e.g. made new after a
                // panic) starts the watchdog now instead of never.
                let since = self.releasing_since.get_or_insert(now).elapsed();
                let dead = main_player_hp().is_some_and(|hp| hp <= 0);
                match releasing_action(dead, since, game::load_pending(), menu_death_screen(), self.force_tries) {
                    Action::Wait => {}
                    Action::ForceReload => {
                        self.force_reload(&format!("released {} s ago, still dead, no load, no death screen", since.as_secs()));
                    }
                    Action::ForceWarp => {
                        self.force_tries = 2;
                        self.releasing_since = Some(now);
                        let idol = game::last_idol();
                        log::line(&format!("spectate: still dead {} s after the forced reload: forcing a warp to idol {idol}", since.as_secs()));
                        game::warp_to_idol(idol);
                    }
                    Action::Off => {
                        if dead {
                            log::line("spectate: forced reload and warp did not bring a load, state reset (the dead watchdog goes on)");
                        } else {
                            log::line("spectate: alive again after the release, state reset");
                        }
                        SPEC_STATE.store(SPEC_OFF, Ordering::Release);
                        self.force_tries = 0;
                        self.releasing_since = None;
                    }
                }
            }
            _ => self.overlay = None,
        }
    }

    /// "Link lost" after its grace period; "friend left the block" when the wait has nobody
    /// to watch at all (each watched peer has its own timer in `watch_step`).
    fn gone_reason(&mut self, connected: bool, peers: &PeerSet, here: Option<i32>, now: Instant) -> Option<&'static str> {
        if connected {
            self.link_lost_since = None;
        } else if now.duration_since(*self.link_lost_since.get_or_insert(now)) >= GONE_AFTER {
            return Some("link lost");
        }
        if !self.watch.is_empty() {
            self.friend_gone_since = None;
        } else if peers.first().is_some_and(|p| p.cutscene)
            || peers.first().and_then(|p| p.snapshot).is_some_and(|r| here == Some(r.block))
        {
            self.friend_gone_since = None;
        } else if now.duration_since(*self.friend_gone_since.get_or_insert(now)) >= GONE_AFTER {
            return Some("friend left the block");
        }
        None
    }

    /// Phase A: runs the reload we held back. Phase B: lets the CSDeadStep through
    /// (penalty, death screen, its own reload). The load that follows ends the state.
    fn release(&mut self, reason: &'static str, net: &mut Option<&mut Net>, world: &mut World, peers: &PeerSet) {
        let secs = self.spec_since.map_or(0, |t| t.elapsed().as_secs());
        log::line(&format!("spectate: release {reason} after {secs} s"));
        self.defeat_wait = None;
        // Every peer we waited for died. A session wipe only when that is every connected
        // peer (two players: always); a living peer elsewhere keeps the world (persist rule).
        if reason == "wipe" && peers.iter().all(|p| self.watch.contains_key(&p.id)) {
            world.note_wipe(peers);
            // Both died: bosses and the dead set start over (persist.rs).
            crate::persist::note_wipe("boss wipe");
        } else if reason == "wipe" {
            log::line("spectate: everyone in the fight died, but a companion elsewhere lives: no session wipe");
        }
        self.watch.clear();
        self.cam_target = None;
        // The camera goes back before anything reloads.
        self.cam.release(reason);
        let was = SPEC_STATE.swap(SPEC_RELEASING, Ordering::AcqRel);
        SPEC_WANTED.store(false, Ordering::Release);
        SPEC_LATCH.store(false, Ordering::Release);
        SPEC_B_OK.store(false, Ordering::Release);
        self.releasing_since = Some(Instant::now());
        self.force_tries = 0;
        self.overlay = None;
        send(net, world::EV_DOWN, 0, 0);
        let held_load = LOAD_HELD_PENDING.swap(false, Ordering::AcqRel);
        let armed = LOAD_DEATH_ARMED.swap(false, Ordering::AcqRel);
        self.load_args_logged = false;
        if was == SPEC_SPECTATE {
            // A load request held while we spectated (live 2026-10-24: the Lua event warp
            // 0x68E850) was the only thing that would reload. Its arguments may point into a
            // stack frame that is long gone. v8.28a: it is replayed anyway (the null-arg reload
            // 0xA8F7E0 wiped emblems, storage and Resurrect nodes); ini `spectate_replay_load = 0`
            // brings the old path back.
            match phase_b_release(u32::from(held_load), crate::tuning::spectate_replay_load()) {
                ReleaseB::ReplayHeld => {
                    HELD_RELOAD.store(false, Ordering::Release);
                    replay_held_request("phase B");
                }
                ReleaseB::NullReload => {
                    HELD_RELOAD.store(false, Ordering::Release);
                    log::line("spectate: spectate_replay_load = 0: using the old null-arg death reload (loses items)");
                    let ret = mod_death_reload();
                    log::line(&format!("spectate: a load was held while spectating: death reload called (returned {ret:#x})"));
                }
                ReleaseB::LetDeathStep => {
                    log::line("spectate: phase B released, the game's own death follows (penalty, death screen, reload)");
                }
            }
            return;
        }
        HELD_RELOAD.store(false, Ordering::Release);
        let ret = replay_or_reload(held_load, armed);
        log::line(&format!("spectate: original reload called (returned {ret:#x})"));
    }

    /// Calls the game's own death reload now (a dead player that no load follows) and
    /// hands the rest to the `RELEASING` watchdog (next step: a warp, 15 s later).
    fn force_reload(&mut self, why: &str) {
        log::line(&format!("spectate: {why}: forcing the reload"));
        SPEC_STATE.store(SPEC_RELEASING, Ordering::Release);
        HELD_RELOAD.store(false, Ordering::Release);
        self.force_tries = 1;
        self.releasing_since = Some(Instant::now());
        let held_load = LOAD_HELD_PENDING.swap(false, Ordering::AcqRel);
        let armed = LOAD_DEATH_ARMED.swap(false, Ordering::AcqRel);
        let ret = replay_or_reload(held_load, armed);
        log::line(&format!("spectate: forced reload called (returned {ret:#x})"));
    }

    /// Dead watchdogs outside the spectate wait: a rest while dead (immediately, after
    /// `REST_FORCE_AFTER`), and dead with no load / death screen for `DEAD_IDLE_MAX`.
    fn dead_watchdog(&mut self, dead: bool, now: Instant) {
        let rest_req = FORCE_REQ.swap(false, Ordering::AcqRel);
        let state = SPEC_STATE.load(Ordering::Acquire);
        let idle_ok = dead && !spectating() && !game::load_pending() && !menu_death_screen();
        if !idle_ok {
            self.dead_idle_since = None;
            return;
        }
        let idle = now.duration_since(*self.dead_idle_since.get_or_insert(now));
        if rest_req && (state == SPEC_RELEASING || idle >= REST_FORCE_AFTER) {
            self.force_reload("a rest while dead with no death screen");
            self.dead_idle_since = None;
        } else if state != SPEC_RELEASING && dead_idle_due(idle) {
            self.force_reload(&format!("dead for {} s with no death screen", idle.as_secs()));
            self.dead_idle_since = None;
        }
    }

    /// Overlay for a peer's `EV_BOSS(1)`: name, distance, and the way in.
    fn announce_friend_boss(&self, name: &str, player: Chr, remote: Option<Snapshot>, here: Option<i32>, alive: bool) {
        let mut text = format!("{name} started a boss fight");
        if let Some(r) = remote.filter(|r| here == Some(r.block))
            && let Some(p) = player.position()
        {
            let dist = game::distance(p, r.pos);
            text += &format!(" ({dist:.0} m away)");
            if dist > 30.0 && alive {
                text += " - go through the fog gate to join";
            }
        }
        // bossgate.rs: its start flags wait until we are near (or the boss falls).
        if crate::bossgate::holding_friend_fight() {
            log::line("boss: the friend's fight is gated here: its start flags wait until we are near");
        }
        notice(text, 8);
    }

    /// Sends our boss lots to the friend; applies the ones that arrived (SPEC §4.2).
    fn reward_step(&mut self, net: &mut Option<&mut Net>, connected: bool, in_world_for: Duration) {
        if let Some((boss, at)) = self.defeat
            && at.elapsed() >= REWARD_SEND_AFTER
        {
            self.defeat = None;
            let lots = COLLECTED.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
            if connected && tuning::boss_reward() {
                if lots.is_empty() {
                    log::line(&format!("reward: no lots collected for boss {boss}"));
                } else if let Some(n) = net.as_deref_mut() {
                    let ok = n.send_bulk(world::BULK_BOSS_REWARD, &encode_reward(boss, &lots));
                    log::line(&format!("reward: sent {} lots for boss {boss} (ok = {ok})", lots.len()));
                    for (lot, _) in &lots {
                        crate::audit::reward_sent(crate::audit::Pipe::BossReward, crate::audit::key2(boss, *lot), *lot);
                    }
                }
            }
        }

        if let Some(n) = net.as_deref_mut() {
            while let Some((from, d)) = n.take_bulk_from(world::BULK_BOSS_REWARD) {
                log::line(&format!("reward: received {} bytes, giving them in {} s", d.len(), REWARD_APPLY_AFTER.as_secs()));
                if let Some((boss, lots)) = decode_reward(&d) {
                    for (lot, _) in lots {
                        crate::audit::reward_received(crate::audit::Pipe::BossReward, crate::audit::key2(boss, lot), lot, from);
                    }
                }
                self.pending_rewards.push((Instant::now(), from, d));
            }
        }
        self.verify_rewards(in_world_for);
        if self.pending_rewards.is_empty() {
            return;
        }
        if !tuning::boss_reward() {
            log::line("reward: option off here, dropping the friend's lots");
            self.pending_rewards.clear();
            return;
        }
        if !self.pending_rewards.iter().any(|(t, ..)| t.elapsed() >= REWARD_APPLY_AFTER) {
            return;
        }
        if let Err(why) = game::safe_for_reward(in_world_for) {
            if self.reward_wait_log_at.is_none_or(|t| t.elapsed().as_secs() >= 5) {
                self.reward_wait_log_at = Some(Instant::now());
                log::line(&format!("reward: waiting ({why})"));
            }
            return;
        }
        let (due, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.pending_rewards).into_iter().partition(|(t, ..)| t.elapsed() >= REWARD_APPLY_AFTER);
        self.pending_rewards = later;
        for (_, from, data) in due {
            self.apply_reward(from, &data);
        }
    }

    /// After a boss lot was awarded here: the unique goods it surely gives that are still
    /// missing are given directly (GrantItems), once per lot.
    fn verify_rewards(&mut self, in_world_for: Duration) {
        if self.reward_verify.is_empty() || !self.reward_verify.iter().any(|(t, ..)| t.elapsed() >= REWARD_VERIFY_AFTER) {
            return;
        }
        if game::safe_for_reward(in_world_for).is_err() {
            return;
        }
        let (due, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.reward_verify).into_iter().partition(|(t, ..)| t.elapsed() >= REWARD_VERIFY_AFTER);
        self.reward_verify = later;
        for (_, boss, lot) in due {
            let missing = crate::keyitems::boss_lot_missing(lot);
            if missing.is_empty() {
                log::line(&format!("reward: lot {lot} for boss {boss} checked, every unique item is owned"));
                continue;
            }
            let ok = crate::keyitems::give_entries(&missing);
            log::line(&format!(
                "reward: lot {lot} for boss {boss}: the award left out {:?}: gave them (ok = {ok})",
                crate::keyitems::describe_all(&missing)
            ));
        }
    }

    fn apply_reward(&mut self, from: PeerId, data: &[u8]) {
        let Some((boss, lots)) = decode_reward(data) else {
            log::line(&format!("reward: unreadable bulk ({} bytes), ignored", data.len()));
            return;
        };
        let Some(items) = read_ptr(module_base() + ITEM_MAN_RVA) else {
            log::line("reward: item manager not readable, lots dropped");
            return;
        };
        let award: extern "C" fn(usize, i32, i32) -> usize = unsafe { std::mem::transmute(module_base() + AWARD_RVA) };
        for (lot, flags) in lots {
            crate::audit::reward_resolved(crate::audit::Pipe::BossReward, crate::audit::key2(boss, lot), lot, &flags, from);
            if !self.applied.insert((boss, lot)) {
                log::line(&format!("reward: lot {lot} for boss {boss} was already handled"));
                continue;
            }
            // Ours already? Our own award of this lot ran (fix 3: lot 2499 has no flags), or
            // its flags are on and we own its unique items (fix 4); flags on but a unique
            // item missing: only that (IMPLEMENTATION-NOTES §33).
            match crate::keyitems::boss_lot_check(lot, &flags) {
                crate::keyitems::BossLot::Skip(why) => {
                    log::line(&format!("reward: lot {lot} for boss {boss} not given: {why}"));
                    self.reward_verify.push((Instant::now(), boss, lot));
                    continue;
                }
                crate::keyitems::BossLot::GiveMissing(entries) => {
                    let ok = crate::keyitems::give_entries(&entries);
                    log::line(&format!(
                        "reward: lot {lot} for boss {boss}: lot flags {flags:?} on here but we lacked {:?}: gave only those (ok = {ok})",
                        crate::keyitems::describe_all(&entries)
                    ));
                    continue;
                }
                crate::keyitems::BossLot::Award => {}
            }
            REWARD_APPLYING.store(true, Ordering::Release);
            award(items, lot, 1);
            REWARD_APPLYING.store(false, Ordering::Release);
            // Our own world's copy of this kill awarding the lot later is the double.
            crate::keyitems::note_replayed(lot);
            log::line(&format!("reward: gave lot {lot} (flags {flags:?})"));
            self.reward_verify.push((Instant::now(), boss, lot));
        }
    }

    /// The friend's rest sent us home from the arena (SPEC §3.1) once the warp gate allows.
    fn expel_step(&mut self, alive: bool, in_world_for: Duration) -> bool {
        // Nobody starts an expulsion warp while a cutscene runs (ours: it would cut it off;
        // a peer's: its world is busy). The request stays up and is taken afterwards.
        if self.expel.is_none() && crate::cutscene::any_in_cutscene() {
            return false;
        }
        if EXPEL_REQ.swap(false, Ordering::AcqRel) && self.expel.is_none() {
            self.expel = Some(game::WarpWait::default());
            notice("Friend rested - sending you back to your idol".into(), 8);
        }
        let Some(wait) = self.expel.as_mut() else { return false };
        if !alive {
            log::line("expel: we died meanwhile, not warping");
            self.expel = None;
            return false;
        }
        match wait.poll(in_world_for) {
            game::WarpGate::Wait => false,
            game::WarpGate::GaveUp => {
                self.expel = None;
                false
            }
            game::WarpGate::Go => {
                let idol = game::last_idol();
                log::line(&format!("expel: friend rested, warping to our last idol {idol}"));
                game::warp_to_idol(idol);
                self.expel = None;
                true
            }
        }
    }

    // --- Flag recorder (SPEC §2, diagnostics only) -------------------------------

    /// Follows the flag ring: keeps the last 10 s, feeds an active recording, logs a finished one.
    fn pump_flags(&mut self, now: Instant) {
        let count = hook::flag_ring_count();
        let mut n = *self.flag_cursor.get_or_insert(count);
        if count.wrapping_sub(n) > hook::FLAG_RING {
            n = count.wrapping_sub(hook::FLAG_RING);
        }
        while n != count {
            match hook::flag_ring_entry(n) {
                FlagEntry::Pending => break,
                FlagEntry::Lost => {}
                FlagEntry::Ready(id, on, 0) => {
                    self.flag_hist.push_back((now, id, on));
                    // A table boss's own defeat flag (synced from the friend or ours) is a
                    // defeat even before our HBD (bosses.rs).
                    if on && let Verdict::Defeated(e) = bosses::classify(Sig::FlagOn(id), &|_| false) {
                        let name = bosses::encounter_of(e).map_or("?", |x| x.name);
                        log::line(&format!("boss: defeat flag {id} on ({name}, entity {e})"));
                        self.flag_defeat = Some((e, now));
                        crate::keyitems::note_defeat_flag();
                    }
                    if let Some(r) = self.recording.as_mut()
                        && r.entries.len() < 4 * REC_LOG_MAX
                    {
                        r.entries.push((now.duration_since(r.at).as_millis() as i64, id, on));
                    }
                }
                FlagEntry::Ready(..) => {}
            }
            n = n.wrapping_add(1);
        }
        self.flag_cursor = Some(n);
        while self.flag_hist.len() > REC_HISTORY
            || self.flag_hist.front().is_some_and(|(t, ..)| now.duration_since(*t) > REC_BEFORE)
        {
            self.flag_hist.pop_front();
        }
        if self.recording.as_ref().is_some_and(|r| now.duration_since(r.at) >= REC_AFTER)
            && let Some(r) = self.recording.take()
        {
            let total = r.entries.len();
            let shown: Vec<String> =
                r.entries.iter().take(REC_LOG_MAX).map(|(ms, id, on)| format!("({id},{},{ms})", u8::from(*on))).collect();
            log::line(&format!(
                "boss-flags: {}: [{}]{}",
                r.entity,
                shown.join(","),
                if total > REC_LOG_MAX { format!(" +{} more", total - REC_LOG_MAX) } else { String::new() }
            ));
            // Boss start gate: what our world switched on around our gauge = start flags.
            crate::bossgate::learn_own(r.entity, &r.entries);
        }
    }

    /// Starts a recording at a gauge rising edge, with the last 10 s of writes.
    fn arm_recording(&mut self, entity: i32, now: Instant) {
        let entries = self
            .flag_hist
            .iter()
            .map(|(t, id, on)| (-(now.duration_since(*t).as_millis() as i64), *id, *on))
            .collect();
        self.recording = Some(Recording { entity, at: now, entries });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hybrid_decision_table() {
        use super::{Hybrid::*, hybrid_decision as d, *};
        assert_eq!(d(ELIG_YES, true), GameDeath);
        assert_eq!(d(ELIG_YES, false), GameDeath);
        assert_eq!(d(ELIG_NO, true), Spectate);
        assert_eq!(d(ELIG_NO, false), GameDeath);
        assert_eq!(d(ELIG_UNKNOWN, true), GameDeath);
        assert_eq!(d(ELIG_UNKNOWN, false), GameDeath);
    }

    #[test]
    fn resurrect_eligible_table() {
        use super::resurrect_eligible as e;
        assert!(!e(100.0, true, 1, 2, false, false), "lock set");
        assert!(!e(0.0, false, 1, 2, false, false), "no Resurrection skill");
        assert!(e(0.0, true, 1, 0, false, false), "temporary node");
        assert!(e(0.0, true, 1, 0, true, false), "temporary node works with SpEffect 400");
        assert!(e(0.0, true, 0, 2, false, false), "nodes");
        assert!(!e(0.0, true, 0, 2, true, false), "nodes blocked by SpEffect 400");
        assert!(!e(0.0, true, 0, 0, false, false), "nothing to spend");
        // v8.61, live 2026-10-28 (5/5): PlayerGameData+0x178 set = never offered, even with lock
        // 0 and nodes (0xA2CC30 keeps the window shut).
        assert!(!e(0.0, true, 1, 2, false, true), "PGD+0x178 set");
    }

    #[test]
    fn a_stale_no_revive_byte_is_cleared_once_alive_after_phase_b() {
        use super::clear_stale_no_revive as c;
        // Live 2026-10-28: after a phase B death (no CSDeadStep) the byte stayed 1 through the
        // reload and the next death had no Resurrect (5/5); a death screen death clears it itself.
        assert!(c(true, true, true, 1));
        assert!(!c(false, true, true, 1), "switched off");
        assert!(!c(true, false, true, 1), "no phase B death: the game's death step cleared it");
        assert!(!c(true, true, false, 1), "still dead / loading");
        assert!(!c(true, true, true, 0), "nothing to clear");
    }

    #[test]
    fn effective_elig_waits_for_the_window_then_spectates() {
        use super::{effective_elig as f, *};
        // Divine Dragon 2026-10-27 (lock 0, nodes, both window fields closed): the game's death
        // screen at once, never held (holding it lost the Resurrect).
        assert_eq!(f(ELIG_YES, true, 0), Some(ELIG_YES));
        assert_eq!(f(ELIG_YES, true, 900), Some(ELIG_YES));
        // Unknown inventory: an open window = the game's death screen, closed = wait, then no.
        assert_eq!(f(ELIG_UNKNOWN, false, 0), Some(ELIG_UNKNOWN));
        assert_eq!(f(ELIG_UNKNOWN, true, RES_WINDOW_GRACE_MS - 1), None);
        assert_eq!(f(ELIG_UNKNOWN, true, RES_WINDOW_GRACE_MS), Some(ELIG_NO));
        // The inventory conditions failing: phase B at once.
        assert_eq!(f(ELIG_NO, true, 0), Some(ELIG_NO));
        assert_eq!(f(ELIG_NO, false, 0), Some(ELIG_NO));
    }

    use super::*;

    fn snap(hp: i32, block: i32) -> Snapshot {
        Snapshot { hp, block, ..Default::default() }
    }

    fn eng(id: u8, boss: bool, near: bool, here: bool, down: bool) -> Engage {
        Engage { id: PeerId(id), boss, near, here, down, recent_boss: false }
    }

    #[test]
    fn spec_latch_keeps_the_wait_wanted_while_dead() {
        // Alive: never wanted, latch or not.
        assert!(!spec_wanted(true, true, false));
        assert!(!spec_wanted(false, false, false));
        // Dead: wanted if the live verdict says so, or the latch from the first dead frame does.
        assert!(spec_wanted(true, false, true));
        assert!(spec_wanted(false, true, true));
        assert!(spec_wanted(true, true, true));
        // Dead, neither: no wait.
        assert!(!spec_wanted(false, false, true));
    }

    #[test]
    fn recent_boss_grace_counts_only_in_our_fight_alive_and_here() {
        let rb = |here: bool, down: bool| Engage { recent_boss: true, ..eng(1, false, false, here, down) };
        assert_eq!(engaged_ids(&[rb(true, false)], true), vec![PeerId(1)]);
        assert!(engaged_ids(&[rb(false, false)], true).is_empty(), "not here");
        assert!(engaged_ids(&[rb(true, true)], true).is_empty(), "down");
        assert!(engaged_ids(&[rb(true, false)], false).is_empty(), "not our fight");
    }

    #[test]
    fn engaged_peers_hold_the_reload() {
        // Two players: exactly the old in_fight && friend_here && !friend_down.
        assert_eq!(engaged_ids(&[eng(1, true, false, true, false)], false), vec![PeerId(1)]);
        assert!(engaged_ids(&[eng(1, false, true, true, false)], false).is_empty());
        assert_eq!(engaged_ids(&[eng(1, false, true, true, false)], true), vec![PeerId(1)]);
        assert!(engaged_ids(&[eng(1, true, true, false, false)], true).is_empty(), "not alive here");
        assert!(engaged_ids(&[eng(1, true, true, true, true)], true).is_empty(), "down");
        // Five peers: only the living, present, fighting ones.
        let list = [
            eng(1, true, false, true, false),
            eng(2, false, false, true, false), // far, own no fight
            eng(3, false, true, true, false),  // near us in our fight
            eng(4, true, true, true, true),    // down
            eng(5, true, true, false, false),  // dead / elsewhere
        ];
        assert_eq!(engaged_ids(&list, true), vec![PeerId(1), PeerId(3)]);
    }

    #[test]
    fn wipe_only_when_no_engaged_peer_is_left() {
        for n in 1..=5u8 {
            let mut watch: BTreeMap<PeerId, Watch> = (1..=n).map(|i| (PeerId(i), Watch::default())).collect();
            for i in 1..=n {
                assert_eq!(wait_verdict(&watch), None, "{n} peers, {} still fighting", n - i + 1);
                watch.get_mut(&PeerId(i)).unwrap().out = Some("wipe");
            }
            assert_eq!(wait_verdict(&watch), Some("wipe"), "{n} peers all dead");
            // One of them left the block instead of dying: no wipe.
            watch.get_mut(&PeerId(n)).unwrap().out = Some("friend left the block");
            assert_eq!(wait_verdict(&watch), Some("friend left the block"));
        }
        assert_eq!(wait_verdict(&BTreeMap::new()), None);
    }

    #[test]
    fn only_our_fights_boss_defeat_ends_the_wait() {
        let watch: BTreeMap<PeerId, Watch> = [(PeerId(1), Watch::default()), (PeerId(2), Watch::default())].into_iter().collect();
        // A watched peer's kill ends it, whatever the entity says.
        assert!(defeat_ends_wait(&watch, 3_000_800, PeerId(1), 0));
        assert!(defeat_ends_wait(&watch, 3_000_800, PeerId(2), 3_000_801));
        // Another group's boss in the same block: the wait goes on ...
        assert!(!defeat_ends_wait(&watch, 3_000_800, PeerId(3), 3_000_900));
        assert!(!defeat_ends_wait(&watch, -1, PeerId(3), 3_000_900));
        assert!(!defeat_ends_wait(&watch, 3_000_800, PeerId(3), 0));
        // ... unless it is the boss we wait for.
        assert!(defeat_ends_wait(&watch, 3_000_800, PeerId(3), 3_000_800));
    }

    #[test]
    fn watched_peer_timers() {
        let t = Instant::now();
        let mut w = Watch::default();
        watch_step(&mut w, Some(snap(0, 5)), Some(5), t);
        assert_eq!(w.out, None);
        watch_step(&mut w, Some(snap(0, 5)), Some(5), t + DEAD_AFTER);
        assert_eq!(w.out, Some("wipe"));
        // Sticky: alive again later does not bring it back into the wait.
        watch_step(&mut w, Some(snap(100, 5)), Some(5), t + DEAD_AFTER * 2);
        assert_eq!(w.out, Some("wipe"));
        let mut g = Watch::default();
        watch_step(&mut g, Some(snap(100, 6)), Some(5), t);
        watch_step(&mut g, None, Some(5), t + GONE_AFTER - Duration::from_millis(1));
        assert_eq!(g.out, None);
        watch_step(&mut g, None, Some(5), t + GONE_AFTER);
        assert_eq!(g.out, Some("friend left the block"));
        // Back in the block in time: the timer restarts.
        let mut b = Watch::default();
        watch_step(&mut b, None, Some(5), t);
        watch_step(&mut b, Some(snap(100, 5)), Some(5), t + Duration::from_secs(4));
        watch_step(&mut b, None, Some(5), t + Duration::from_secs(8));
        assert_eq!(b.out, None);
    }

    /// The camera (spectate_cam.rs `choose_target`) with the wait's peers: the nearest living
    /// watched one, sticky, switching when it is out; never a peer outside the fight.
    #[test]
    fn spectate_target_follows_the_wait() {
        let at = |slot: u8, x: f32, hp: i32| PeerFrame {
            snapshot: Some(Snapshot { pos: [x, 0.0, 0.0], hp, block: 1, ..Default::default() }),
            view: Some(([x, 0.0, 0.0], 0.0)),
            ..PeerFrame::bare(PeerId(slot))
        };
        let mut set = PeerSet::new(PeerId::HOST, true);
        for (s, x) in [(1, 40.0), (2, 12.0), (3, 7.5), (4, 7.6), (5, 2.0)] {
            set.push(at(s, x, 100));
        }
        // Peer 5 is nearest but not in our fight.
        let mut watch: BTreeMap<PeerId, Watch> = (1..=4).map(|i| (PeerId(i), Watch::default())).collect();
        let pick = |w: &BTreeMap<PeerId, Watch>, cur| {
            spectate_cam::choose_target(&set, cur, Some([0.0; 3]), &|p: &PeerFrame| w.get(&p.id).is_some_and(|w| w.out.is_none()))
        };
        assert_eq!(pick(&watch, None), Some(PeerId(3)));
        assert_eq!(pick(&watch, Some(PeerId(2))), Some(PeerId(2)), "sticky while in the fight");
        watch.get_mut(&PeerId(2)).unwrap().out = Some("wipe");
        assert_eq!(pick(&watch, Some(PeerId(2))), Some(PeerId(3)), "switches on its death");
    }

    #[test]
    fn our_fight_at_death_adds_only_the_near_multi_life_case() {
        assert!(!our_fight_at_death(false, false, false, false, false), "nothing near: unchanged");
        assert!(our_fight_at_death(false, false, false, false, true), "died next to a multi-life enemy");
        assert!(our_fight_at_death(true, false, false, false, false));
        assert!(our_fight_at_death(false, true, false, false, false));
        assert!(our_fight_at_death(false, false, true, false, false));
        assert!(our_fight_at_death(false, false, false, true, false));
    }

    #[test]
    fn miniboss_needs_lives_life_and_nearness() {
        // 2 orbs, hurt, close: a fight.
        assert!(mini_engaged(2, 500, 1000, 2, 10.0, None));
        // Single-life enemy: never.
        assert!(!mini_engaged(1, 500, 1000, 0, 10.0, Some(5.0)));
        // Dead or too far.
        assert!(!mini_engaged(2, 0, 1000, 1, 10.0, None));
        assert!(!mini_engaged(2, 500, 1000, 2, 40.0, Some(5.0)));
    }

    #[test]
    fn miniboss_engaged_by_lives_or_friend() {
        // Untouched, nobody else near: idle, not a fight.
        assert!(!mini_engaged(2, 1000, 1000, 2, 10.0, None));
        assert!(!mini_engaged(2, 1000, 1000, 2, 10.0, Some(60.0)));
        // A life gone, or the friend at it.
        assert!(mini_engaged(2, 1000, 1000, 1, 10.0, None));
        assert!(mini_engaged(2, 1000, 1000, 2, 10.0, Some(20.0)));
    }

    #[test]
    fn dead_watchdog_steps() {
        let s = Duration::from_secs;
        // (a) dead 15 s, nothing loading, no death screen: reload.
        assert_eq!(releasing_action(true, s(15), false, false, 0), Action::ForceReload);
        assert_eq!(releasing_action(true, s(14), false, false, 0), Action::Wait);
        // (b) still dead 30 s after the forced reload: warp.
        assert_eq!(releasing_action(true, s(30), false, false, 1), Action::ForceWarp);
        assert_eq!(releasing_action(true, s(30), false, false, 2), Action::Off);
        // (c) alive: done.
        assert_eq!(releasing_action(false, s(40), false, false, 0), Action::Off);
        // (d) a load pending or the death screen up: wait.
        assert_eq!(releasing_action(true, s(40), true, false, 0), Action::Wait);
        assert_eq!(releasing_action(true, s(40), false, true, 1), Action::Wait);
        // Global watchdog.
        assert!(!dead_idle_due(s(44)));
        assert!(dead_idle_due(s(45)));
    }

    #[test]
    fn rest_releases_when_dead_or_waiting() {
        // (e) dead and OFF / RELEASING: a rest releases (forces the reload).
        assert!(should_release(SPEC_OFF, true, true));
        assert!(should_release(SPEC_RELEASING, true, true));
        assert!(should_release(SPEC_SPECTATE, false, true));
        assert!(should_release(SPEC_WAITING, true, true));
        // Alive and not waiting, or no rest: nothing.
        assert!(!should_release(SPEC_OFF, false, true));
        assert!(!should_release(SPEC_RELEASING, true, false));
    }

    #[test]
    fn friend_wipe_needs_90_s_or_ev_death() {
        let s = Duration::from_secs;
        // (f) HP 0 for 2 s, then back up (the caller clears the timer): no wipe.
        assert!(!friend_wipe(Some(s(2)), false));
        assert!(!friend_wipe(None, false));
        // The Resurrect choice waits without limit: 60 s at 0 HP is no wipe (live 2026-10-24).
        assert!(!friend_wipe(Some(s(60)), false));
        assert!(friend_wipe(Some(s(90)), false));
        // The committed death wipes at once.
        assert!(friend_wipe(None, true));
    }

    #[test]
    fn release_call_decision() {
        assert_eq!(release_call(true, true), ReleaseCall::ReplayLoad);
        assert_eq!(release_call(true, false), ReleaseCall::OriginalReload);
        assert_eq!(release_call(false, true), ReleaseCall::OriginalReload);
        assert_eq!(release_call(false, false), ReleaseCall::OriginalReload);
    }

    #[test]
    fn spectate_max_and_phase_b_release() {
        // (g)
        assert_eq!(spec_timeout(SPECTATE_MAX), Some("timeout"));
        assert_eq!(spec_timeout(SPECTATE_MAX - Duration::from_secs(1)), None);
        assert_eq!(SPECTATE_MAX, Duration::from_secs(300));
        // (h)
        assert_eq!(phase_b_release(1, true), ReleaseB::ReplayHeld);
        assert_eq!(phase_b_release(1, false), ReleaseB::NullReload);
        assert_eq!(phase_b_release(0, true), ReleaseB::LetDeathStep);
        assert_eq!(phase_b_release(0, false), ReleaseB::LetDeathStep);
    }

    /// A cutscene holds a release back and the clocks stop; afterwards it goes through.
    #[test]
    fn cutscene_postpones_releases_and_pauses_the_wait_clocks() {
        let t0 = Instant::now();
        let mut held = None;
        // No cutscene: a release passes, nothing is held, nothing without a release.
        assert_eq!(postpone_release(Some("rest"), &mut held, false, t0), Some("rest"));
        assert_eq!(postpone_release(None, &mut held, false, t0), None);
        assert!(held.is_none());
        // A cutscene: held, retried each frame, still held; then it ends and the release goes.
        assert_eq!(postpone_release(Some("wipe"), &mut held, true, t0), None);
        assert_eq!(held.map(|h| h.0), Some("wipe"));
        assert_eq!(postpone_release(None, &mut held, true, t0 + Duration::from_secs(10)), None);
        assert_eq!(postpone_release(None, &mut held, false, t0 + Duration::from_secs(12)), Some("wipe"));
        assert!(held.is_none());
        // A stuck cutscene flag cannot hold a release for ever.
        assert_eq!(postpone_release(Some("timeout"), &mut held, true, t0), None);
        assert_eq!(postpone_release(None, &mut held, true, t0 + POSTPONE_MAX - Duration::from_secs(1)), None);
        assert_eq!(postpone_release(None, &mut held, true, t0 + POSTPONE_MAX), Some("timeout"));
        // SPECTATE_MAX and DEAD_IDLE_MAX do not count a cutscene: shifted by the frame delta
        // they are not due after 10 minutes of frames inside one.
        let (mut spec, mut idle) = (t0, t0);
        let frame = Duration::from_millis(16);
        let mut now = t0;
        for _ in 0..(600_000 / 16) {
            now += frame;
            crate::cutscene::pause(&mut spec, frame);
            crate::cutscene::pause(&mut idle, frame);
        }
        assert_eq!(spec_timeout(now.duration_since(spec)), None);
        assert!(!dead_idle_due(now.duration_since(idle)));
        // ... and are due as before once it is over.
        assert_eq!(spec_timeout(now.duration_since(spec) + SPECTATE_MAX), Some("timeout"));
        assert!(dead_idle_due(now.duration_since(idle) + DEAD_IDLE_MAX));
    }
}
