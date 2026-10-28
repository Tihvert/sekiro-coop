//! Native remote player (RE-FINDINGS-2 § Native remote player, pass 4).
//!
//! Instead of our ReplayGhostIns puppet, the friend is a plain `PlayerIns`
//! with the game's own `NetworkManipulator` (manipulator type 2), created
//! the way the developers' `TestNetStep` "Create LocalRemotePlayer" button
//! does it (`0xA796B0`, mode 1: separate PlayerGameData). The remote reads
//! one packet per few frames through the small `PlayerSession` interface at
//! `chr+0x2038`; the local player's PadManipulator writes into its own.
//!
//! Two uses, both on our FrameBegin task:
//! - **N0 (one machine)**: the clone keeps the `PlayerDebugSession` the
//!   game gives it, so it replays the local player through the game's
//!   loopback queue `*(base+0x3D93618)` one frame late. Only a counting tee
//!   on the local player's session is added (forwards nothing).
//! - **N1 (network)**: the clone's `chr+0x2038` points to our 8-byte session
//!   whose vtable is a copy of PlayerDebugSession's with slot 6 (`recv`)
//!   serving the friend's packets by type; the local player's session is a
//!   copy whose slot 1 (and slot 3, which pushes type 37 itself) tees every
//!   packet into an outbox for UDP and then calls the original.
//!
//! Multi-player (PLAN-MULTIPLAYER P3): one Net remote per connected peer (host + up to
//! 5: playerNo 1..5, PGD slots 0..4), each with its own 8-byte recv session object (one
//! shared vtable copy; `recv_slot` tells them apart by `this`) and its own raw inbox
//! keyed by the sender's slot. Creations for different peers are at least 3 s apart;
//! a peer that leaves loses exactly its own remote, inbox and position pin.
//!
//! Removal is the game's general chr delete `0xA4BF90(WorldChrMan, chr)`
//! (also what FrpgSessionManager uses for a leaving member, `0x8E5B70`),
//! then `0x7B1F80(idx)` frees the PlayerGameData slot.
//!
//! Every RVA below was re-read in IDA on a private copy (re7db, out120-127,
//! 2026-10-22). Nothing here has run in the game yet.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use coop_net::MAX_PLAYER_RAW;

use crate::game::{Chr, module_base, read, read_ptr, write};
use crate::peers::{MAX_PLAYERS, PeerId, PeerSet};
use crate::{hook, log, rtti};

// --- Game functions (RVAs) -------------------------------------------------
const GAME_DATA_MAN_RVA: usize = 0x3D5AAC0;
const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const DEBUG_QUEUE_RVA: usize = 0x3D93618;
/// `() -> i32`: first free of the 5 remote PGD slots, -1 if none.
const PGD_FREE_SLOT_RVA: usize = 0x7B2230;
/// `(i32 idx)`: marks the slot used.
const PGD_MARK_USED_RVA: usize = 0x7B4450;
/// `(u32 idx) -> PlayerGameData*`.
const PGD_GET_RVA: usize = 0x7B2980;
/// `(u32 idx)`: frees the slot (`+0x10 = -1`, in-use byte 0).
const PGD_RELEASE_RVA: usize = 0x7B1F80;
/// `(ParamRef* out, i32 id)`: CharaInitParam (#21) row lookup.
const CHARA_INIT_LOOKUP_RVA: usize = 0x10B8240;
/// `(GameDataMan, pgd, ParamRef*, u8 0)` and `(GameDataMan, pgd, ParamRef*)`.
const PGD_FILL_A_RVA: usize = 0x7B7470;
const PGD_FILL_B_RVA: usize = 0x7B7120;
/// `(init* 40 B, u32 playerNo) -> init*`: `{playerNo, manipType 2, pgd,
/// member 0, MenuMan+0x3238, 0}`.
const PLAYER_INIT_RVA: usize = 0x7F86F0;
/// `(WorldChrMan, u32 chrType, init*) -> PlayerIns*` (player chr set).
const CREATE_PLAYER_RVA: usize = 0xA49460;
const SET_CHR_TYPE_RVA: usize = 0x9F5170;
/// `(chr)`: what the dev test calls with "warp" after setting `+0x1A11 |= 8`.
/// `(WorldChrMan, chr)`: removes the chr from every set and hands it to the
/// delay deleter.
const DELETE_CHR_RVA: usize = 0xA4BF90;
/// `(chr) -> u8`: the "driven by a remote packet" test.
const IS_REMOTE_RVA: usize = 0x9F36C0;
const DEBUG_SESSION_VT_RVA: usize = 0x2B86048;
const PLAYER_INS_VT_RVA: usize = 0x2A2B338;

// --- Layout -----------------------------------------------------------------
const GDM_LOCAL_PGD: usize = 0x08;
const GDM_REMOTE_PGDS: usize = 0x10;
const PGD_PLAYER_NO: usize = 0x10;
const PGD_STRIDE: usize = 0xA10;
const PGD_SLOTS: u32 = 5;
const WCM_PLAYER_SET_CAP: usize = 0x40;
const WCM_PLAYER_SET: usize = 0x48;
const SET_ENTRY: usize = 56;
const CHR_MODEL: usize = 0x48;
const CHR_MANIP: usize = 0x58;
const CHR_TEAM: usize = 0x74;
const CHR_FLAGS_1A11: usize = 0x1A11;
const CHR_MODULES: usize = 0x1FF8;
const CHR_PGD: usize = 0x2000;
const CHR_SESSION: usize = 0x2038;
const MOD_BEHAVIOR_SYNC: usize = 0x30;
const Q_WRITE_IDX: usize = 0x414;
const Q_DELAY: usize = 0x418;
const Q_SUBSCRIBERS: usize = 0x420;

const SESSION_SLOTS: usize = 12;
const SLOT_SEND: usize = 1;
const SLOT_SEND_37: usize = 3;
const SLOT_RECV: usize = 6;
const PLAYER_INS_SLOTS: usize = 253;
const SLOT_INIT_18: usize = 18;
const SLOT_PHANTOM_ROW: usize = 130;
const MANIP_SLOT_TYPE: usize = 22;

/// Chr type the dev test uses (white phantom).
const CHR_TYPE_REMOTE: u32 = 1;
/// First template of the dev test's table at `0x2A41340` (1080100..1080104).
const CHARA_INIT_ROW: i32 = 1_080_100;
/// Fallback scan range when that row is missing from the loaded param.
const CHARA_INIT_SCAN_MAX: i32 = 200_000;

/// Type of the game's full player frame (pos, rot, HP, sync states, vars).
const PTYPE_FRAME: u32 = 1;
/// The player's SpEffect list entries (`0x8DAE90`, receiver `0xA32790`).
const PTYPE_SPEFFECT_LIST: u32 = 61;
/// Type-1 frames are released at the sender's cadence (every 5 frames at
/// 60 fps): two frames handed over in consecutive frames would make the
/// NetworkManipulator see a ~17 ms interval and snap instead of blend.
/// More queued frames of one type than this: drop the oldest (latency).
const MAX_QUEUED_PER_TYPE: usize = 10;
const MAX_INBOX: usize = 64;
const MAX_OUTBOX: usize = 64;
const STATS_EVERY: Duration = Duration::from_secs(10);
/// The world must have settled this long after a load before we delete a
/// remote that survived it.
const STALE_CLEANUP_AFTER: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// N0: the game's own PlayerDebugSession loopback (mirrors us).
    Loopback,
    /// N1: our session, fed with the friend's packets.
    Net,
}

#[derive(Clone, Copy, Debug)]
pub struct Remote {
    pub chr: Chr,
    pub kind: Kind,
    pgd_idx: u32,
    pgd: usize,
    player_no: u32,
    since: Instant,
    late_logged: bool,
    /// Seen in the player chr set at least once (else "vanished" waits 2 s).
    seen_in_set: bool,
    /// Our equipment goes on at this time (native_dress_delay_ms), not right after the
    /// factory: the driver crash came 1-7 s after create + dress (gpuguard.rs).
    dress_due: Option<Instant>,
    /// The peer a Net remote stands for (None for N0 clones).
    peer: Option<PeerId>,
    /// Last seen look (v8.53): model ins (ChrIns+0x48) and its draw group (model+0x23). A change
    /// is logged at once ("my model is not visible" left no trace in the log, 2026-10-27).
    look: Option<(Option<usize>, Option<u8>)>,
}

// --- Shared with the game's calls into our vtables (any thread) ------------

/// Bumped once per FrameBegin; recv hands out one packet per type per frame.
static FRAME: AtomicU64 = AtomicU64::new(0);
/// Bit = slot of a peer that asked for our packets (their snapshots' `want_raw`).
static FORWARD: AtomicU8 = AtomicU8::new(0);
/// Lock-free chr -> peer map for hooks (`peer_of_chr`): the native remote chr of each
/// peer, by `PeerId::index`, 0 = none. Rewritten by `Native::publish_peers`.
static PEER_CHR: [AtomicUsize; MAX_PLAYERS] = [const { AtomicUsize::new(0) }; MAX_PLAYERS];
/// Bit = peer index whose Net remote (with our recv session) exists: keep its packets.
static RECV_ACTIVE: AtomicU8 = AtomicU8::new(0);
/// The remote recv vtable copy (one for every peer) and one leaked 8-byte session object
/// per peer index pointing at it: `recv_slot` tells the peers apart by `this`. Built on the
/// first spawn for that peer and reused for the life of the process.
static RECV_VT: AtomicUsize = AtomicUsize::new(0);
static RECV_OBJS: [AtomicUsize; MAX_PLAYERS] = [const { AtomicUsize::new(0) }; MAX_PLAYERS];
/// `recv` calls whose `this` was none of our objects (stats; served by the single active
/// peer if exactly one is active, else nothing).
static RECV_UNMAPPED: AtomicU64 = AtomicU64::new(0);
/// Our leaked local send tee object (8 bytes: the vptr).
static TEE_OBJ: AtomicUsize = AtomicUsize::new(0);
/// The local player's own session object and its original slots 1 / 3.
static TEE_ORIG_OBJ: AtomicUsize = AtomicUsize::new(0);
static TEE_ORIG_SEND: AtomicUsize = AtomicUsize::new(0);
static TEE_ORIG_SEND37: AtomicUsize = AtomicUsize::new(0);
/// Shared PlayerIns vtable copy with slot 130 = our phantom-row answer.
static SOLID_VT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Inbox {
    packets: VecDeque<(u32, Vec<u8>)>,
    served_frame: HashMap<u32, u64>,
    frame_released_at: Option<Instant>,
    served: u64,
    dropped_old: u64,
    dropped_big: u64,
    received: u64,
    /// Type-1 arrivals: last time and the smoothed interval (ms, 0 = unknown).
    last_frame_at: Option<Instant>,
    frame_interval_ms: f32,
}

#[derive(Default)]
struct Outbox {
    packets: VecDeque<(u32, Vec<u8>)>,
    dropped: u64,
    too_big: u64,
}

#[derive(Default)]
struct TeeStats {
    /// type -> (packets, bytes) since the last stats line.
    per_type: HashMap<u32, (u64, u64)>,
}

/// One raw inbox per peer index (the packets its native remote plays out).
static INBOXES: [Mutex<Option<Inbox>>; MAX_PLAYERS] = [const { Mutex::new(None) }; MAX_PLAYERS];
static OUTBOX: Mutex<Option<Outbox>> = Mutex::new(None);
static TEE_STATS: Mutex<Option<TeeStats>> = Mutex::new(None);

/// Once per FrameBegin, before anything else.
pub fn begin_frame() {
    FRAME.fetch_add(1, Ordering::Relaxed);
}

/// net.rs: the slots (bit = slot) that want our player's packets; 0 = tee forwards nothing.
pub fn set_forward(mask: u8) {
    FORWARD.store(mask, Ordering::Relaxed);
}

fn forwarding() -> bool {
    FORWARD.load(Ordering::Relaxed) != 0
}

/// Lock-free, hook-safe (any game thread): the peer whose native remote `chr` is.
#[allow(dead_code)] // P5 (nsession LAST23 mapping)
pub fn peer_of_chr(chr: usize) -> Option<PeerId> {
    if chr == 0 {
        return None;
    }
    PEER_CHR.iter().position(|a| a.load(Ordering::Acquire) == chr).and_then(|i| PeerId::new(i as u8))
}

fn recv_active(i: usize) -> bool {
    i < MAX_PLAYERS && RECV_ACTIVE.load(Ordering::Relaxed) & (1 << i) != 0
}

fn set_recv_active(i: usize, on: bool) {
    if i >= MAX_PLAYERS {
        return;
    }
    if on {
        RECV_ACTIVE.fetch_or(1 << i, Ordering::Relaxed);
    } else {
        RECV_ACTIVE.fetch_and(!(1 << i), Ordering::Relaxed);
    }
}

/// net.rs: one raw packet from peer `from`, queued for that peer's native remote only.
/// Dropped while `from` has no remote with our recv session.
pub fn push_inbound(from: PeerId, ptype: u32, data: Vec<u8>) {
    let i = from.index();
    if !recv_active(i) || data.is_empty() {
        return;
    }
    let Ok(mut g) = INBOXES[i].lock() else { return };
    let ib = g.get_or_insert_with(Inbox::default);
    ib.received += 1;
    if ptype == PTYPE_FRAME {
        let now = Instant::now();
        if let Some(t) = ib.last_frame_at {
            let ms = (now.duration_since(t).as_secs_f32() * 1000.0).clamp(5.0, 300.0);
            ib.frame_interval_ms = if ib.frame_interval_ms <= 0.0 { ms } else { ib.frame_interval_ms * 0.95 + ms * 0.05 };
        }
        ib.last_frame_at = Some(now);
    }
    if ib.packets.iter().filter(|(t, _)| *t == ptype).count() >= MAX_QUEUED_PER_TYPE
        && let Some(i) = ib.packets.iter().position(|(t, _)| *t == ptype)
    {
        ib.packets.remove(i);
        ib.dropped_old += 1;
    }
    if ib.packets.len() >= MAX_INBOX {
        ib.packets.pop_front();
        ib.dropped_old += 1;
    }
    ib.packets.push_back((ptype, data));
}

fn clear_inbox(i: usize) {
    if let Some(Ok(mut g)) = INBOXES.get(i).map(Mutex::lock) {
        *g = None;
    }
}

fn clear_inboxes() {
    for i in 0..MAX_PLAYERS {
        clear_inbox(i);
    }
}

/// net.rs: everything our player's session sent since the last call.
pub fn take_outbound() -> Vec<(u32, Vec<u8>)> {
    let Ok(mut g) = OUTBOX.lock() else { return Vec::new() };
    g.as_mut().map(|o| o.packets.drain(..).collect()).unwrap_or_default()
}

#[repr(C)]
struct RecvOut {
    ptr: usize,
    cap: u32,
    _pad: u32,
    size: u64,
}

#[repr(C)]
struct RecvBuf {
    ptr: usize,
    cap: u32,
    _pad: u32,
}

/// Picks the packet `recv(type)` hands out now from peer index `i`'s inbox, if any.
fn next_packet(i: usize, ptype: u32, cap: usize) -> Option<Vec<u8>> {
    let mut g = INBOXES.get(i)?.lock().ok()?;
    let ib = g.as_mut()?;
    let frame = FRAME.load(Ordering::Relaxed);
    if ib.served_frame.get(&ptype) == Some(&frame) {
        return None;
    }
    let queued = ib.packets.iter().filter(|(t, _)| *t == ptype).count();
    if queued == 0 {
        return None;
    }
    let now = Instant::now();
    // Steady play-out at the sender's cadence (12 frames/s = 83 ms, measured
    // live), keeping about two frames buffered: the manipulator blends over the
    // interval it sees, so uneven release (fixed 60 ms, then stalls) made the
    // friend move in jumps and cut corners through walls (live 2026-10-23).
    // A deeper queue (burst, hitch) plays out a little faster instead of
    // dropping frames.
    // The sender's cadence is measured (83 ms at the game's default, ~33 ms
    // with our send-rate patch); depth stretches or shortens it a little.
    let nominal = if ib.frame_interval_ms > 0.0 { ib.frame_interval_ms.clamp(15.0, 120.0) } else { 83.0 };
    let factor = match queued {
        0..=1 => 1.08,
        2 => 1.0,
        3..=4 => 0.85,
        _ => 0.55,
    };
    let spacing = Duration::from_secs_f32(nominal * factor / 1000.0);
    if ptype == PTYPE_FRAME && ib.frame_released_at.is_some_and(|t| now.duration_since(t) < spacing) {
        return None;
    }
    let i = ib.packets.iter().position(|(t, _)| *t == ptype)?;
    let (_, data) = ib.packets.remove(i)?;
    if data.len() > cap {
        ib.dropped_big += 1;
        return None;
    }
    ib.served_frame.insert(ptype, frame);
    if ptype == PTYPE_FRAME {
        ib.frame_released_at = Some(now);
    }
    ib.served += 1;
    Some(data)
}

/// The peer index whose recv session object `this` is. Not one of ours: the single active
/// peer if exactly one is active (the 2-player case can never be served wrong), else None.
fn recv_peer_index(this: usize) -> Option<usize> {
    if this != 0
        && let Some(i) = RECV_OBJS.iter().position(|a| a.load(Ordering::Acquire) == this)
    {
        return Some(i);
    }
    RECV_UNMAPPED.fetch_add(1, Ordering::Relaxed);
    let active = RECV_ACTIVE.load(Ordering::Relaxed);
    (active.count_ones() == 1).then(|| active.trailing_zeros() as usize)
}

/// Our PlayerSession slot 6 for the remote: `recv(this, out, buf, type) ->
/// out`. Called by the NetworkManipulator update (`0x8D8050`) in a loop
/// until it returns nothing. `this` = the remote's session object (`chr+0x2038`),
/// one per peer. Never panics into the game.
extern "C" fn recv_slot(this: usize, out: *mut RecvOut, buf: *const RecvBuf, ptype: u32) -> *mut RecvOut {
    if out.is_null() {
        return out;
    }
    let got = std::panic::catch_unwind(|| {
        if buf.is_null() {
            return None;
        }
        let (bptr, bcap) = unsafe { ((*buf).ptr, (*buf).cap) };
        if bptr < 0x10000 || bcap == 0 {
            return None;
        }
        let i = recv_peer_index(this)?;
        let data = next_packet(i, ptype, bcap as usize)?;
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), bptr as *mut u8, data.len()) };
        Some((bptr, bcap, data.len()))
    })
    .ok()
    .flatten();
    let v = match got {
        Some((ptr, cap, n)) => RecvOut { ptr, cap, _pad: 0, size: n as u64 },
        None => RecvOut { ptr: 0, cap: 0, _pad: 0, size: 0 },
    };
    unsafe { out.write(v) };
    out
}

/// Our slot 1 (and 3) on the remote's session: it sends nothing.
extern "C" fn recv_side_send(_this: usize, _data: usize, _size: u32, _ptype: u32) -> u64 {
    0
}
extern "C" fn recv_side_send37(_this: usize, _value: i32) -> u64 {
    0
}

fn tee_note(ptype: u32, data: *const u8, size: u32) {
    let _ = std::panic::catch_unwind(|| {
        if let Ok(mut g) = TEE_STATS.lock() {
            let e = g.get_or_insert_with(TeeStats::default).per_type.entry(ptype).or_insert((0, 0));
            e.0 += 1;
            e.1 += size as u64;
        }
        if !forwarding() || data.is_null() || size == 0 {
            return;
        }
        let Ok(mut g) = OUTBOX.lock() else { return };
        let o = g.get_or_insert_with(Outbox::default);
        if size as usize > MAX_PLAYER_RAW {
            o.too_big += 1;
            return;
        }
        if o.packets.len() >= MAX_OUTBOX {
            o.packets.pop_front();
            o.dropped += 1;
        }
        let mut bytes = unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec();
        if ptype == PTYPE_SPEFFECT_LIST {
            hide_own_speffects(&mut bytes);
        }
        o.packets.push_back((ptype, bytes));
    });
}

/// Type 61 = the player's SpEffect list, `n * {u32 time bits, i32 id}`. In IDA it
/// is only sent by `0x8DAE90` through the FrpgSessionManager broadcast `0x8E7210`
/// (with the type-60 header `{u32 n, u32 time}`), never through PlayerSession
/// slot 1/3, so this tee should never see it. If it ever does, our own death
/// debuff rows (speffect.rs, only this machine has them) go out as id -1, which
/// the receiver `0xA32790` skips (it also drops ids whose row is missing or whose
/// stateInfo is not 95). The size stays, so the type-60 count still matches.
fn hide_own_speffects(list: &mut [u8]) {
    for e in list.chunks_exact_mut(8) {
        let id = i32::from_le_bytes([e[4], e[5], e[6], e[7]]);
        if crate::speffect::is_own_row(id) {
            e[4..8].copy_from_slice(&(-1i32).to_le_bytes());
        }
    }
}

/// Our slot 1 on the local player's session: `send(this, data, size, type)`.
/// Tees the packet, then calls the original on the original object, so the
/// game's own loopback keeps working exactly as before.
extern "C" fn tee_send(_this: usize, data: *const u8, size: u32, ptype: u32) -> u64 {
    tee_note(ptype, data, size);
    let (obj, f) = (TEE_ORIG_OBJ.load(Ordering::Acquire), TEE_ORIG_SEND.load(Ordering::Acquire));
    if obj == 0 || f == 0 {
        return 0;
    }
    let f: extern "C" fn(usize, *const u8, u32, u32) -> u64 = unsafe { std::mem::transmute(f) };
    f(obj, data, size, ptype)
}

/// Slot 3 pushes a 4-byte type-37 packet into the queue itself (not through
/// slot 1), so it is teed separately.
extern "C" fn tee_send37(_this: usize, value: i32) -> u64 {
    let bytes = value.to_le_bytes();
    tee_note(37, bytes.as_ptr(), 4);
    let (obj, f) = (TEE_ORIG_OBJ.load(Ordering::Acquire), TEE_ORIG_SEND37.load(Ordering::Acquire));
    if obj == 0 || f == 0 {
        return 0;
    }
    let f: extern "C" fn(usize, i32) -> u64 = unsafe { std::mem::transmute(f) };
    f(obj, value)
}

/// A leaked copy of the PlayerDebugSession vtable (12 slots + RTTI locator)
/// with some slots replaced.
fn make_session_vtable(replace: &[(usize, usize)]) -> Option<usize> {
    let vt = module_base() + DEBUG_SESSION_VT_RVA;
    let mut copy: Vec<usize> = Vec::with_capacity(SESSION_SLOTS + 1);
    for i in 0..=SESSION_SLOTS {
        copy.push(read_ptr(vt - 8 + 8 * i)?);
    }
    for &(slot, f) in replace {
        copy[1 + slot] = f;
    }
    let copy: &'static mut [usize] = copy.leak();
    let new_vt = copy.as_ptr() as usize + 8;
    rtti::register_vtable_copy(new_vt);
    Some(new_vt)
}

/// A leaked 8-byte session object whose vptr is `vt`.
fn leak_session_object(vt: usize, label: &str) -> usize {
    let obj: &'static mut usize = Box::leak(Box::new(vt));
    let addr = obj as *mut usize as usize;
    log::line(&format!("native: {label} session object {addr:#x} (vtable copy {vt:#x})"));
    addr
}

/// A leaked copy of the PlayerDebugSession vtable with some slots replaced, and a leaked
/// 8-byte object pointing at it.
fn make_session(replace: &[(usize, usize)], label: &str) -> Option<usize> {
    let vt = make_session_vtable(replace)?;
    Some(leak_session_object(vt, label))
}

/// Peer index `i`'s recv session object (built once; every peer shares the vtable copy).
fn recv_obj(i: usize) -> Option<usize> {
    let slot = RECV_OBJS.get(i)?;
    let have = slot.load(Ordering::Acquire);
    if have != 0 {
        return Some(have);
    }
    let mut vt = RECV_VT.load(Ordering::Acquire);
    if vt == 0 {
        vt = make_session_vtable(&[
            (SLOT_SEND, recv_side_send as *const () as usize),
            (SLOT_SEND_37, recv_side_send37 as *const () as usize),
            (SLOT_RECV, recv_slot as *const () as usize),
        ])?;
        RECV_VT.store(vt, Ordering::Release);
    }
    let obj = leak_session_object(vt, &format!("remote recv (peer slot {i})"));
    slot.store(obj, Ordering::Release);
    Some(obj)
}

fn tee_obj() -> Option<usize> {
    let have = TEE_OBJ.load(Ordering::Acquire);
    if have != 0 {
        return Some(have);
    }
    let obj = make_session(
        &[(SLOT_SEND, tee_send as *const () as usize), (SLOT_SEND_37, tee_send37 as *const () as usize)],
        "local send tee",
    )?;
    TEE_OBJ.store(obj, Ordering::Release);
    Some(obj)
}

/// Shared PlayerIns vtable copy whose slot 130 answers with the local
/// player's PhantomParam row (RE-FINDINGS-2 § Solid ghost (2)), so chr type
/// 1's white-phantom row is never applied. One copy serves every remote:
/// the per-object part is the vtable pointer itself.
fn solid_vtable() -> Option<usize> {
    let have = SOLID_VT.load(Ordering::Acquire);
    if have != 0 {
        return Some(have);
    }
    let vt = module_base() + PLAYER_INS_VT_RVA;
    let mut copy: Vec<usize> = Vec::with_capacity(PLAYER_INS_SLOTS + 1);
    for i in 0..=PLAYER_INS_SLOTS {
        copy.push(read_ptr(vt - 8 + 8 * i)?);
    }
    copy[1 + SLOT_PHANTOM_ROW] = crate::ghost::ghost_phantom_row as *const () as usize;
    let copy: &'static mut [usize] = copy.leak();
    let addr = copy.as_ptr() as usize + 8;
    rtti::register_vtable_copy(addr);
    SOLID_VT.store(addr, Ordering::Release);
    log::line(&format!("native: PlayerIns vtable copy (slot 130 override) at {addr:#x}"));
    Some(addr)
}

// --- Game-thread helpers ------------------------------------------------------

fn wcm() -> Option<usize> {
    read_ptr(module_base() + WORLD_CHR_MAN_RVA)
}

fn gdm() -> Option<usize> {
    read_ptr(module_base() + GAME_DATA_MAN_RVA)
}

/// (capacity, occupied entries) of the player chr set (entry 0 = us).
fn player_set() -> Option<(i32, Vec<usize>)> {
    let w = wcm()?;
    let cap = read::<i32>(w + WCM_PLAYER_SET_CAP)?;
    let entries = read_ptr(w + WCM_PLAYER_SET)?;
    let used = (0..cap.clamp(0, 64) as usize).filter_map(|i| read_ptr(entries + SET_ENTRY * i)).collect();
    Some((cap, used))
}

fn in_player_set(r: &Remote) -> bool {
    player_set().is_some_and(|(_, used)| used.contains(&r.chr.0)) && read_ptr(r.chr.0 + CHR_PGD) == Some(r.pgd)
}

fn vfn(obj: usize, slot: usize) -> Option<usize> {
    read_ptr(read_ptr(obj)? + 8 * slot)
}

fn class(p: Option<usize>) -> String {
    p.and_then(rtti::class_name).unwrap_or_else(|| "?".into())
}

fn local_pgd_no() -> Option<i32> {
    read::<i32>(read_ptr(gdm()? + GDM_LOCAL_PGD)? + PGD_PLAYER_NO)
}

#[repr(C)]
struct ParamRef {
    _id: i32,
    _pad: u32,
    row: usize,
    _extra: i32,
    _pad2: u32,
}

fn release_pgd(idx: u32) {
    let f: extern "C" fn(u32) = unsafe { std::mem::transmute(module_base() + PGD_RELEASE_RVA) };
    f(idx);
}

/// The dev test's mode 1, step by step. Game thread only (FrameBegin).
fn create(player: Chr) -> Result<Remote, String> {
    let base = module_base();
    let gdm = gdm().ok_or("GameDataMan null")?;
    let wcm = wcm().ok_or("WorldChrMan null")?;
    let (cap, used) = player_set().ok_or("player chr set unreadable")?;
    let local_no = local_pgd_no();
    log::line(&format!(
        "native: player chr set capacity {cap} (WCM+0x40), {} used; local PGD+0x10 = {local_no:?}; player session {} at {:?}",
        used.len(),
        class(read_ptr(player.0 + CHR_SESSION)),
        read_ptr(player.0 + CHR_SESSION).map(|p| format!("{p:#x}"))
    ));
    if used.len() >= cap.max(0) as usize {
        return Err(format!("player chr set full ({cap})"));
    }

    let free_slot: extern "C" fn() -> i32 = unsafe { std::mem::transmute(base + PGD_FREE_SLOT_RVA) };
    let mark: extern "C" fn(i32) = unsafe { std::mem::transmute(base + PGD_MARK_USED_RVA) };
    let get: extern "C" fn(u32) -> usize = unsafe { std::mem::transmute(base + PGD_GET_RVA) };
    let idx = free_slot();
    if !(0..PGD_SLOTS as i32).contains(&idx) {
        return Err(format!("no free remote PlayerGameData slot ({idx})"));
    }
    let idx = idx as u32;
    // playerNo: 1..5 (`pick_player_no`): the netId's low part, unique per remote.
    let remotes = read_ptr(gdm + GDM_REMOTE_PGDS).ok_or("remote PGD array null")?;
    let taken: Vec<i32> = (0..PGD_SLOTS)
        .filter(|&j| j != idx)
        .filter_map(|j| read::<i32>(remotes + PGD_STRIDE * j as usize + PGD_PLAYER_NO))
        .collect();
    let player_no = pick_player_no(local_no, &taken).ok_or("no free playerNo in 1..5")?;

    mark(idx as i32);
    let pgd = get(idx);
    if pgd < 0x10000 {
        release_pgd(idx);
        return Err(format!("PGD slot {idx} null"));
    }
    let lookup: extern "C" fn(*mut ParamRef, i32) = unsafe { std::mem::transmute(base + CHARA_INIT_LOOKUP_RVA) };
    let fill_a: extern "C" fn(usize, usize, *const ParamRef, u8) = unsafe { std::mem::transmute(base + PGD_FILL_A_RVA) };
    let fill_b: extern "C" fn(usize, usize, *const ParamRef) = unsafe { std::mem::transmute(base + PGD_FILL_B_RVA) };
    let mut param = ParamRef { _id: -1, _pad: 0, row: 0, _extra: 0, _pad2: 0 };
    lookup(&mut param, CHARA_INIT_ROW);
    if param.row == 0 {
        // Retail Sekiro has no DS3 debug rows (1080100..): use the first row that exists.
        for id in 0..=CHARA_INIT_SCAN_MAX {
            let mut cand = ParamRef { _id: -1, _pad: 0, row: 0, _extra: 0, _pad2: 0 };
            lookup(&mut cand, id);
            if cand.row != 0 {
                log::line(&format!("native: CharaInitParam {CHARA_INIT_ROW} missing, using first existing row {id}"));
                param = cand;
                break;
            }
        }
    }
    if param.row == 0 {
        release_pgd(idx);
        return Err(format!("CharaInitParam {CHARA_INIT_ROW} not found (no row in 0..={CHARA_INIT_SCAN_MAX})"));
    }
    fill_a(gdm, pgd, &param, 0);
    fill_b(gdm, pgd, &param);
    if !write(pgd + PGD_PLAYER_NO, player_no as i32) {
        release_pgd(idx);
        return Err("PGD playerNo write failed".into());
    }

    // 40-byte init (padded); its PGD must resolve to ours.
    let mut init = [0u64; 8];
    let build: extern "C" fn(*mut u64, u32) -> usize = unsafe { std::mem::transmute(base + PLAYER_INIT_RVA) };
    build(init.as_mut_ptr(), player_no);
    if init[1] as usize != pgd || init[0] as u32 != player_no || (init[0] >> 32) != 2 {
        release_pgd(idx);
        return Err(format!("player init mismatch: {:x?} (pgd {pgd:#x})", &init[..5]));
    }
    let create: extern "C" fn(usize, u32, *const u64) -> usize = unsafe { std::mem::transmute(base + CREATE_PLAYER_RVA) };
    let p = create(wcm, CHR_TYPE_REMOTE, init.as_ptr());
    if p < 0x10000 {
        release_pgd(idx);
        return Err("create returned null (player chr set full?)".into());
    }
    let chr = Chr(p);
    let set_type: extern "C" fn(usize, u32) = unsafe { std::mem::transmute(base + SET_CHR_TYPE_RVA) };
    set_type(p, CHR_TYPE_REMOTE);
    if let Some(f) = vfn(p, SLOT_INIT_18) {
        let f: extern "C" fn(usize) = unsafe { std::mem::transmute(f) };
        f(p);
    }
    // The dev test clears bit 0x08 for types 1/2/12 and sets it again for
    // its "warp" placement.
    if let Some(b) = read::<u8>(p + CHR_FLAGS_1A11) {
        write(p + CHR_FLAGS_1A11, b | 0x08);
    }
    // 0x9F6320 (PLACE_RVA) crashed live (2026-10-23, read of -1 inside sub_140BC2C50):
    // not called. The first packet snaps the remote to its position anyway.
    log::line(&format!(
        "native: CREATED {p:#x} {} (PGD slot {idx} at {pgd:#x}, playerNo {player_no}, CharaInitParam {CHARA_INIT_ROW})",
        class(Some(p))
    ));
    Ok(Remote { chr, kind: Kind::Loopback, pgd_idx: idx, pgd, player_no, since: Instant::now(), late_logged: false, seen_in_set: false, dress_due: None, peer: None, look: None })
}

fn delete_chr(r: &Remote) {
    if let Some(w) = wcm()
        && in_player_set(r)
    {
        let del: extern "C" fn(usize, usize) = unsafe { std::mem::transmute(module_base() + DELETE_CHR_RVA) };
        del(w, r.chr.0);
        log::line(&format!("native: deleted {:#x} (0xA4BF90)", r.chr.0));
    } else {
        log::line(&format!("native: {:#x} no longer in the player chr set, nothing to delete", r.chr.0));
    }
    release_pgd(r.pgd_idx);
}

/// The "Must check live" list of RE-FINDINGS-2, for one remote.
fn log_checks(r: &Remote, player: Chr, stage: &str) {
    let c = r.chr.0;
    let manip = read_ptr(c + CHR_MANIP);
    let manip_type = manip.and_then(|m| {
        let f = vfn(m, MANIP_SLOT_TYPE)?;
        let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(f) };
        Some(f(m))
    });
    let bsync = read_ptr(c + CHR_MODULES).and_then(|m| read_ptr(m + MOD_BEHAVIOR_SYNC));
    let is_remote: extern "C" fn(usize) -> u8 = unsafe { std::mem::transmute(module_base() + IS_REMOTE_RVA) };
    let row = |x: usize| -> Option<i32> {
        let f = vfn(x, SLOT_PHANTOM_ROW)?;
        let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(f) };
        Some(f(x))
    };
    // What the plain PlayerIns slot 130 (0x9F0720) gives the remote, i.e.
    // the row our override hides (expected: chr type 1's phantom row).
    let plain_row = read_ptr(module_base() + PLAYER_INS_VT_RVA + 8 * SLOT_PHANTOM_ROW).map(|f| {
        let f: extern "C" fn(usize) -> i32 = unsafe { std::mem::transmute(f) };
        f(c)
    });
    let pdist = match (player.position(), r.chr.position()) {
        (Some(a), Some(b)) => Some(crate::game::distance(a, b)),
        _ => None,
    };
    log::line(&format!(
        "native[{stage}]: {:?} chr {c:#x} playerNo {} PGD slot {}; manip {} type {manip_type:?} (2 = Network); session {}; bsync+0x19 {:?} +0x1A {:?}; team +0x74 remote {:?} player {:?}; remote check {}; phantom row remote {:?} (plain slot 130 {plain_row:?}) player {:?}; draw group {:?}; hp {:?}; {:?} m from player",
        r.kind,
        r.player_no,
        r.pgd_idx,
        class(manip),
        class(read_ptr(c + CHR_SESSION)),
        bsync.and_then(|b| read::<u8>(b + 0x19)),
        bsync.and_then(|b| read::<u8>(b + 0x1A)),
        read::<u8>(c + CHR_TEAM),
        read::<u8>(player.0 + CHR_TEAM),
        is_remote(c),
        row(c),
        row(player.0),
        read_ptr(c + CHR_MODEL).and_then(|m| read::<u8>(m + 0x23)),
        r.chr.hp(),
        pdist,
    ));
}

fn log_queue() -> String {
    match read_ptr(module_base() + DEBUG_QUEUE_RVA) {
        Some(q) => format!(
            "loopback queue write {:?} delay {:?} subscribers {:?}",
            read::<i32>(q + Q_WRITE_IDX),
            read::<i32>(q + Q_DELAY),
            read::<i32>(q + Q_SUBSCRIBERS)
        ),
        None => "loopback queue null".into(),
    }
}

// --- Per-frame owner -----------------------------------------------------------

/// One peer's spawn bookkeeping. Survives brief "not wanted" spells (the peer dead,
/// loading, gone for a moment); only the option (`native_remote = 0`) resets it.
#[derive(Default, Clone, Copy, Debug)]
struct PeerNative {
    /// Net spawn failures in a row (the friend's ghost fallback while > 0).
    fails: u32,
    retry_at: Option<Instant>,
    /// The peer alive again while our remote of it lies dead since then.
    revive_seen: Option<Instant>,
}

#[derive(Default)]
pub struct Native {
    /// The peers' Net remotes (`Kind::Net`): at most one per peer, sorted by peer id.
    remotes: Vec<Remote>,
    peer_state: BTreeMap<PeerId, PeerNative>,
    /// Forgotten at a load: deleted / its PGD slot freed once the new world
    /// has settled (the object may have survived the load).
    stale: Vec<Remote>,
    /// Player we put our send tee on.
    tee_on: Option<Chr>,
    stats_at: Option<Instant>,
    last_error: Option<String>,
    blocked_logged: bool,
    /// Time in the world as of the last `tick` (for the spawn / dress log lines).
    in_world: Duration,
    /// N0 test clones (`Kind::Loopback`, `[DEBUG] n0_clones`): local copies of us through
    /// the game's PlayerDebugSession loopback, one per PGD slot (at most `PGD_SLOTS`).
    /// `remotes` only ever holds the peers' Net remotes.
    tests: Vec<Remote>,
    /// N0: clones still to create in this world, the clone count asked for, and when the
    /// next one may be created (staggered `N0_STAGGER` apart).
    n0_left: u32,
    n0_asked: u32,
    n0_next_at: Option<Instant>,
    /// N0 was asked for in this world (reset by `forget` / `remove_tests`).
    n0_requested: bool,
    /// The last Net remote creation: for which peer, when (`ensure_peers` staggers
    /// creations for different peers `N0_STAGGER` apart).
    last_create: Option<(PeerId, Instant)>,
    /// `follow_peers`: one position pin per peer, each in its own UpdateCoords hook slot.
    pins: BTreeMap<PeerId, Pin>,
    pin_stats: PinStats,
}

/// Net remotes and N0 clones are created at least this far apart (one PlayerIns per
/// step: PLAN-MULTIPLAYER §2 native.rs, "staggered spawns").
const N0_STAGGER: Duration = Duration::from_secs(3);

/// One peer's wish for a native remote this frame (puppet.rs, PLAN-MULTIPLAYER §5.3).
#[derive(Clone, Copy, Debug)]
pub struct NetWant {
    pub peer: PeerId,
    pub want: bool,
    /// The peer's snapshot HP (the revive check).
    pub hp: Option<i32>,
}

/// v7.5's friend rule, per peer: the option on, the link up, the peer not in another map
/// block, and either its remote exists already or it is alive in our block after the
/// world settled (`native_spawn_delay_s`). `snap` = (hp, block) of its latest visible
/// snapshot.
fn want_rule(option_on: bool, connected: bool, snap: Option<(i32, i32)>, here: Option<i32>, has_remote: bool, settled: bool) -> bool {
    let elsewhere = snap.is_some_and(|(_, block)| here != Some(block));
    let spawnable = snap.is_some_and(|(hp, block)| hp > 0 && here == Some(block)) && settled;
    option_on && connected && !elsewhere && (has_remote || spawnable)
}

/// The listed remotes that must go: their peer is not in `wants` with `want` set.
fn unwanted(have: &[PeerId], wants: &[NetWant]) -> Vec<PeerId> {
    have.iter().copied().filter(|p| !wants.iter().any(|w| w.peer == *p && w.want)).collect()
}

/// The peer whose remote is created this frame, lowest id first: it wants one, has none,
/// is not backing off after a failure, and no OTHER peer's remote was created within
/// `N0_STAGGER` (one new PlayerIns per 3 s; a peer's own respawn is not delayed, so two
/// players keep v7.5's timing).
fn pick_spawn(
    wants: &[NetWant],
    have: &[PeerId],
    backing_off: impl Fn(PeerId) -> bool,
    last_create: Option<(PeerId, Instant)>,
    now: Instant,
) -> Option<PeerId> {
    let mut cand: Vec<PeerId> =
        wants.iter().filter(|w| w.want && !have.contains(&w.peer) && !backing_off(w.peer)).map(|w| w.peer).collect();
    cand.sort();
    cand.dedup();
    cand.into_iter()
        .find(|&p| !last_create.is_some_and(|(q, t)| q != p && now.saturating_duration_since(t) < N0_STAGGER))
}

/// playerNo for a new remote: 1..5 like the dev test, never the local PGD's number (the
/// lookup 0x7B2C80 checks the local PGD first) nor one another remote slot holds.
fn pick_player_no(local: Option<i32>, taken: &[i32]) -> Option<u32> {
    (1..=PGD_SLOTS).find(|&n| Some(n as i32) != local && !taken.contains(&(n as i32)))
}

impl Native {
    /// Compat: the lowest-id peer's Net remote.
    pub fn net_remote(&self) -> Option<Chr> {
        self.remotes().next().map(|(_, c)| c)
    }

    /// The Net remote standing for `peer`.
    pub fn remote_of(&self, peer: PeerId) -> Option<Chr> {
        self.remotes().find(|(p, _)| *p == peer).map(|(_, c)| c)
    }

    /// Every Net remote, by peer id.
    pub fn remotes(&self) -> impl Iterator<Item = (PeerId, Chr)> + '_ {
        self.remotes.iter().filter_map(|r| Some((r.peer?, r.chr)))
    }

    /// True after a failed native spawn for `peer` (or its remote vanished): the ghost may
    /// stand in for it meanwhile (puppet.rs: only for the friend, `peers.first()`).
    pub fn failing_for(&self, peer: PeerId) -> bool {
        self.peer_state.get(&peer).is_some_and(|s| s.fails > 0)
    }

    /// puppet.rs: one `NetWant` per peer of the frame's set (id order), by `want_rule`.
    pub fn wants_for(&self, peers: &PeerSet, here: Option<i32>, in_world_for: Duration, connected: bool) -> Vec<NetWant> {
        let on = crate::tuning::native_remote();
        let settled = in_world_for.as_secs_f32() > crate::tuning::native_spawn_delay_s() as f32;
        peers
            .iter()
            .map(|p| {
                let snap = p.snapshot.map(|s| (s.hp, s.block));
                let has = self.remote_of(p.id).is_some();
                NetWant { peer: p.id, want: want_rule(on, connected, snap, here, has, settled), hp: p.hp() }
            })
            .collect()
    }

    /// Removes `peer`'s Net remote, if any (it left, or no longer wants one). The other
    /// peers' remotes, inboxes and pins are untouched.
    pub fn remove_peer(&mut self, peer: PeerId, why: &str) {
        let Some(i) = self.remotes.iter().position(|r| r.peer == Some(peer)) else { return };
        set_recv_active(peer.index(), false);
        clear_inbox(peer.index());
        if let Some(s) = self.peer_state.get_mut(&peer) {
            s.revive_seen = None;
        }
        let r = self.remotes.remove(i);
        // The hook map and the pin let go before the chr does.
        self.publish_peers();
        if let Some(pin) = self.pins.get_mut(&peer) {
            pin.release(peer);
        }
        log::line(&format!("native: removing {:?} remote of peer slot {} ({why})", r.kind, peer.slot()));
        delete_chr(&r);
    }

    /// Rewrites the lock-free chr -> peer map (`peer_of_chr`) from `remotes`.
    fn publish_peers(&self) {
        let mut chrs = [0usize; MAX_PLAYERS];
        for (p, c) in self.remotes() {
            if let Some(slot) = chrs.get_mut(p.index()) {
                *slot = c.0;
            }
        }
        for (a, c) in PEER_CHR.iter().zip(chrs) {
            a.store(c, Ordering::Release);
        }
    }

    /// N0 test clones exist or are still being created (the peers then stay ghosts).
    pub fn has_test(&self) -> bool {
        !self.tests.is_empty() || self.n0_left > 0
    }

    /// N0 was asked for in this world (`spawn_test`), whatever came of it.
    pub fn n0_requested(&self) -> bool {
        self.n0_requested
    }

    /// N0 clones that exist now.
    pub fn test_count(&self) -> usize {
        self.tests.len()
    }

    /// Dev status text (the menu that showed it is gone; kept for the debug tools).
    #[allow(dead_code)]
    pub fn status(&self) -> String {
        if !self.tests.is_empty() {
            return format!("{} paikallista testikloonia", self.tests.len());
        }
        match (self.remotes.first(), &self.last_error) {
            (Some(r), _) => format!(
                "{} kaveri(a) (natiivi), ensimmainen {:#x}, {} s",
                self.remotes.len(),
                r.chr.0,
                r.since.elapsed().as_secs()
            ),
            (None, Some(e)) => format!("ei (viimeisin virhe: {e})"),
            (None, None) => "ei".into(),
        }
    }

    /// The world was rebuilt (or is being): touch nothing, remember the
    /// remotes for `tick` (deleted / PGD slots freed once the world settled).
    pub fn forget(&mut self, why: &str) {
        RECV_ACTIVE.store(0, Ordering::Relaxed);
        clear_inboxes();
        self.tee_on = None;
        for s in self.peer_state.values_mut() {
            s.revive_seen = None;
        }
        for r in std::mem::take(&mut self.remotes) {
            log::line(&format!("native: {why}, remote {:#x} forgotten", r.chr.0));
            self.stale.push(r);
        }
        self.publish_peers();
        // puppet.rs clears every hook target at a world rebuild; the pins start over.
        for (peer, pin) in self.pins.iter_mut() {
            pin.release(*peer);
        }
        if !self.tests.is_empty() || self.n0_left > 0 {
            log::line(&format!(
                "native: {why}, {} N0 clones forgotten ({} not created yet); they are deleted once the new world settled",
                self.tests.len(),
                self.n0_left
            ));
        }
        self.stale.append(&mut self.tests);
        // A new world asks again (puppet.rs, while n0_clones > 0).
        self.n0_left = 0;
        self.n0_next_at = None;
        self.n0_requested = false;
    }

    /// Deletes every N0 clone (setting back to 0) and stops the ones not created yet.
    pub fn remove_tests(&mut self, why: &str) {
        if !self.tests.is_empty() || self.n0_left > 0 {
            log::line(&format!("native: removing {} N0 clones ({why})", self.tests.len()));
        }
        for r in std::mem::take(&mut self.tests) {
            delete_chr(&r);
        }
        self.n0_left = 0;
        self.n0_next_at = None;
        self.n0_requested = false;
    }

    /// N0 one-machine test (`[DEBUG] n0_clones = k`): `k` (1..5) local clones mirroring us
    /// through the game's PlayerDebugSession loopback, one per remote PGD slot (0..4) with
    /// playerNo 1..5, created by `tick` one at a time `N0_STAGGER` apart. Checks the player
    /// chr set capacity (6), the playerNo / PGD slot allocation, FPS and the delete on load
    /// with as many PlayerIns as a 6-player session has. Not while a peer's Net remote
    /// exists (it holds a PGD slot; the peers show as a ghost while clones exist).
    pub fn spawn_test(&mut self, k: u32) {
        self.n0_requested = true;
        let k = k.min(PGD_SLOTS);
        if k == 0 {
            return;
        }
        if !self.remotes.is_empty() {
            log::line("native: N0 test: a peer's native remote exists, no clones (set native_remote = 0 or play alone)");
            return;
        }
        let have = self.tests.len() as u32;
        self.n0_asked = k;
        self.n0_left = k.saturating_sub(have);
        self.n0_next_at = Some(Instant::now());
        log::line(&format!(
            "native: N0 test requested: {k} clones ({have} exist), one every {} s; {}",
            N0_STAGGER.as_secs(),
            log_queue()
        ));
    }

    /// One N0 clone when due (`tick`, game thread).
    fn n0_step(&mut self, player: Chr) {
        if self.n0_left == 0 || self.n0_next_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let i = self.tests.len() + 1;
        match create(player) {
            Ok(mut r) => {
                log::line(&format!(
                    "native: N0 clone {i} playerNo {} PGD slot {} created ({:#x}, {:.1} s in the world)",
                    r.player_no,
                    r.pgd_idx,
                    r.chr.0,
                    self.in_world.as_secs_f32()
                ));
                self.finish_spawn(&mut r, player);
                log_checks(&r, player, "N0 spawn");
                self.tests.push(r);
                self.last_error = None;
                self.n0_left -= 1;
                self.n0_next_at = Some(Instant::now() + N0_STAGGER);
            }
            Err(e) => {
                log::line(&format!("native: N0 clone {i} failed: {e}; no more clones in this world"));
                self.last_error = Some(e);
                self.n0_left = 0;
            }
        }
        if self.n0_left == 0 {
            let (cap, used) = player_set().map_or((-1, 0), |(c, u)| (c, u.len()));
            let list: Vec<String> =
                self.tests.iter().map(|r| format!("playerNo {} PGD {}", r.player_no, r.pgd_idx)).collect();
            log::line(&format!(
                "native: N0 done: {} of {} clones [{}]; player chr set {used} of {cap} used",
                self.tests.len(),
                self.n0_asked,
                list.join(", ")
            ));
        }
    }

    /// Per-frame bookkeeping of the N0 clones: in the player chr set, gone, the 3 s check.
    fn n0_tick(&mut self, player: Chr) {
        let mut i = 0;
        while i < self.tests.len() {
            let in_set = in_player_set(&self.tests[i]);
            let r = &mut self.tests[i];
            if in_set && !r.seen_in_set {
                r.seen_in_set = true;
                log::line(&format!("native: N0 clone {} ({:#x}) is in the player chr set", i + 1, r.chr.0));
            }
            if !in_set && (r.seen_in_set || r.since.elapsed() >= Duration::from_secs(2)) {
                log::line(&format!(
                    "native: N0 clone {} ({:#x}, playerNo {}) not in the player chr set (seen before: {}), dropping it",
                    i + 1,
                    r.chr.0,
                    r.player_no,
                    r.seen_in_set
                ));
                release_pgd(r.pgd_idx);
                self.tests.remove(i);
                continue;
            }
            let look = look_of(r.chr);
            if r.look != Some(look) {
                if let Some(old) = r.look {
                    log::line(&format!(
                        "native: look of remote {:#x} changed: model {:x?} -> {:x?}, draw group {:?} -> {:?} (hp {:?}, {} s after creation)",
                        r.chr.0,
                        old.0,
                        look.0,
                        old.1,
                        look.1,
                        r.chr.hp(),
                        r.since.elapsed().as_secs()
                    ));
                }
                r.look = Some(look);
            }
            if !r.late_logged && r.since.elapsed() >= Duration::from_secs(3) {
                r.late_logged = true;
                let r = *r;
                log_checks(&r, player, &format!("N0 clone {} after 3 s", i + 1));
            }
            i += 1;
        }
    }

    /// Look options, applied right after the factory (before the model is
    /// built in a later frame). The dress waits `native_dress_delay_ms` (tick).
    fn finish_spawn(&self, r: &mut Remote, player: Chr) {
        if crate::tuning::native_solid() {
            let plain = module_base() + PLAYER_INS_VT_RVA;
            match (read_ptr(r.chr.0), solid_vtable()) {
                (Some(v), Some(copy)) if v == plain => {
                    write(r.chr.0, copy);
                }
                (v, _) => log::line(&format!("native: solid look skipped (vtable {v:x?})")),
            }
        }
        if !crate::tuning::native_dress() {
            log::line("native: not dressed (native_dress = 0): the remote keeps its CharaInitParam equipment");
            return;
        }
        let delay = crate::tuning::native_dress_delay_ms();
        if delay == 0 {
            log::line("native: dressing right after the factory (native_dress_delay_ms = 0)");
            if let Err(e) = crate::ghost::dress_like(r.chr, player) {
                log::line(&format!("native: dress failed: {e}"));
            }
        } else {
            r.dress_due = Some(Instant::now() + Duration::from_millis(delay as u64));
            log::line(&format!("native: dress of {:#x} deferred {delay} ms (until it is in the player chr set and the load has settled)", r.chr.0));
        }
    }

    /// The deferred dress: due, the remote in the player chr set, and not inside the
    /// first `native_spawn_delay_s` of a world (a load meanwhile forgets the remote anyway).
    fn dress_when_due(&mut self, player: Chr) {
        let in_world = self.in_world;
        for r in self.remotes.iter_mut().chain(self.tests.iter_mut()) {
            Self::dress_one(r, player, in_world);
        }
    }

    /// `dress_when_due` for one remote (a peer's or an N0 clone).
    fn dress_one(r: &mut Remote, player: Chr, in_world: Duration) {
        let Some(due) = r.dress_due else { return };
        if Instant::now() < due || !r.seen_in_set {
            return;
        }
        if in_world < Duration::from_secs(crate::tuning::native_spawn_delay_s() as u64) {
            return;
        }
        r.dress_due = None;
        if !crate::tuning::native_dress() {
            log::line("native: deferred dress dropped (native_dress = 0 now)");
            return;
        }
        // Diagnostics only: ChrIns+0x48 (model ins, ghost.rs log_phantom_rows).
        let model = read_ptr(r.chr.0 + 0x48);
        log::line(&format!(
            "native: deferred dress of {:#x} now, {} ms after creation, {:.1} s in the world (model {:x?})",
            r.chr.0,
            r.since.elapsed().as_millis(),
            in_world.as_secs_f32(),
            model
        ));
        if let Err(e) = crate::ghost::dress_like(r.chr, player) {
            log::line(&format!("native: dress failed: {e}"));
        }
    }

    /// N1: keeps a native remote for every peer in `wants` with `want` set and removes the
    /// ones no longer wanted (or no longer listed: a peer that left). At most one creation
    /// per call, and creations for different peers at least `N0_STAGGER` (3 s) apart. Read
    /// the result with `remote_of` after `tick`.
    pub fn ensure_peers(&mut self, player: Chr, wants: &[NetWant]) {
        let have: Vec<PeerId> = self.remotes.iter().filter_map(|r| r.peer).collect();
        for p in unwanted(&have, wants) {
            self.remove_peer(p, "not wanted any more");
        }
        if !crate::tuning::native_remote() {
            // Failures (ghost fallback, back-off) survive brief "not wanted" spells
            // (peer dead, loading); only the option resets them.
            self.peer_state.clear();
        }
        for w in wants.iter().filter(|w| w.want) {
            self.check_revive(*w);
        }
        // Never a creation in the frame a remote was deleted (v7.5 never did either).
        if self.remotes.len() == have.len() {
            self.spawn_one(player, wants);
        }
        self.publish_peers();
    }

    /// The peer back on their feet while our remote of it lies dead (the engine killed it
    /// from the packet HP): replace it after 2 s.
    fn check_revive(&mut self, want: NetWant) {
        let Some(chr) = self.remote_of(want.peer) else { return };
        let dead = chr.hp().is_some_and(|hp| hp <= 0);
        let st = self.peer_state.entry(want.peer).or_default();
        let revived = if dead && want.hp.is_some_and(|hp| hp > 0) {
            st.revive_seen.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(2)
        } else {
            st.revive_seen = None;
            false
        };
        if revived {
            self.remove_peer(want.peer, "friend revived");
        }
    }

    /// At most one Net remote creation (`pick_spawn`).
    fn spawn_one(&mut self, player: Chr, wants: &[NetWant]) {
        let have: Vec<PeerId> = self.remotes.iter().filter_map(|r| r.peer).collect();
        if !wants.iter().any(|w| w.want && !have.contains(&w.peer)) {
            return;
        }
        if self.has_test() {
            if !self.blocked_logged {
                self.blocked_logged = true;
                log::line("native: N0 test clones present, the peers stay ghosts until they are removed (n0_clones = 0)");
            }
            return;
        }
        self.blocked_logged = false;
        let now = Instant::now();
        let state = &self.peer_state;
        let backing_off = |p: PeerId| state.get(&p).and_then(|s| s.retry_at).is_some_and(|t| now < t);
        let Some(peer) = pick_spawn(wants, &have, backing_off, self.last_create, now) else { return };
        log::line(&format!(
            "native: N1 spawn for peer slot {} ({} other remote(s)); {}",
            peer.slot(),
            self.remotes.len(),
            log_queue()
        ));
        let i = peer.index();
        // Built before the chr, so a failure here leaves nothing behind.
        let r = recv_obj(i).ok_or_else(|| "recv session not built".to_string()).and_then(|obj| {
            let mut r = create(player)?;
            r.kind = Kind::Net;
            r.peer = Some(peer);
            clear_inbox(i);
            set_recv_active(i, true);
            if !write(r.chr.0 + CHR_SESSION, obj) {
                set_recv_active(i, false);
                delete_chr(&r);
                return Err("session write failed".to_string());
            }
            Ok(r)
        });
        match r {
            Ok(mut r) => {
                log::line(&format!(
                    "native: remote of peer slot {} created {:.1} s after the load (native_spawn_delay_s {})",
                    peer.slot(),
                    self.in_world.as_secs_f32(),
                    crate::tuning::native_spawn_delay_s()
                ));
                self.finish_spawn(&mut r, player);
                log_checks(&r, player, "N1 spawn");
                let at = self.remotes.iter().position(|q| q.peer > Some(peer)).unwrap_or(self.remotes.len());
                self.remotes.insert(at, r);
                self.last_create = Some((peer, Instant::now()));
                let st = self.peer_state.entry(peer).or_default();
                st.fails = 0;
                st.retry_at = None;
                self.last_error = None;
            }
            Err(e) => {
                let st = self.peer_state.entry(peer).or_default();
                st.fails += 1;
                let wait = Duration::from_secs(10 * st.fails.min(6) as u64);
                st.retry_at = Some(Instant::now() + wait);
                log::line(&format!(
                    "native: N1 spawn for peer slot {} failed ({e}), ghost fallback, retry in {} s (failure {})",
                    peer.slot(),
                    wait.as_secs(),
                    st.fails
                ));
                self.last_error = Some(e);
            }
        }
    }

    /// Every in-world frame: stale cleanup, vanished remotes, the send tee
    /// and the rate-limited stats.
    pub fn tick(&mut self, player: Chr, in_world: Duration) {
        self.in_world = in_world;
        if in_world >= STALE_CLEANUP_AFTER {
            for r in std::mem::take(&mut self.stale) {
                if in_player_set(&r) {
                    log::line(&format!("native: remote {:#x} survived the load, deleting it", r.chr.0));
                } else {
                    log::line(&format!("native: remote {:#x} did not survive the load", r.chr.0));
                }
                delete_chr(&r);
            }
        }
        // N0 test clones: bookkeeping, then at most one new clone per N0_STAGGER (only
        // after the stale ones of the previous world are gone, so PGD slots are free).
        self.n0_tick(player);
        if self.stale.is_empty() {
            self.n0_step(player);
        }
        let mut i = 0;
        while i < self.remotes.len() {
            let in_set = in_player_set(&self.remotes[i]);
            let r = &mut self.remotes[i];
            if in_set && !r.seen_in_set {
                r.seen_in_set = true;
                log::line(&format!("native: remote {:#x} is in the player chr set", r.chr.0));
            }
            if !in_set && (r.seen_in_set || r.since.elapsed() >= Duration::from_secs(2)) {
                log::line(&format!(
                    "native: remote {:#x} not in the player chr set (seen before: {}), dropping it",
                    r.chr.0, r.seen_in_set
                ));
                let r = self.remotes.remove(i);
                self.publish_peers();
                release_pgd(r.pgd_idx);
                if let Some(peer) = r.peer {
                    set_recv_active(peer.index(), false);
                    if let Some(pin) = self.pins.get_mut(&peer) {
                        pin.release(peer);
                    }
                    let st = self.peer_state.entry(peer).or_default();
                    st.fails += 1;
                    st.retry_at = Some(Instant::now() + Duration::from_secs(10));
                }
                continue;
            }
            if !r.late_logged && r.since.elapsed() >= Duration::from_secs(3) {
                r.late_logged = true;
                let r = *r;
                log_checks(&r, player, "after 3 s");
            }
            i += 1;
        }

        self.dress_when_due(player);

        // The tee counts packets for N0 and forwards them for N1.
        let want_tee = forwarding() || !self.tests.is_empty();
        if want_tee && self.tee_on != Some(player) {
            self.install_tee(player);
        } else if !want_tee && self.tee_on.is_some() {
            self.remove_tee(player);
        }

        if (!self.remotes.is_empty() || !self.tests.is_empty() || self.tee_on.is_some())
            && self.stats_at.is_none_or(|t| t.elapsed() >= STATS_EVERY)
        {
            let secs = self.stats_at.map_or(STATS_EVERY.as_secs_f32(), |t| t.elapsed().as_secs_f32()).max(0.1);
            self.stats_at = Some(Instant::now());
            self.log_stats(player, secs);
        }
    }

    fn install_tee(&mut self, player: Chr) {
        let Some(tee) = tee_obj() else { return };
        let Some(cur) = read_ptr(player.0 + CHR_SESSION) else { return };
        if cur == tee {
            self.tee_on = Some(player);
            return;
        }
        let debug_vt = module_base() + DEBUG_SESSION_VT_RVA;
        if read_ptr(cur) != Some(debug_vt) {
            // Only once per player: this runs every frame otherwise.
            log::line(&format!("native: player session is {}, not PlayerDebugSession: no tee", class(Some(cur))));
            self.tee_on = Some(player);
            return;
        }
        TEE_ORIG_SEND.store(read_ptr(debug_vt + 8 * SLOT_SEND).unwrap_or(0), Ordering::Release);
        TEE_ORIG_SEND37.store(read_ptr(debug_vt + 8 * SLOT_SEND_37).unwrap_or(0), Ordering::Release);
        TEE_ORIG_OBJ.store(cur, Ordering::Release);
        if write(player.0 + CHR_SESSION, tee) {
            log::line(&format!("native: send tee on the player (original session {cur:#x})"));
        }
        self.tee_on = Some(player);
    }

    fn remove_tee(&mut self, player: Chr) {
        if self.tee_on == Some(player)
            && let (Some(tee), orig) = (tee_obj(), TEE_ORIG_OBJ.load(Ordering::Acquire))
            && read_ptr(player.0 + CHR_SESSION) == Some(tee)
            && orig != 0
        {
            write(player.0 + CHR_SESSION, orig);
            log::line("native: send tee removed, player session restored");
        }
        self.tee_on = None;
    }

    fn log_stats(&self, player: Chr, secs: f32) {
        let tee: Vec<String> = TEE_STATS
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().map(|s| std::mem::take(&mut s.per_type)))
            .map(|m| {
                let mut v: Vec<_> = m.into_iter().collect();
                v.sort();
                v.into_iter().map(|(t, (n, b))| format!("type {t}: {n} ({:.1}/s, {b} B)", n as f32 / secs)).collect()
            })
            .unwrap_or_default();
        let (out_dropped, out_big) =
            OUTBOX.lock().ok().and_then(|g| g.as_ref().map(|o| (o.dropped, o.too_big))).unwrap_or((0, 0));
        let inboxes: Vec<String> = (0..MAX_PLAYERS)
            .filter_map(|i| {
                let g = INBOXES[i].lock().ok()?;
                let ib = g.as_ref()?;
                Some(format!(
                    "slot {i}: received {} served {} dropped old {} too big {} queued {}, frame interval {:.0} ms",
                    ib.received,
                    ib.served,
                    ib.dropped_old,
                    ib.dropped_big,
                    ib.packets.len(),
                    ib.frame_interval_ms
                ))
            })
            .collect();
        let inbox = if inboxes.is_empty() { "empty".to_string() } else { inboxes.join("; ") };
        let unmapped = RECV_UNMAPPED.load(Ordering::Relaxed);
        let unmapped = if unmapped > 0 { format!(" (recv on an unknown session object {unmapped} times)") } else { String::new() };
        let remotes: Vec<String> = self
            .remotes
            .iter()
            .map(|r| {
                let d = match (player.position(), r.chr.position()) {
                    (Some(a), Some(b)) => format!("{:.2} m", crate::game::distance(a, b)),
                    _ => "?".into(),
                };
                let slot = r.peer.map_or_else(|| "?".to_string(), |p| p.slot().to_string());
                format!("{:?} remote of slot {slot} hp {:?}, {d} from player", r.kind, r.chr.hp())
            })
            .collect();
        let remote = if remotes.is_empty() { "no remote".to_string() } else { remotes.join(", ") };
        let clones = if self.tests.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = self
                .tests
                .iter()
                .map(|r| {
                    let d = match (player.position(), r.chr.position()) {
                        (Some(a), Some(b)) => format!("{:.1} m", crate::game::distance(a, b)),
                        _ => "?".into(),
                    };
                    format!("playerNo {} PGD {} hp {:?} {d}", r.player_no, r.pgd_idx, r.chr.hp())
                })
                .collect();
            let (cap, used) = player_set().map_or((-1, 0), |(c, u)| (c, u.len()));
            format!("; N0 clones {} [{}], player chr set {used} of {cap}", self.tests.len(), list.join(", "))
        };
        log::line(&format!(
            "native stats ({secs:.0} s): tee {} [{}], forward {:#08b}, outbox dropped {out_dropped} too big {out_big}; inbox {inbox}{unmapped}; {remote}; {}{clones}",
            self.tee_on.is_some(),
            tee.join(", "),
            FORWARD.load(Ordering::Relaxed),
            log_queue()
        ));
    }

    /// Position 1:1, animation native, for every peer's remote. The game's
    /// NetworkManipulator moves a remote only through the locomotion graph (move vector
    /// levels walk/run/dash from distance / (1.2 x time between packets)), never onto the
    /// packet position, so it drifted and rested ~2 m off (live T lines 2026-10-23).
    /// Every frame we write the peer's interpolated snapshot position (fixed delay
    /// `NATIVE_POS_DELAY_MS`) into its remote's physics store through the UpdateCoords
    /// hook (slot `pin_slot(peer)`); walking, running and attacks still animate from the
    /// packets. Call once per in-world frame, before `ensure_peers` (moved here from
    /// puppet.rs `follow_native`, which did this for the friend only).
    ///
    /// The pin is the only writer of the position, so the two never fight frame by
    /// frame; what can show as a teleport is a jump of the target itself (a late
    /// snapshot after extrapolation, a burst after a hitch). Jumps above the normal
    /// per-frame motion and below a real warp are eased over a few frames instead of
    /// set at once, real warps (>= 8 m) are set at once, and both are logged with the
    /// distance, as is any move of the remote by the game between our writes (a
    /// throw, a game warp).
    pub fn follow_peers(&mut self, net: Option<&crate::net::Net>) {
        let now = Instant::now();
        for r in &self.remotes {
            let Some(peer) = r.peer else { continue };
            let target = net
                .and_then(|n| n.peer(peer))
                .and_then(|p| p.remote_smoothed(NATIVE_POS_DELAY_MS))
                .filter(|(p, _)| crate::game::sane_pos(*p));
            let pin = self.pins.entry(peer).or_default();
            // 2026-10-24 (v7.11, video, Lady Butterfly): a deathblow / grab with this peer's
            // player in it is a paired animation the game places both characters for
            // (type 18); our 130 ms old, throw-unaware snapshot dragged the remote out of
            // it. The game owns this remote's position meanwhile (per peer).
            // 2026-10-26 (v8.29b): the window is only 4 s, a boss deathblow lasts ~8 s: the
            // remote's own ThrowModule node keeps the pin off until the throw ends
            // (hard cap `PIN_THROW_CAP`).
            let in_window = crate::nsession::remote_in_throw(peer.slot());
            let node_throw = crate::enemies::throw_active(r.chr);
            let since = if in_window || node_throw {
                now.duration_since(*pin.throw_since.get_or_insert(now))
            } else {
                pin.throw_since = None;
                pin.extended_logged = false;
                pin.gave_up_logged = false;
                Duration::ZERO
            };
            if pin_released(in_window, node_throw, since) {
                if !pin.in_throw {
                    pin.in_throw = true;
                    log::line(&format!("native: peer {}'s remote in a throw, pin released for 4 s", peer.slot()));
                }
                if !in_window && node_throw && !pin.extended_logged {
                    pin.extended_logged = true;
                    log::line(&format!(
                        "native: peer {}'s remote still in a throw at {} ms: pin kept off",
                        peer.slot(),
                        since.as_millis()
                    ));
                }
                pin.release(peer);
                continue;
            }
            if since >= PIN_THROW_CAP && !pin.gave_up_logged {
                pin.gave_up_logged = true;
                log::line("native: pin kept off for 15 s: giving up, pin back on");
            }
            if std::mem::take(&mut pin.in_throw) {
                pin.resume = true; // ease from where the throw left the remote
            }
            match target {
                Some((p, _)) if let (Some(physics), Some(cur)) = (r.chr.physics(), r.chr.position4()) => {
                    let out = pin.step(&mut self.pin_stats, r.chr, [cur[0], cur[1], cur[2]], p, now);
                    hook::set_target(pin_slot(peer), physics, [out[0], out[1], out[2], cur[3]]);
                    pin.on = true;
                }
                _ => pin.release(peer),
            }
        }
        let have: Vec<PeerId> = self.remotes.iter().filter_map(|r| r.peer).collect();
        for (peer, pin) in self.pins.iter_mut().filter(|(p, _)| !have.contains(p)) {
            pin.release(*peer);
        }
        self.pin_stats.log();
    }
}

// --- Position pin (follow_peers) -----------------------------------------------

/// The peer's interpolated snapshot position this far in the past: about as late as the
/// animation, which comes through the game's own packet queue (latency + ~1.5 packets of
/// play-out at 12/s).
const NATIVE_POS_DELAY_MS: u32 = 130;
/// UpdateCoords hook slots of the pins: one per peer index, the top `MAX_PLAYERS` slots of
/// the table (enemies.rs allocates below `PIN_SLOT_BASE`; puppet's ghost uses slot 0).
pub const PIN_SLOT_BASE: usize = hook::TARGETS - MAX_PLAYERS;

pub fn pin_slot(peer: PeerId) -> usize {
    PIN_SLOT_BASE + peer.index().min(MAX_PLAYERS - 1)
}

/// Above this many metres per second of target motion a frame's jump is a snap
/// (the fastest own movement, the grappling hook, stays below it).
const NATIVE_SNAP_SPEED: f32 = 40.0;
/// Never call less than this a snap (frame hitches).
const NATIVE_SNAP_MIN_M: f32 = 1.5;
/// From this jump on the peer really warped: set at once.
const NATIVE_WARP_M: f32 = 8.0;
/// Each frame an eased pin closes this share of the gap (about 10 frames).
const NATIVE_EASE: f32 = 0.25;

/// One peer's pin: the last written position, easing, and whether its hook slot is set.
#[derive(Default)]
struct Pin {
    last: Option<(Chr, [f32; 3], Instant)>,
    easing: bool,
    on: bool,
    /// A throw window of this peer is open (log once per window).
    in_throw: bool,
    /// The throw window just ended: the next step eases from `cur`, no snap to the target.
    resume: bool,
    /// When the current throw (window or node) was first seen.
    throw_since: Option<Instant>,
    extended_logged: bool,
    gave_up_logged: bool,
}

/// The pin never stays released longer than this for one throw.
const PIN_THROW_CAP: Duration = Duration::from_secs(15);

/// Whether the pin stays off: a throw window or a live throw node on the remote, but no
/// longer than `PIN_THROW_CAP` since the throw was first seen.
fn pin_released(in_window: bool, node_throw: bool, since: Duration) -> bool {
    (in_window || node_throw) && since < PIN_THROW_CAP
}

/// The snap statistics over every pin (one 10 s line).
#[derive(Default)]
struct PinStats {
    snaps: u32,
    snap_max: f32,
    warps: u32,
    moved: u32,
    moved_max: f32,
    line_logs: u32,
    stats_at: Option<Instant>,
}

/// A move of the remote by the game, away from our last write, that is eased instead of
/// being set straight back (a visible pop): above the frame-hitch floor, below a real warp.
fn moved_eased(moved: f32) -> bool {
    moved > NATIVE_SNAP_MIN_M && moved < NATIVE_WARP_M
}

fn ease_toward(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * NATIVE_EASE)
}

impl Pin {
    /// Forgets the pin and lets go of its hook slot.
    fn release(&mut self, peer: PeerId) {
        self.last = None;
        self.easing = false;
        if self.on {
            hook::clear_target(pin_slot(peer));
            self.on = false;
        }
    }

    /// The position to write this frame for `target`; `cur` = where the remote is now.
    fn step(&mut self, st: &mut PinStats, chr: Chr, cur: [f32; 3], target: [f32; 3], now: Instant) -> [f32; 3] {
        let mut out = target;
        if std::mem::take(&mut self.resume) && self.last.is_none() {
            // After a throw (v7.11): continue from where the game left the remote.
            self.easing = true;
            let out = ease_toward(cur, target);
            self.last = Some((chr, out, now));
            return out;
        }
        if let Some((_, last, at)) = self.last.filter(|(c, ..)| *c == chr) {
            // Our last write was the remote's position; a gap now means the game moved it.
            let moved = crate::game::distance(cur, last);
            if moved > NATIVE_SNAP_MIN_M {
                st.moved += 1;
                st.moved_max = st.moved_max.max(moved);
                if st.line_logs < 30 {
                    st.line_logs += 1;
                    log::line(&format!(
                        "native: remote moved {moved:.1} m off our pin by the game since the last frame (target {:.1} m from cur)",
                        crate::game::distance(cur, target)
                    ));
                }
            }
            if moved_eased(moved) {
                // Ease from where the game put it, not back to the target in one frame (v7.11).
                self.easing = true;
                out = ease_toward(cur, target);
                self.last = Some((chr, out, now));
                return out;
            }
            let jump = crate::game::distance(last, target);
            let dt = now.duration_since(at).as_secs_f32().clamp(0.001, 0.25);
            let snap = (NATIVE_SNAP_SPEED * dt).max(NATIVE_SNAP_MIN_M);
            if jump >= NATIVE_WARP_M {
                st.warps += 1;
                self.easing = false;
                if st.line_logs < 30 {
                    st.line_logs += 1;
                    log::line(&format!("native: pin warp {jump:.1} m (the friend warped), set at once"));
                }
            } else if jump > snap {
                if !self.easing {
                    st.snaps += 1;
                    st.snap_max = st.snap_max.max(jump);
                    if st.line_logs < 30 {
                        st.line_logs += 1;
                        log::line(&format!("native: pin snap {jump:.1} m in {:.0} ms (late snapshot?), eased", dt * 1000.0));
                    }
                }
                self.easing = true;
                out = ease_toward(last, target);
            } else {
                self.easing = false;
            }
        }
        self.last = Some((chr, out, now));
        out
    }
}

impl PinStats {
    /// Every 10 s, when anything happened.
    fn log(&mut self) {
        let at = *self.stats_at.get_or_insert_with(Instant::now);
        if at.elapsed().as_secs() < 10 {
            return;
        }
        self.stats_at = Some(Instant::now());
        if self.snaps + self.warps + self.moved > 0 {
            log::line(&format!(
                "native: pin (10 s): {} snaps eased (max {:.1} m), {} warps, {} moves by the game (max {:.1} m)",
                self.snaps, self.snap_max, self.warps, self.moved, self.moved_max
            ));
        }
        self.snaps = 0;
        self.snap_max = 0.0;
        self.warps = 0;
        self.moved = 0;
        self.moved_max = 0.0;
        // 2026-10-24 (v7.11): the budget was spent early by one 216 m spam and never came back.
        self.line_logs = 0;
    }
}

/// The sender side (PadManipulator slot 11, `0x7EEB50`) re-arms its send
/// countdown with `ChrSync.FrameSkipCount` = 5 frames (`mov ecx, 5` before
/// `0x96F8A0` at `0x7EEF47`): 12 frames/s, which the remote shows as small
/// jumps on fast moves. NOT INSTALLED: the remote's locomotion level is distance /
/// (1.2 x time between packets), so faster sending made it read walking as dashing;
/// the position now comes from our 60/s snapshots instead (puppet.rs follow_native).
/// 1 = every frame (60/s at 60 fps, ≤ 60 KB/s; 2 still
/// showed small jumps, live 2026-10-23). Both sides patch
/// their own sender; the receiver measures the cadence (`frame_interval_ms`).
#[allow(dead_code)]
pub fn install_send_rate() -> bool {
    crate::hook::patch_code(0x7EEF47, &[0xB9, 0x05, 0x00, 0x00, 0x00], &[0xB9, 0x01, 0x00, 0x00, 0x00], "player send rate (every frame)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_release_rules() {
        let s = Duration::from_secs;
        assert!(!pin_released(false, false, s(0)));
        assert!(pin_released(true, false, s(1)));
        assert!(pin_released(false, true, s(8)));
        assert!(pin_released(false, true, Duration::from_millis(14_999)));
        assert!(!pin_released(false, true, s(15)));
        assert!(!pin_released(true, true, s(20)));
        assert!(!pin_released(false, false, s(8)));
    }

    #[test]
    fn moved_is_eased_between_hitch_floor_and_warp() {
        assert!(!moved_eased(NATIVE_SNAP_MIN_M));
        assert!(moved_eased(NATIVE_SNAP_MIN_M + 0.1));
        assert!(moved_eased(NATIVE_WARP_M - 0.1));
        assert!(!moved_eased(NATIVE_WARP_M));
        assert!(!moved_eased(216.0));
    }

    #[test]
    fn ease_closes_a_quarter_of_the_gap_from_cur() {
        let out = ease_toward([0.0, 0.0, 0.0], [4.0, 8.0, -4.0]);
        assert_eq!(out, [1.0, 2.0, -1.0]);
    }

    #[test]
    fn peer_of_chr_reads_the_published_map() {
        assert_eq!(peer_of_chr(0), None);
        PEER_CHR[1].store(0x1234_0000, Ordering::Release);
        assert_eq!(peer_of_chr(0x1234_0000), Some(PeerId(1)));
        assert_eq!(peer_of_chr(0x5678_0000), None);
        // An empty Native clears it.
        Native::default().publish_peers();
        assert_eq!(peer_of_chr(0x1234_0000), None);
    }

    fn want(slot: u8, want: bool) -> NetWant {
        NetWant { peer: PeerId(slot), want, hp: Some(100) }
    }

    #[test]
    fn unwanted_maps_removal_to_exactly_that_peer() {
        let have = [PeerId(0), PeerId(2), PeerId(4)];
        // Peer 2 left (not listed), peer 4 no longer wants one, peer 0 keeps its remote.
        let wants = [want(0, true), want(3, true), want(4, false)];
        assert_eq!(unwanted(&have, &wants), vec![PeerId(2), PeerId(4)]);
        assert!(unwanted(&have, &[want(0, true), want(2, true), want(4, true)]).is_empty());
        assert_eq!(unwanted(&[PeerId(1)], &[]), vec![PeerId(1)]);
    }

    #[test]
    fn spawns_are_staggered_across_peers_and_lowest_id_goes_first() {
        let now = Instant::now();
        let none = |_: PeerId| false;
        let wants = [want(3, true), want(1, true), want(2, false)];
        // Nothing created yet: the lowest wanting id.
        assert_eq!(pick_spawn(&wants, &[], none, None, now), Some(PeerId(1)));
        // Peer 1 got one 1 s ago: peer 3 waits until 3 s have passed.
        let last = Some((PeerId(1), now - Duration::from_secs(1)));
        assert_eq!(pick_spawn(&wants, &[PeerId(1)], none, last, now), None);
        let last = Some((PeerId(1), now - Duration::from_secs(3)));
        assert_eq!(pick_spawn(&wants, &[PeerId(1)], none, last, now), Some(PeerId(3)));
        // A peer's own respawn is not delayed (2-player timing).
        let last = Some((PeerId(1), now - Duration::from_millis(500)));
        assert_eq!(pick_spawn(&wants, &[], none, last, now), Some(PeerId(1)));
        // A peer backing off after a failure does not hold the others back.
        let off = |p: PeerId| p == PeerId(1);
        assert_eq!(pick_spawn(&wants, &[], off, None, now), Some(PeerId(3)));
        // Everyone has one: nothing to do.
        assert_eq!(pick_spawn(&wants, &[PeerId(1), PeerId(3)], none, None, now), None);
    }

    #[test]
    fn five_peers_get_one_spawn_per_three_seconds() {
        let t0 = Instant::now();
        let wants: Vec<NetWant> = (1..=5).map(|s| want(s, true)).collect();
        let mut have: Vec<PeerId> = Vec::new();
        let mut last: Option<(PeerId, Instant)> = None;
        let mut created = Vec::new();
        // One call per simulated 100 ms frame over 15 s.
        for f in 0..150u64 {
            let now = t0 + Duration::from_millis(100 * f);
            if let Some(p) = pick_spawn(&wants, &have, |_| false, last, now) {
                have.push(p);
                last = Some((p, now));
                created.push((p.slot(), now.duration_since(t0).as_millis()));
            }
        }
        assert_eq!(created, vec![(1, 0), (2, 3000), (3, 6000), (4, 9000), (5, 12000)]);
    }

    #[test]
    fn player_numbers_skip_the_local_one_and_taken_ones() {
        assert_eq!(pick_player_no(Some(0), &[]), Some(1));
        assert_eq!(pick_player_no(Some(1), &[2, 3]), Some(4));
        assert_eq!(pick_player_no(None, &[1, 2, 3, 4]), Some(5));
        assert_eq!(pick_player_no(Some(5), &[1, 2, 3, 4]), None);
    }

    #[test]
    fn want_rule_is_v75s_friend_rule() {
        let here = Some(7);
        // Alive in our block after the settle time: wanted.
        assert!(want_rule(true, true, Some((100, 7)), here, false, true));
        // Not yet settled, dead, or another block: not spawnable.
        assert!(!want_rule(true, true, Some((100, 7)), here, false, false));
        assert!(!want_rule(true, true, Some((0, 7)), here, false, true));
        assert!(!want_rule(true, true, Some((100, 8)), here, true, true));
        // An existing remote is kept while dead or hidden (no snapshot), in our block.
        assert!(want_rule(true, true, Some((0, 7)), here, true, false));
        assert!(want_rule(true, true, None, here, true, false));
        // Option off or link down: never.
        assert!(!want_rule(false, true, Some((100, 7)), here, true, true));
        assert!(!want_rule(true, false, Some((100, 7)), here, true, true));
    }

    #[test]
    fn pin_slots_are_one_per_peer_above_the_enemy_range() {
        let slots: Vec<usize> = (0..MAX_PLAYERS as u8).map(|s| pin_slot(PeerId(s))).collect();
        assert_eq!(slots, (PIN_SLOT_BASE..hook::TARGETS).collect::<Vec<_>>());
        assert!(PIN_SLOT_BASE > 1);
    }

    #[test]
    fn inbound_packets_reach_only_their_peers_inbox_and_recv_object() {
        // Peer indices 3 and 4 are used by this test only (the statics are shared).
        let (a, b) = (3usize, 4usize);
        RECV_OBJS[a].store(0xA000_0000, Ordering::Release);
        RECV_OBJS[b].store(0xB000_0000, Ordering::Release);
        set_recv_active(a, true);
        set_recv_active(b, true);
        clear_inbox(a);
        clear_inbox(b);
        push_inbound(PeerId(a as u8), 37, vec![1, 2, 3, 4]);
        push_inbound(PeerId(b as u8), 37, vec![9, 9, 9, 9]);
        // A peer without an active remote: dropped.
        push_inbound(PeerId(5), 37, vec![5]);
        assert_eq!(recv_peer_index(0xA000_0000), Some(a));
        assert_eq!(recv_peer_index(0xB000_0000), Some(b));
        assert_eq!(next_packet(a, 37, 64), Some(vec![1, 2, 3, 4]));
        assert_eq!(next_packet(a, 37, 64), None);
        assert_eq!(next_packet(b, 37, 64), Some(vec![9, 9, 9, 9]));
        assert!(INBOXES[5].lock().unwrap().as_ref().is_none_or(|ib| ib.packets.is_empty()));
        // Removing one peer's remote leaves the other's queue alone.
        push_inbound(PeerId(b as u8), 38, vec![7]);
        set_recv_active(a, false);
        clear_inbox(a);
        push_inbound(PeerId(a as u8), 38, vec![1]);
        assert!(INBOXES[a].lock().unwrap().is_none());
        assert_eq!(next_packet(b, 38, 64), Some(vec![7]));
        set_recv_active(b, false);
        clear_inbox(b);
        RECV_OBJS[a].store(0, Ordering::Release);
        RECV_OBJS[b].store(0, Ordering::Release);
    }

    #[test]
    fn own_speffect_rows_leave_as_minus_one() {
        let ids = [3000i32, 2_140_000_001, 2_140_000_005, 2_140_000_006];
        let mut list: Vec<u8> = ids.iter().flat_map(|id| [0x11u8, 0x22, 0x33, 0x44].into_iter().chain(id.to_le_bytes())).collect();
        hide_own_speffects(&mut list);
        let out: Vec<i32> = list.chunks_exact(8).map(|e| i32::from_le_bytes([e[4], e[5], e[6], e[7]])).collect();
        assert_eq!(out, [3000, -1, -1, 2_140_000_006]);
        // The time bits and the size stay.
        assert_eq!(list.len(), 32);
        assert!(list.chunks_exact(8).all(|e| e[..4] == [0x11, 0x22, 0x33, 0x44]));
    }
}

/// Model ins (ChrIns+0x48) and its draw group (model+0x23, ghost.rs: 7 = clone in time).
fn look_of(chr: Chr) -> (Option<usize>, Option<u8>) {
    let model = read_ptr(chr.0 + 0x48);
    (model, model.and_then(|m| crate::game::read::<u8>(m + 0x23)))
}

/// Hex of `len` bytes at `addr` in 16-byte rows ("+off: bytes"), or why not.
fn hex_rows(addr: usize, len: usize) -> String {
    let mut buf = vec![0u8; len];
    if !crate::game::read_bytes(addr, &mut buf) {
        return format!("unreadable at {addr:#x}");
    }
    buf.chunks(16)
        .enumerate()
        .map(|(i, c)| format!("+{:03x}: {}", i * 16, c.iter().map(|b| format!("{b:02x}")).collect::<String>()))
        .collect::<Vec<_>>()
        .join(" ")
}

impl Native {
    /// F11 (v8.53): every remote's check line plus raw bytes of its ChrIns and model ins, and the
    /// same for our own player as the visible reference, so a remote that is in place but not drawn
    /// can be diffed against one that is (read only).
    pub fn f11_dump(&self, player: Chr) {
        log::line(&format!(
            "native F11: own player {:#x} look {:x?}; chr {}; model {}",
            player.0,
            look_of(player),
            hex_rows(player.0, 0x200),
            look_of(player).0.map_or_else(|| "none".to_string(), |m| hex_rows(m, 0x100))
        ));
        if self.remotes.is_empty() {
            log::line("native F11: no remote");
        }
        for r in &self.remotes {
            log_checks(r, player, "F11");
            let look = look_of(r.chr);
            log::line(&format!(
                "native F11: remote {:#x} look {:x?}; chr {}; model {}",
                r.chr.0,
                look,
                hex_rows(r.chr.0, 0x200),
                look.0.map_or_else(|| "none".to_string(), |m| hex_rows(m, 0x100))
            ));
        }
    }
}
