//! Combat art and prosthetic effects of the friend on the native remote
//! (setting `sync_art_effects`, IMPLEMENTATION-NOTES § 14).
//!
//! Why they were missing [V, IDA on a private re7db copy, F:\Temp\ida_fx o1-o15]:
//! - Every TAE bullet of a player is created with the request flag `+0x3C & 8`
//!   ("network synced"): the TAE bullet handlers (`0x7D5A70`, `0x7D5760`,
//!   `0x7D5920`) set it from `chr.vtbl[53]` (is player).
//! - The chr bullet slot `0x9E5660` refuses such a request when the owner is a
//!   remote chr (`0x9F36C0`). So the native remote never makes its own wind
//!   slashes, spiral clouds, shuriken etc.; the game expects them from the owner.
//! - The owner's BulletManager (`*(base+0x3D84F70)`) queues a 224-byte
//!   `BULLET_SYNC_SETTING` per synced bullet (`0xB1AD20` -> `0xB20250`, vector
//!   at `+0x80`), and its update `0xB1C050` -> `0xB1D600` serialises them
//!   (`u8 count` + raw 224-byte records, `0xB1BA00`/`0xB1F6D0`) and broadcasts
//!   them as FrpgSessionManager type **63** (`0x8E7210` at `0xB1D6AE`). Offline
//!   the member list is empty, so the bytes go nowhere.
//! - The receive side of the same function asks every session member for
//!   type 63 (`0x8E4010` member vector at `0xB1D748`, `0x8E6B50` at `0xB1D77B`
//!   / `0xB1D7BF`), deserialises (`0xB1E930`), and creates each bullet with
//!   `0xB06B70` (record -> create param) + `0xB1A960` (at `0xB1D7FB`).
//!
//! What this module does (call-site patches inside `0xB1D600` only):
//! - Sender: the type-63 broadcast call goes through `bcast_hook`, which keeps
//!   the records whose owner is our own player and queues them for the friend
//!   (PlayerRaw packets of type `PTYPE`, no protocol change), then calls the
//!   original.
//! - Receiver: while any peer's records are queued, the member-vector call
//!   returns a fake one-member vector and the two recv calls serve the queued
//!   messages for that member. Each message's owner is rewritten to the native
//!   remote of the peer that sent it (the game's own `0xE8F040` net form of its
//!   handle, which `0xE8F170` resolves back to it), the target chr (a
//!   machine-local netId) is cleared. The game then deserialises and creates the
//!   bullets itself, exactly as for a real member.
//! - Multi-player (PLAN-MULTIPLAYER 5.1e / 5.2.2): ONE fake member serves every
//!   peer. The type-63 loop drains a member into one vector and takes each bullet's
//!   owner from its record (+0), never from the member, so per-message owners on one
//!   member work like k members. A peer without a fresh native remote has its
//!   messages dropped (counted "no remote"); the other peers' are still served.
//!
//! No double damage, with the game's own table (review fix 2026-10-23, IDA
//! `F:\Temp\ida_fix` o2/o3/o17): the damage router `0xB6A040` picks an action
//! from a 7x7 table `[victim class][attacker class]` (`0xB6F4C0`: 1 local
//! player, 2 remote player, 3-6 NPC/enemy). Offline only our native remotes are
//! class 2. Vanilla `[3..6][2]` is action 0 in both tables: `0xB6AB40` only
//! (guard direction, hit SFX `0x841C80`, `0xB6E5A0`), no HP or posture write.
//! Every other action calls `0xB68FF0(.., word_2A7C728[action])`; for action 3
//! that byte is 0, and `0xB68FF0` -> `0xB6F690` -> `0xB6E6A0(a5 = 0)` subtracts
//! HP (`+480`, `0xBD4D40`) AND posture (`+484`, `0xBD4DE0`) locally. So the
//! friend's bullet copies and sword hits on the native remote are already
//! visual only; the earlier patch `[3..6][2] = 3` made them real local damage
//! on top of the client's HP report (double HP). The table is no longer
//! patched; `check_damage_table` only logs that it reads vanilla.
//!
//! Game thread only: every hook runs inside the BulletManager update; `tick`
//! runs on our FrameBegin task. Hooks never panic into the game.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use coop_net::MAX_PLAYER_RAW;

use crate::game::{Chr, module_base, now_ms, read, read_bytes};
use crate::peers::{MAX_PLAYERS, PeerId, PeerSet};
use crate::{hook, log, tuning};

/// PlayerRaw type of our bullet messages (the game's own player packet types are < 100).
pub const PTYPE: u32 = 0xAF63;

// --- Game code (RVAs) ------------------------------------------------------

/// Call sites inside BulletManager net sync `0xB1D600`, with their original targets.
const SITE_BCAST: usize = 0xB1D6AE;
const FN_BCAST: usize = 0x8E7210;
const SITE_MEMBERS: usize = 0xB1D748;
const FN_MEMBERS: usize = 0x8E4010;
const SITE_RECV_A: usize = 0xB1D77B;
const SITE_RECV_B: usize = 0xB1D7BF;
const FN_RECV: usize = 0x8E6B50;
const SITE_CREATE: usize = 0xB1D7FB;
const FN_CREATE: usize = 0xB1A960;
/// `int* (int* out, int handle)`: chr handle -> network form (player handles carry PGD+0x10).
const TO_NET_OWNER_RVA: usize = 0xE8F040;
/// Damage action tables `u32 [7][7]` used by `0xB6A040` (`+0xFC` byte selects one).
const DMG_TABLES: [usize; 2] = [0x2A7C590, 0x2A7C660];
const DMG_COLS: usize = 7;
const ATTACKER_REMOTE: usize = 2;
const VICTIMS: [usize; 4] = [3, 4, 5, 6];
/// Vanilla action of `[3..6][2]`: `0xB6AB40` only, no HP / posture write.
const ACTION_REACTION_ONLY: u32 = 0;

const BULLET_SYNC_TYPE: u32 = 63;
const REC: usize = 224;
const REC_OWNER: usize = 0;
/// u16 target netId (`0xBF1830` lookup) and its "keep a lock" byte.
const REC_TARGET_NETID: usize = 20;
const REC_TARGET_LOCK: usize = 22;
const REC_BULLET_ID: usize = 32;
/// Records per message: 4 + 1 + 4 * 224 = 901 B (PlayerRaw allows 1100).
const PER_MSG: usize = 4;
const HEADER: usize = 4;
const MAX_OUTBOX: usize = 32;
const MAX_INBOX: usize = 16;
const NONE: u32 = u32::MAX;
/// A remote handle older than this is not used (no tick during a load).
const HANDLE_FRESH_MS: u64 = 1000;
const STATS_EVERY: Duration = Duration::from_secs(20);
const DETAIL_LOGS: u32 = 30;

// --- State -----------------------------------------------------------------

static INSTALL_TRIED: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// Setting on and hooks installed: the hooks do their work.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static TABLE_CHECKED: AtomicBool = AtomicBool::new(false);
/// The friend shows us natively (net.rs `forward`): only then are records queued.
static FORWARD: AtomicBool = AtomicBool::new(false);

static ORIG_BCAST: AtomicUsize = AtomicUsize::new(0);
static ORIG_MEMBERS: AtomicUsize = AtomicUsize::new(0);
static ORIG_RECV: AtomicUsize = AtomicUsize::new(0);
static ORIG_CREATE: AtomicUsize = AtomicUsize::new(0);

static OWN_HANDLE: AtomicU32 = AtomicU32::new(NONE);
/// Each peer's native remote chr handle (by `PeerId::index`) and when `tick` last saw it.
static REMOTE_HANDLE: [AtomicU32; MAX_PLAYERS] = [const { AtomicU32::new(NONE) }; MAX_PLAYERS];
static REMOTE_AT_MS: [AtomicU64; MAX_PLAYERS] = [const { AtomicU64::new(0) }; MAX_PLAYERS];
/// Net-form owner of each peer's native remote for the current BulletManager pass (set
/// by `members_hook`); NONE when that peer's records are not served this pass.
static PASS_OWNER: [AtomicU32; MAX_PLAYERS] = [const { AtomicU32::new(NONE) }; MAX_PLAYERS];
/// The current BulletManager pass serves our queued records (the fake member is out).
static PASS_SERVING: AtomicBool = AtomicBool::new(false);

/// One empty session member (56 B) and the vector `{_, begin, end}` the
/// type-63 receive loop iterates (`[rax+8]`, `[rax+0x10]`).
static FAKE_MEMBER: [AtomicU64; 7] = [const { AtomicU64::new(0) }; 7];
static FAKE_VEC: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];

/// Outgoing messages: `u32 owner, u8 n, n records`.
static OUTBOX: Mutex<VecDeque<Vec<u8>>> = Mutex::new(VecDeque::new());
/// Incoming, as the game's own format `u8 n, n records`, with the sending peer.
static INBOX: Mutex<VecDeque<(PeerId, Vec<u8>)>> = Mutex::new(VecDeque::new());

struct Counters {
    sent_msgs: AtomicU32,
    sent_bullets: AtomicU32,
    foreign_skipped: AtomicU32,
    outbox_full: AtomicU32,
    recv_msgs: AtomicU32,
    recv_bad: AtomicU32,
    served: AtomicU32,
    created: AtomicU32,
    create_failed: AtomicU32,
    no_remote: AtomicU32,
    too_big: AtomicU32,
    inbox_full: AtomicU32,
    send_logs: AtomicU32,
    create_logs: AtomicU32,
}

static C: Counters = Counters {
    sent_msgs: AtomicU32::new(0),
    sent_bullets: AtomicU32::new(0),
    foreign_skipped: AtomicU32::new(0),
    outbox_full: AtomicU32::new(0),
    recv_msgs: AtomicU32::new(0),
    recv_bad: AtomicU32::new(0),
    served: AtomicU32::new(0),
    created: AtomicU32::new(0),
    create_failed: AtomicU32::new(0),
    no_remote: AtomicU32::new(0),
    too_big: AtomicU32::new(0),
    inbox_full: AtomicU32::new(0),
    send_logs: AtomicU32::new(0),
    create_logs: AtomicU32::new(0),
};

static LAST_STATS: Mutex<Option<Instant>> = Mutex::new(None);

// --- Per-frame (game thread, FrameBegin) -------------------------------------

/// puppet.rs, every frame: our player and the other players (their native remotes).
pub fn tick(player: Option<Chr>, peers: &PeerSet) {
    let on = tuning::sync_art_effects();
    if on && !INSTALL_TRIED.swap(true, Ordering::Relaxed) {
        let ok = install();
        INSTALLED.store(ok, Ordering::Relaxed);
    }
    let active = on && INSTALLED.load(Ordering::Relaxed);
    if ACTIVE.swap(active, Ordering::Relaxed) != active {
        log::line(&format!("artfx: friend's bullet effects {}", if active { "on" } else { "off" }));
    }
    // Damage-free copies come from the game's own table (module doc): only checked.
    if active && !TABLE_CHECKED.swap(true, Ordering::Relaxed) {
        check_damage_table();
    }

    let handle = |c: Chr| read::<u32>(c.0 + 8).filter(|&h| h != NONE && h != 0);
    OWN_HANDLE.store(player.and_then(handle).unwrap_or(NONE), Ordering::Relaxed);
    for i in 0..MAX_PLAYERS {
        let remote = PeerId::new(i as u8).and_then(|id| peers.by_id(id)).and_then(|p| p.native);
        match remote.and_then(handle) {
            Some(h) => {
                REMOTE_HANDLE[i].store(h, Ordering::Relaxed);
                REMOTE_AT_MS[i].store(now_ms(), Ordering::Relaxed);
            }
            None => {
                // That peer's remote went: only its queued messages go with it.
                if REMOTE_HANDLE[i].swap(NONE, Ordering::Relaxed) != NONE
                    && let Ok(mut q) = INBOX.lock()
                {
                    q.retain(|(from, _)| from.index() != i);
                }
            }
        }
    }
    if !active && let Ok(mut q) = INBOX.lock() {
        q.clear();
    }
    log_stats();
}

/// net.rs: messages for the peers since the last call. `forward` = at least one peer
/// shows us as a native remote (their snapshots ask for raw packets), so it has a chr
/// to own our bullets; net.rs sends to the peers in its raw-forward mask only.
pub fn take_outbound(forward: bool) -> Vec<Vec<u8>> {
    FORWARD.store(forward, Ordering::Relaxed);
    let Ok(mut q) = OUTBOX.lock() else { return Vec::new() };
    if !forward {
        q.clear();
        return Vec::new();
    }
    q.drain(..).collect()
}

/// net.rs: a PlayerRaw packet from `from`. True if it was ours (then native.rs never
/// sees it). Served on `from`'s native remote.
pub fn on_raw(from: PeerId, ptype: u32, data: &[u8]) -> bool {
    if ptype != PTYPE {
        return false;
    }
    C.recv_msgs.fetch_add(1, Ordering::Relaxed);
    if !ACTIVE.load(Ordering::Relaxed) {
        return true;
    }
    let Some((_, n)) = parse_message(data) else {
        C.recv_bad.fetch_add(1, Ordering::Relaxed);
        return true;
    };
    if !remote_fresh(from.index()) {
        C.no_remote.fetch_add(n as u32, Ordering::Relaxed);
        return true;
    }
    let Ok(mut q) = INBOX.lock() else { return true };
    if q.len() >= MAX_INBOX {
        q.pop_front();
        C.inbox_full.fetch_add(1, Ordering::Relaxed);
    }
    q.push_back((from, data[HEADER..].to_vec()));
    true
}

/// `u32 owner, u8 n (1..=PER_MSG), n records`, every record owned by `owner`.
fn parse_message(data: &[u8]) -> Option<(u32, usize)> {
    if data.len() < HEADER + 1 {
        return None;
    }
    let owner = u32::from_le_bytes(data[..4].try_into().ok()?);
    let n = data[HEADER] as usize;
    if n == 0 || n > PER_MSG || data.len() != HEADER + 1 + n * REC || owner == NONE || owner >> 28 != 1 {
        return None;
    }
    let all_owned = (0..n).all(|i| {
        let o = HEADER + 1 + i * REC + REC_OWNER;
        data[o..o + 4] == owner.to_le_bytes()
    });
    all_owned.then_some((owner, n))
}

/// Peer index `i` has a native remote that `tick` saw within `HANDLE_FRESH_MS`.
fn remote_fresh(i: usize) -> bool {
    i < MAX_PLAYERS
        && REMOTE_HANDLE[i].load(Ordering::Relaxed) != NONE
        && now_ms().saturating_sub(REMOTE_AT_MS[i].load(Ordering::Relaxed)) < HANDLE_FRESH_MS
}

/// The game's network form of a chr handle (`0xE8F040`), or None.
fn net_owner(handle: u32) -> Option<u32> {
    if handle == NONE {
        return None;
    }
    let f: extern "C" fn(*mut i32, i32) -> *mut i32 = unsafe { std::mem::transmute(module_base() + TO_NET_OWNER_RVA) };
    let mut out: i32 = -1;
    f(&mut out, handle as i32);
    let v = out as u32;
    (v != NONE).then_some(v)
}

// --- Install ---------------------------------------------------------------

/// Checks that `site` is `call rel32` to `base + target` and returns the rel32 bytes.
fn call_rel(site: usize, target: usize) -> Option<[u8; 4]> {
    let base = module_base();
    let op = read::<u8>(base + site)?;
    let rel = read::<[u8; 4]>(base + site + 1)?;
    let to = (base + site + 5) as i64 + i32::from_le_bytes(rel) as i64;
    (op == 0xE8 && to == (base + target) as i64).then_some(rel)
}

fn install() -> bool {
    let base = module_base();
    let sites: [(usize, usize, usize, &str); 5] = [
        (SITE_BCAST, FN_BCAST, bcast_hook as *const () as usize, "artfx bullet broadcast"),
        (SITE_MEMBERS, FN_MEMBERS, members_hook as *const () as usize, "artfx bullet members"),
        (SITE_RECV_A, FN_RECV, recv_hook as *const () as usize, "artfx bullet recv 1"),
        (SITE_RECV_B, FN_RECV, recv_hook as *const () as usize, "artfx bullet recv 2"),
        (SITE_CREATE, FN_CREATE, create_hook as *const () as usize, "artfx bullet create"),
    ];
    // Check every site first: all or nothing.
    let mut rels = Vec::with_capacity(sites.len());
    for &(site, target, _, name) in &sites {
        match call_rel(site, target) {
            Some(r) => rels.push(r),
            None => {
                log::line(&format!("artfx: {name} at {:#x} is not call {:#x}, art effects off", base + site, base + target));
                return false;
            }
        }
    }
    let Some(page) = hook::alloc_near_pub(base + SITE_BCAST) else {
        log::line("artfx: no memory near the module for the bullet hooks, art effects off");
        return false;
    };
    ORIG_BCAST.store(base + FN_BCAST, Ordering::Relaxed);
    ORIG_MEMBERS.store(base + FN_MEMBERS, Ordering::Relaxed);
    ORIG_RECV.store(base + FN_RECV, Ordering::Relaxed);
    ORIG_CREATE.store(base + FN_CREATE, Ordering::Relaxed);
    let member = FAKE_MEMBER.as_ptr() as usize;
    FAKE_VEC[1].store(member, Ordering::Relaxed);
    FAKE_VEC[2].store(member + 56, Ordering::Relaxed);

    for (i, (&(site, _, hook_fn, name), rel)) in sites.iter().zip(&rels).enumerate() {
        // Stub: mov rax, hook_fn; jmp rax (16 bytes apart, code only on this page).
        let stub = page + i * 16;
        let mut code = [0u8; 12];
        code[0] = 0x48;
        code[1] = 0xB8;
        code[2..10].copy_from_slice(&(hook_fn as u64).to_le_bytes());
        code[10] = 0xFF;
        code[11] = 0xE0;
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
        let Ok(new_rel) = i32::try_from(stub as i64 - (base + site + 5) as i64) else {
            log::line(&format!("artfx: stub {stub:#x} out of rel32 range for {name}"));
            return false;
        };
        if !hook::patch_code(site + 1, rel, &new_rel.to_le_bytes(), name) {
            // Earlier sites stay redirected; their hooks pass straight through
            // to the originals while ACTIVE is false.
            return false;
        }
    }
    log::line(&format!(
        "artfx: bullet sync hooks installed in 0xB1D600 (broadcast, members, recv x2, create), stubs {page:#x}"
    ));
    true
}

/// Remote-player attacker vs. NPC/enemy victims must read action 0 (reaction
/// only). Read-only: logs the entries once, so a changed exe shows up in the log.
fn check_damage_table() {
    let base = module_base();
    let mut vals = Vec::with_capacity(8);
    for table in DMG_TABLES {
        for victim in VICTIMS {
            vals.push(read::<u32>(base + table + (victim * DMG_COLS + ATTACKER_REMOTE) * 4).unwrap_or(u32::MAX));
        }
    }
    let vanilla = vals.iter().all(|&v| v == ACTION_REACTION_ONLY);
    log::line(&format!(
        "artfx: damage table [3..6][2] = {vals:?}: {}",
        if vanilla {
            "remote-player hits on NPCs/enemies are reaction-only (vanilla, not patched)"
        } else {
            "NOT vanilla, the friend's copies may deal local damage"
        }
    ));
}

/// lib.rs, when the mod disables itself: every hook passes straight through.
pub fn shutdown() {
    ACTIVE.store(false, Ordering::Relaxed);
    FORWARD.store(false, Ordering::Relaxed);
    PASS_SERVING.store(false, Ordering::Relaxed);
    for o in &PASS_OWNER {
        o.store(NONE, Ordering::Relaxed);
    }
    if let Ok(mut q) = INBOX.lock() {
        q.clear();
    }
    if let Ok(mut q) = OUTBOX.lock() {
        q.clear();
    }
}

// --- Hooks (game thread, inside the BulletManager update) ---------------------
//
// Arity re-checked in IDA (review 2026-10-23, `F:\Temp\ida_fix` o1/o2): `0x8E7210`
// takes 4 register args (the site sets rcx/edx/r8/r9d only), `0x8E6B50` 5 (the 5th,
// cap 0x19000, at [rsp+20h] of the caller), `0xB1A960` 3, `0x8E4010` none (rcx
// ignored). The type-63 loop `0xB1D748..0xB1D7D0` reads only the vector's
// `+8`/`+0x10` and passes the member pointer on: no field of FAKE_MEMBER is read.

/// `0x8E7210(mgr, type, data, size) -> u8` at the type-63 broadcast.
extern "C" fn bcast_hook(mgr: usize, ptype: u32, data: *const u8, size: u32) -> u8 {
    if ptype == BULLET_SYNC_TYPE && ACTIVE.load(Ordering::Relaxed) && FORWARD.load(Ordering::Relaxed) {
        let _ = std::panic::catch_unwind(|| capture(data as usize, size as usize));
    }
    let f: extern "C" fn(usize, u32, *const u8, u32) -> u8 =
        unsafe { std::mem::transmute(ORIG_BCAST.load(Ordering::Relaxed)) };
    f(mgr, ptype, data, size)
}

fn capture(data: usize, size: usize) {
    let mut count = [0u8; 1];
    if size < 1 || !read_bytes(data, &mut count) {
        return;
    }
    let n = count[0] as usize;
    if n == 0 || size < 1 + n * REC {
        return;
    }
    let mut blob = vec![0u8; n * REC];
    if !read_bytes(data + 1, &mut blob) {
        return;
    }
    let Some(own) = net_owner(OWN_HANDLE.load(Ordering::Relaxed)) else { return };
    // Only our player's bullets: enemy bullets exist on both machines anyway.
    let mine: Vec<&[u8]> = blob.chunks_exact(REC).filter(|r| r[REC_OWNER..REC_OWNER + 4] == own.to_le_bytes()).collect();
    C.foreign_skipped.fetch_add((n - mine.len()) as u32, Ordering::Relaxed);
    if mine.is_empty() {
        return;
    }
    let Ok(mut q) = OUTBOX.lock() else { return };
    for part in mine.chunks(PER_MSG) {
        if q.len() >= MAX_OUTBOX {
            C.outbox_full.fetch_add(part.len() as u32, Ordering::Relaxed);
            continue;
        }
        let mut msg = Vec::with_capacity(HEADER + 1 + part.len() * REC);
        msg.extend_from_slice(&own.to_le_bytes());
        msg.push(part.len() as u8);
        for r in part {
            msg.extend_from_slice(r);
        }
        debug_assert!(msg.len() <= MAX_PLAYER_RAW);
        q.push_back(msg);
        C.sent_msgs.fetch_add(1, Ordering::Relaxed);
        C.sent_bullets.fetch_add(part.len() as u32, Ordering::Relaxed);
    }
    if C.send_logs.fetch_add(1, Ordering::Relaxed) < DETAIL_LOGS {
        let ids: Vec<i32> = mine.iter().map(|r| i32::from_le_bytes(r[REC_BULLET_ID..REC_BULLET_ID + 4].try_into().unwrap_or([0xFF; 4]))).collect();
        log::line(&format!("artfx: sending {} of our bullets to the friend, bullet ids {ids:?} (owner {own:#x})", mine.len()));
    }
}

/// `0x8E4010(mgr) -> member vector` at the type-63 receive loop.
extern "C" fn members_hook(mgr: usize) -> usize {
    let f: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(ORIG_MEMBERS.load(Ordering::Relaxed)) };
    let v = f(mgr);
    PASS_SERVING.store(false, Ordering::Relaxed);
    for o in &PASS_OWNER {
        o.store(NONE, Ordering::Relaxed);
    }
    if !ACTIVE.load(Ordering::Relaxed) {
        return v;
    }
    std::panic::catch_unwind(|| {
        let Ok(mut q) = INBOX.try_lock() else { return v };
        if q.is_empty() {
            return v;
        }
        // A real session member list (never offline) is left alone.
        let real_empty = v == 0 || read::<usize>(v + 8) == read::<usize>(v + 0x10);
        if !real_empty {
            return v;
        }
        // The owner of every sender with queued messages; the others' messages go.
        let mut owners = [NONE; MAX_PLAYERS];
        for (i, o) in owners.iter_mut().enumerate() {
            if q.iter().any(|(from, _)| from.index() == i) && remote_fresh(i) {
                *o = net_owner(REMOTE_HANDLE[i].load(Ordering::Relaxed)).unwrap_or(NONE);
            }
        }
        let before = q.len();
        q.retain(|(from, _)| owners.get(from.index()).is_some_and(|&o| o != NONE));
        C.no_remote.fetch_add((before - q.len()) as u32, Ordering::Relaxed);
        if q.is_empty() {
            return v;
        }
        for (slot, o) in PASS_OWNER.iter().zip(owners) {
            slot.store(o, Ordering::Relaxed);
        }
        PASS_SERVING.store(true, Ordering::Relaxed);
        FAKE_VEC.as_ptr() as usize
    })
    .unwrap_or(v)
}

/// `0x8E6B50(mgr, member, type, buf, cap) -> i32` in the type-63 receive loop.
extern "C" fn recv_hook(mgr: usize, member: usize, ptype: u32, buf: *mut u8, cap: u32) -> i32 {
    if member == FAKE_MEMBER.as_ptr() as usize {
        return std::panic::catch_unwind(|| serve(ptype, buf as usize, cap as usize)).unwrap_or(0);
    }
    let f: extern "C" fn(usize, usize, u32, *mut u8, u32) -> i32 =
        unsafe { std::mem::transmute(ORIG_RECV.load(Ordering::Relaxed)) };
    f(mgr, member, ptype, buf, cap)
}

/// Copies one queued message into the game's buffer, owner = its sender's native remote.
fn serve(ptype: u32, buf: usize, cap: usize) -> i32 {
    if ptype != BULLET_SYNC_TYPE || !PASS_SERVING.load(Ordering::Relaxed) || buf < 0x10000 {
        return 0;
    }
    let Ok(mut q) = INBOX.try_lock() else { return 0 };
    let owner_of =
        |from: PeerId| PASS_OWNER.get(from.index()).map(|o| o.load(Ordering::Relaxed)).filter(|&o| o != NONE);
    while let Some((from, mut msg)) = q.pop_front() {
        if msg.len() > cap {
            C.too_big.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let Some(owner) = owner_of(from) else {
            // Queued after members_hook looked: no owner known for this pass.
            C.no_remote.fetch_add(((msg.len() - 1) / REC) as u32, Ordering::Relaxed);
            continue;
        };
        rewrite_records(&mut msg, owner);
        unsafe { std::ptr::copy_nonoverlapping(msg.as_ptr(), buf as *mut u8, msg.len()) };
        C.served.fetch_add(((msg.len() - 1) / REC) as u32, Ordering::Relaxed);
        return msg.len() as i32;
    }
    0
}

/// `u8 n, n records`: every record's owner becomes `owner` and its target is cleared
/// (the sender's lock-on netId means another chr here).
fn rewrite_records(msg: &mut [u8], owner: u32) {
    for r in msg[1..].chunks_exact_mut(REC) {
        r[REC_OWNER..REC_OWNER + 4].copy_from_slice(&owner.to_le_bytes());
        r[REC_TARGET_NETID..REC_TARGET_NETID + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());
        r[REC_TARGET_LOCK] = 0;
    }
}

/// `0xB1A960(bulletMan, int* out, param) -> out`: counts the bullets made from our records.
extern "C" fn create_hook(bm: usize, out: *mut i32, param: usize) -> *mut i32 {
    let f: extern "C" fn(usize, *mut i32, usize) -> *mut i32 =
        unsafe { std::mem::transmute(ORIG_CREATE.load(Ordering::Relaxed)) };
    let r = f(bm, out, param);
    if PASS_SERVING.load(Ordering::Relaxed) {
        let _ = std::panic::catch_unwind(|| {
            let handle = if r.is_null() { -1 } else { unsafe { r.read() } };
            if handle == -1 {
                C.create_failed.fetch_add(1, Ordering::Relaxed);
            } else {
                C.created.fetch_add(1, Ordering::Relaxed);
            }
            if C.create_logs.fetch_add(1, Ordering::Relaxed) < DETAIL_LOGS {
                let id = read::<i32>(param + 4).unwrap_or(-1);
                log::line(&format!("artfx: friend's bullet {id} created on the native remote, handle {handle:#x}"));
            }
        });
    }
    r
}

fn log_stats() {
    let Ok(mut last) = LAST_STATS.lock() else { return };
    let now = Instant::now();
    let Some(t) = *last else {
        *last = Some(now);
        return;
    };
    if now.duration_since(t) < STATS_EVERY {
        return;
    }
    *last = Some(now);
    let take = |a: &AtomicU32| a.swap(0, Ordering::Relaxed);
    let (sm, sb, fs, of) = (take(&C.sent_msgs), take(&C.sent_bullets), take(&C.foreign_skipped), take(&C.outbox_full));
    let (rm, rb, sv, cr, cf) = (take(&C.recv_msgs), take(&C.recv_bad), take(&C.served), take(&C.created), take(&C.create_failed));
    let (nr, tb, inf) = (take(&C.no_remote), take(&C.too_big), take(&C.inbox_full));
    if sm + fs + of + rm + rb + sv + cr + cf + nr + tb + inf == 0 {
        return;
    }
    log::line(&format!(
        "artfx stats (20 s): sent {sb} bullets in {sm} msgs (other owners skipped {fs}, outbox full {of}); \
         received {rm} msgs (bad {rb}), served {sv}, created {cr}, failed {cf}; dropped: no remote {nr}, too big {tb}, inbox full {inf}"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(owner: u32, n: usize) -> Vec<u8> {
        let mut m = owner.to_le_bytes().to_vec();
        m.push(n as u8);
        for _ in 0..n {
            let mut r = vec![0u8; REC];
            r[..4].copy_from_slice(&owner.to_le_bytes());
            m.extend_from_slice(&r);
        }
        m
    }

    #[test]
    fn messages_fit_a_player_raw_packet() {
        assert!(HEADER + 1 + PER_MSG * REC <= MAX_PLAYER_RAW);
    }

    #[test]
    fn parse_accepts_only_whole_player_owned_messages() {
        let owner = 0x1004_C001;
        assert_eq!(parse_message(&message(owner, 1)), Some((owner, 1)));
        assert_eq!(parse_message(&message(owner, PER_MSG)), Some((owner, PER_MSG)));
        assert_eq!(parse_message(&message(owner, 0)), None);
        assert_eq!(parse_message(&message(owner, PER_MSG + 1)), None);
        // Not a player handle (enemy group) and "no owner".
        assert_eq!(parse_message(&message(0x2010_0001, 1)), None);
        assert_eq!(parse_message(&message(NONE, 1)), None);
        let mut short = message(owner, 2);
        short.pop();
        assert_eq!(parse_message(&short), None);
        // A record owned by someone else.
        let mut mixed = message(owner, 2);
        mixed[HEADER + 1 + REC] ^= 1;
        assert_eq!(parse_message(&mixed), None);
    }

    #[test]
    fn records_get_their_senders_owner_and_lose_the_target() {
        let mut msg = vec![2u8];
        for _ in 0..2 {
            let mut r = vec![0u8; REC];
            r[..4].copy_from_slice(&0x1004_C001u32.to_le_bytes());
            r[REC_TARGET_NETID..REC_TARGET_NETID + 2].copy_from_slice(&0xA001u16.to_le_bytes());
            r[REC_TARGET_LOCK] = 1;
            r[REC_BULLET_ID] = 0x55;
            msg.extend_from_slice(&r);
        }
        rewrite_records(&mut msg, 0x1004_C003);
        for r in msg[1..].chunks_exact(REC) {
            assert_eq!(r[..4], 0x1004_C003u32.to_le_bytes());
            assert_eq!(r[REC_TARGET_NETID..REC_TARGET_NETID + 2], [0xFF, 0xFF]);
            assert_eq!(r[REC_TARGET_LOCK], 0);
            assert_eq!(r[REC_BULLET_ID], 0x55);
        }
    }

    #[test]
    fn damage_table_entries_are_remote_attacker_column() {
        // [victim][attacker], u32: victim 5 (enemy owned here) x attacker 2 (remote) = byte 148.
        assert_eq!((5 * DMG_COLS + ATTACKER_REMOTE) * 4, 148);
    }
}
