//! Mid-function hook on the per-frame physics position store.
//!
//! At `sekiro.exe+0xBC4796` the game runs `movaps [rsi+0x80], xmm6`
//! (`0F 29 B6 80 00 00 00`), writing every character's position from Havok into
//! its ChrPhysicsModule (rsi). Writes we make earlier in the frame are
//! overwritten there; Phase 1a test 2026-10-22: memory matched, the wolf did
//! not move on screen. SekiroTool's noclip hooks this same instruction
//! (`Hooks.UpdateCoords`, original bytes listed in UtilityService.cs).
//!
//! Our stub: if rsi == PUPPET_PHYSICS, replace xmm6 with TARGET_POS, then run
//! the original store and jump back.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::log;

const UPDATE_COORDS_RVA: usize = 0xBC4796;
const ORIGINAL: [u8; 7] = [0x0F, 0x29, 0xB6, 0x80, 0x00, 0x00, 0x00];

// Stub layout (one page): data first, then code.
// Target table: TARGETS entries of 32 bytes { physics: u64, _pad: u64, pos: [f32; 4] }.
// Entry 0 is the friend's ghost; 1.. are host-driven enemies on the client.
const DATA_TABLE: usize = 0x100;
pub const TARGETS: usize = 64;
const ENTRY: usize = 32;
// Code lives on its own page: writing data on a page that also holds running
// code triggers self-modifying-code pipeline flushes on every write (FPS
// collapse with ghosts, 2026-10-22).
const CODE: usize = 0x1000;

/// Address of the stub page; 0 until installed.
static STUB: AtomicUsize = AtomicUsize::new(0);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn VirtualAlloc(addr: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void;
    fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
    fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
}

const MEM_COMMIT_RESERVE: u32 = 0x1000 | 0x2000;
const PAGE_EXECUTE_READWRITE: u32 = 0x40;

/// Finds a page within rel32 range of the module (2 pages: data, then code).
fn alloc_near(target: usize) -> Option<usize> {
    alloc_near_sized(target, 0x2000)
}

// --- Near arena (live 2026-10-23: "no memory near the module for the fake session") ---
//
// Every stub used to take its own 64 KB-granular VirtualAlloc region, searched only
// downwards and only 1 GB, and the host ran out. Now one 4 MB RWX arena is reserved
// near the module on the first allocation (hook::install, at start, before anything
// else) and every stub / fake page is carved from it, page aligned, never reused (so
// it is zeroed, and data and code of different users never share a page). If it is
// full, a region is searched in the whole +-1.75 GB window, both directions.

const ARENA_SIZE: usize = 4 << 20;
/// Distance from the module base an allocation may have: every site lies within
/// ~0x5000000 of the base, so this keeps every rel32 in range.
const NEAR_WINDOW: usize = 0x7000_0000;
static ARENA: AtomicUsize = AtomicUsize::new(0);
static ARENA_USED: AtomicUsize = AtomicUsize::new(0);
static ARENA_COUNT: AtomicUsize = AtomicUsize::new(0);
static OUTSIDE_COUNT: AtomicUsize = AtomicUsize::new(0);
static ARENA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A fresh RWX region of `size` bytes within `NEAR_WINDOW` of `target`, nearest first,
/// both directions (64 KB steps: VirtualAlloc fails fast on used addresses).
fn find_free_near(target: usize, size: usize) -> Option<usize> {
    let center = target & !0xFFFF;
    let mut d = 0x10000;
    while d < NEAR_WINDOW {
        for addr in [center.checked_sub(d), center.checked_add(d)].into_iter().flatten() {
            let p = unsafe { VirtualAlloc(addr as *mut c_void, size, MEM_COMMIT_RESERVE, PAGE_EXECUTE_READWRITE) };
            if !p.is_null() {
                return Some(p as usize);
            }
        }
        d += 0x10000;
    }
    None
}

fn near(p: usize, size: usize, target: usize) -> bool {
    p.abs_diff(target) < NEAR_WINDOW && (p + size).abs_diff(target) < NEAR_WINDOW
}

/// `size` bytes (rounded up to pages) from the arena, created on first use.
fn carve(size: usize) -> Option<usize> {
    let size = size.div_ceil(0x1000) * 0x1000;
    let _g = ARENA_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut base = ARENA.load(Ordering::Acquire);
    if base == 0 {
        base = find_free_near(crate::game::module_base(), ARENA_SIZE)?;
        ARENA.store(base, Ordering::Release);
        log::line(&format!(
            "hook: near arena {base:#x}..{:#x} ({} KB) for stubs and fake pages",
            base + ARENA_SIZE,
            ARENA_SIZE / 1024
        ));
    }
    let used = ARENA_USED.load(Ordering::Relaxed);
    if used + size > ARENA_SIZE {
        return None;
    }
    ARENA_USED.store(used + size, Ordering::Relaxed);
    ARENA_COUNT.fetch_add(1, Ordering::Relaxed);
    Some(base + used)
}

/// Same, for `size` bytes (a multiple of 0x1000): the arena first, else a region of its own.
fn alloc_near_sized(target: usize, size: usize) -> Option<usize> {
    if let Some(p) = carve(size)
        && near(p, size, target)
    {
        return Some(p);
    }
    let p = find_free_near(target, size)?;
    OUTSIDE_COUNT.fetch_add(1, Ordering::Relaxed);
    log::line(&format!("hook: near arena full or far, own region {p:#x} ({size:#x} bytes) for {target:#x}"));
    Some(p)
}

/// One line: the arena's range and how much of it is in use.
pub fn log_arena() {
    let base = ARENA.load(Ordering::Acquire);
    log::line(&format!(
        "hook: near arena {base:#x}..{:#x}: {} of {} KB used by {} allocations, {} outside it",
        base + ARENA_SIZE,
        ARENA_USED.load(Ordering::Relaxed) / 1024,
        ARENA_SIZE / 1024,
        ARENA_COUNT.load(Ordering::Relaxed),
        OUTSIDE_COUNT.load(Ordering::Relaxed)
    ));
}

fn rel32(from_next: usize, to: usize) -> Option<[u8; 4]> {
    let d = to as i64 - from_next as i64;
    i32::try_from(d).ok().map(i32::to_le_bytes)
}

pub fn install() -> bool {
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let site = base + UPDATE_COORDS_RVA;

    let found: [u8; 7] = unsafe { (site as *const [u8; 7]).read() };
    if found != ORIGINAL {
        log::line(&format!("hook: UpdateCoords bytes differ {found:02x?}, NOT installing"));
        return false;
    }
    let Some(stub) = alloc_near(site) else {
        log::line("hook: no memory near module, NOT installing");
        return false;
    };

    // Build the stub code.
    let code = stub + CODE;
    let mut c: Vec<u8> = Vec::new();
    c.push(0x50); // push rax
    c.push(0x51); // push rcx
    let at = code + c.len();
    c.extend([0x48, 0x8D, 0x05]); // lea rax, [rip+disp32] -> table
    c.extend(rel32(at + 7, stub + DATA_TABLE).unwrap());
    c.extend([0xB9, TARGETS as u8, 0, 0, 0]); // mov ecx, TARGETS
    // loop:
    c.extend([0x48, 0x39, 0x30]); // cmp [rax], rsi
    c.extend([0x74, 0x0A]); // je found (+10)
    c.extend([0x48, 0x83, 0xC0, ENTRY as u8]); // add rax, ENTRY
    c.extend([0xFF, 0xC9]); // dec ecx
    c.extend([0x75, 0xF3]); // jnz loop (-13)
    c.extend([0xEB, 0x04]); // jmp done (+4)
    // found:
    c.extend([0x0F, 0x10, 0x70, 0x10]); // movups xmm6, [rax+0x10]
    // done:
    c.push(0x59); // pop rcx
    c.push(0x58); // pop rax
    c.extend(ORIGINAL); // movaps [rsi+0x80], xmm6
    let at = code + c.len();
    c.push(0xE9); // jmp back
    let Some(back) = rel32(at + 5, site + ORIGINAL.len()) else {
        log::line("hook: stub out of jump range, NOT installing");
        return false;
    };
    c.extend(back);

    unsafe {
        std::ptr::write_bytes(stub as *mut u8, 0, DATA_TABLE + TARGETS * ENTRY);
        std::ptr::copy_nonoverlapping(c.as_ptr(), code as *mut u8, c.len());
    }

    // Patch the site: jmp rel32 to stub + 2 NOPs.
    let Some(to_stub) = rel32(site + 5, code) else {
        log::line("hook: site out of jump range, NOT installing");
        return false;
    };
    let mut patch = [0x90u8; 7];
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&to_stub);

    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, 7, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            log::line("hook: VirtualProtect failed, NOT installing");
            return false;
        }
        std::ptr::copy_nonoverlapping(patch.as_ptr(), site as *mut u8, 7);
        VirtualProtect(site as *mut c_void, 7, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 7);
    }

    STUB.store(stub, Ordering::Release);
    log::line(&format!("hook: UpdateCoords hook installed at {site:#x}, stub {stub:#x}"));
    true
}

pub fn installed() -> bool {
    STUB.load(Ordering::Acquire) != 0
}

/// Forces the character whose physics module is `physics` to `pos` (w kept),
/// using table entry `index` (0 = ghost, 1.. = enemies).
pub fn set_target(index: usize, physics: usize, pos: [f32; 4]) {
    let stub = STUB.load(Ordering::Acquire);
    if stub == 0 || index >= TARGETS {
        return;
    }
    // Last line of defence: the stub copies this straight into Havok's
    // output, and a NaN/huge position there reaches the GPU (see
    // game::sane_pos). Drop the entry instead of pinning garbage.
    if physics < 0x10000 || !crate::game::sane_pos([pos[0], pos[1], pos[2]]) || !pos[3].is_finite() {
        clear_target(index);
        return;
    }
    let e = stub + DATA_TABLE + index * ENTRY;
    unsafe {
        // Switching the entry to another character: unkey it first, else the
        // old one would get the new target for the moment between the stores.
        let old = (e as *const usize).read_volatile();
        if old != physics && old != 0 {
            (e as *mut usize).write_volatile(0);
        }
        // Position first, then the key, so a reader never sees a new key with a stale position.
        ((e + 0x10) as *mut [f32; 4]).write_volatile(pos);
        (e as *mut usize).write_volatile(physics);
    }
}

pub fn clear_target(index: usize) {
    let stub = STUB.load(Ordering::Acquire);
    if stub != 0 && index < TARGETS {
        unsafe { ((stub + DATA_TABLE + index * ENTRY) as *mut usize).write_volatile(0) };
    }
}

/// Unkeys every entry (ghost and enemies). Called whenever the world is
/// rebuilt: the stub only compares the stored physics pointers, but after a
/// load the game reuses those addresses for new characters (possibly our own
/// player), which would then be pinned to an old target.
pub fn clear_all_targets() {
    for i in 0..TARGETS {
        clear_target(i);
    }
}

pub fn set_puppet(physics: usize, pos: [f32; 4]) {
    set_target(0, physics, pos);
}

pub fn clear_puppet() {
    clear_target(0);
}

// --- Null guard for debug-spawned c0000 ---------------------------------
//
// IDA (re/out3): sub_1409CBE40(pg, out, slot) reads equipment from a
// PlayerGameData-like struct `pg` (rdi) with `mov eax, [rdi + slot*4 + 0x24]`.
// A c0000 created by CSDebugChrCreator has no such struct, so pg == null and
// the game crashed at sekiro.exe+0x9CBF09 (read of 0x24), 2026-10-22.
// The guard remembers the last non-null pg (in practice the local player's)
// and lends it to a null caller, so a debug c0000 wears the player's outfit
// (with 0xFF "no equipment" it spawned invisible, 2026-10-22 screenshot).
// Before any player read happened it falls back to the 0xFF dummy.

const NULL_GUARD_RVA: usize = 0x9CBF05;
// lea rbx, [rdi+r14*4] ; mov eax, [rbx+24h]
const NULL_GUARD_ORIGINAL: [u8; 7] = [0x4A, 0x8D, 0x1C, 0xB7, 0x8B, 0x43, 0x24];
const NG_DUMMY: usize = 0x00; // 0xE0 bytes of 0xFF
const NG_LAST_RDI: usize = 0xE0; // u64: last non-null rdi (the player's equip struct)
const NG_HITS: usize = 0xF0; // u32 counter
const NG_CODE: usize = 0x1000;

static NULL_GUARD: AtomicUsize = AtomicUsize::new(0);

fn patch_jmp(site: usize, to: usize, len: usize) -> bool {
    let Some(rel) = rel32(site + 5, to) else { return false };
    let mut patch = vec![0x90u8; len];
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&rel);
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, len, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(patch.as_ptr(), site as *mut u8, len);
        VirtualProtect(site as *mut c_void, len, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, len);
    }
    true
}

pub fn install_null_guard() -> bool {
    if NULL_GUARD.load(Ordering::Acquire) != 0 {
        return true;
    }
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let site = base + NULL_GUARD_RVA;
    let found: [u8; 7] = unsafe { (site as *const [u8; 7]).read() };
    if found != NULL_GUARD_ORIGINAL {
        log::line(&format!("hook: null-guard bytes differ {found:02x?}, NOT installing"));
        return false;
    }
    let Some(stub) = alloc_near(site) else {
        log::line("hook: null-guard no memory near module");
        return false;
    };
    let code = stub + NG_CODE;
    let back = site + NULL_GUARD_ORIGINAL.len();
    let mut c: Vec<u8> = Vec::new();
    c.extend([0x48, 0x85, 0xFF]); // test rdi, rdi
    c.extend([0x74, 0x1C]); // jz null_path (skip cmp+jz+mov+lea+mov+jmp = 28 bytes)
    // Store rdi only when it changed: this runs thousands of times per frame.
    let at = code + c.len();
    c.extend([0x48, 0x39, 0x3D]); // cmp [rip+disp32], rdi
    c.extend(rel32(at + 7, stub + NG_LAST_RDI).unwrap());
    c.extend([0x74, 0x07]); // je skip_store
    let at = code + c.len();
    c.extend([0x48, 0x89, 0x3D]); // mov [rip+disp32], rdi -> last_rdi
    c.extend(rel32(at + 7, stub + NG_LAST_RDI).unwrap());
    // skip_store:
    c.extend(NULL_GUARD_ORIGINAL); // lea rbx,[rdi+r14*4]; mov eax,[rbx+24h]
    let at = code + c.len();
    c.push(0xE9);
    c.extend(rel32(at + 5, back).unwrap());
    // null_path:
    let at = code + c.len();
    c.extend([0xF0, 0xFF, 0x05]); // lock inc dword [rip+disp32] -> hits
    c.extend(rel32(at + 7, stub + NG_HITS).unwrap());
    let at = code + c.len();
    c.extend([0x48, 0x8B, 0x1D]); // mov rbx, [rip+disp32] -> last_rdi
    c.extend(rel32(at + 7, stub + NG_LAST_RDI).unwrap());
    c.extend([0x48, 0x85, 0xDB]); // test rbx, rbx
    c.extend([0x75, 0x07]); // jnz have_pg (skip 7-byte lea)
    let at = code + c.len();
    c.extend([0x48, 0x8D, 0x1D]); // lea rbx, [rip+disp32] -> dummy
    c.extend(rel32(at + 7, stub + NG_DUMMY).unwrap());
    // have_pg:
    c.extend([0x4A, 0x8D, 0x1C, 0xB3]); // lea rbx, [rbx+r14*4]
    c.extend([0x8B, 0x43, 0x24]); // mov eax, [rbx+24h]
    let at = code + c.len();
    c.push(0xE9);
    c.extend(rel32(at + 5, back).unwrap());

    unsafe {
        std::ptr::write_bytes(stub as *mut u8, 0xFF, NG_LAST_RDI);
        std::ptr::write_bytes((stub + NG_LAST_RDI) as *mut u8, 0, 0x14);
        std::ptr::copy_nonoverlapping(c.as_ptr(), code as *mut u8, c.len());
    }
    if !patch_jmp(site, code, NULL_GUARD_ORIGINAL.len()) {
        log::line("hook: null-guard patch failed");
        return false;
    }
    NULL_GUARD.store(stub, Ordering::Release);
    log::line(&format!("hook: null-guard installed at {site:#x}"));
    true
}

/// How many times the null guard saved a read (0 if not installed).
pub fn null_guard_hits() -> u32 {
    let stub = NULL_GUARD.load(Ordering::Acquire);
    if stub == 0 {
        return 0;
    }
    unsafe { ((stub + NG_HITS) as *const u32).read_volatile() }
}

// --- Event-flag write hook (shared progress) ------------------------------
//
// SetEventFlag `0x6C4520(SprjEventFlagMan* fm, u32 id, bool on, bool a4)`
// (RE-FINDINGS-2 §1, 88 callers, every single-bit flag write goes through it).
// Entry hook: claim a slot with `lock xadd`, write {id, on, bank} and then
// the slot's sequence number last (x86 keeps stores in order, so a reader
// that sees seq == n+1 sees the whole entry), run the relocated first 6
// bytes (`push rbp; push r12; push r13`, no RIP-relative code) and jump back
// to +6. rax, r10, r11 are scratch at a function entry (r10/r11 carry no
// arguments). Data page (stub+0) and code page (stub+0x1000) are separate:
// writing data on a code page made the CPU flush its pipeline (SMC).

const SET_FLAG_RVA: usize = 0x6C4520;
const SET_FLAG_PROLOGUE: [u8; 6] = [0x40, 0x55, 0x41, 0x54, 0x41, 0x55];
const FR_INDEX: usize = 0x00; // u32, total entries ever claimed
const FR_ENTRIES: usize = 0x10; // FLAG_RING x { u32 seq, u32 id, u8 on, u8 bank }
const FR_STRIDE: usize = 0x10;
/// Live 7.4 (2026-10-24): 128 overflowed during every rest (`lost` +~420 on both machines:
/// the rest's event restart writes hundreds of flags in a frame or two). 2048 * 16 =
/// 0x8000: entries end at 0x8010, below the code page. Must stay a power of two (the stub
/// masks the index, and `n % FLAG_RING` must agree across the u32 wrap).
pub const FLAG_RING: u32 = 2048;
const FR_CODE: usize = 0x9000;
/// Data pages + one code page.
const FR_ALLOC: usize = FR_CODE + 0x1000;
const _: () = assert!(FLAG_RING.is_power_of_two() && FR_ENTRIES + FR_STRIDE * FLAG_RING as usize <= FR_CODE);
/// SprjEventFlagMan active bank (0 = the real world, 1 = Remnant/reflection).
const FM_BANK: u32 = 0x220;

static FLAG_RING_STUB: AtomicUsize = AtomicUsize::new(0);

pub fn install_flag_hook() -> bool {
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let site = base + SET_FLAG_RVA;
    let found: [u8; 8] = unsafe { (site as *const [u8; 8]).read() };
    if found[..6] != SET_FLAG_PROLOGUE || site % 8 != 0 {
        log::line(&format!("hook: SetEventFlag prologue differs {found:02x?}, NOT installing"));
        return false;
    }
    let Some(stub) = alloc_near_sized(site, FR_ALLOC) else {
        log::line("hook: flag hook no memory near module");
        return false;
    };
    let code = stub + FR_CODE;
    let mut c: Vec<u8> = Vec::new();
    c.push(0x50); // push rax
    c.extend([0xB8, 1, 0, 0, 0]); // mov eax, 1
    let at = code + c.len();
    c.extend([0xF0, 0x0F, 0xC1, 0x05]); // lock xadd [rip+disp32], eax  (eax = n)
    c.extend(rel32(at + 8, stub + FR_INDEX).unwrap());
    c.extend([0x44, 0x8D, 0x50, 0x01]); // lea r10d, [rax+1]            (seq)
    c.push(0x25); // and eax, imm32 (FLAG_RING-1; imm8 is sign-extended, max 127)
    c.extend((FLAG_RING - 1).to_le_bytes());
    c.extend([0xC1, 0xE0, 0x04]); // shl eax, 4                         (* FR_STRIDE)
    let at = code + c.len();
    c.extend([0x4C, 0x8D, 0x1D]); // lea r11, [rip+disp32] -> entries
    c.extend(rel32(at + 7, stub + FR_ENTRIES).unwrap());
    c.extend([0x49, 0x01, 0xC3]); // add r11, rax
    c.extend([0x41, 0xC7, 0x03, 0, 0, 0, 0]); // mov dword [r11], 0  (invalidate before payload)
    c.extend([0x41, 0x89, 0x53, 0x04]); // mov [r11+4], edx            (id)
    c.extend([0x45, 0x88, 0x43, 0x08]); // mov [r11+8], r8b            (on)
    c.extend([0x8A, 0x81]); // mov al, [rcx+FM_BANK]
    c.extend(FM_BANK.to_le_bytes());
    c.extend([0x41, 0x88, 0x43, 0x09]); // mov [r11+9], al             (bank)
    c.extend([0x45, 0x89, 0x13]); // mov [r11], r10d                  (seq, last)
    c.push(0x58); // pop rax
    c.extend(SET_FLAG_PROLOGUE); // relocated: push rbp; push r12; push r13
    let at = code + c.len();
    c.push(0xE9); // jmp site+6
    c.extend(rel32(at + 5, site + SET_FLAG_PROLOGUE.len()).unwrap());

    unsafe {
        std::ptr::write_bytes(stub as *mut u8, 0, FR_ENTRIES + FR_STRIDE * FLAG_RING as usize);
        std::ptr::copy_nonoverlapping(c.as_ptr(), code as *mut u8, c.len());
        FlushInstructionCache(GetCurrentProcess(), code as *const c_void, c.len());
    }
    FLAG_RING_STUB.store(stub, Ordering::Release);

    // Game threads may be inside SetEventFlag right now: replace the first 8
    // bytes with one aligned store (jmp rel32; nop; original bytes 6..8).
    let Some(rel) = rel32(site + 5, code) else { return false };
    let mut patch = found;
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&rel);
    patch[5] = 0x90;
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, 8, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            log::line("hook: flag hook patch failed");
            FLAG_RING_STUB.store(0, Ordering::Release);
            return false;
        }
        (site as *mut u64).write_volatile(u64::from_le_bytes(patch));
        VirtualProtect(site as *mut c_void, 8, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 8);
    }
    log::line(&format!("hook: SetEventFlag hook installed at {site:#x}"));
    true
}

/// Total flag writes claimed so far (wraps).
pub fn flag_ring_count() -> u32 {
    let stub = FLAG_RING_STUB.load(Ordering::Acquire);
    if stub == 0 {
        return 0;
    }
    unsafe { ((stub + FR_INDEX) as *const u32).read_volatile() }
}

pub enum FlagEntry {
    /// (flag id, on, bank)
    Ready(u32, bool, u8),
    /// Slot claimed but not written yet: stop and retry next frame.
    Pending,
    /// Overwritten by a newer write (we fell 128 behind): skip.
    Lost,
}

/// Write number `n` of the ring.
pub fn flag_ring_entry(n: u32) -> FlagEntry {
    let stub = FLAG_RING_STUB.load(Ordering::Acquire);
    if stub == 0 {
        return FlagEntry::Lost;
    }
    let e = stub + FR_ENTRIES + FR_STRIDE * (n % FLAG_RING) as usize;
    unsafe {
        let seq = (e as *const u32).read_volatile();
        let want = n.wrapping_add(1);
        if seq != want {
            return if (seq.wrapping_sub(want) as i32) > 0 { FlagEntry::Lost } else { FlagEntry::Pending };
        }
        let id = ((e + 4) as *const u32).read_volatile();
        let on = ((e + 8) as *const u8).read_volatile() != 0;
        let bank = ((e + 9) as *const u8).read_volatile();
        // Re-check: the writer may have lapped us while we read.
        if (e as *const u32).read_volatile() != want {
            return FlagEntry::Lost;
        }
        FlagEntry::Ready(id, on, bank)
    }
}

// --- Behavior (hkb) event hook (replay-style animation sync) ---------------
//
// `0xFF1D10 bool raise(HkPair* p, int eventIdx)` queues a behavior event on
// graph `p->graph` (RE-FINDINGS-2 § Path B); every attack, roll, jump... of
// every character goes through it. Entry hook records {seq, idx, graph} the
// same way as the flag hook (seq written last). The game task keeps the ones
// for graphs it cares about (the player's; the host's enemies).

const RAISE_RVA: usize = 0xFF1D10;
const RAISE_PROLOGUE: [u8; 6] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x40]; // push rbx; sub rsp,40h
const HR_INDEX: usize = 0x00;
const HR_ENTRIES: usize = 0x10; // HKB_RING x { u32 seq, i32 idx, u64 graph }
const HR_STRIDE: usize = 0x10;
pub const HKB_RING: u32 = 1024; // 16 KB: ends at 0x4010, code page after it
const HR_CODE: usize = 0x5000;
const HR_SIZE: usize = 0x6000;

static HKB_RING_STUB: AtomicUsize = AtomicUsize::new(0);

/// Replaces the first 8 bytes at an 8-aligned `site` with `jmp code; nop` and
/// the original bytes 6..8, in one store (game threads may be inside it).
fn patch_jmp_atomic(site: usize, code: usize, original: [u8; 8]) -> bool {
    let Some(rel) = rel32(site + 5, code) else { return false };
    let mut patch = original;
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&rel);
    patch[5] = 0x90;
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, 8, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        (site as *mut u64).write_volatile(u64::from_le_bytes(patch));
        VirtualProtect(site as *mut c_void, 8, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 8);
    }
    true
}

pub fn install_hkb_hook() -> bool {
    if HKB_RING_STUB.load(Ordering::Acquire) != 0 {
        return true;
    }
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    let site = base + RAISE_RVA;
    let found: [u8; 8] = unsafe { (site as *const [u8; 8]).read() };
    if found[..6] != RAISE_PROLOGUE || site % 8 != 0 {
        log::line(&format!("hook: hkb raise prologue differs {found:02x?}, NOT installing"));
        return false;
    }
    let Some(stub) = alloc_near_sized(site, HR_SIZE) else {
        log::line("hook: hkb hook no memory near module");
        return false;
    };
    let code = stub + HR_CODE;
    let mut c: Vec<u8> = Vec::new();
    c.push(0x50); // push rax
    c.extend([0xB8, 1, 0, 0, 0]); // mov eax, 1
    let at = code + c.len();
    c.extend([0xF0, 0x0F, 0xC1, 0x05]); // lock xadd [rip+disp32], eax  (eax = n)
    c.extend(rel32(at + 8, stub + HR_INDEX).unwrap());
    c.extend([0x44, 0x8D, 0x50, 0x01]); // lea r10d, [rax+1]            (seq)
    c.push(0x25); // and eax, HKB_RING-1
    c.extend((HKB_RING - 1).to_le_bytes());
    c.extend([0xC1, 0xE0, 0x04]); // shl eax, 4                         (* HR_STRIDE)
    let at = code + c.len();
    c.extend([0x4C, 0x8D, 0x1D]); // lea r11, [rip+disp32] -> entries
    c.extend(rel32(at + 7, stub + HR_ENTRIES).unwrap());
    c.extend([0x49, 0x01, 0xC3]); // add r11, rax
    c.extend([0x41, 0xC7, 0x03, 0, 0, 0, 0]); // mov dword [r11], 0  (invalidate before payload)
    c.extend([0x48, 0x8B, 0x01]); // mov rax, [rcx]                     (p->graph)
    c.extend([0x49, 0x89, 0x43, 0x08]); // mov [r11+8], rax
    c.extend([0x41, 0x89, 0x53, 0x04]); // mov [r11+4], edx            (idx)
    c.extend([0x45, 0x89, 0x13]); // mov [r11], r10d                  (seq, last)
    c.push(0x58); // pop rax
    c.extend(RAISE_PROLOGUE); // relocated: push rbx; sub rsp, 40h
    let at = code + c.len();
    c.push(0xE9); // jmp site+6
    c.extend(rel32(at + 5, site + RAISE_PROLOGUE.len()).unwrap());

    unsafe {
        std::ptr::copy_nonoverlapping(c.as_ptr(), code as *mut u8, c.len());
        FlushInstructionCache(GetCurrentProcess(), code as *const c_void, c.len());
    }
    HKB_RING_STUB.store(stub, Ordering::Release);
    if !patch_jmp_atomic(site, code, found) {
        log::line("hook: hkb hook patch failed");
        HKB_RING_STUB.store(0, Ordering::Release);
        return false;
    }
    log::line(&format!("hook: hkb raise hook installed at {site:#x}"));
    true
}

pub fn hkb_ring_count() -> u32 {
    let stub = HKB_RING_STUB.load(Ordering::Acquire);
    if stub == 0 {
        return 0;
    }
    unsafe { ((stub + HR_INDEX) as *const u32).read_volatile() }
}

pub enum HkbEntry {
    /// (graph, event index)
    Ready(usize, i32),
    Pending,
    Lost,
}

pub fn hkb_ring_entry(n: u32) -> HkbEntry {
    let stub = HKB_RING_STUB.load(Ordering::Acquire);
    if stub == 0 {
        return HkbEntry::Lost;
    }
    let e = stub + HR_ENTRIES + HR_STRIDE * (n % HKB_RING) as usize;
    unsafe {
        let want = n.wrapping_add(1);
        let seq = (e as *const u32).read_volatile();
        if seq != want {
            return if (seq.wrapping_sub(want) as i32) > 0 { HkbEntry::Lost } else { HkbEntry::Pending };
        }
        let idx = ((e + 4) as *const i32).read_volatile();
        let graph = ((e + 8) as *const usize).read_volatile();
        if (e as *const u32).read_volatile() != want {
            return HkbEntry::Lost;
        }
        HkbEntry::Ready(graph, idx)
    }
}

// ---------------------------------------------------------------------------
// Movement-vector hook (2026-10-23): the ghost's walk/run vector must be in the
// manipulator right before the game turns it into MoveSpeedLevel / MoveAngle
// (`0xB4AFD0`, called from one site, `0xB49D56`). Written earlier in the frame,
// it was cleared again (live: ghost speed level stayed 0, the ghost slid).
// The call is redirected to `motion::move_wrapper`, which writes the vector
// for the ghost and then runs the original.
// ---------------------------------------------------------------------------

const MOVE_CALL_RVA: usize = 0xB49D56;
const MOVE_FN_RVA: usize = 0xB4AFD0;

pub fn install_move_hook() -> bool {
    let base = crate::game::module_base();
    let site = base + MOVE_CALL_RVA;
    let Some(found) = crate::game::read::<[u8; 5]>(site) else { return false };
    let want = rel32(site + 5, base + MOVE_FN_RVA);
    if found[0] != 0xE8 || want != Some([found[1], found[2], found[3], found[4]]) {
        crate::log::line(&format!("hook: movement call site has unexpected bytes {found:02x?}, not hooking"));
        return false;
    }
    let Some(stub) = alloc_near(site) else {
        crate::log::line("hook: no memory near the module for the movement hook");
        return false;
    };
    // mov rax, imm64; jmp rax
    let mut code = [0u8; 12];
    code[0] = 0x48;
    code[1] = 0xB8;
    code[2..10].copy_from_slice(&(crate::motion::move_wrapper as *const () as usize as u64).to_le_bytes());
    code[10] = 0xFF;
    code[11] = 0xE0;
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    let Some(rel) = rel32(site + 5, stub) else { return false };
    let mut old = 0u32;
    unsafe {
        if VirtualProtect((site + 1) as *mut c_void, 4, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(rel.as_ptr(), (site + 1) as *mut u8, 4);
        VirtualProtect((site + 1) as *mut c_void, 4, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, 5);
    }
    crate::log::line(&format!("hook: movement hook installed at {site:#x}, stub {stub:#x}"));
    true
}

/// Replaces a whole game function with `target` (same signature): the first 5
/// bytes become `jmp rel32` to a near stub `mov rax, target; jmp rax`. Only for
/// small leaf functions we reimplement completely. `expected` = the original
/// first bytes (checked first; nothing is written on a mismatch).
pub fn redirect_function(rva: usize, expected: &[u8], target: usize, name: &str) -> bool {
    let site = crate::game::module_base() + rva;
    let found: Vec<u8> = (0..expected.len()).filter_map(|i| crate::game::read::<u8>(site + i)).collect();
    if found != expected {
        crate::log::line(&format!("hook: {name} at {site:#x} has unexpected bytes {found:02x?}, not hooking"));
        return false;
    }
    let Some(stub) = alloc_near(site) else {
        crate::log::line(&format!("hook: no memory near the module for {name}"));
        return false;
    };
    let mut code = [0u8; 12];
    code[0] = 0x48;
    code[1] = 0xB8;
    code[2..10].copy_from_slice(&(target as u64).to_le_bytes());
    code[10] = 0xFF;
    code[11] = 0xE0;
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    if !patch_jmp(site, stub, 5) {
        return false;
    }
    crate::log::line(&format!("hook: {name} at {site:#x} redirected (stub {stub:#x})"));
    true
}

/// Overwrites code bytes at `rva` if they currently equal `expected`.
#[allow(dead_code)]
pub fn patch_code(rva: usize, expected: &[u8], new: &[u8], name: &str) -> bool {
    let site = crate::game::module_base() + rva;
    let found: Vec<u8> = (0..expected.len()).filter_map(|i| crate::game::read::<u8>(site + i)).collect();
    if found != expected {
        crate::log::line(&format!("hook: {name} at {site:#x} has unexpected bytes {found:02x?}, not patching"));
        return false;
    }
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, new.len(), PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(new.as_ptr(), site as *mut u8, new.len());
        VirtualProtect(site as *mut c_void, new.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, new.len());
    }
    crate::log::line(&format!("hook: {name} patched at {site:#x}"));
    true
}

/// `alloc_near` for other modules (data caves within rel32 range of `target`).
pub fn alloc_near_pub(target: usize) -> Option<usize> {
    alloc_near(target)
}

/// `patch_jmp` for other modules: a 5-byte `jmp rel32` at `site`, the rest of
/// `len` filled with int3 (`len` covers the whole relocated prologue).
pub fn patch_jmp_pub(site: usize, to: usize, len: usize) -> bool {
    let Some(rel) = rel32(site + 5, to) else { return false };
    let mut patch = vec![0xCCu8; len];
    patch[0] = 0xE9;
    patch[1..5].copy_from_slice(&rel);
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(site as *mut c_void, len, PAGE_EXECUTE_READWRITE, &mut old) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(patch.as_ptr(), site as *mut u8, len);
        VirtualProtect(site as *mut c_void, len, old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), site as *const c_void, len);
    }
    true
}

// --- General entry detour --------------------------------------------------
//
// Layout of the allocation (two pages): code page first, data page second.
// - `+0x00` trampoline: the relocated prologue, then `jmp [rip+0]; dq site+len`.
// - `+0x40` stub the site jumps to: stores the caller's return address in the
//   data page, then `mov rax, detour; jmp rax`. The detour therefore sees the
//   original registers and stack (rax, r10, r11 are scratch at a function entry).
// - `+0x1000` the last return address (`detour_caller`), kept off the code page
//   so the store does not trigger self-modifying-code flushes.

const DETOUR_STUB: usize = 0x40;
const DETOUR_DATA: usize = 0x1000;

/// Hooks the entry of the function at `rva`. `expected_prologue` is checked
/// byte for byte and then relocated into the trampoline, so it must end on an
/// instruction boundary and contain nothing position dependent (no RIP-relative
/// operands, no relative jumps/calls). A mismatch installs nothing (logged).
/// `detour` must be an `extern "C" fn` with the original's argument registers;
/// it calls the original through the returned trampoline address. The site
/// gets a 5-byte `jmp rel32` to a near stub, the rest of the prologue is int3.
pub fn install_detour(rva: usize, expected_prologue: &[u8], detour: usize, name: &str) -> Option<usize> {
    install_detour_inner(rva, expected_prologue, detour, name, false)
}

/// `install_detour` for a function with at most two arguments: the stub also copies the
/// caller's return address into `r8`, so the detour gets it as its third argument without
/// the race of `detour_caller` (one slot shared by every thread). `r8` is volatile across a
/// call and no argument of the original, so setting it at the entry changes nothing else.
pub fn install_detour_caller_r8(rva: usize, expected_prologue: &[u8], detour: usize, name: &str) -> Option<usize> {
    install_detour_inner(rva, expected_prologue, detour, name, true)
}

fn install_detour_inner(rva: usize, expected_prologue: &[u8], detour: usize, name: &str, caller_r8: bool) -> Option<usize> {
    let len = expected_prologue.len();
    if !(5..=32).contains(&len) {
        log::line(&format!("hook: {name} prologue length {len} is not usable, not hooking"));
        return None;
    }
    let site = crate::game::module_base() + rva;
    let found: Vec<u8> = (0..len).filter_map(|i| crate::game::read::<u8>(site + i)).collect();
    if found != expected_prologue {
        log::line(&format!("hook: {name} at {site:#x} has unexpected bytes {found:02x?}, not hooking"));
        return None;
    }
    let Some(page) = alloc_near(site) else {
        log::line(&format!("hook: no memory near the module for {name}"));
        return None;
    };
    let mut tramp: Vec<u8> = Vec::with_capacity(len + 14);
    tramp.extend_from_slice(expected_prologue);
    tramp.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]); // jmp [rip+0]
    tramp.extend_from_slice(&((site + len) as u64).to_le_bytes());

    let mut stub: Vec<u8> = Vec::with_capacity(32);
    stub.extend_from_slice(&[0x4C, 0x8B, 0x1C, 0x24]); // mov r11, [rsp]   (return address)
    stub.extend_from_slice(&[0x49, 0xBA]); // mov r10, imm64
    stub.extend_from_slice(&((page + DETOUR_DATA) as u64).to_le_bytes());
    stub.extend_from_slice(&[0x4D, 0x89, 0x1A]); // mov [r10], r11
    if caller_r8 {
        stub.extend_from_slice(&[0x4D, 0x8B, 0xC3]); // mov r8, r11
    }
    stub.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    stub.extend_from_slice(&(detour as u64).to_le_bytes());
    stub.extend_from_slice(&[0xFF, 0xE0]); // jmp rax

    unsafe {
        std::ptr::copy_nonoverlapping(tramp.as_ptr(), page as *mut u8, tramp.len());
        std::ptr::copy_nonoverlapping(stub.as_ptr(), (page + DETOUR_STUB) as *mut u8, stub.len());
        FlushInstructionCache(GetCurrentProcess(), page as *const c_void, DETOUR_STUB + stub.len());
    }
    // Game threads may already run this function: write with them suspended.
    if !patch_jmp_frozen(site, page + DETOUR_STUB, len) {
        log::line(&format!("hook: {name} at {site:#x} patch failed"));
        return None;
    }
    log::line(&format!("hook: {name} at {site:#x} detoured (trampoline {page:#x}, {len} bytes relocated)"));
    Some(page)
}

/// The return address of the last call that went through the detour whose
/// trampoline is `trampoline` (diagnostics; racy across threads).
pub fn detour_caller(trampoline: usize) -> usize {
    if trampoline == 0 {
        return 0;
    }
    unsafe { ((trampoline + DETOUR_DATA) as *const usize).read_volatile() }
}

// --- Writes with the game's threads stopped -------------------------------

/// Code writes with every other thread of the process suspended (nsession review c; shared since the icons review #2):
/// no game thread can be executing a byte we change. Nothing in here allocates or logs
/// while the threads are stopped (one of them may hold the heap lock).
pub mod freeze {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Thread32First(snap: *mut c_void, entry: *mut ThreadEntry) -> i32;
        fn Thread32Next(snap: *mut c_void, entry: *mut ThreadEntry) -> i32;
        fn OpenThread(access: u32, inherit: i32, tid: u32) -> *mut c_void;
        fn SuspendThread(h: *mut c_void) -> u32;
        fn ResumeThread(h: *mut c_void) -> u32;
        fn GetThreadContext(h: *mut c_void, ctx: *mut Ctx) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetCurrentThreadId() -> u32;
        fn GetCurrentProcessId() -> u32;
        fn VirtualProtect(addr: *mut c_void, size: usize, new: u32, old: *mut u32) -> i32;
        fn FlushInstructionCache(process: *mut c_void, addr: *const c_void, size: usize) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn GetModuleHandleExW(flags: u32, addr: *const u16, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, buf: *mut u16, len: u32) -> u32;
    }

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationThread(h: *mut c_void, class: u32, out: *mut c_void, len: u32, ret: *mut u32) -> i32;
    }

    /// FROM_ADDRESS | UNCHANGED_REFCOUNT.
    const GET_MODULE_FROM_ADDRESS: u32 = 0x4 | 0x2;
    /// ThreadQuerySetWin32StartAddress.
    const THREAD_WIN32_START_ADDRESS: u32 = 9;
    /// Budget (ms) for the suspend + GetThreadContext phase of one `write_all_timed` when a caller asks for one:
    /// past it every thread is resumed and the write is reported as busy (the caller tries again later).
    pub const ENGAGE_BUDGET_MS: u64 = 250;

    /// `module.dll+0xOFFSET` of an address (`0x... (no module)` if no module holds it). Allocates:
    /// never call with a thread suspended.
    pub fn module_offset(addr: usize) -> String {
        let mut module: *mut c_void = std::ptr::null_mut();
        // SAFETY: the address is only looked up, never dereferenced.
        let ok = unsafe { GetModuleHandleExW(GET_MODULE_FROM_ADDRESS, addr as *const u16, &mut module) };
        if ok == 0 || module.is_null() {
            return format!("{addr:#x} (no module)");
        }
        let mut buf = [0u16; 260];
        let n = (unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize).min(buf.len());
        let path = String::from_utf16_lossy(&buf[..n]);
        let name = path.rsplit('\\').next().unwrap_or(&path);
        format!("{name}+{:#x}", addr - module as usize)
    }

    /// Start address of a thread (0 = unknown, e.g. it already exited).
    pub fn thread_start_address(tid: u32) -> usize {
        let h = unsafe { OpenThread(THREAD_QUERY_INFORMATION, 0, tid) };
        if h.is_null() {
            return 0;
        }
        let mut addr: usize = 0;
        let st = unsafe {
            NtQueryInformationThread(h, THREAD_WIN32_START_ADDRESS, &mut addr as *mut usize as *mut c_void, std::mem::size_of::<usize>() as u32, std::ptr::null_mut())
        };
        unsafe { CloseHandle(h) };
        if st < 0 { 0 } else { addr }
    }

    pub fn current_thread_id() -> u32 {
        unsafe { GetCurrentThreadId() }
    }

    /// RIP of another thread, read with it suspended for as short a time as possible (stall
    /// watchdog). Returns `(rip, ms GetThreadContext took)`; `None` if it could not be opened,
    /// suspended or read. Nothing allocates or logs while the thread is stopped, and it is
    /// always resumed. Never pass the calling thread's own id.
    pub fn thread_rip(tid: u32) -> Option<(usize, u64)> {
        if tid == 0 || tid == current_thread_id() {
            return None;
        }
        let h = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, 0, tid) };
        if h.is_null() {
            return None;
        }
        let mut ctx = Ctx([0; 1232]);
        ctx.0[0x30..0x34].copy_from_slice(&CONTEXT_CONTROL.to_le_bytes());
        let t = std::time::Instant::now();
        let mut got = false;
        // --- thread stopped: no allocation until ResumeThread ---
        if unsafe { SuspendThread(h) } != u32::MAX {
            got = unsafe { GetThreadContext(h, &mut ctx) } != 0;
            unsafe { ResumeThread(h) };
        }
        // --- thread runs again ---
        let ms = t.elapsed().as_millis() as u64;
        unsafe { CloseHandle(h) };
        got.then(|| (usize::from_le_bytes(ctx.0[0xF8..0x100].try_into().unwrap_or_default()), ms))
    }

    const TH32CS_SNAPTHREAD: u32 = 0x4;
    const THREAD_SUSPEND_RESUME: u32 = 0x2;
    const THREAD_GET_CONTEXT: u32 = 0x8;
    const THREAD_QUERY_INFORMATION: u32 = 0x40;
    const CONTEXT_CONTROL: u32 = 0x0010_0001;
    const PAGE_EXECUTE_READWRITE: u32 = 0x40;
    const INVALID: *mut c_void = usize::MAX as *mut c_void;

    #[repr(C)]
    struct ThreadEntry {
        size: u32,
        usage: u32,
        tid: u32,
        owner_pid: u32,
        base_pri: i32,
        delta_pri: i32,
        flags: u32,
    }

    /// x64 CONTEXT (1232 bytes, 16-aligned): `ContextFlags` at 0x30, `Rip` at 0xF8.
    #[repr(C, align(16))]
    struct Ctx([u8; 1232]);

    #[derive(Debug, PartialEq)]
    pub enum Error {
        /// A thread was inside a target range (retry later).
        Busy(usize),
        /// Item `i` did not hold its expected bytes (nothing written).
        Bytes(usize),
        Api(&'static str),
    }

    /// Timing of one `write_all` (2026-10-24: host log showed `patches restored 29/29` taking
    /// 13.7 s in one tick and 2.5-3.4 s elsewhere: one thread slow to suspend / report its
    /// context stalls the whole chain). Plain numbers, filled without allocating.
    #[derive(Default, Clone, Copy)]
    pub struct Stats {
        pub threads: u32,
        pub total_ms: u64,
        pub slowest_tid: u32,
        pub slowest_ms: u64,
        /// The suspend phase ran past the caller's budget: everything was resumed, nothing written.
        pub timed_out: bool,
        /// GPU driver threads left running (`SKIP_DRIVER_THREADS`).
        pub skipped: u32,
    }

    /// `[DEBUG] freeze_skip_driver_threads` (default on, v8.61): threads that started in a GPU
    /// driver module are neither suspended nor waited for. They never run sekiro.exe code (our
    /// patch sites), but one of them blocked GetThreadContext for 5378 ms (host 10:56:15,
    /// 2026-10-28, `nvwgf2umx.dll+0xfd2cfc`) and others for 97-562 ms.
    pub static SKIP_DRIVER_THREADS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    /// GPU user-mode driver DLLs (lower case file names): NVIDIA D3D11/D3D12, AMD, Intel.
    pub fn is_driver_module(name: &str) -> bool {
        const DRIVERS: [&str; 10] = [
            "nvwgf2umx.dll", "nvwgf2um.dll", "nvwgf2umx_cfg.dll", "nvldumdx.dll", "amdxx64.dll", "atidxx64.dll", "amdxc64.dll",
            "igd10iumd64.dll", "igd12umd64.dll", "igdumdim64.dll",
        ];
        let n = name.to_ascii_lowercase();
        DRIVERS.contains(&n.as_str())
    }

    /// The lower-case file name of the module holding `addr` ("" if none). Allocates.
    fn module_name(addr: usize) -> String {
        let mut module: *mut c_void = std::ptr::null_mut();
        // SAFETY: the address is only looked up, never dereferenced.
        let ok = unsafe { GetModuleHandleExW(GET_MODULE_FROM_ADDRESS, addr as *const u16, &mut module) };
        if ok == 0 || module.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 260];
        let n = (unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize).min(buf.len());
        let path = String::from_utf16_lossy(&buf[..n]);
        path.rsplit('\\').next().unwrap_or(&path).to_ascii_lowercase()
    }

    /// Writes every `(address, expected, new)` (equal lengths), all or nothing, and returns its
    /// timing (the caller logs it once the threads run again).
    pub fn write_all_timed(items: &[(usize, &[u8], &[u8])], budget_ms: u64) -> (Result<(), Error>, Stats) {
        use std::time::Instant;
        let t0 = Instant::now();
        let mut stats = Stats::default();
        // Everything that allocates happens before the first suspend.
        let me = unsafe { GetCurrentThreadId() };
        let pid = unsafe { GetCurrentProcessId() };
        let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snap.is_null() || snap == INVALID {
            return (Err(Error::Api("CreateToolhelp32Snapshot")), stats);
        }
        let mut tids: Vec<u32> = Vec::with_capacity(256);
        let mut e = ThreadEntry { size: std::mem::size_of::<ThreadEntry>() as u32, usage: 0, tid: 0, owner_pid: 0, base_pri: 0, delta_pri: 0, flags: 0 };
        let mut ok = unsafe { Thread32First(snap, &mut e) } != 0;
        let skip_drivers = SKIP_DRIVER_THREADS.load(std::sync::atomic::Ordering::Relaxed);
        while ok {
            if e.owner_pid == pid && e.tid != me {
                if skip_drivers && is_driver_module(&module_name(thread_start_address(e.tid))) {
                    stats.skipped += 1;
                } else {
                    tids.push(e.tid);
                }
            }
            e.size = std::mem::size_of::<ThreadEntry>() as u32;
            ok = unsafe { Thread32Next(snap, &mut e) } != 0;
        }
        unsafe { CloseHandle(snap) };
        let mut held: Vec<(*mut c_void, u32)> = Vec::with_capacity(tids.len());
        let mut ctx = Ctx([0; 1232]);
        let mut result = Ok(());
        let slow = |tid: u32, since: Instant, stats: &mut Stats| {
            let ms = since.elapsed().as_millis() as u64;
            if ms > stats.slowest_ms {
                stats.slowest_ms = ms;
                stats.slowest_tid = tid;
            }
        };
        // --- no allocation from here until every thread runs again ---
        // Pass 1 (2026-10-24): suspend every thread first, so the "wait until it really
        // stopped" delays overlap instead of chaining one thread after the other.
        for &tid in &tids {
            if budget_ms != 0 && t0.elapsed().as_millis() as u64 > budget_ms {
                // Out of time (callers retry Busy later): resume what is held below.
                stats.timed_out = true;
                result = Err(Error::Busy(0));
                break;
            }
            let t = Instant::now();
            let h = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, 0, tid) };
            if h.is_null() {
                continue; // exited meanwhile
            }
            if unsafe { SuspendThread(h) } == u32::MAX {
                unsafe { CloseHandle(h) };
                continue;
            }
            held.push((h, tid));
            slow(tid, t, &mut stats);
        }
        // Pass 2: the context of each (GetThreadContext waits for the suspend to complete).
        let mut i = 0;
        while result.is_ok() && i < held.len() {
            if budget_ms != 0 && t0.elapsed().as_millis() as u64 > budget_ms {
                stats.timed_out = true;
                result = Err(Error::Busy(0));
                break;
            }
            let (h, tid) = held[i];
            let t = Instant::now();
            ctx.0.fill(0);
            ctx.0[0x30..0x34].copy_from_slice(&CONTEXT_CONTROL.to_le_bytes());
            let got = unsafe { GetThreadContext(h, &mut ctx) } != 0;
            slow(tid, t, &mut stats);
            if !got {
                // Exiting between the snapshot and now (review #4): not a reason to give
                // up; it runs none of our code any more. Let it go.
                held.swap_remove(i);
                unsafe {
                    ResumeThread(h);
                    CloseHandle(h);
                }
                continue;
            }
            i += 1;
            let rip = usize::from_le_bytes(ctx.0[0xF8..0x100].try_into().unwrap_or_default());
            if items.iter().any(|(a, old, _)| rip >= *a && rip < *a + old.len()) {
                result = Err(Error::Busy(rip));
                break;
            }
        }
        stats.threads = tids.len() as u32;
        if result.is_ok() {
            for (i, (a, old, _)) in items.iter().enumerate() {
                // SAFETY: game code, mapped and readable.
                let cur = unsafe { std::slice::from_raw_parts(*a as *const u8, old.len()) };
                if cur != *old {
                    result = Err(Error::Bytes(i));
                    break;
                }
            }
        }
        if result.is_ok() {
            for (a, _, new) in items {
                let mut prot = 0u32;
                unsafe {
                    if VirtualProtect(*a as *mut c_void, new.len(), PAGE_EXECUTE_READWRITE, &mut prot) == 0 {
                        result = Err(Error::Api("VirtualProtect"));
                        break;
                    }
                    std::ptr::copy_nonoverlapping(new.as_ptr(), *a as *mut u8, new.len());
                    VirtualProtect(*a as *mut c_void, new.len(), prot, &mut prot);
                    FlushInstructionCache(GetCurrentProcess(), *a as *const c_void, new.len());
                }
            }
        }
        for &(h, _) in &held {
            unsafe {
                ResumeThread(h);
                CloseHandle(h);
            }
        }
        // --- threads run again ---
        stats.total_ms = t0.elapsed().as_millis() as u64;
        (result, stats)
    }
}

/// How often a busy write is retried in one go before giving up for this frame.
pub const FREEZE_TRIES: u32 = 20;

/// `freeze::write_all`, retried while a thread sits inside a target. Logs one line when
/// the whole thing took > 100 ms (thread count, total, slowest thread, retries).
pub fn write_frozen(items: &[(usize, &[u8], &[u8])]) -> Result<(), freeze::Error> {
    write_frozen_budget(items, 0)
}

/// `write_frozen` with a time budget for the suspend phase (0 = none): past it every thread is
/// resumed, nothing is written and the result is `Busy`, so the caller retries on a later frame.
pub fn write_frozen_budget(items: &[(usize, &[u8], &[u8])], budget_ms: u64) -> Result<(), freeze::Error> {
    let t0 = std::time::Instant::now();
    let mut last = Err(freeze::Error::Busy(0));
    let mut worst = freeze::Stats::default();
    let mut tries = 0u32;
    for _ in 0..FREEZE_TRIES {
        tries += 1;
        let (r, st) = freeze::write_all_timed(items, budget_ms);
        let timed_out = worst.timed_out || st.timed_out;
        if st.slowest_ms >= worst.slowest_ms {
            worst = st;
        }
        worst.timed_out = timed_out;
        last = r;
        // Busy because the suspend phase ran out of its budget: no retry in this go (the
        // caller tries again on a later frame), or the 20 tries would freeze for 20 x budget.
        if worst.timed_out || !matches!(last, Err(freeze::Error::Busy(_))) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let total = t0.elapsed().as_millis() as u64;
    if total > 100 {
        // Who the slowest thread is (start address of its entry function), to name the stall.
        let who = if worst.slowest_tid != 0 {
            match freeze::thread_start_address(worst.slowest_tid) {
                0 => "start unknown".to_owned(),
                a => format!("start {}", freeze::module_offset(a)),
            }
        } else {
            "no thread".to_owned()
        };
        log::line(&format!(
            "hook: freeze slow: {} threads ({} GPU driver threads left running), {total} ms total, slowest tid {} ({who}) {} ms, {} retries{}",
            worst.threads, worst.skipped, worst.slowest_tid, worst.slowest_ms, tries - 1,
            if worst.timed_out { ", TIMED OUT (resumed all, nothing written)" } else { "" }
        ));
    }
    last
}


/// `patch_jmp_pub` with every other thread suspended and none inside the prologue
/// (icons review #2: a loader thread paused inside a 9-byte prologue would resume into
/// the rel32 / int3 bytes). False if the bytes changed or a thread stayed inside.
fn patch_jmp_frozen(site: usize, to: usize, len: usize) -> bool {
    let Some(rel) = rel32(site + 5, to) else { return false };
    let mut new = vec![0xCCu8; len];
    new[0] = 0xE9;
    new[1..5].copy_from_slice(&rel);
    let mut old = vec![0u8; len];
    if !crate::game::read_bytes(site, &mut old) {
        return false;
    }
    match write_frozen(&[(site, &old, &new)]) {
        Ok(()) => true,
        Err(e) => {
            log::line(&format!("hook: frozen write at {site:#x} failed: {e:?}"));
            false
        }
    }
}

#[cfg(test)]
mod freeze_skip_tests {
    use super::freeze::is_driver_module;

    #[test]
    fn only_gpu_driver_threads_are_left_running() {
        // Host 2026-10-28: the 5378 ms GetThreadContext was a thread started in nvwgf2umx.dll.
        assert!(is_driver_module("nvwgf2umx.dll"));
        assert!(is_driver_module("NVWGF2UMX.DLL"));
        assert!(is_driver_module("amdxx64.dll"));
        // Game workers, thread pools and the overlay may run sekiro.exe code: always frozen.
        for m in ["sekiro.exe", "ntdll.dll", "kernel32.dll", "sekiro_coop.dll", "d3d11.dll", ""] {
            assert!(!is_driver_module(m), "{m}");
        }
    }
}
