//! Raw Sekiro v1.06 (1.6.0.0) memory access for the puppet spike.
//!
//! Offsets come from borgCode/SekiroTool (MIT), `SekiroTool/Memory/Offsets.cs`,
//! for `Version1_6_0`. WorldChrMan's RVA was independently seen in the
//! user's cheat-menu log (`WorldChrMan[0x143D7A1E0]`). Every pointer read is
//! checked with `VirtualQuery` so a wrong chain logs instead of crashing.

// `entity_id` and `force_animation` are wired up in Phase 1b (animation).
#![allow(dead_code)]

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};

const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const GET_CHR_INS_BY_ENTITY_ID_RVA: usize = 0x6C1A10;
const FORCE_ANIMATION_RVA: usize = 0x6D5A90;

const WCM_PLAYER_INS: usize = 0x88;
const CHR_CHARACTER_ID: usize = 0x68;
const CHR_ENTITY_ID: usize = 0x1A24;
const CHR_BIT_FLAGS: usize = 0x1F40;
const CHR_MODULES: usize = 0x1FF8;
const MODULE_DATA: usize = 0x18;
const MODULE_PHYSICS: usize = 0x68;
const DATA_HP: usize = 0x130;
const DATA_MAX_HP: usize = 0x134;
const PHYSICS_ANGLE: usize = 0x74;
const PHYSICS_POS: usize = 0x80;

#[repr(C)]
struct MemoryBasicInformation {
    base_address: *mut c_void,
    allocation_base: *mut c_void,
    allocation_protect: u32,
    partition_id: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

const MEM_COMMIT: u32 = 0x1000;
const PAGE_NOACCESS: u32 = 0x01;
const PAGE_GUARD: u32 = 0x100;
const PAGE_READWRITE_MASK: u32 = 0x04 | 0x08 | 0x40 | 0x80; // RW, WRITECOPY, X_RW, X_WRITECOPY

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn VirtualQuery(addr: *const c_void, info: *mut MemoryBasicInformation, len: usize) -> usize;
    fn ReadProcessMemory(process: *mut c_void, addr: *const c_void, buf: *mut c_void, len: usize, read: *mut usize) -> i32;
}

/// Pseudo-handle for the current process (GetCurrentProcess() == -1).
const CURRENT_PROCESS: *mut c_void = -1isize as *mut c_void;

/// Fault-safe copy from our own address space. Unlike `VirtualQuery`, whose
/// cost grows with the size of the region (it walks pages to compute
/// RegionSize), this stays cheap inside me3's ~6 GB heap arena: per-read
/// VirtualQuery cost 7-40 ms per frame once a ghost existed (2026-10-22).
pub fn read_bytes(addr: usize, out: &mut [u8]) -> bool {
    if addr < 0x10000 {
        return false;
    }
    let mut got = 0usize;
    let ok = unsafe {
        ReadProcessMemory(CURRENT_PROCESS, addr as *const c_void, out.as_mut_ptr().cast(), out.len(), &mut got)
    };
    ok != 0 && got == out.len()
}

pub fn module_base() -> usize {
    unsafe { GetModuleHandleW(std::ptr::null()) as usize }
}

fn region_ok(addr: usize, len: usize, need_write: bool) -> bool {
    if addr < 0x10000 {
        return false;
    }
    let mut info = MaybeUninit::<MemoryBasicInformation>::uninit();
    let got = unsafe {
        VirtualQuery(addr as *const c_void, info.as_mut_ptr(), size_of::<MemoryBasicInformation>())
    };
    if got == 0 {
        return false;
    }
    let info = unsafe { info.assume_init() };
    let end = info.base_address as usize + info.region_size;
    info.state == MEM_COMMIT
        && info.protect & (PAGE_NOACCESS | PAGE_GUARD) == 0
        && (!need_write || info.protect & PAGE_READWRITE_MASK != 0)
        && addr + len <= end
}

pub fn read<T: Copy>(addr: usize) -> Option<T> {
    let mut v = MaybeUninit::<T>::uninit();
    let bytes = unsafe { std::slice::from_raw_parts_mut(v.as_mut_ptr().cast::<u8>(), size_of::<T>()) };
    read_bytes(addr, bytes).then(|| unsafe { v.assume_init() })
}

pub fn write<T: Copy>(addr: usize, value: T) -> bool {
    // Probe with a fast fault-safe read instead of VirtualQuery (slow in me3's
    // arena). All our write targets are game heap data, which is writable.
    // The probe lives on the stack: this runs every frame (ghost angle).
    if read::<T>(addr).is_none() {
        return false;
    }
    unsafe { (addr as *mut T).write_unaligned(value) };
    true
}

/// Copies `data` to `addr` after the same fault-safe probe (whole range).
pub fn write_bytes(addr: usize, data: &[u8]) -> bool {
    let mut probe = vec![0u8; data.len()];
    if !read_bytes(addr, &mut probe) {
        return false;
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len()) };
    true
}

/// Largest world coordinate we accept (Sekiro maps are a few km at most).
const MAX_COORD: f32 = 100_000.0;

/// True for a position we may hand to the game: finite and inside the world.
/// A NaN/huge value from a half-loaded character or a bad packet, written
/// into a character's physics, ends up in skinning and culling math on the
/// GPU (the friend's crash in the NVIDIA driver, 2026-10-22, came 3 s after
/// a ghost appeared right after a warp).
pub fn sane_pos(p: [f32; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() < MAX_COORD)
}

/// Same for a facing angle (radians, the game keeps -pi..pi).
pub fn sane_angle(a: f32) -> bool {
    a.is_finite() && a.abs() <= 7.0
}

pub fn read_ptr(addr: usize) -> Option<usize> {
    read::<usize>(addr).filter(|&p| p >= 0x10000)
}

/// A character instance (`ChrIns*`) in game memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chr(pub usize);

impl Chr {
    fn module(self, offset: usize) -> Option<usize> {
        read_ptr(read_ptr(self.0 + CHR_MODULES)? + offset)
    }

    pub fn entity_id(self) -> Option<i32> {
        read(self.0 + CHR_ENTITY_ID)
    }

    pub fn character_id(self) -> Option<i32> {
        read(self.0 + CHR_CHARACTER_ID)
    }

    pub fn hp(self) -> Option<i32> {
        read(self.module(MODULE_DATA)? + DATA_HP)
    }

    /// Max HP, data module +0x134 (livesync uses the same slot for enemies; for the
    /// player it is [INFERRED], 2026-10-24).
    pub fn max_hp(self) -> Option<i32> {
        read(self.module(MODULE_DATA)? + DATA_MAX_HP)
    }

    pub fn set_hp(self, hp: i32) -> bool {
        let Some(data) = self.module(MODULE_DATA) else { return false };
        write(data + DATA_HP, hp)
    }

    pub fn position(self) -> Option<[f32; 3]> {
        read(self.module(MODULE_PHYSICS)? + PHYSICS_POS)
    }

    pub fn angle(self) -> Option<f32> {
        read(self.module(MODULE_PHYSICS)? + PHYSICS_ANGLE)
    }

    /// The ChrPhysicsModule pointer (what `rsi` holds at the UpdateCoords hook).
    pub fn physics(self) -> Option<usize> {
        self.module(MODULE_PHYSICS)
    }

    /// Position including the 4th (w) component, as stored by the game.
    pub fn position4(self) -> Option<[f32; 4]> {
        read(self.module(MODULE_PHYSICS)? + PHYSICS_POS)
    }

    pub fn set_position(self, pos: [f32; 3], angle: f32) -> bool {
        if !sane_pos(pos) || !sane_angle(angle) {
            return false;
        }
        let Some(physics) = self.module(MODULE_PHYSICS) else {
            return false;
        };
        write(physics + PHYSICS_POS, pos) && write(physics + PHYSICS_ANGLE, angle)
    }

    /// Sets a bit in the ChrIns flag block (layout from SekiroTool `ChrIns.BitFlag`).
    fn set_flag(self, byte: usize, mask: u8, on: bool) -> bool {
        let addr = self.0 + CHR_BIT_FLAGS + byte;
        let Some(v) = read::<u8>(addr) else {
            return false;
        };
        write(addr, if on { v | mask } else { v & !mask })
    }

    /// Makes the character harmless: no attacks, can't be hit. AI stays on:
    /// test 2026-10-22 suggests FreezeAi also stops the physics step, so the
    /// hooked position never reached the visible model until a respawn.
    pub fn make_puppet(self) -> bool {
        self.set_flag(0x0, 1 << 6, true) // NoAttack
            && self.set_flag(0x0, 1 << 5, true) // NoHit
    }

    pub fn set_freeze_ai(self, on: bool) -> bool {
        self.set_flag(0x1, 1 << 4, on)
    }
}

/// RE helper: finds every copy of `pos` (3 consecutive f32 within `tol`)
/// reachable from `chr`: the ChrIns body, every module, and one pointer level
/// below the physics module. Returns human-readable paths.
pub fn find_position_copies(chr: Chr, pos: [f32; 3], tol: f32) -> Vec<String> {
    let mut hits = Vec::new();
    let scan = |label: &str, base: usize, len: usize, hits: &mut Vec<String>| {
        let mut off = 0;
        while off + 12 <= len {
            if let Some(v) = read::<[f32; 3]>(base + off)
                && (0..3).all(|i| (v[i] - pos[i]).abs() <= tol)
            {
                hits.push(format!("{label}+{off:#x} = {v:?}"));
            }
            off += 4;
        }
    };

    scan("ChrIns", chr.0, 0x2400, &mut hits);
    let Some(modules) = read_ptr(chr.0 + CHR_MODULES) else {
        return hits;
    };
    for slot in (0..0x100).step_by(8) {
        if let Some(m) = read_ptr(modules + slot) {
            scan(&format!("module[{slot:#x}]"), m, 0x1000, &mut hits);
        }
    }
    if let Some(physics) = read_ptr(modules + MODULE_PHYSICS) {
        for off in (0..0x400).step_by(8) {
            if let Some(p) = read_ptr(physics + off) {
                scan(&format!("physics->[{off:#x}]"), p, 0x400, &mut hits);
            }
        }
    }
    hits
}

pub fn player() -> Option<Chr> {
    let wcm = read_ptr(module_base() + WORLD_CHR_MAN_RVA)?;
    read_ptr(wcm + WCM_PLAYER_INS).map(Chr)
}

/// Native `GetChrInsByEntityId(const int*) -> ChrIns*`; returns `None` if not loaded.
/// Takes a POINTER to the id (SekiroTool loads `rcx` with the id's address;
/// passing the value crashed at `sekiro.exe+0x6C1A29`, 2026-10-22).
/// Must be called on the game thread (we call it from a game task).
pub fn chr_by_entity_id(entity_id: i32) -> Option<Chr> {
    let f: extern "C" fn(*const i32) -> usize =
        unsafe { std::mem::transmute(module_base() + GET_CHR_INS_BY_ENTITY_ID_RVA) };
    let p = f(&entity_id);
    (p >= 0x10000 && read::<usize>(p + CHR_MODULES).is_some()).then_some(Chr(p))
}

/// Native `ForceAnimation(entityId, animId)` (EMEVD-style, by entity id).
pub fn force_animation(entity_id: i32, anim_id: i32) {
    let f: extern "C" fn(i32, i32) =
        unsafe { std::mem::transmute(module_base() + FORCE_ANIMATION_RVA) };
    f(entity_id, anim_id);
}

pub fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

/// FieldArea (fromsoftware-rs `field_area_ptr` RVA) `+0x28` = current world
/// block index (SekiroTool `FieldArea.CurrentWorldBlockIndex`). Positions are
/// only comparable between players in the same block.
const FIELD_AREA_RVA: usize = 0x3D5C0A0;
const FIELD_AREA_BLOCK_INDEX: usize = 0x28;

pub fn current_block() -> Option<i32> {
    read(read_ptr(module_base() + FIELD_AREA_RVA)? + FIELD_AREA_BLOCK_INDEX)
}

// --- Boss gauge, warp gate and idol warps (docs/SPEC-BOSS-RULES.md §0.1, §3) ---

const GAME_MAN_RVA: usize = 0x3D5C0D0;
/// GameMan +0x10: set by `0x832EE0` when a load is requested.
const GAME_MAN_LOAD_REQUEST: usize = 0x10;
/// Last rested idol + 1000: `0x831940` returns `+0xB50` if it is >= 0, else `+0xB4C`.
const GAME_MAN_LAST_IDOL: usize = 0xB50;
const GAME_MAN_LAST_IDOL_ALT: usize = 0xB4C;
/// Same as `0x6795F0` (travel menu): no idol known = the first Dilapidated Temple idol.
const DEFAULT_IDOL: i32 = 1101950;
/// `base+0x3EA0D74` becomes 1 once the death penalty is done (idol reload next).
const DEAD_LATCH_RVA: usize = 0x3EA0D74;
const EVENT_FLAG_MAN_RVA: usize = 0x3D55FE8;
/// Active flag bank: 0 = the real world, 1 = Remnant/reflection.
const EFM_BANK: usize = 0x220;
const EFM_READY: usize = 0x228;
const GET_EVENT_FLAG_RVA: usize = 0x6C3E60;
const MENU_MAN_RVA: usize = 0x3D67408;
/// MenuMan boss gauge slots: 3 x 0xA8 bytes; `+0` name id, `+4` != -1 while shown.
const MENU_BOSS_SLOTS: usize = 0x2E10;
const MENU_BOSS_SLOT_SIZE: usize = 0xA8;
const MENU_BOSS_SLOT_COUNT: usize = 3;
/// `0x8C58E0`: bit mask of the open menus, assembled from MenuMan fields.
const MENU_MASK_RVA: usize = 0x8C58E0;
/// GameDataMan +0xC0: entity id of the most recent boss.
const GAME_DATA_MAN_RVA: usize = 0x3D5AAC0;
const GDM_LAST_BOSS: usize = 0xC0;
/// Talk-ESD manager (the live crash 2026-10-23 read it from `0x11BAAF0`).
const TALK_MAN_RVA: usize = 0x3EA0CE0;
const WARP_RVA: usize = 0x68E870;
/// Idol table of the travel menu: `begin()` / `end()` getters, 32-byte rows.
const IDOL_TABLE_BEGIN_RVA: usize = 0x10B7270;
const IDOL_TABLE_END_RVA: usize = 0x10B7340;
const IDOL_ROW: usize = 32;
const IDOL_ROW_MAX: usize = 4096;

/// Settled time in the world before a warp or a reward is allowed.
const SAFE_IN_WORLD: std::time::Duration = std::time::Duration::from_secs(3);
/// No second warp this soon after the last one (any caller).
const WARP_COOLDOWN_MS: u64 = 15_000;
/// A warp that is not allowed yet is retried every frame for this long.
const WARP_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(20);

/// Milliseconds since our first call, never 0 (0 = "never" in atomics).
pub fn now_ms() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64 + 1
}

static LAST_WARP_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A boss gauge is on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BossState {
    /// Entity id of the most recent boss (GameDataMan +0xC0), -1 if unreadable.
    pub entity: i32,
    /// Gauge name id (BossGauge text).
    pub name_id: i32,
}

impl BossState {
    /// The boss, if it is loaded: the same lookup as everywhere else (`0x6C1A10`;
    /// the spec's wrapper `0x6D5B20` does the same). Not part of the per-frame
    /// poll, nothing there needs the character.
    pub fn chr(&self) -> Option<Chr> {
        (self.entity > 0).then(|| chr_by_entity_id(self.entity)).flatten()
    }
}

/// "I am in a boss fight": any of the three gauge slots shown (§0.1).
pub fn boss_state() -> Option<BossState> {
    let base = module_base();
    let menu = read_ptr(base + MENU_MAN_RVA)?;
    let name_id = (0..MENU_BOSS_SLOT_COUNT).find_map(|s| {
        let slot = menu + MENU_BOSS_SLOTS + MENU_BOSS_SLOT_SIZE * s;
        (read::<i32>(slot + 4)? != -1).then(|| read::<i32>(slot).unwrap_or(-1))
    })?;
    let entity = read_ptr(base + GAME_DATA_MAN_RVA).and_then(|g| read::<i32>(g + GDM_LAST_BOSS)).unwrap_or(-1);
    Some(BossState { entity, name_id })
}

/// The flag bank is not the real world's (a Remnant memory is active).
/// Unreadable counts as Remnant: the safe answer for every caller.
pub fn remnant() -> bool {
    read_ptr(module_base() + EVENT_FLAG_MAN_RVA).and_then(|fm| read::<u32>(fm + EFM_BANK)) != Some(0)
}

/// An event flag of the active bank, `None` while the flag manager is not ready.
pub fn event_flag(id: u32) -> Option<bool> {
    let fm = read_ptr(module_base() + EVENT_FLAG_MAN_RVA).filter(|&fm| read::<u8>(fm + EFM_READY).unwrap_or(0) != 0)?;
    let get: extern "C" fn(usize, u32, bool) -> bool = unsafe { std::mem::transmute(module_base() + GET_EVENT_FLAG_RVA) };
    Some(get(fm, id, false))
}

/// Bit mask of the open menus (`0x8C58E0`); 0 = free to move. `None` before MenuMan exists.
/// [LIVE] the spec calls it without arguments; MenuMan is passed in rcx in case it is a method.
pub fn menu_mask() -> Option<u32> {
    let menu = read_ptr(module_base() + MENU_MAN_RVA)?;
    let f: extern "C" fn(usize) -> u32 = unsafe { std::mem::transmute(module_base() + MENU_MASK_RVA) };
    Some(f(menu))
}

/// No talk menu / item prompt is up (the game's own condition in `0x668CC0`).
pub fn talk_idle() -> bool {
    let Some(t) = read_ptr(module_base() + TALK_MAN_RVA) else { return true };
    let idle = || -> Option<bool> {
        let slot = read::<i32>(t + 0x20EC)?;
        let flag = read::<u8>(t + 0x20E8)?;
        let a = read::<f32>(t + 0x210C)?;
        let b = read::<f32>(t + 0x2110)?;
        Some((slot == -1 || flag != 0) && a <= 0.0 && b <= 0.0)
    };
    // An unreadable manager is not known to be idle.
    idle().unwrap_or(false)
}

/// Why we must not touch the world's state right now (death, load, menu): the
/// part shared by warps and boss rewards. `None` = fine.
fn world_blocker(in_world_for: std::time::Duration) -> Option<&'static str> {
    let Some(player) = player() else { return Some("no player") };
    if in_world_for < SAFE_IN_WORLD {
        return Some("just loaded");
    }
    if read::<u32>(module_base() + DEAD_LATCH_RVA).unwrap_or(0) != 0 || player.hp().is_none_or(|hp| hp <= 0) {
        return Some("dead");
    }
    if remnant() {
        return Some("remnant");
    }
    match menu_mask() {
        Some(0) => None,
        Some(_) => Some("menu"),
        None => Some("no menu manager"),
    }
}

/// A load is requested or GameMan cannot be read (GameMan+0x10 != 0). Used by
/// `safe_to_warp` and the no-pause detour (nopause.rs).
pub fn load_pending() -> bool {
    read_ptr(module_base() + GAME_MAN_RVA).and_then(|gm| read::<u8>(gm + GAME_MAN_LOAD_REQUEST)) != Some(0)
}

/// A warp may start now (§3.3). `in_world_for` = time since the player appeared
/// after the last load. The `Err` is the reason, for the log.
/// [LIVE] If "load pending" or "menu" never clear in normal play (GameMan+0x10 or the
/// menu mask behave differently than decompiled), warps wait the full 20 s and give up.
pub fn safe_to_warp(in_world_for: std::time::Duration) -> Result<(), &'static str> {
    if load_pending() {
        return Err("load pending");
    }
    if let Some(why) = world_blocker(in_world_for) {
        return Err(why);
    }
    if !talk_idle() {
        return Err("talk");
    }
    // Our own cutscene: a warp now would cut it off (cutscene.rs). A peer's cutscene only
    // pauses the wait's give-up clock (`WarpWait`).
    if crate::cutscene::active() {
        return Err("cutscene");
    }
    let last = LAST_WARP_MS.load(std::sync::atomic::Ordering::Relaxed);
    if last != 0 && now_ms().saturating_sub(last) < WARP_COOLDOWN_MS {
        return Err("recent warp");
    }
    Ok(())
}

/// A reward may be given now: alive, in the real world, no menu (§4.2).
pub fn safe_for_reward(in_world_for: std::time::Duration) -> Result<(), &'static str> {
    world_blocker(in_world_for).map_or(Ok(()), Err)
}

/// The idol we last rested at (§3.2), `DEFAULT_IDOL` if the game has none.
pub fn last_idol() -> i32 {
    let v = read_ptr(module_base() + GAME_MAN_RVA)
        .and_then(|gm| match read::<i32>(gm + GAME_MAN_LAST_IDOL)? {
            v if v >= 0 => Some(v),
            _ => read::<i32>(gm + GAME_MAN_LAST_IDOL_ALT),
        })
        .unwrap_or(-1);
    match v - 1000 {
        idol if idol >= 0 => idol,
        _ => DEFAULT_IDOL,
    }
}

/// Param index of `idol` in the travel menu's table, -1 if it has no row (then
/// `0x68E870` skips its param step, as the travel menu does). [LIVE] the two
/// getters are called without arguments, as in the decompiled `0x6795F0`.
pub fn idol_param_index(idol: i32) -> i32 {
    static BAD_TABLE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let base = module_base();
    let begin: extern "C" fn() -> usize = unsafe { std::mem::transmute(base + IDOL_TABLE_BEGIN_RVA) };
    let end: extern "C" fn() -> usize = unsafe { std::mem::transmute(base + IDOL_TABLE_END_RVA) };
    let (b, e) = (begin(), end());
    if b < 0x10000 || e <= b || (e - b) % IDOL_ROW != 0 || (e - b) / IDOL_ROW > IDOL_ROW_MAX {
        if !BAD_TABLE_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::log::line(&format!("warp: idol table looks wrong (begin {b:#x}, end {e:#x}), using index -1"));
        }
        return -1;
    }
    (b..e)
        .step_by(IDOL_ROW)
        .find(|&row| read::<i32>(row + 4) == Some(idol))
        .and_then(|row| read::<i16>(row + 22))
        .map_or(-1, i32::from)
}

/// Starts the game's own warp to `idol` (a loading screen follows). Call only
/// when `safe_to_warp` said yes, on the game thread.
pub fn warp_to_idol(idol: i32) {
    let index = idol_param_index(idol);
    let warp: extern "C" fn(usize, i64, i32) = unsafe { std::mem::transmute(module_base() + WARP_RVA) };
    warp(0, idol as i64, index);
    LAST_WARP_MS.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
    crate::log::line(&format!("warp: to idol {idol} (param index {index})"));
}

/// What a caller does with `WarpWait::poll`.
pub enum WarpGate {
    /// Warp now.
    Go,
    /// Not safe yet, ask again next frame.
    Wait,
    /// Still not safe after `WARP_WAIT_MAX`: forget the warp.
    GaveUp,
}

/// Retry timer for a warp that the gate does not allow yet: asked every frame
/// for up to 20 s, one log line per second.
#[derive(Default)]
pub struct WarpWait {
    since: Option<std::time::Instant>,
    logged_at: Option<std::time::Instant>,
    /// The last poll, for pausing the give-up clock while somebody is in a cutscene.
    last_poll: Option<std::time::Instant>,
}

impl WarpWait {
    pub fn poll(&mut self, in_world_for: std::time::Duration) -> WarpGate {
        self.poll_at(std::time::Instant::now(), crate::cutscene::any_in_cutscene(), in_world_for)
    }

    /// `poll` with the clock and "somebody (us or a peer) is in a cutscene" given. While that
    /// holds the 20 s give-up clock does not run: a cutscene is not a reason to forget the warp.
    pub fn poll_at(&mut self, now: std::time::Instant, paused: bool, in_world_for: std::time::Duration) -> WarpGate {
        let Err(why) = safe_to_warp(in_world_for) else {
            self.reset();
            return WarpGate::Go;
        };
        let since = self.since.get_or_insert(now);
        if paused && let Some(last) = self.last_poll {
            crate::cutscene::pause(since, now.saturating_duration_since(last));
        }
        self.last_poll = Some(now);
        let since = *since;
        if now.saturating_duration_since(since) >= WARP_WAIT_MAX {
            crate::log::line(&format!("warp: gave up ({why})"));
            self.reset();
            return WarpGate::GaveUp;
        }
        if self.logged_at.is_none_or(|t| t.elapsed().as_secs() >= 1) {
            self.logged_at = Some(std::time::Instant::now());
            crate::log::line(&format!("warp: waiting ({why})"));
        }
        WarpGate::Wait
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Size of the supported `sekiro.exe` (Steam 1.06 = 1.6.0.0, build 5794815): 68,005,144 bytes
/// [VAR: WEB-PRIOR-ART.md, and the local Steam install measured 2026-10-24].
pub const SUPPORTED_EXE_SIZE: u64 = 68_005_144;

/// Verdict on the exe file size (`None` = could not read it: not blocking, the old
/// fromsoftware-rs panic guard in `init` still catches a foreign exe).
pub fn exe_size_verdict(size: Option<u64>) -> Result<(), String> {
    match size {
        Some(SUPPORTED_EXE_SIZE) | None => Ok(()),
        Some(n) => Err(format!("sekiro.exe is {n} bytes, the supported 1.06 is {SUPPORTED_EXE_SIZE}")),
    }
}

/// Is the running exe the supported Sekiro 1.06? Checked before ANY hook or memory patch.
pub fn check_exe() -> Result<(), String> {
    let size = std::env::current_exe().ok().and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
    exe_size_verdict(size)
}

#[cfg(test)]
mod exe_tests {
    use super::*;

    #[test]
    fn only_the_1_06_size_passes() {
        assert!(exe_size_verdict(Some(SUPPORTED_EXE_SIZE)).is_ok());
        assert!(exe_size_verdict(None).is_ok());
        let e = exe_size_verdict(Some(67_000_000)).unwrap_err();
        assert!(e.contains("67000000") && e.contains("68005144"));
    }
}

#[cfg(test)]
mod warp_wait_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// No game memory in a test: `safe_to_warp` always says no, so the wait is on its clock.
    fn poll_for(w: &mut WarpWait, t0: Instant, from_ms: u64, to_ms: u64, paused: bool) -> WarpGate {
        let mut last = WarpGate::Wait;
        for ms in (from_ms..=to_ms).step_by(100) {
            last = w.poll_at(t0 + Duration::from_millis(ms), paused, Duration::from_secs(60));
            if matches!(last, WarpGate::GaveUp) {
                break;
            }
        }
        last
    }

    #[test]
    fn the_give_up_clock_stops_while_somebody_is_in_a_cutscene() {
        let t0 = Instant::now();
        // Control: 21 s without a cutscene gives up.
        let mut w = WarpWait::default();
        assert!(matches!(poll_for(&mut w, t0, 0, 21_000, false), WarpGate::GaveUp));
        // 15 s waiting, 30 s of somebody's cutscene, then 4.8 s: 19.8 s counted, still waiting.
        let mut w = WarpWait::default();
        assert!(matches!(poll_for(&mut w, t0, 0, 15_000, false), WarpGate::Wait));
        assert!(matches!(poll_for(&mut w, t0, 15_100, 45_000, true), WarpGate::Wait));
        assert!(matches!(poll_for(&mut w, t0, 45_100, 49_800, false), WarpGate::Wait));
        // ... and the rest runs out after the cutscene.
        assert!(matches!(poll_for(&mut w, t0, 49_900, 52_000, false), WarpGate::GaveUp));
    }
}
