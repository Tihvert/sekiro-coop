//! Frame rate unlock (default 120 fps), the way Sekiro FPS Unlocker and more
//! does it (uberhalit, MIT; patterns and the speed table from its GameData.cs):
//!
//! - `fFrameTick`: the frame limiter's target frame time is the immediate of
//!   `mov dword ptr [rbx+18h], 3C888889h` (1/60 s). It is found by the pattern
//!   `88 88 3C 4C 89 AB` (the float's last bytes + the next instruction), or
//!   fuzzy `C7 43 ?? ?? ?? ?? ?? 4C 89 AB`, and set to 1/fps.
//! - Speed fix: a `mulss xmm0, [rip+X]` in the movement code multiplies by a
//!   "critical running speed" that must be about fps/2 (30 at 60 fps). The
//!   unlocker points the rel32 at a data cave holding the value from its table
//!   (60 for 120 fps). Without it the player's running speed is wrong.
//!
//! The game still forces vsync (60 Hz in exclusive fullscreen): turn vsync off
//! for sekiro.exe in the NVIDIA control panel, or play borderless on a monitor
//! that runs at 120 Hz or more.

use crate::log;

pub const TARGET_FPS: u32 = 120;
/// Unlocker table entry closest to TARGET_FPS / 2.
const SPEED_FIX: f32 = 60.0;

/// Settings (settings file `fps_unlock`, `vsync`), applied live by `apply`.
pub static UNLOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub static VSYNC_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// Patched addresses, 0 until `install` found them.
static TICK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CAVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static VSYNC_SITE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// What is written now: bit 0 unlocked, bit 1 vsync off; 0xFF = nothing yet.
static APPLIED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0xFF);

/// `IDXGISwapChain::Present(SyncInterval, 0)` in `0x1B63170`: the interval comes
/// from `mov edx, ebp` (`8B D5`) at `0x1B63325`. `xor edx, edx` (`33 D2`) presents
/// without waiting for the monitor, so 120 fps is reached on a 60 Hz screen too
/// (with tearing, as with vsync off anywhere).
const VSYNC_RVA: usize = 0x1B63325;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualProtect(addr: *mut std::ffi::c_void, size: usize, new: u32, old: *mut u32) -> i32;
    fn FlushInstructionCache(process: *mut std::ffi::c_void, addr: *const std::ffi::c_void, size: usize) -> i32;
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
}

/// The exe's `.text` section as a slice (from the PE headers).
fn text() -> Option<(usize, &'static [u8])> {
    let base = crate::game::module_base();
    unsafe {
        let pe = base + *((base + 0x3C) as *const u32) as usize;
        let sections = *((pe + 6) as *const u16) as usize;
        let opt_size = *((pe + 20) as *const u16) as usize;
        let first = pe + 24 + opt_size;
        for i in 0..sections {
            let s = first + i * 40;
            let name = std::slice::from_raw_parts(s as *const u8, 8);
            if name.starts_with(b".text") {
                let vsize = *((s + 8) as *const u32) as usize;
                let rva = *((s + 12) as *const u32) as usize;
                return Some((base + rva, std::slice::from_raw_parts((base + rva) as *const u8, vsize)));
            }
        }
    }
    None
}

/// First match of a pattern (`None` = wildcard).
fn find(hay: &[u8], pat: &[Option<u8>]) -> Option<usize> {
    hay.windows(pat.len()).position(|w| w.iter().zip(pat).all(|(b, p)| p.is_none_or(|p| *b == p)))
}

fn pattern(s: &str) -> Vec<Option<u8>> {
    s.split_whitespace().map(|t| if t == "??" { None } else { u8::from_str_radix(t, 16).ok() }).collect()
}

fn write_code(addr: usize, bytes: &[u8]) -> bool {
    let mut old = 0u32;
    unsafe {
        if VirtualProtect(addr as *mut _, bytes.len(), 0x40, &mut old) == 0 {
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
        VirtualProtect(addr as *mut _, bytes.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), addr as *const _, bytes.len());
    }
    true
}

pub fn install() -> bool {
    let Some((start, text)) = text() else {
        log::line("fps: .text not found, frame rate stays 60");
        return false;
    };
    let tick = find(text, &pattern("88 88 3C 4C 89 AB"))
        .map(|i| start + i - 1)
        .or_else(|| find(text, &pattern("C7 43 ?? ?? ?? ?? ?? 4C 89 AB")).map(|i| start + i + 3));
    let speed = find(text, &pattern("F3 0F 58 ?? 0F C6 ?? 00 0F 51 ?? F3 0F 59 ?? ?? ?? ?? ?? 0F 2F")).map(|i| start + i + 15);
    let (Some(tick), Some(speed)) = (tick, speed) else {
        log::line(&format!("fps: patterns not found (frame tick {tick:?}, speed fix {speed:?}), frame rate stays 60"));
        return false;
    };
    // Speed fix first: a data cave near the code with the new value, and the
    // mulss rel32 pointed at it (rel32 is relative to the next instruction).
    let Some(cave) = crate::hook::alloc_near_pub(speed) else {
        log::line("fps: no memory near the code for the speed fix, frame rate stays 60");
        return false;
    };
    unsafe { *(cave as *mut f32) = SPEED_FIX };
    let rel = cave as i64 - (speed as i64 + 4);
    let Ok(rel) = i32::try_from(rel) else {
        log::line("fps: speed fix cave out of range, frame rate stays 60");
        return false;
    };
    // The same "x 30" constant classifies the REMOTE player's per-frame move in
    // NetworkManipulator slot 41 (`mulss xmm1, [rip+X]` at 0x7ECFC1): at 120 fps the
    // frame displacement halves, so without this a running friend reads as walking.
    // Only redirected if it still points at the constant the speed fix pointed at.
    let orig_const = (speed as i64 + 4 + i32::from_le_bytes(unsafe { *(speed as *const [u8; 4]) }) as i64) as usize;
    let net_ins = crate::game::module_base() + 0x7ECFC1;
    let net_ok = unsafe { *(net_ins as *const [u8; 4]) } == [0xF3, 0x0F, 0x59, 0x0D]
        && (net_ins as i64 + 8 + i32::from_le_bytes(unsafe { *((net_ins + 4) as *const [u8; 4]) }) as i64) as usize == orig_const;
    let net_rel = i32::try_from(cave as i64 - (net_ins as i64 + 8)).ok();
    let ok_net = match (net_ok, net_rel) {
        (true, Some(r)) => write_code(net_ins + 4, &r.to_le_bytes()),
        _ => false,
    };
    let ok_speed = write_code(speed, &rel.to_le_bytes());
    let frame = 1.0f32 / TARGET_FPS as f32;
    let ok_tick = ok_speed;
    if ok_speed {
        TICK.store(tick, std::sync::atomic::Ordering::Release);
        CAVE.store(cave, std::sync::atomic::Ordering::Release);
    }
    let vs = crate::game::module_base() + VSYNC_RVA;
    if unsafe { *(vs as *const [u8; 2]) } == [0x8B, 0xD5] {
        VSYNC_SITE.store(vs, std::sync::atomic::Ordering::Release);
    } else {
        log::line("fps: vsync call site has unexpected bytes, vsync stays as the game sets it");
    }
    apply();
    log::line(&format!(
        "fps: unlock ready, {TARGET_FPS} when fps_unlock = 1 (the limit line above is what runs; frame tick at {tick:#x}, {frame} unlocked, speed fix at {speed:#x} -> {SPEED_FIX} unlocked, remote move classify {ok_net}); ok = {}",
        ok_speed && ok_tick
    ));
    ok_speed && ok_tick
}

/// Writes the current settings (cheap; call every frame, writes only on change).
pub fn apply() {
    use std::sync::atomic::Ordering::{Acquire, Relaxed};
    let want = UNLOCK.load(Relaxed) as u8 | (VSYNC_OFF.load(Relaxed) as u8) << 1;
    if APPLIED.load(Relaxed) == want {
        return;
    }
    let (tick, cave, vs) = (TICK.load(Acquire), CAVE.load(Acquire), VSYNC_SITE.load(Acquire));
    if tick != 0 && cave != 0 {
        let unlocked = want & 1 != 0;
        let fps = if unlocked { TARGET_FPS as f32 } else { 60.0 };
        unsafe { *(cave as *mut f32) = if unlocked { SPEED_FIX } else { 30.0 } };
        write_code(tick, &(1.0f32 / fps).to_le_bytes());
    }
    if vs != 0 {
        write_code(vs, if want & 2 != 0 { &[0x33, 0xD2] } else { &[0x8B, 0xD5] });
    }
    APPLIED.store(want, Relaxed);
    log::line(&format!("fps: limit {} fps, vsync {}", if want & 1 != 0 { TARGET_FPS } else { 60 }, if want & 2 != 0 { "off" } else { "game default" }));
}
