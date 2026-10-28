//! Custom icons for the three co-op items, ERSC style: no game file is replaced. While the
//! game turns a loaded TPF into texture-creation jobs, we overwrite the BC7 blocks of three
//! vanilla icon cells (MENU_ItemIcon_00251 / 00552 / 00962, unused by any param row) in the
//! TPF's memory, before the GPU textures are created. NOT verified live (2026-10-23).
//!
//! Hook (IDA, static dump `quick_re7`): `0x71B750` = per-TPF "create texture resources" loop
//! `(a1, a2 = array of 80-byte texture entry handles, i32 a3, a4 = job list, a5)`.
//!   * count `*(u32*)(a1+104)`; each handle: `+8` = TPF file base (0x1202A0 reads the TPF
//!     header there), `+48` = TPF entry iterator (0x1424C3F30 / vtable 0x1430DAED8: entry 20 bytes,
//!     `+0` data offset, `+4` size, `+12` name offset; validity = `"DDS "` magic at the data).
//!   * per entry: FD4ResName from the entry name (0x71B3F0), texture object, then a
//!     `CSGXTextureCreateFragment` (0xF70610, vtable 0x2B5C220) holding the entry handle is
//!     queued; its Run (0xF706C0) creates the GPU texture later from the TPF bytes.
//!   * so the TPF buffer is still the upload source when 0x71B750 is entered: patch, then
//!     call the original.
//! The me3 file override of `menu/hi/01_common.tpf.dcx` crashed in this same function
//! (`sekiro.exe+0x71B2E9`, inside the name-string ctor 0x71B250 called from 0x71B3F0/0x71B750:
//! null entry name = the TPF iterator rejected the buffer it was given).
//!
//! Safety: a cell is written only when the texture name, width/height, TPF format byte (102),
//! DDS DX10 format (98 = BC7_UNORM), mip count 1, DDS byte size and an FNV-1a hash of the
//! vanilla blocks all match the embedded record, and the pages are committed and writable.
//! Anything else: one log line, texture untouched. A panic inside is caught.
//!
//! Data: `assets/icons_bc7.bin`, made by `tools/icons/build_icon_blob.py` (format there).

use std::ffi::c_void;
use std::mem::{MaybeUninit, size_of};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::{game, hook, log, settings, tuning};

const TEX_CREATE_RVA: usize = 0x71B750;
/// `push rbp; push rbx; push rsi; push rdi; push r12; push r13` (9 bytes, no RIP-relative).
const TEX_CREATE_PROLOGUE: [u8; 9] = [0x40, 0x55, 0x53, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55];
/// How long `install_early` waits for the function bytes to appear (code unpacking).
const INSTALL_WAIT: Duration = Duration::from_secs(120);

const BLOB: &[u8] = include_bytes!("../assets/icons_bc7.bin");
const DDS_HEADER: usize = 148;
const TPF_FORMAT_BC7: u8 = 102;
const DXGI_BC7_UNORM: u32 = 98;
const MAX_TPF_TEXTURES: u32 = 4096;
const MAX_NAME_CHARS: usize = 32;
/// Keys (table hash + address) of TPFs already scanned that hold no icon sheet.
static SCANNED: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
fn scanned() -> std::sync::MutexGuard<'static, HashSet<u64>> {
    SCANNED.get_or_init(|| Mutex::new(HashSet::new())).lock().unwrap_or_else(|e| e.into_inner())
}

static TRAMP: AtomicUsize = AtomicUsize::new(0);
/// Cells written so far (all TPF loads together), for the log / status.
pub static PATCHED: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone)]
struct Cell {
    texture: String,
    width: u32,
    height: u32,
    dds_size: u32,
    bx: u32,
    by: u32,
    bw: u32,
    bh: u32,
    icon_id: u32,
    vanilla_hash: u64,
    new_hash: u64,
    data: &'static [u8],
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

fn parse_blob(b: &'static [u8]) -> Result<Vec<Cell>, String> {
    if b.get(0..4) != Some(b"SCIC".as_slice()) || u32_at(b, 4) != Some(1) {
        return Err("bad header".into());
    }
    let n = u32_at(b, 8).ok_or("short")? as usize;
    let mut p = 12;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let name = b.get(p..p + 32).ok_or(format!("record {i}: short"))?;
        let texture = String::from_utf8_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(32)]).into_owned();
        let f = |k: usize| u32_at(b, p + 32 + 4 * k).ok_or(format!("record {i}: short"));
        let (width, height, dds_size, bx, by, bw, bh, icon_id) = (f(0)?, f(1)?, f(2)?, f(3)?, f(4)?, f(5)?, f(6)?, f(7)?);
        let vanilla_hash = u64_at(b, p + 64).ok_or("short")?;
        let new_hash = u64_at(b, p + 72).ok_or("short")?;
        let len = (bw * bh * 16) as usize;
        let data = b.get(p + 80..p + 80 + len).ok_or(format!("record {i}: data short"))?;
        if width % 4 != 0 || height % 4 != 0 || bx + bw > width / 4 || by + bh > height / 4 || fnv1a64(data) != new_hash {
            return Err(format!("record {i} ({texture}) inconsistent"));
        }
        out.push(Cell { texture, width, height, dds_size, bx, by, bw, bh, icon_id, vanilla_hash, new_hash, data });
        p += 80 + len;
    }
    if p != b.len() {
        return Err("trailing bytes".into());
    }
    Ok(out)
}

fn cells() -> &'static [Cell] {
    static CELLS: OnceLock<Vec<Cell>> = OnceLock::new();
    CELLS.get_or_init(|| match parse_blob(BLOB) {
        Ok(c) => c,
        Err(e) => {
            log::line(&format!("icons: embedded icon data invalid ({e}), custom icons off"));
            Vec::new()
        }
    })
}

pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01B3);
    }
    h
}

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

#[link(name = "kernel32")]
unsafe extern "system" {
    fn VirtualQuery(addr: *const c_void, info: *mut MemoryBasicInformation, len: usize) -> usize;
}

/// Every page of `[addr, addr+len)` committed, not guard/no-access, and writable.
fn writable(addr: usize, len: usize) -> bool {
    const MEM_COMMIT: u32 = 0x1000;
    const BAD: u32 = 0x01 | 0x100; // PAGE_NOACCESS | PAGE_GUARD
    const WRITE: u32 = 0x04 | 0x08 | 0x40 | 0x80; // RW, WRITECOPY, X_RW, X_WRITECOPY
    let end = addr + len;
    let mut a = addr;
    while a < end {
        let mut info = MaybeUninit::<MemoryBasicInformation>::uninit();
        if unsafe { VirtualQuery(a as *const c_void, info.as_mut_ptr(), size_of::<MemoryBasicInformation>()) } == 0 {
            return false;
        }
        let info = unsafe { info.assume_init() };
        if info.state != MEM_COMMIT || info.protect & BAD != 0 || info.protect & WRITE == 0 {
            return false;
        }
        a = info.base_address as usize + info.region_size;
    }
    true
}

fn read_vec(addr: usize, len: usize) -> Option<Vec<u8>> {
    let mut v = vec![0u8; len];
    game::read_bytes(addr, &mut v).then_some(v)
}

/// UTF-16 name at `addr` (at most `MAX_NAME_CHARS`), None if unreadable or longer.
fn read_name(addr: usize) -> Option<String> {
    // One fault-safe read for the whole name (v8.44): a read per character was a syscall per character, per
    // entry, per texture created (loading slow-down). Falls back to the slow path near the end of a mapping.
    let mut buf = [0u8; (MAX_NAME_CHARS + 1) * 2];
    if game::read_bytes(addr, &mut buf) {
        let mut chars = Vec::new();
        for p in buf.chunks_exact(2) {
            let c = u16::from_le_bytes([p[0], p[1]]);
            if c == 0 {
                return String::from_utf16(&chars).ok();
            }
            chars.push(c);
        }
        return None;
    }
    let mut chars = Vec::new();
    for i in 0..=MAX_NAME_CHARS {
        let c = game::read::<u16>(addr + 2 * i)?;
        if c == 0 {
            return String::from_utf16(&chars).ok();
        }
        chars.push(c);
    }
    None
}

/// What happened to one cell (also the log text).
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Patched,
    AlreadyPatched,
    Mismatch(String),
}

/// Bytes of the cell's block rows inside a DDS that starts at `dds` (row stride = width/4 blocks).
fn row_addr(dds: usize, c: &Cell, r: u32) -> usize {
    dds + DDS_HEADER + (((c.by + r) * (c.width / 4) + c.bx) * 16) as usize
}

fn patch_cell(dds: usize, c: &Cell) -> Outcome {
    let row_len = (c.bw * 16) as usize;
    let mut current = Vec::with_capacity(c.data.len());
    for r in 0..c.bh {
        match read_vec(row_addr(dds, c, r), row_len) {
            Some(v) => current.extend_from_slice(&v),
            None => return Outcome::Mismatch("block rows unreadable".into()),
        }
    }
    let h = fnv1a64(&current);
    if h == c.new_hash {
        return Outcome::AlreadyPatched;
    }
    if h != c.vanilla_hash {
        return Outcome::Mismatch(format!("blocks differ from vanilla (hash {h:016x}, expected {:016x})", c.vanilla_hash));
    }
    let first = row_addr(dds, c, 0);
    let last_end = row_addr(dds, c, c.bh - 1) + row_len;
    if !writable(first, last_end - first) {
        return Outcome::Mismatch("texture memory not writable".into());
    }
    for r in 0..c.bh {
        let src = &c.data[r as usize * row_len..(r as usize + 1) * row_len];
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), row_addr(dds, c, r) as *mut u8, row_len) };
    }
    Outcome::Patched
}

/// Checks the TPF at `base` and patches every matching cell. Cheap for other TPFs: one
/// 16-byte header read, the entry table, and a name read per entry.
fn patch_tpf(base: usize) {
    let Some(hdr) = read_vec(base, 16) else { return };
    if &hdr[0..4] != b"TPF\0" || hdr[12] != 0 || hdr[13] != 3 || hdr[14] != 1 || hdr[15] & 1 != 0 {
        return;
    }
    let count = u32_at(&hdr, 8).unwrap_or(0);
    if count == 0 || count > MAX_TPF_TEXTURES {
        return;
    }
    let cells = cells();
    let Some(table) = read_vec(base + 16, 20 * count as usize) else { return };
    // A TPF without an icon sheet is seen once per texture created from it: remember it (v8.44).
    let key = fnv1a64(&table) ^ (base as u64).rotate_left(17);
    if scanned().contains(&key) {
        return;
    }
    let mut any_icon = false;
    for i in 0..count as usize {
        let e = &table[20 * i..20 * i + 20];
        let Some(name) = u32_at(e, 12).and_then(|o| read_name(base + o as usize)) else { continue };
        if !name.starts_with("SB_Icon") {
            continue;
        }
        let matching: Vec<&Cell> = cells.iter().filter(|c| c.texture == name).collect();
        if matching.is_empty() {
            continue;
        }
        any_icon = true;
        let (off, size, fmt, mips) = (u32_at(e, 0).unwrap_or(0) as usize, u32_at(e, 4).unwrap_or(0), e[8], e[10]);
        let dds = base + off;
        let Some(dh) = read_vec(dds, DDS_HEADER) else {
            log::line(&format!("icons: {name}: DDS header unreadable, skipped"));
            continue;
        };
        let (h, w, dmips) = (u32_at(&dh, 12).unwrap_or(0), u32_at(&dh, 16).unwrap_or(0), u32_at(&dh, 28).unwrap_or(0));
        for c in matching {
            if (w, h) != (c.width, c.height) {
                continue; // the other resolution's record (hi vs low)
            }
            let outcome = if &dh[0..4] != b"DDS "
                || &dh[84..88] != b"DX10"
                || u32_at(&dh, 128) != Some(DXGI_BC7_UNORM)
                || fmt != TPF_FORMAT_BC7
                || mips != 1
                || dmips > 1
                || size != c.dds_size
            {
                Outcome::Mismatch(format!("format differs (tpf fmt {fmt}, mips {mips}/{dmips}, size {size})"))
            } else {
                patch_cell(dds, c)
            };
            if outcome == Outcome::Patched {
                PATCHED.fetch_add(1, Ordering::Relaxed);
            }
            log::line(&format!(
                "icons: {name} {w}x{h} MENU_ItemIcon_{:05} blocks ({},{}) {}x{}: {outcome:?}",
                c.icon_id, c.bx, c.by, c.bw, c.bh
            ));
        }
    }
    if !any_icon {
        let mut s = scanned();
        if s.len() >= 16384 {
            s.clear();
        }
        s.insert(key);
    }
}

type TexCreateFn = extern "C" fn(usize, usize, i32, usize, usize) -> u64;

extern "C" fn tex_create_detour(a1: usize, a2: usize, a3: i32, a4: usize, a5: usize) -> u64 {
    if tuning::custom_icons() && game::read::<u32>(a1 + 104).is_some_and(|n| n != 0) {
        if let Some(base) = game::read_ptr(a2 + 8) {
            let _ = std::panic::catch_unwind(|| patch_tpf(base));
        }
    }
    let t = loop {
        match TRAMP.load(Ordering::Acquire) {
            // Hooked but the trampoline is not stored yet (install window).
            0 => std::hint::spin_loop(),
            t => break t,
        }
    };
    let f: TexCreateFn = unsafe { std::mem::transmute(t) };
    f(a1, a2, a3, a4, a5)
}

/// Run on its own thread from DllMain: the menu textures load at boot, long before the
/// task system `init` waits for. Waits until the function's bytes are there, then hooks it.
pub fn install_early() {
    let on = settings::early_flag("custom_icons", true);
    tuning::CUSTOM_ICONS.store(on, Ordering::Relaxed);
    if !on {
        log::line("icons: custom_icons = 0, menu textures stay vanilla");
        return;
    }
    if cells().is_empty() {
        return;
    }
    let site = game::module_base() + TEX_CREATE_RVA;
    let start = Instant::now();
    let mut found = [0u8; TEX_CREATE_PROLOGUE.len()];
    while !(game::read_bytes(site, &mut found) && found == TEX_CREATE_PROLOGUE) {
        if start.elapsed() > INSTALL_WAIT {
            log::line(&format!("icons: texture-create function at {site:#x} never matched ({found:02x?}), custom icons off"));
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let detour: TexCreateFn = tex_create_detour;
    match hook::install_detour(TEX_CREATE_RVA, &TEX_CREATE_PROLOGUE, detour as usize, "TPF texture create (0x71B750)") {
        Some(t) => {
            TRAMP.store(t, Ordering::Release);
            log::line(&format!(
                "icons: hooked after {} ms, {} cells ready (patched so far {})",
                start.elapsed().as_millis(),
                cells().len(),
                PATCHED.load(Ordering::Relaxed)
            ));
        }
        None => log::line("icons: hook failed, menu textures stay vanilla"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_blob_parses_and_covers_hi_and_low() {
        let cells = parse_blob(BLOB).expect("blob");
        assert_eq!(cells.len(), 6);
        for id in [251, 552, 962] {
            let sizes: Vec<_> = cells.iter().filter(|c| c.icon_id == id).map(|c| (c.width, c.height)).collect();
            assert_eq!(sizes.len(), 2, "icon {id}: one hi and one low record");
        }
        for c in &cells {
            assert!(c.texture.starts_with("SB_Icon"));
            assert_eq!(c.dds_size as usize, DDS_HEADER + (c.width * c.height) as usize);
            assert_ne!(c.vanilla_hash, c.new_hash);
        }
    }

    #[test]
    fn fnv_matches_the_python_generator() {
        assert_eq!(fnv1a64(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xAF63_DC4C_8601_EC8C);
    }

    #[test]
    fn patch_cell_on_a_fake_texture() {
        let cells = parse_blob(BLOB).unwrap();
        let c = cells.iter().find(|c| c.width == 2048).unwrap().clone();
        // Fake DDS with the cell's vanilla bytes unknown: the hash mismatch must leave it alone.
        let mut dds = vec![0u8; c.dds_size as usize];
        let base = dds.as_mut_ptr() as usize;
        assert!(matches!(patch_cell(base, &c), Outcome::Mismatch(_)));
        assert!(dds.iter().all(|&b| b == 0));
        // With a record whose vanilla hash is that of the zero blocks, it patches, then sees it done.
        let zero = vec![0u8; c.data.len()];
        let c2 = Cell { vanilla_hash: fnv1a64(&zero), ..c };
        assert_eq!(patch_cell(base, &c2), Outcome::Patched);
        assert_eq!(patch_cell(base, &c2), Outcome::AlreadyPatched);
        let row_len = (c2.bw * 16) as usize;
        let at = row_addr(base, &c2, 1) - base;
        assert_eq!(&dds[at..at + row_len], &c2.data[row_len..2 * row_len]);
    }
}
