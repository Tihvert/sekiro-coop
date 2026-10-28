//! Host-driven enemies through the game's own NetAIManipulator: the low-level
//! parts (RE-FINDINGS-2 § Enemy AI authority; layout re-checked in IDA
//! 2026-10-22, out80/out81: table accessors 0xA1BD90/0xA1AEF0/0xA1B950, host
//! writers set flag 0x02 movement / 0x08 behavior / 0x80 action, NetAI slot 9
//! stores the turn target at manipulator +0x420 mode / +0x430 position /
//! +0x440 netId, EnemyIns vtable slot 37 = 0xA14B50).
//!
//! Every EnemyIns owns two manipulators: `+0x2000` the AI, `+0x2008` the
//! NetAIManipulator (remote-driven, no thinking). `+0x58` is the active one.
//! The host's AI writes its movement / behavior-sync / action state into
//! per-netId records of `WorldChrMan+0x630` every frame; on the client we
//! write those records into the client's own entry and set the "pending"
//! bits, and the NetAIManipulator moves and animates the enemy itself.
//!
//! WorldChrMan re-picks the manipulator every frame through vtable slot 37
//! with `forceLocal = (session members <= 1) || ...`, which is always true
//! offline, so a one-time switch does not stick. Host-driven enemies get a
//! vtable copy whose slot 37 asks for the NetAI side (`force = 0`) while the
//! enemy is in our registry. One shared copy serves every enemy: what makes
//! it per-enemy is the object's own vtable pointer (and the registry), and a
//! single leaked 2 KB copy cannot pile up over hundreds of loads the way one
//! copy per enemy would.
//! Nothing here is verified live yet; the live checks are logged by
//! enemies.rs.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::game::{Chr, module_base, read, read_bytes, read_ptr, write};

const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const WCM_SYNC_TABLE: usize = 0x630;
const T_BLOCK_COUNT: usize = 0x08;
const T_BLOCKS: usize = 0x10;
const B_COUNT: usize = 0x10;
const B_MOVE: usize = 0x20;
const B_BEHAVIOR: usize = 0x28;
const B_FLAGS: usize = 0x38;
const B_ACTION: usize = 0x40;
const MOVE_STRIDE: usize = 28;
const BEHAVIOR_STRIDE: usize = 344;
const ACTION_STRIDE: usize = 48;

/// Client: records waiting for the NetAIManipulator (it clears them).
pub const F_MOVE_PENDING: u16 = 0x01;
pub const F_BEHAVIOR_PENDING: u16 = 0x04;
pub const F_ACTION_PENDING: u16 = 0x40;
/// Host: set by the AI manipulator when it (re)wrote a record. Nothing
/// clears them offline, so we do after reading.
pub const F_MOVE_WRITTEN: u16 = 0x02;
pub const F_BEHAVIOR_WRITTEN: u16 = 0x08;

const CHR_CTRL: usize = 0x50;
const CHR_ACTIVE_MANIP: usize = 0x58;
const CHR_NET_ID: usize = 0x78;
const CHR_NETAI_MANIP: usize = 0x2008;
/// u8, slot 175: "always local" (forces the AI side in slot 37).
const CHR_FORCE_LOCAL: usize = 0x2040;
/// ChrCtrl "CompletelyNoMove" (Lua SetCompletelyNoMove writes it).
const CTRL_NO_MOVE: usize = 0x192;
const MANIP_TARGET_MODE: usize = 0x420;
const MANIP_TARGET_POS: usize = 0x430;
const PHYSICS_ROT: usize = 0x70;

const ENEMY_VTABLE_RVA: usize = 0x2A27F28;
const VT_SLOTS: usize = 248;
const SLOT_PICK_MANIP: usize = 37;
const PICK_MANIP_RVA: usize = 0xA14B50;
const NET_ID_OF_RVA: usize = 0xA4CE60;

/// Behavior record fields (344 B).
pub const BEH_EVENT_COUNT: usize = 0x140;
pub const BEH_VAR_COUNT: usize = 0x141;
pub const BEH_VALID: usize = 0x142;
pub const BEH_TARGET_MODE: usize = 0x148;
pub const BEH_TARGET: usize = 0x14C;
pub const TARGET_NONE: i32 = 0;
pub const TARGET_CHR: i32 = 1;
pub const TARGET_POS: i32 = 2;
/// Wire-only target codes (never written into game memory): the host's AI
/// targets characters by its own netIds, which mean nothing on the client.
/// A player is `WIRE_PLAYER_BASE + session slot` (host 0x10, clients 0x11..0x15;
/// with two players these are the old HOST/CLIENT codes, mode4.rs); an enemy is
/// `WIRE_ENEMY` with the entity id as the i32 at BEH_TARGET (was 0x12 before
/// N players, which is now slot 2).
pub const WIRE_PLAYER_BASE: i32 = 0x10;
pub const WIRE_ENEMY: i32 = 0x20;

fn wcm() -> Option<usize> {
    read_ptr(module_base() + WORLD_CHR_MAN_RVA)
}

/// The netId the enemy was created with (0xFFFF = none, no NetAI).
pub fn net_id(chr: Chr) -> Option<u16> {
    read::<u16>(chr.0 + CHR_NET_ID).filter(|&n| n != 0xFFFF)
}

/// The game's own netId lookup `0xA4CE60(WCM, chr)` (players included).
/// Pure lookup; game thread.
pub fn net_id_of(chr: Chr) -> Option<u16> {
    let wcm = wcm()?;
    let f: extern "C" fn(usize, usize) -> i16 = unsafe { std::mem::transmute(module_base() + NET_ID_OF_RVA) };
    let n = f(wcm, chr.0) as u16;
    (n != 0xFFFF).then_some(n)
}

/// One netId's records in the sync table (addresses, all checked readable).
#[derive(Clone, Copy, Debug)]
pub struct Entry {
    pub movement: usize,
    pub behavior: usize,
    pub action: usize,
    pub flags: usize,
}

pub fn entry(net_id: u16) -> Option<Entry> {
    let t = read_ptr(wcm()? + WCM_SYNC_TABLE)?;
    let b = (net_id >> 11) as usize;
    let i = (net_id & 0x7FF) as usize;
    if b as i32 >= read::<i32>(t + T_BLOCK_COUNT)? {
        return None;
    }
    let blk = read_ptr(t + T_BLOCKS + 8 * b)?;
    if i as i32 >= read::<i32>(blk + B_COUNT)? {
        return None;
    }
    let e = Entry {
        movement: read_ptr(blk + B_MOVE)? + MOVE_STRIDE * i,
        behavior: read_ptr(blk + B_BEHAVIOR)? + BEHAVIOR_STRIDE * i,
        action: read_ptr(blk + B_ACTION)? + ACTION_STRIDE * i,
        flags: read_ptr(blk + B_FLAGS)? + 2 * i,
    };
    // The last byte of each record must be readable too.
    let ok = read::<u8>(e.movement + MOVE_STRIDE - 1).is_some()
        && read::<u8>(e.behavior + BEHAVIOR_STRIDE - 1).is_some()
        && read::<u8>(e.action + ACTION_STRIDE - 1).is_some()
        && read::<u16>(e.flags).is_some();
    ok.then_some(e)
}

pub fn flags(e: &Entry) -> u16 {
    read::<u16>(e.flags).unwrap_or(0)
}

/// Atomic read-modify-write of the u16 flags: the game sets and clears its
/// own bits in the same word (`*f |= 8` in 0xA1B550, `*f & 0xFEFE | 0x100`
/// in 0xA1BD10), so a plain read + write could drop one of its updates.
fn update_flags(e: &Entry, set: u16, clear: u16) -> bool {
    if read::<u16>(e.flags).is_none() || e.flags % 2 != 0 {
        return false;
    }
    // SAFETY: probed readable just above, u16-aligned; the flags array is
    // plain game memory that the game itself writes in place.
    let a = unsafe { std::sync::atomic::AtomicU16::from_ptr(e.flags as *mut u16) };
    if set != 0 {
        a.fetch_or(set, Ordering::AcqRel);
    }
    if clear != 0 {
        a.fetch_and(!clear, Ordering::AcqRel);
    }
    true
}

pub fn set_flags(e: &Entry, bits: u16) -> bool {
    update_flags(e, bits, 0)
}

pub fn clear_flags(e: &Entry, bits: u16) -> bool {
    update_flags(e, 0, bits)
}

pub fn read_record(addr: usize, len: usize) -> Option<Vec<u8>> {
    let mut v = vec![0u8; len];
    read_bytes(addr, &mut v).then_some(v)
}

/// Copies `data` into game memory (probed first, like `game::write`).
pub fn write_record(addr: usize, data: &[u8]) -> bool {
    crate::game::write_bytes(addr, data)
}

/// Host fallback for a movement record from the physics module (the AI
/// manipulator fills the record from these same values).
pub fn movement_from_physics(chr: Chr) -> Option<Vec<u8>> {
    let ph = chr.physics()?;
    let rot: [f32; 3] = read(ph + PHYSICS_ROT)?;
    let pos = chr.position()?;
    if !crate::game::sane_pos(pos) || !rot.iter().all(|f| f.is_finite()) {
        return None;
    }
    Some(pos.iter().chain(rot.iter()).flat_map(|f| f.to_le_bytes()).collect())
}

pub fn move_pos(rec: &[u8]) -> Option<[f32; 3]> {
    let f = |i: usize| Some(f32::from_le_bytes(rec.get(4 * i..4 * i + 4)?.try_into().ok()?));
    Some([f(0)?, f(1)?, f(2)?])
}

pub fn move_yaw(rec: &[u8]) -> f32 {
    rec.get(16..20).and_then(|b| b.try_into().ok()).map_or(0.0, f32::from_le_bytes)
}

pub fn get_i32(rec: &[u8], at: usize) -> i32 {
    rec.get(at..at + 4).and_then(|b| b.try_into().ok()).map_or(0, i32::from_le_bytes)
}

pub fn put_i32(rec: &mut [u8], at: usize, v: i32) {
    if let Some(b) = rec.get_mut(at..at + 4) {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

pub fn put_pos(rec: &mut [u8], at: usize, p: [f32; 3]) {
    for (k, v) in p.iter().enumerate() {
        if let Some(b) = rec.get_mut(at + 4 * k..at + 4 * k + 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// True if the behavior record raises at least one state as fresh.
pub fn has_fresh(rec: &[u8]) -> bool {
    let n = (rec.get(BEH_EVENT_COUNT).copied().unwrap_or(0) as usize).min(16);
    (0..n).any(|i| rec.get(4 * i + 2).is_some_and(|&b| b != 0))
}

pub fn clear_fresh(rec: &mut [u8]) {
    let n = (rec.get(BEH_EVENT_COUNT).copied().unwrap_or(0) as usize).min(16);
    for i in 0..n {
        if let Some(b) = rec.get_mut(4 * i + 2) {
            *b = 0;
        }
    }
}

/// Counts the game itself would accept (garbage guard before writing).
pub fn behavior_sane(rec: &[u8]) -> bool {
    rec.len() == BEHAVIOR_STRIDE && rec[BEH_EVENT_COUNT] <= 16 && rec[BEH_VAR_COUNT] <= 32
}

// --- Registry + slot 37 -------------------------------------------------

const MAX_DRIVEN: usize = 128;
static DRIVEN: [AtomicUsize; MAX_DRIVEN] = [const { AtomicUsize::new(0) }; MAX_DRIVEN];
/// Address of 0xA14B50 (set before the copy is installed anywhere).
static PICK_FN: AtomicUsize = AtomicUsize::new(0);
/// Our vtable copy (address of slot 0), 0 until built.
static VT_COPY: AtomicUsize = AtomicUsize::new(0);

fn is_driven(chr: usize) -> bool {
    DRIVEN.iter().any(|d| d.load(Ordering::Acquire) == chr)
}

fn register(chr: usize) -> bool {
    if is_driven(chr) {
        return true;
    }
    DRIVEN.iter().any(|d| d.compare_exchange(0, chr, Ordering::AcqRel, Ordering::Acquire).is_ok())
}

fn unregister(chr: usize) {
    for d in &DRIVEN {
        let _ = d.compare_exchange(chr, 0, Ordering::AcqRel, Ordering::Acquire);
    }
}

/// Forget every host-driven enemy without touching game memory (the world
/// was rebuilt: the objects may be freed). Enemies still carrying our
/// vtable just get the original slot-37 behaviour (local AI).
pub fn forget_all() {
    for d in &DRIVEN {
        d.store(0, Ordering::Release);
    }
}

/// Our slot 37. Called by WorldChrMan's character loop for every enemy that
/// carries the copy; must never panic (extern "C") and touches only atomics.
/// Calls of our slot 37 for driven enemies (live diagnostics).
pub static PICK_CALLS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn pick_manipulator(chr: usize, force_local: u8) -> usize {
    let f = PICK_FN.load(Ordering::Acquire);
    let f: extern "C" fn(usize, u8) -> usize = unsafe { std::mem::transmute(f) };
    if !is_driven(chr) {
        return f(chr, force_local);
    }
    PICK_CALLS.fetch_add(1, Ordering::Relaxed);
    // Live 2026-10-22: with force = 0 the original still chose the AI side for most
    // of the enemies, so the host's records were ignored. Run the NetAI branch of
    // 0xA14B50 ourselves, unconditionally.
    match switch_to_net(chr) {
        Some(r) => r,
        None => f(chr, 0),
    }
}

/// The NetAI branch of `0xA14B50` (activate, set `+0x58`, notify, update the
/// controller). None = the manipulator pointers are unreadable.
pub fn switch_to_net(chr: usize) -> Option<usize> {
    let (Some(old), Some(net)) = (read::<usize>(chr + CHR_ACTIVE_MANIP), read::<usize>(chr + CHR_NETAI_MANIP)) else {
        return None;
    };
    if net == 0 {
        return None;
    }
    #[repr(align(16))]
    #[allow(dead_code)]
    struct Buf([u8; 16]);
    let base = module_base();
    unsafe {
        let vcall = |obj: usize, slot: usize| -> usize { *((*(obj as *const usize) + slot * 8) as *const usize) };
        if old != net {
            let activate: extern "C" fn(usize) = std::mem::transmute(vcall(net, 31));
            activate(net);
        }
        write(chr + CHR_ACTIVE_MANIP, net);
        if old != net {
            let mut a = Buf([0; 16]);
            let mut b = Buf([0; 16]);
            let get_a: extern "C" fn(usize, *mut Buf) -> usize = std::mem::transmute(base + 0x9EEC10);
            let get_b: extern "C" fn(usize, *mut Buf) -> usize = std::mem::transmute(base + 0x9F0870);
            let v10 = get_a(chr, &mut a);
            let v11 = get_b(chr, &mut b);
            let on_switch: extern "C" fn(usize, usize, usize) = std::mem::transmute(vcall(net, 6));
            on_switch(net, v11, v10);
        }
        let set_ctrl: extern "C" fn(usize, usize) -> usize = std::mem::transmute(base + 0x7DE3D0);
        match read_ptr(chr + CHR_CTRL) {
            Some(m) => Some(set_ctrl(m, net)),
            None => Some(net),
        }
    }
}

fn vtable_copy() -> Option<usize> {
    let have = VT_COPY.load(Ordering::Acquire);
    if have != 0 {
        return Some(have);
    }
    let base = module_base();
    let vt = base + ENEMY_VTABLE_RVA;
    let mut copy: Vec<usize> = Vec::with_capacity(VT_SLOTS + 1);
    // [-1] = RTTI locator, so class_name() still names the enemy.
    for i in 0..=VT_SLOTS {
        copy.push(read::<usize>(vt - 8 + 8 * i)?);
    }
    if copy[1 + SLOT_PICK_MANIP] != base + PICK_MANIP_RVA {
        crate::log::line(&format!(
            "netai: EnemyIns slot {SLOT_PICK_MANIP} is {:#x}, expected {:#x}: not switching any enemy",
            copy[1 + SLOT_PICK_MANIP],
            base + PICK_MANIP_RVA
        ));
        return None;
    }
    PICK_FN.store(base + PICK_MANIP_RVA, Ordering::Release);
    copy[1 + SLOT_PICK_MANIP] = pick_manipulator as *const () as usize;
    // Leaked on purpose: the game may call through it as long as any enemy
    // carries it, also after we forgot that enemy.
    let copy: &'static mut [usize] = copy.leak();
    let addr = copy.as_ptr() as usize + 8;
    crate::rtti::register_vtable_copy(addr);
    VT_COPY.store(addr, Ordering::Release);
    crate::log::line(&format!("netai: EnemyIns vtable copy at {addr:#x}"));
    Some(addr)
}

/// Makes `chr` host-driven: our vtable and a registry entry. Its records
/// must already be in the table (or the enemy just stands).
pub fn engage(chr: Chr) -> Result<(), String> {
    let nid = net_id(chr).ok_or("no netId (0xFFFF)")?;
    read_ptr(chr.0 + CHR_NETAI_MANIP).ok_or("no NetAIManipulator at +0x2008")?;
    if forced_local(chr) {
        return Err("the game forces its local AI (+0x2040 set)".into());
    }
    let ent = entry(nid).ok_or_else(|| format!("netId {nid:#x} has no table entry"))?;
    let copy = vtable_copy().ok_or("no vtable copy")?;
    // Nothing older than this engagement may be consumed (a record left
    // pending by an earlier one that was only forgotten).
    clear_flags(&ent, F_MOVE_PENDING | F_BEHAVIOR_PENDING | F_ACTION_PENDING);
    let orig = module_base() + ENEMY_VTABLE_RVA;
    match read_ptr(chr.0) {
        Some(v) if v == copy => {}
        Some(v) if v == orig => {
            if !register(chr.0) {
                return Err("registry full".into());
            }
            if !write(chr.0, copy) {
                unregister(chr.0);
                return Err("vtable write failed".into());
            }
        }
        Some(v) => {
            return Err(format!("other vtable {v:#x} ({:?})", crate::rtti::class_name(chr.0)));
        }
        None => return Err("unreadable".into()),
    }
    if !register(chr.0) {
        return Err("registry full".into());
    }
    Ok(())
}

/// Hands `chr` back to its own AI. `valid`: the object is known to be alive
/// (resolved by entity id this frame), so the original vtable can go back.
pub fn release(chr: Chr, valid: bool) {
    unregister(chr.0);
    if !valid {
        return;
    }
    let copy = VT_COPY.load(Ordering::Acquire);
    if copy != 0 && read_ptr(chr.0) == Some(copy) {
        write(chr.0, module_base() + ENEMY_VTABLE_RVA);
    }
    // Records we wrote but NetAI did not take yet must not be taken on a
    // later re-engage: an old behavior record would replay its attacks.
    if let Some(e) = net_id(chr).and_then(entry) {
        clear_flags(&e, F_MOVE_PENDING | F_BEHAVIOR_PENDING | F_ACTION_PENDING);
    }
}

/// `+0x2040` set forces the AI side even with our slot 37. Only the ctor
/// (0) and EnemyIns slot 174 (`0xA14A20`, called virtually, i.e. by game
/// logic) write it, so it is the game's own "keep this one local": never
/// cleared by us (it could not be restored on release), the enemy is left
/// to its AI instead.
pub fn forced_local(chr: Chr) -> bool {
    read::<u8>(chr.0 + CHR_FORCE_LOCAL).is_some_and(|v| v != 0)
}

/// Which manipulator is active: 0 = NetAI, 1 = the AI one (`+0x2000`), 2 = another, 3 = unreadable.
pub fn active_kind(chr: Chr) -> u8 {
    let (Some(active), Some(net)) = (read_ptr(chr.0 + CHR_ACTIVE_MANIP), read_ptr(chr.0 + CHR_NETAI_MANIP)) else { return 3 };
    if active == net {
        0
    } else if read_ptr(chr.0 + 0x2000) == Some(active) {
        1
    } else {
        2
    }
}

pub fn netai_manip(chr: Chr) -> Option<usize> {
    read_ptr(chr.0 + CHR_NETAI_MANIP)
}

/// Keeps a position turn target current (the record only carries a point
/// as of when it was written). Only while NetAI already holds mode 2.
pub fn refresh_target_pos(chr: Chr, p: [f32; 3]) {
    if !crate::game::sane_pos(p) {
        return;
    }
    let Some(m) = netai_manip(chr) else { return };
    if read::<i32>(m + MANIP_TARGET_MODE) == Some(TARGET_POS) {
        write(m + MANIP_TARGET_POS, [p[0], p[1], p[2], 1.0f32]);
    }
}

/// Update interval `chr+0x110` (0 every frame, 2/5/30 every n-th, -1 / -2 off; set
/// from the distance to the main player by `0xA4E8E0`, activation.rs).
const CHR_UPDATE_INTERVAL: usize = 0x110;
static DORMANT_LOGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static DORMANT_SKIPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static DORMANT_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// Host, mode 4: the game does not update this enemy here (interval < 0, far from
/// the host; activation.rs leaves those alone). Its AI writes no records, so a
/// host-driven copy on the client would be NetAI without data: frozen or walking in
/// place (live 7.1: the Chained Ogre near the friend, 340 m from the host, "off
/// (-1/-2) 1161" in the activation line). Such enemies are not sent: the client's
/// copy keeps (or gets back, after the stale timeout) its own AI.
pub fn dormant_here(id: i32, chr: Chr) -> bool {
    use std::sync::atomic::Ordering::Relaxed;
    let Some(n) = read::<i32>(chr.0 + CHR_UPDATE_INTERVAL).filter(|&n| n < 0) else { return false };
    DORMANT_SKIPS.fetch_add(1, Relaxed);
    if DORMANT_LOGS.fetch_add(1, Relaxed) < 20 {
        crate::log::line(&format!(
            "netai host: enemy {id} is not updated here (interval {n}): not host-driven, the friend's copy keeps its own AI"
        ));
    }
    if let Ok(mut g) = DORMANT_AT.try_lock() {
        let at = *g.get_or_insert_with(std::time::Instant::now);
        if at.elapsed().as_secs() >= 10 {
            *g = Some(std::time::Instant::now());
            crate::log::line(&format!(
                "netai host: 10 s: {} enemy checks skipped as not updated here",
                DORMANT_SKIPS.swap(0, Relaxed)
            ));
        }
    }
    true
}

/// Fallback: ChrCtrl CompletelyNoMove (the AI still thinks and attacks).
pub fn set_no_move(chr: Chr, on: bool) -> bool {
    let Some(ctrl) = read_ptr(chr.0 + CHR_CTRL) else { return false };
    write(ctrl + CTRL_NO_MOVE, on as u8)
}
