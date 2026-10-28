//! v8.34a: log-only probe of the game's Options menu (docs/DESIGN-NATIVE-OPTIONS-PAGE.md (b), (d)).
//! `[DEBUG] options_probe = 1` (default on this version). No behaviour change: every detour calls
//! the original through its trampoline with all arguments and forwards the return value.
//!
//! - `0x140CB5E30` (OptionTop ctor, CSCommonOptionUnit): after the original returns, logs
//!   `optprobe: OptionTop kinds = [...]` read from `*(unit+2744)` (u32 entries at +8, count at +56).
//!   Prologue [VAR, IDA dis, F:\Temp\ida_opt\o12\summary.txt]: push rbp/rsi/rdi/r12/r13/r14/r15,
//!   i.e. `40 55 56 57 41 54 41 55 41 56 41 57` (same 12 bytes as `0x140CC37D0`), then `lea rbp,[rsp-2F80h]`.
//! - `0x140CC4FE0(kind, a2, a3, std::function*)` (page factory): logs `optprobe: page kind K opened`.
//!   Prologue from the design report (`48 8B C4 4C 89 48 20 57 48 83 EC 70`), checked at install.
//! - `quitblock::msg_detour` (text lookup `0x1410AA310`) calls [`note_text`].
//!
//! The only unverified assumption is the exact layout behind `unit+2744`; the raw bytes are logged
//! next to the decoded kinds so the next version can correct it.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::{game, hook, log};

/// `[DEBUG] options_probe` (`tuning::OPTIONS_PROBE`).
fn enabled() -> bool {
    crate::tuning::OPTIONS_PROBE.load(Ordering::Relaxed)
}

const OPTION_TOP_RVA: usize = 0xCB5E30;
const OPTION_TOP_PROLOGUE: [u8; 12] = [0x40, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57];
const FACTORY_RVA: usize = 0xCC4FE0;
const FACTORY_PROLOGUE: [u8; 12] = [0x48, 0x8B, 0xC4, 0x4C, 0x89, 0x48, 0x20, 0x57, 0x48, 0x83, 0xEC, 0x70];

/// `*(unit + KINDS_OFFSET)` points at the kinds block.
const KINDS_OFFSET: usize = 2744;
/// Inside the block: u32 entries from +8, the count at +56.
const KINDS_ENTRIES: usize = 8;
const KINDS_COUNT: usize = 56;
const KINDS_BLOCK_LEN: usize = 64;
const MAX_KINDS: usize = 32;

/// After the pad F11 event, the first this many distinct ids within this long are logged.
const PAD_WINDOW_MS: u64 = 2_000;
const PAD_MAX_IDS: usize = 40;
const MAX_TEXT_CHARS: usize = 80;

const OPEN_RVA: usize = 0xC4B40;
/// `Open(u32 msg, i32 buttons, u32 type)`: three `mov [rsp+..], reg` (15 bytes, relocatable) + `push rdi`.
const OPEN_PROLOGUE: [u8; 16] = [0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x89, 0x6C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x57];
/// Relocated into the trampoline: the first 15 bytes (ends before `push rdi`, on an instruction boundary).
const OPEN_HOOK_LEN: usize = 15;
const OPEN_PER_PAIR: u8 = 3;
const OPEN_MAX_LINES: u32 = 200;

static OPEN_TRAMP: AtomicUsize = AtomicUsize::new(0);
static OPEN_CALLS: AtomicU32 = AtomicU32::new(0);
static OPEN_LIMIT: Mutex<OpenLimiter> = Mutex::new(OpenLimiter::new());
static OPTION_TOP_TRAMP: AtomicUsize = AtomicUsize::new(0);
static FACTORY_TRAMP: AtomicUsize = AtomicUsize::new(0);
/// `game::now_ms()` of the last pad F11 event (0 = never).
static PAD_F11_MS: AtomicU64 = AtomicU64::new(0);
static SEEN_WATCH: Mutex<Option<HashSet<(u32, u32)>>> = Mutex::new(None);
static SEEN_PAD: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static LAST_KINDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

// ---------- pure helpers ----------

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// The text ids the design wants labelled (FDP_MenuText 110xxx/111xxx, the Default button 40330/405xx).
pub fn watched(id: u32) -> bool {
    matches!(id, 110000..=110010 | 110100 | 110400 | 110500 | 110600..=110605 | 110700 | 111000 | 111024 | 111030 | 111100..=111114
        | 40330 | 40400 | 40401 | 40500)
}

/// Decodes the kinds block bytes: u32 entries from +8, count (u32) at +56, entries at most
/// `MAX_KINDS`, never past the end of `block`. `None` if the block is too short for the count field.
pub fn parse_kinds(block: &[u8]) -> Option<Vec<u32>> {
    let c = block.get(KINDS_COUNT..KINDS_COUNT + 4)?;
    let count = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize;
    let n = count.min(MAX_KINDS).min((KINDS_COUNT - KINDS_ENTRIES) / 4);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let at = KINDS_ENTRIES + 4 * i;
        let e = block.get(at..at + 4)?;
        out.push(u32::from_le_bytes([e[0], e[1], e[2], e[3]]));
    }
    Some(out)
}

/// Is `now` within the pad window that began at `start` (0 = no event yet)?
pub fn in_window(start: u64, now: u64) -> bool {
    start != 0 && now >= start && now - start <= PAD_WINDOW_MS
}

fn units_to_string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// Rate limiter for the `Open` log: at most `OPEN_PER_PAIR` lines per distinct (msg, caller) pair and
/// `OPEN_MAX_LINES` lines in total. `caller` is the RVA, or `usize::MAX` for the mod's own calls.
pub struct OpenLimiter {
    pairs: Vec<((u32, usize), u8)>,
    lines: u32,
}

impl OpenLimiter {
    pub const fn new() -> Self {
        Self { pairs: Vec::new(), lines: 0 }
    }

    pub fn allow(&mut self, msg: u32, caller: usize) -> bool {
        if self.lines >= OPEN_MAX_LINES {
            return false;
        }
        let key = (msg, caller);
        if let Some(e) = self.pairs.iter_mut().find(|e| e.0 == key) {
            if e.1 >= OPEN_PER_PAIR {
                return false;
            }
            e.1 += 1;
        } else {
            self.pairs.push((key, 1));
        }
        self.lines += 1;
        true
    }
}

/// `caller` None = outside the game image (the mod's own dialogs).
pub fn format_open(msg: u32, buttons: i32, kind: u32, caller: Option<usize>) -> String {
    match caller {
        Some(rva) => format!("optprobe: Open msg={msg} buttons={buttons} type={kind} caller={rva:#x}"),
        None => format!("optprobe: Open msg={msg} buttons={buttons} type={kind} caller=mod"),
    }
}

/// Number of `Open` calls seen by the detour while the probe flag was on (tests, diagnostics).
#[allow(dead_code)]
pub fn open_calls_seen() -> u32 {
    OPEN_CALLS.load(Ordering::Relaxed)
}

fn image_size(base: usize) -> usize {
    game::read::<u32>(base + 0x3C).and_then(|nt| game::read::<u32>(base + nt as usize + 0x50)).unwrap_or(0) as usize
}

// ---------- game reads ----------

fn read_wide(p: usize) -> Option<String> {
    if p == 0 {
        return None;
    }
    let mut v: Vec<u16> = Vec::new();
    for i in 0..MAX_TEXT_CHARS {
        match game::read::<u16>(p + 2 * i)? {
            0 => break,
            c => v.push(c),
        }
    }
    Some(units_to_string(&v))
}

fn read_block(p: usize) -> Option<Vec<u8>> {
    let mut v = Vec::with_capacity(KINDS_BLOCK_LEN);
    for i in 0..KINDS_BLOCK_LEN / 8 {
        v.extend_from_slice(&game::read::<u64>(p + 8 * i)?.to_le_bytes());
    }
    Some(v)
}

fn log_kinds(unit: usize) {
    let Some(block_ptr) = game::read_ptr(unit + KINDS_OFFSET).filter(|p| *p != 0) else {
        log::line(&format!("optprobe: OptionTop unit {unit:#x}: no kinds block at +{KINDS_OFFSET}"));
        return;
    };
    let Some(block) = read_block(block_ptr) else {
        log::line(&format!("optprobe: OptionTop kinds block {block_ptr:#x} unreadable"));
        return;
    };
    let Some(kinds) = parse_kinds(&block) else { return };
    // The ctor runs for each menu build; log only when the list changes.
    if let Ok(mut last) = LAST_KINDS.lock() {
        if *last == kinds {
            return;
        }
        *last = kinds.clone();
    }
    let list: Vec<String> = kinds.iter().map(|k| k.to_string()).collect();
    log::line(&format!("optprobe: OptionTop kinds = [{}]", list.join(", ")));
    log::line(&format!("optprobe: OptionTop kinds block {block_ptr:#x} raw = {}", hex(&block)));
}

// ---------- detours ----------

extern "C" fn option_top_detour(a1: usize, a2: usize, a3: usize, a4: usize) -> usize {
    let t = OPTION_TOP_TRAMP.load(Ordering::Acquire);
    if t == 0 {
        return 0;
    }
    let f: extern "C" fn(usize, usize, usize, usize) -> usize = unsafe { std::mem::transmute(t) };
    let r = f(a1, a2, a3, a4);
    if enabled() {
        let _ = std::panic::catch_unwind(|| log_kinds(a1));
    }
    r
}

extern "C" fn open_detour(msg: u32, buttons: i32, kind: u32, a4: u32) -> u64 {
    let t = OPEN_TRAMP.load(Ordering::Acquire);
    if t == 0 {
        return 0;
    }
    // Read the caller slot before anything else can overwrite it (racy across threads, diagnostics only).
    let caller = hook::detour_caller(t);
    if enabled() {
        let _ = std::panic::catch_unwind(|| {
            OPEN_CALLS.fetch_add(1, Ordering::Relaxed);
            let base = game::module_base();
            let rva = (caller >= base && caller < base + image_size(base)).then(|| caller - base);
            let key = rva.unwrap_or(usize::MAX);
            let allow = OPEN_LIMIT.lock().map(|mut l| l.allow(msg, key)).unwrap_or(false);
            if allow {
                log::line(&format_open(msg, buttons, kind, rva));
            }
        });
    }
    let f: extern "C" fn(u32, i32, u32, u32) -> u64 = unsafe { std::mem::transmute(t) };
    f(msg, buttons, kind, a4)
}

extern "C" fn factory_detour(kind: u32, a2: usize, a3: usize, a4: usize) -> usize {
    if enabled() {
        log::line(&format!("optprobe: page kind {kind} opened"));
    }
    let t = FACTORY_TRAMP.load(Ordering::Acquire);
    if t == 0 {
        return 0;
    }
    let f: extern "C" fn(u32, usize, usize, usize) -> usize = unsafe { std::mem::transmute(t) };
    f(kind, a2, a3, a4)
}

/// Called by the pad F11 handler (puppet.rs, next to the `input: gamepad button triggered F11` log).
#[allow(dead_code)] // wired from puppet.rs by the integrator
pub fn note_pad_f11() {
    if !enabled() {
        return;
    }
    PAD_F11_MS.store(game::now_ms().max(1), Ordering::Relaxed);
    if let Ok(mut s) = SEEN_PAD.lock() {
        s.clear();
    }
}

/// Called by `quitblock::msg_detour` for every text lookup; `result` is the returned wchar_t*.
pub fn note_text(bank: u32, id: u32, result: usize) {
    if !enabled() {
        return;
    }
    note_keyguide(bank, id, result);
    let watch = watched(id);
    let pad = in_window(PAD_F11_MS.load(Ordering::Relaxed), game::now_ms());
    if !watch && !pad {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        let mut emit = false;
        if watch
            && let Ok(mut g) = SEEN_WATCH.lock()
            && g.get_or_insert_with(HashSet::new).insert((bank, id))
        {
            emit = true;
        }
        if pad
            && let Ok(mut s) = SEEN_PAD.lock()
            && s.len() < PAD_MAX_IDS
            && !s.contains(&(bank, id))
        {
            s.push((bank, id));
            emit = true;
        }
        if emit {
            let text = read_wide(result).map(|t| format!("\"{t}\"")).unwrap_or_else(|| "(null)".to_string());
            log::line(&format!("optprobe: text bank {bank} id {id} = {text}"));
        }
    });
}

/// Installs the two detours when `[DEBUG] options_probe = 1`. Never installs on a prologue mismatch
/// (the bytes found are logged by `hook::install_detour`).
pub fn install() {
    if !enabled() {
        return;
    }
    let base = game::module_base();
    for (rva, name) in [(OPTION_TOP_RVA, "0x140CB5E30"), (FACTORY_RVA, "0x140CC4FE0")] {
        let bytes: Vec<u8> = (0..12).filter_map(|i| game::read::<u8>(base + rva + i)).collect();
        log::line(&format!("optprobe: prologue of {name} = {}", hex(&bytes)));
    }
    let top: extern "C" fn(usize, usize, usize, usize) -> usize = option_top_detour;
    let fac: extern "C" fn(u32, usize, usize, usize) -> usize = factory_detour;
    let mut ok = 0;
    if let Some(t) = hook::install_detour(OPTION_TOP_RVA, &OPTION_TOP_PROLOGUE, top as usize, "OptionTop ctor (0xCB5E30)") {
        OPTION_TOP_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    if let Some(t) = hook::install_detour(FACTORY_RVA, &FACTORY_PROLOGUE, fac as usize, "options page factory (0xCC4FE0)") {
        FACTORY_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    let open_bytes: Vec<u8> = (0..OPEN_PROLOGUE.len()).filter_map(|i| game::read::<u8>(base + OPEN_RVA + i)).collect();
    log::line(&format!("optprobe: prologue of Open = {}", hex(&open_bytes)));
    if open_bytes == OPEN_PROLOGUE {
        let op: extern "C" fn(u32, i32, u32, u32) -> u64 = open_detour;
        if let Some(t) = hook::install_detour(OPEN_RVA, &OPEN_PROLOGUE[..OPEN_HOOK_LEN], op as usize, "dialog Open (0x1408C4B40)") {
            OPEN_TRAMP.store(t, Ordering::Release);
            ok += 1;
        }
    } else {
        log::line("optprobe: Open prologue differs, not hooking");
    }
    log::line(&format!("optprobe: {ok}/3 hooks installed (log only)"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_format() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x40, 0x5, 0xFF]), "40 05 FF");
    }

    #[test]
    fn watch_set() {
        for id in [110000, 110010, 110100, 110400, 110500, 110600, 110605, 110700, 111000, 111024, 111030, 111100, 111114, 40330, 40400, 40401, 40500] {
            assert!(watched(id), "{id}");
        }
        for id in [109999, 110011, 110101, 110606, 110701, 111001, 111115, 40331, 0] {
            assert!(!watched(id), "{id}");
        }
    }

    fn block(kinds: &[u32], count: u32) -> Vec<u8> {
        let mut b = vec![0u8; KINDS_BLOCK_LEN];
        for (i, k) in kinds.iter().enumerate() {
            b[8 + 4 * i..12 + 4 * i].copy_from_slice(&k.to_le_bytes());
        }
        b[56..60].copy_from_slice(&count.to_le_bytes());
        b
    }

    #[test]
    fn kinds_reader() {
        assert_eq!(parse_kinds(&block(&[0, 1, 2, 3, 9, 5], 6)), Some(vec![0, 1, 2, 3, 9, 5]));
        assert_eq!(parse_kinds(&block(&[7], 0)), Some(vec![]));
        // a count larger than the buffer is capped, never reads past the block
        assert_eq!(parse_kinds(&block(&[1; 12], 1000)).map(|v| v.len()), Some(12));
        assert_eq!(parse_kinds(&block(&[1, 2], u32::MAX)).map(|v| v.len()), Some(12));
        // too short for the count field
        assert_eq!(parse_kinds(&[0u8; 40]), None);
        assert_eq!(parse_kinds(&[]), None);
    }

    #[test]
    fn pad_window() {
        assert!(!in_window(0, 100));
        assert!(in_window(1000, 1000));
        assert!(in_window(1000, 3000));
        assert!(!in_window(1000, 3001));
        assert!(!in_window(1000, 999));
    }

    #[test]
    fn open_limiter() {
        let mut l = OpenLimiter::new();
        for _ in 0..3 {
            assert!(l.allow(321011, 0x68C270));
        }
        assert!(!l.allow(321011, 0x68C270));
        assert!(l.allow(321011, 0x68DF90)); // other caller
        assert!(l.allow(5, 0x68C270)); // other msg
        assert!(l.allow(5, usize::MAX));
        let mut l = OpenLimiter::new();
        let mut n = 0;
        for m in 0..1000 {
            if l.allow(m, 1) {
                n += 1;
            }
        }
        assert_eq!(n, OPEN_MAX_LINES);
    }

    #[test]
    fn open_format() {
        assert_eq!(format_open(321011, 1, 2, Some(0x68C270)), "optprobe: Open msg=321011 buttons=1 type=2 caller=0x68c270");
        assert_eq!(format_open(7, -1, 0, None), "optprobe: Open msg=7 buttons=-1 type=0 caller=mod");
    }

    #[test]
    fn open_prologue_shape() {
        assert_eq!(OPEN_PROLOGUE[15], 0x57);
        assert_eq!(open_calls_seen(), 0);
    }

    #[test]
    fn utf16_decode() {
        let u: Vec<u16> = "Network Settings".encode_utf16().collect();
        assert_eq!(units_to_string(&u), "Network Settings");
    }
}

static SEEN_KEYGUIDE: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());

/// v8.44: the key guide texts (banks FDP_KeyGuide 202 / 234 / 274) with every UTF-16 unit that is not plain
/// ASCII shown as `<hex>`, so the icon markup of a key guide line is visible in the log (needed to build a key
/// guide line with the LT/RT icons natively). At most 160 distinct (bank, id) pairs per run.
fn note_keyguide(bank: u32, id: u32, result: usize) {
    if !matches!(bank, 202 | 234 | 274) || result == 0 {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        let Ok(mut seen) = SEEN_KEYGUIDE.lock() else { return };
        if seen.len() >= 160 || seen.contains(&(bank, id)) {
            return;
        }
        seen.push((bank, id));
        drop(seen);
        let mut text = String::new();
        for i in 0..MAX_TEXT_CHARS {
            let Some(c) = game::read::<u16>(result + 2 * i) else { break };
            match c {
                0 => break,
                0x20..=0x7e => text.push(c as u8 as char),
                _ => text.push_str(&format!("<{c:x}>")),
            }
        }
        log::line(&format!("optprobe: keyguide bank {bank} id {id} = \"{text}\""));
    });
}
