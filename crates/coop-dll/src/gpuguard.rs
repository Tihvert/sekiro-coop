//! Guard for the game's material constant-buffer upload (the recurring driver crash on both
//! PCs, 2026-10-22/23): `CRASH? code 0xc0000005 at nvwgf2umx.dll+0xfbbc10 (read of X)`,
//! stack `[..]+0x1b5c732 [..]+0x30c9d2`, often two render threads at once with the same rdx.
//!
//! What the game does (IDA, `F:\Temp\ida_gpu`): `GXFlverMaterial` vtable slot 2
//! (0x2FEC80) -> 0x30B870 -> 0x30C750 binds a material's constants. 0x30C750 takes the
//! material block `a1` (= material + 208) whose `+0x28 / +0x30` is a vector of float4
//! constants (begin / end) and uploads it into the per-material constant buffer with
//! `0x1B5C6F0` = `ID3D11DeviceContext::UpdateSubresource(cb, 0, NULL, src, 0, 0)`:
//!   * texture-scroll materials (`a2+0x5C` mask != 0) first copy the vector into a 400-byte
//!     stack buffer (25 float4) and upload that;
//!   * every other material (the `else` branch, call at 0x30C9CD) passes the vector itself.
//!
//! With a NULL box D3D11 reads the buffer's whole ByteWidth (400 bytes) from `src`. Most
//! materials have fewer constants, so the driver reads past the end of the vector into
//! whatever follows on the heap. That is harmless garbage until the vector happens to end
//! at the end of committed memory: then the read faults. Every crash had the fault at
//! src + 0x170 / 0x180 (the copy's last 32 bytes of 400) and src 0x20..0x180 bytes below a
//! 64 KB boundary, i.e. vectors of 2..24 constants sitting at a region end. A vanilla latent
//! over-read; it fires most right after loads (new models / materials allocated: area
//! loads, death reloads, the native remote's creation and dress). me3 knows it: Sekiro
//! "consistently causes OOB reads in a certain GXFlverMaterial function" (me3
//! `mod-host/src/alloc_hooks/mimalloc.rs`, FEASIBILITY.md 5.3/5.4) and swaps the allocator
//! for mimalloc on a 6 GB pre-committed heap to absorb the reads. The crashes show that
//! does not always hold; this removes the over-read itself.
//!
//! The guard: the 5-byte `call 0x1B5C6F0` at 0x30C9CD is pointed at a near stub that passes
//! the material block (`rdi`) as a 9th argument to `guarded_update`, which uploads a copy
//! padded with zeros to the buffer's ByteWidth when the vector is shorter. Everything else
//! (a box, an unexpected source, a vector at least as long as the buffer) goes to the
//! original unchanged. The shader sees zeros instead of heap garbage in the registers the
//! material does not define. Setting `gpu_guard = 0` ([DEBUG], read at start) leaves the
//! call alone (A/B test). Counters only on the render threads; the frame task logs them.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

use crate::log;

/// `call sub_141B5C6F0` in 0x30C750's non-scroll branch (returns to 0x30C9D2).
const SITE_RVA: usize = 0x30C9CD;
/// `ID3D11DeviceContext::UpdateSubresource` wrapper `(holder, _, buffer wrapper, sub, box,
/// src, row pitch, depth pitch)`; the resource is `*(wrapper + 0x18)`.
const UPDATE_RVA: usize = 0x1B5C6F0;
/// `mov rax, [rdi+28h]` at 0x30C9AB: rdi is the material block, rax its vector begin = src.
const SRC_LOAD_RVA: usize = 0x30C9AB;
const SRC_LOAD: [u8; 4] = [0x48, 0x8B, 0x47, 0x28];
const VEC_BEGIN: usize = 0x28;
const VEC_END: usize = 0x30;
const WRAP_RESOURCE: usize = 0x18;
/// COM vtable slots: ID3D11Resource::GetType, ID3D11Buffer::GetDesc.
const SLOT_GET_TYPE: usize = 7;
const SLOT_GET_DESC: usize = 10;
const DIM_BUFFER: u32 = 1;
/// D3D11 constant buffers are at most 64 KB.
const MAX_CB: usize = 0x10000;
/// Padded copies up to this size use the stack.
const STACK_COPY: usize = 4096;
const REPORT_EVERY: Duration = Duration::from_secs(60);
/// An averted crash reports early, but at most this often: averts happen on most frames in some
/// areas, and a line per frame was 54 % of a friend's log (2026-10-27). The minute line still
/// carries the counts and the last source.
const AVERT_LINE_GAP: Duration = Duration::from_secs(10);

type Update = unsafe extern "system" fn(usize, usize, usize, usize, usize, usize, usize, usize) -> usize;

static ORIG: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicU64 = AtomicU64::new(0);
static PADDED: AtomicU64 = AtomicU64::new(0);
/// Padded uploads whose vector ended exactly on a page boundary.
static EDGE: AtomicU64 = AtomicU64::new(0);
/// Of those, the next page was not readable: the vanilla call would have crashed.
static AVERTED: AtomicU64 = AtomicU64::new(0);
static LAST_AVERTED: AtomicUsize = AtomicUsize::new(0);
static MIN_LEN: AtomicUsize = AtomicUsize::new(usize::MAX);
static MAX_BW: AtomicUsize = AtomicUsize::new(0);
static PASSED_ODD: AtomicU64 = AtomicU64::new(0);

/// One byte at `addr` can be read (fault-safe ReadProcessMemory, game.rs; VirtualQuery is
/// slow inside me3's large heap arena).
fn readable(addr: usize) -> bool {
    let mut b = [0u8; 1];
    crate::game::read_bytes(addr, &mut b)
}

/// What to do with one upload: the vector's length against the buffer's ByteWidth.
#[derive(Debug, PartialEq)]
enum Plan {
    Original,
    Pad { len: usize, bw: usize },
}

fn plan(len: usize, bw: usize) -> Plan {
    if bw == 0 || bw > MAX_CB || len >= bw { Plan::Original } else { Plan::Pad { len, bw } }
}

/// The first 64 KB boundary inside the over-read `[tail, read_end)`, if any
/// (a boundary exactly at `tail` counts: the read starts there).
fn crossed_region_edge(tail: usize, read_end: usize) -> Option<usize> {
    let edge = tail.checked_add(0xFFFF)? & !0xFFFF;
    (edge < read_end).then_some(edge)
}

/// `out[..len] = src`, the rest of `out` zero.
fn fill_padded(src: &[u8], out: &mut [u8]) {
    let n = src.len().min(out.len());
    out[..n].copy_from_slice(&src[..n]);
    out[n..].fill(0);
}

/// ByteWidth of the buffer behind the game's wrapper, or 0 (not a buffer / unreadable).
unsafe fn byte_width(wrap: usize) -> usize {
    unsafe {
        let res = *((wrap + WRAP_RESOURCE) as *const usize);
        if res < 0x10000 {
            return 0;
        }
        let vt = *(res as *const usize);
        if vt < 0x10000 {
            return 0;
        }
        let get_type: extern "system" fn(usize, *mut u32) = std::mem::transmute(*((vt + 8 * SLOT_GET_TYPE) as *const usize));
        let mut dim = 0u32;
        get_type(res, &mut dim);
        if dim != DIM_BUFFER {
            return 0;
        }
        let get_desc: extern "system" fn(usize, *mut [u32; 6]) = std::mem::transmute(*((vt + 8 * SLOT_GET_DESC) as *const usize));
        let mut desc = [0u32; 6];
        get_desc(res, &mut desc);
        desc[0] as usize
    }
}

/// v8.50: the wrapper's own entry, `push rbx; sub rsp, 40h` (nothing RIP-relative).
const WRAP_PROLOGUE: [u8; 6] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x40];
/// Trampoline of the wrapper detour (0 = not installed).
static WRAP_TRAMP: AtomicUsize = AtomicUsize::new(0);
static WRAP_CALLS: AtomicU64 = AtomicU64::new(0);
static WRAP_EDGE: AtomicU64 = AtomicU64::new(0);
static WRAP_AVERTED: AtomicU64 = AtomicU64::new(0);
static WRAP_LAST: AtomicUsize = AtomicUsize::new(0);
static WRAP_LAST_CALLER: AtomicUsize = AtomicUsize::new(0);

/// The first 64 KB boundary after `src` that a `bw`-byte read from it crosses, if any.
fn read_crosses_edge(src: usize, bw: usize) -> Option<usize> {
    let edge = (src | 0xFFFF).checked_add(1)?;
    (src.checked_add(bw)? > edge).then_some(edge)
}

/// v8.50: every caller of the wrapper (288 sites; the material site above is one). Live
/// 2026-10-27 09:30: the same over-read crash came through 0x357622 right after a rest
/// (`read of 0x4a120000`, src 0x4a11ffc0), where the material guard does not sit. Here the
/// source length is unknown, so only the faulting case is changed: a NULL-box upload into a
/// buffer whose read would cross a 64 KB boundary into unreadable memory gets the readable
/// part plus zeros. Everything else goes to the original untouched.
unsafe extern "system" fn wrapper_detour(holder: usize, a2: usize, wrap: usize, sub: usize, boxp: usize, src: usize, rp: usize, dp: usize) -> usize {
    let orig: Update = unsafe { std::mem::transmute(WRAP_TRAMP.load(Relaxed)) };
    WRAP_CALLS.fetch_add(1, Relaxed);
    if boxp != 0 || src < 0x10000 || wrap < 0x10000 {
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    }
    let bw = unsafe { byte_width(wrap) };
    let Some(edge) = (if bw == 0 || bw > MAX_CB { None } else { read_crosses_edge(src, bw) }) else {
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    };
    WRAP_EDGE.fetch_add(1, Relaxed);
    if readable(edge) {
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    }
    WRAP_AVERTED.fetch_add(1, Relaxed);
    WRAP_LAST.store(src, Relaxed);
    WRAP_LAST_CALLER.store(crate::hook::detour_caller(WRAP_TRAMP.load(Relaxed)), Relaxed);
    let data = unsafe { std::slice::from_raw_parts(src as *const u8, edge - src) };
    let mut v = vec![0u8; bw];
    fill_padded(data, &mut v);
    unsafe { orig(holder, a2, wrap, sub, boxp, v.as_ptr() as usize, rp, dp) }
}

/// Called by the stub in place of `0x1B5C6F0` (render threads, possibly several at once).
/// Never panics: raw reads of the objects the game itself just used.
unsafe extern "system" fn guarded_update(holder: usize, a2: usize, wrap: usize, sub: usize, boxp: usize, src: usize, rp: usize, dp: usize, block: usize) -> usize {
    let orig: Update = unsafe { std::mem::transmute(ORIG.load(Relaxed)) };
    CALLS.fetch_add(1, Relaxed);
    if boxp != 0 || src < 0x10000 || block < 0x10000 || wrap < 0x10000 {
        PASSED_ODD.fetch_add(1, Relaxed);
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    }
    // Plain reads (v8.44): the game read `block` itself in the same call, and a ReadProcessMemory syscall per
    // read (twice per upload, ~40000 uploads a second) cost more than the guard's whole job.
    let (begin, end) = unsafe { (std::ptr::read_volatile((block + VEC_BEGIN) as *const usize), std::ptr::read_volatile((block + VEC_END) as *const usize)) };
    if begin != src || end < begin {
        PASSED_ODD.fetch_add(1, Relaxed);
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    }
    let len = end - begin;
    let bw = unsafe { byte_width(wrap) };
    let Plan::Pad { len, bw } = plan(len, bw) else {
        return unsafe { orig(holder, a2, wrap, sub, boxp, src, rp, dp) };
    };
    PADDED.fetch_add(1, Relaxed);
    MIN_LEN.fetch_min(len, Relaxed);
    MAX_BW.fetch_max(bw, Relaxed);
    // The crash case: the vanilla over-read [src+len, src+bw) crosses a 64 KB boundary
    // (every fault was just past one) into memory that is not readable. Probed only then:
    // well under 1 % of the padded uploads.
    if let Some(edge) = crossed_region_edge(src + len, src + bw) {
        EDGE.fetch_add(1, Relaxed);
        if !readable(edge) {
            AVERTED.fetch_add(1, Relaxed);
            LAST_AVERTED.store(src, Relaxed);
        }
    }
    let data = unsafe { std::slice::from_raw_parts(src as *const u8, len) };
    if bw <= STACK_COPY {
        // Only the first `bw` bytes are written (and all of them): no 4 KB memset per draw.
        let mut buf = std::mem::MaybeUninit::<[u128; STACK_COPY / 16]>::uninit();
        let out = buf.as_mut_ptr() as *mut u8;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), out, len);
            std::ptr::write_bytes(out.add(len), 0, bw - len);
            orig(holder, a2, wrap, sub, boxp, out as usize, rp, dp)
        }
    } else {
        let mut v = vec![0u8; bw];
        fill_padded(data, &mut v);
        unsafe { orig(holder, a2, wrap, sub, boxp, v.as_ptr() as usize, rp, dp) }
    }
}

/// The stub the call site jumps to: same stack arguments 5..8, `rdi` (the material block)
/// as argument 9, then `guarded_update`. Entry rsp is 8 mod 16; `sub rsp, 0x58` aligns it.
fn stub_bytes(target: usize) -> Vec<u8> {
    let mut s = Vec::with_capacity(80);
    s.extend_from_slice(&[0x48, 0x83, 0xEC, 0x58]); // sub rsp, 58h
    for (from, to) in [(0x80u32, 0x20u8), (0x88, 0x28), (0x90, 0x30), (0x98, 0x38)] {
        s.extend_from_slice(&[0x48, 0x8B, 0x84, 0x24]); // mov rax, [rsp+disp32]
        s.extend_from_slice(&from.to_le_bytes());
        s.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, to]); // mov [rsp+disp8], rax
    }
    s.extend_from_slice(&[0x48, 0x89, 0x7C, 0x24, 0x40]); // mov [rsp+40h], rdi
    s.extend_from_slice(&[0x48, 0xB8]); // mov rax, imm64
    s.extend_from_slice(&(target as u64).to_le_bytes());
    s.extend_from_slice(&[0xFF, 0xD0]); // call rax
    s.extend_from_slice(&[0x48, 0x83, 0xC4, 0x58]); // add rsp, 58h
    s.push(0xC3); // ret
    s
}

fn call_bytes(site: usize, to: usize) -> Option<[u8; 5]> {
    let rel = i32::try_from(to as i64 - (site as i64 + 5)).ok()?;
    let mut b = [0xE8, 0, 0, 0, 0];
    b[1..].copy_from_slice(&rel.to_le_bytes());
    Some(b)
}

/// Installs the guard (at start, after the settings). Logs the decision either way.
pub fn install(enabled: bool) -> bool {
    if !enabled {
        log::line("gpuguard: OFF (gpu_guard = 0): material constant uploads go to the driver unchanged (vanilla over-read)");
        return false;
    }
    let base = crate::game::module_base();
    let site = base + SITE_RVA;
    // The wrapper detour first: the material stub below then calls its trampoline, so a
    // material upload is not checked twice.
    let update = match crate::hook::install_detour(UPDATE_RVA, &WRAP_PROLOGUE, wrapper_detour as *const () as usize, "gpuguard: UpdateSubresource wrapper (0x1B5C6F0)") {
        Some(t) => {
            WRAP_TRAMP.store(t, Relaxed);
            log::line("gpuguard: ON for every caller of 0x1B5C6F0: a constant-buffer read that would cross a 64 KB boundary into unreadable memory is uploaded zero-padded");
            t
        }
        None => base + UPDATE_RVA,
    };
    let update_site = base + UPDATE_RVA;
    let Some(expected) = call_bytes(site, update_site) else { return false };
    let mut found = [0u8; 5];
    let mut load = [0u8; 4];
    if !crate::game::read_bytes(site, &mut found) || !crate::game::read_bytes(base + SRC_LOAD_RVA, &mut load) || found != expected || load != SRC_LOAD {
        log::line(&format!("gpuguard: unexpected bytes at {site:#x} ({found:02x?}, src load {load:02x?}), not installed"));
        return false;
    }
    let Some(stub) = crate::hook::alloc_near_pub(site) else {
        log::line("gpuguard: no memory near the module for the stub, not installed");
        return false;
    };
    let code = stub_bytes(guarded_update as *const () as usize);
    unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len()) };
    let Some(new) = call_bytes(site, stub) else {
        log::line("gpuguard: stub out of rel32 range, not installed");
        return false;
    };
    ORIG.store(update, Relaxed);
    // Render threads run this call all the time: write with them suspended.
    match crate::hook::write_frozen(&[(site, &expected, &new)]) {
        Ok(()) => {
            log::line(&format!(
                "gpuguard: ON: material constant upload at {site:#x} -> stub {stub:#x}: vectors shorter than the constant buffer are uploaded zero-padded (no read past their end)"
            ));
            true
        }
        Err(e) => {
            // ORIG must be set before the patch goes live (render threads resume inside
            // write_frozen and may call the stub at once), so a failed write undoes it here.
            ORIG.store(0, Relaxed);
            log::line(&format!("gpuguard: patch at {site:#x} failed ({e:?}), not installed"));
            false
        }
    }
}

/// A report is due every minute, or after an averted crash once the last line is AVERT_LINE_GAP old.
fn due(since_last: Duration, new_averted: bool) -> bool {
    since_last >= REPORT_EVERY || (new_averted && since_last >= AVERT_LINE_GAP)
}

struct Report {
    at: Instant,
    calls: u64,
    padded: u64,
    edge: u64,
    averted: u64,
}

static REPORT: std::sync::Mutex<Option<Report>> = std::sync::Mutex::new(None);

/// Frame task: a line every minute while uploads happen, and at once after an averted crash.
pub fn tick() {
    wrapper_tick();
    if ORIG.load(Relaxed) == 0 {
        return;
    }
    let Ok(mut g) = REPORT.try_lock() else { return };
    let (calls, padded, edge, averted) = (CALLS.load(Relaxed), PADDED.load(Relaxed), EDGE.load(Relaxed), AVERTED.load(Relaxed));
    let r = g.get_or_insert_with(|| Report { at: Instant::now(), calls, padded, edge, averted });
    let new_averted = averted > r.averted;
    if !due(r.at.elapsed(), new_averted) {
        return;
    }
    if calls != r.calls || new_averted {
        let min = MIN_LEN.load(Relaxed);
        log::line(&format!(
            "gpuguard: {:.0} s: {} material uploads, {} padded (shortest vector {} B, buffer up to {} B), {} ended on a page edge, {} would have read unmapped memory{}{}",
            r.at.elapsed().as_secs_f32(),
            calls - r.calls,
            padded - r.padded,
            if min == usize::MAX { 0 } else { min },
            MAX_BW.load(Relaxed),
            edge - r.edge,
            averted - r.averted,
            if new_averted { format!(" = DRIVER CRASH AVERTED (last source {:#x})", LAST_AVERTED.load(Relaxed)) } else { String::new() },
            match PASSED_ODD.load(Relaxed) {
                0 => String::new(),
                n => format!("; {n} unexpected calls passed through unchanged (total)"),
            }
        ));
    }
    *r = Report { at: Instant::now(), calls, padded, edge, averted };
}

/// The wrapper detour's line: at once after an averted crash, else with the minute report.
fn wrapper_tick() {
    static SEEN: AtomicU64 = AtomicU64::new(0);
    static AT: std::sync::Mutex<Option<(Instant, u64, u64)>> = std::sync::Mutex::new(None);
    if WRAP_TRAMP.load(Relaxed) == 0 {
        return;
    }
    let averted = WRAP_AVERTED.load(Relaxed);
    let new_averted = averted > SEEN.load(Relaxed);
    let Ok(mut g) = AT.try_lock() else { return };
    let (calls, edge) = (WRAP_CALLS.load(Relaxed), WRAP_EDGE.load(Relaxed));
    let r = g.get_or_insert((Instant::now(), calls, edge));
    if !due(r.0.elapsed(), new_averted) {
        return;
    }
    log::line(&format!(
        "gpuguard: wrapper {:.0} s: {} uploads, {} crossed a 64 KB edge, {averted} averted in total{}",
        r.0.elapsed().as_secs_f32(),
        calls - r.1,
        edge - r.2,
        if new_averted {
            format!(
                " = DRIVER CRASH AVERTED (source {:#x}, caller RVA {:#x})",
                WRAP_LAST.load(Relaxed),
                WRAP_LAST_CALLER.load(Relaxed).wrapping_sub(crate::game::module_base())
            )
        } else {
            String::new()
        }
    ));
    SEEN.store(averted, Relaxed);
    *r = (Instant::now(), calls, edge);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averted_lines_are_rate_limited() {
        assert!(!due(Duration::from_secs(3), true), "an avert 3 s after the last line waits");
        assert!(due(Duration::from_secs(10), true));
        assert!(!due(Duration::from_secs(30), false));
        assert!(due(Duration::from_secs(60), false));
    }

    #[test]
    fn wrapper_edge_of_the_0930_crash() {
        // Host 2026-10-27 09:30: rdx 0x4a11ffc0, fault at 0x4a120000.
        assert_eq!(read_crosses_edge(0x4a11ffc0, 400), Some(0x4a120000));
        assert_eq!(read_crosses_edge(0x4a11ffc0, 0x40), None, "ends exactly on the edge");
        assert_eq!(read_crosses_edge(0x4a110000, 0x10000), None, "aligned source, full 64 KB");
        assert_eq!(read_crosses_edge(0x4a110100, 0x400), None);
    }

    #[test]
    fn plan_pads_only_short_vectors() {
        assert_eq!(plan(224, 400), Plan::Pad { len: 224, bw: 400 });
        assert_eq!(plan(0, 400), Plan::Pad { len: 0, bw: 400 });
        assert_eq!(plan(400, 400), Plan::Original);
        assert_eq!(plan(480, 400), Plan::Original);
        assert_eq!(plan(16, 0), Plan::Original);
        assert_eq!(plan(16, MAX_CB + 16), Plan::Original);
    }

    #[test]
    fn region_edges_of_the_logged_crashes() {
        // Host 04:13: src 0x29fbff20, fault 0x29fc0090: a 224-byte vector ending at the edge.
        assert_eq!(crossed_region_edge(0x29fc0000, 0x29fbff20 + 400), Some(0x29fc0000));
        // Friend 13:43: src 0x2031afe80, 400-byte read crosses 0x2031b0000.
        assert_eq!(crossed_region_edge(0x2031afe80 + 16, 0x2031afe80 + 400), Some(0x2031b0000));
        // Inside one region: nothing to check.
        assert_eq!(crossed_region_edge(0x1000_0100, 0x1000_0290), None);
        assert_eq!(crossed_region_edge(0x1000_0000 - 0x10, 0x1000_0000), None);
    }

    #[test]
    fn padded_copy_is_zero_filled() {
        let src: Vec<u8> = (1..=32).collect();
        let mut out = [0xAAu8; 64];
        fill_padded(&src, &mut out);
        assert_eq!(&out[..32], &src[..]);
        assert!(out[32..].iter().all(|&b| b == 0));
        let mut small = [0u8; 8];
        fill_padded(&src, &mut small);
        assert_eq!(&small[..], &src[..8]);
    }

    #[test]
    fn stub_encoding() {
        let s = stub_bytes(0x1122_3344_5566_7788);
        assert_eq!(&s[..4], &[0x48, 0x83, 0xEC, 0x58]);
        // First copy: box from [rsp+0x80] to [rsp+0x20].
        assert_eq!(&s[4..17], &[0x48, 0x8B, 0x84, 0x24, 0x80, 0, 0, 0, 0x48, 0x89, 0x44, 0x24, 0x20]);
        // Last copy: depth pitch from [rsp+0x98] to [rsp+0x38].
        assert_eq!(&s[43..56], &[0x48, 0x8B, 0x84, 0x24, 0x98, 0, 0, 0, 0x48, 0x89, 0x44, 0x24, 0x38]);
        assert_eq!(&s[56..61], &[0x48, 0x89, 0x7C, 0x24, 0x40]);
        assert_eq!(&s[61..63], &[0x48, 0xB8]);
        assert_eq!(&s[63..71], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&s[71..], &[0xFF, 0xD0, 0x48, 0x83, 0xC4, 0x58, 0xC3]);
        // 0x58 + the return address keeps rsp 16-aligned at `call rax`.
        assert_eq!((0x58 + 8) % 16, 0);
    }

    #[test]
    fn call_rel32_matches_the_game_bytes() {
        // The vanilla bytes at 0x14030C9CD: e8 1e fd 84 01 -> 0x141B5C6F0.
        let base = 0x1_4000_0000usize;
        assert_eq!(call_bytes(base + SITE_RVA, base + UPDATE_RVA), Some([0xE8, 0x1E, 0xFD, 0x84, 0x01]));
        assert_eq!(call_bytes(0x1000, 0x1000 + (1usize << 33)), None);
    }
}
