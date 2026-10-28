//! v8.35c: the native "Co-op Settings" page (docs/DESIGN-NATIVE-OPTIONS-PAGE.md, addendum).
//!
//! `[GAMEPLAY] native_mod_menu = 1` replaces the page builder of the game's Network Settings entry
//! (OptionTop kind 2, `0x140CCDED0(int a1, i64 a2) -> page*`) by ours. With the flag off, the mod not
//! alive, or [`msfunc::failed`] latched, the detour only calls the original through its trampoline.
//!
//! Sequence of the original builder [VAR, F:\Temp\ida_opt\o13\f_140ccded0.c] and what we do:
//! 1. `page = 0x1419DE730(4696, 8, heap)` where `heap = *0x143D9CE28` (the thunk jumps to
//!    `heap->vtbl[10](heap, 4696, 8)`). `0x1419DE750(page, 8, heap)` is the DEALLOCATION and the original
//!    only reaches it in the exception path of the constructor: we never call it.
//! 2. `0x140CB8980(page, a1, a2)` generic CSOptionPageComponent constructor.
//! 3. The original adds ONE native row (bound to `a2+109`) and the Defaults row `0x140CC7FC0`: we add
//!    neither. Instead, per model row: label buffer (176 bytes) filled by `0x140CC5D10` while
//!    `nativetext::arm_override(label, help)` is armed (it asks `0x140C7B910(buf+64, 110710)` and
//!    `0x140C7B110(buf+120, 110701)`), then the combo row helper `0x140CADBA0(page, label, &11,
//!    &{7, 0, L"02_201_CommandList"}, &getter, &setter, &list, &empty)`, then `0x140C609A0(label)`.
//! 4. `0x140CA0AB0(page, 0)` finalise, return `page`.
//!
//! The getter / setter / list are MSVC `std::function`s built by [`msfunc::init_function`]; their handlers
//! ([`h_get`], [`h_set`], [`h_list`]) dispatch on the row index into [`optmenu_model::rows`]. The list is
//! built into the game's `ComboItemList<uint8_t>` (count at +3848, capacity 32 items of 120 bytes) with the
//! per-item sequence of `0x140CFDA60` ([`build_items`]).
//!
//! The callbacks only touch Rust state and the settings file; game memory is written only by the native
//! helpers above (and into the list buffer / page the game handed us).
#![allow(dead_code)]

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::menu::{self, Role, Values};
use crate::optmenu_model::{self as model, Authority, MAX_ITEMS, RowDef, RowKind};
use crate::{game, hook, log, msfunc, nativetext, settings, tuning};

// ---------- native addresses and prologues (all from the IDA decompiles/disassembly in F:\Temp\ida_opt) ----------

/// One native function we call (or detour): RVA and the expected first bytes.
pub struct Native {
    pub name: &'static str,
    pub rva: usize,
    pub expect: &'static [u8],
}

const BUILDER_RVA: usize = 0xCCDED0;
/// push rdi / mov eax, 1020h (the 5th instruction on is a rel32 call: detour 7 bytes only). [o13/summary.txt]
const BUILDER_PROLOGUE: [u8; 7] = [0x40, 0x57, 0xB8, 0x20, 0x10, 0x00, 0x00];
/// The routine that (re)builds the right-hand page for the selected Options entry (`0x140CC4B50(unit)`); first 11 bytes = 4 whole instructions.
const OPEN_RVA: usize = 0xCC4B50;
const OPEN_PROLOGUE: [u8; 11] = [0x48, 0x8B, 0xC4, 0x56, 0x57, 0x41, 0x56, 0x48, 0x83, 0xEC, 0x70];
/// The game's list shows 11 rows and does not scroll (live test 2026-10-26): only 10 rows fit (live test): slot 0 is a page selector, the other 9 slots show 9 settings of the current page.
const SELECTOR_ID: u64 = 1000;
const PER_PAGE: usize = 9;
static PAGE: AtomicU8 = AtomicU8::new(0);
static PAGING: AtomicBool = AtomicBool::new(false);
static UNIT: AtomicUsize = AtomicUsize::new(0);
static OPEN_TRAMP: AtomicUsize = AtomicUsize::new(0);
static REBUILD_AT: AtomicU64 = AtomicU64::new(0);
static LAST_PAGE: AtomicUsize = AtomicUsize::new(0);

fn page_count() -> usize {
    model::rows().len().div_ceil(PER_PAGE)
}

/// `(row id, label, help)` of the slots of the current page.
fn page_slots() -> Vec<(u64, &'static str, &'static str)> {
    if !PAGING.load(Ordering::Relaxed) {
        return model::page_rows().iter().enumerate().map(|(i, r)| (i as u64, r.label, r.help)).collect();
    }
    let page = (PAGE.load(Ordering::Relaxed) as usize).min(page_count() - 1);
    let mut v = vec![(SELECTOR_ID, "Page", "Left and right switch the page of settings.")];
    let rows = model::rows();
    for i in page * PER_PAGE..((page + 1) * PER_PAGE).min(rows.len()) {
        v.push((i as u64, rows[i].label, rows[i].help));
    }
    v
}

/// `*(RVA)` = the game heap object (`qword_143D9CE28`), also what `0x140C7CD60` returns.
const HEAP_GLOBAL_RVA: usize = 0x3D9CE28;
const PAGE_SIZE: usize = 4696;
const PAGE_ALIGN: usize = 8;
/// `ComboItemList_uint8`: item count at +3848, items of 120 bytes from the 8-aligned base.
const LIST_COUNT_OFFSET: usize = 3848;
const ITEM_SIZE: usize = 120;
/// Label buffer of `0x140CC5D10` / `0x140C609A0`.
const LABEL_SIZE: usize = 176;
/// The text object of `0x140C7B910`: `{wchar_t* raw; DLWString{alloc, buf[16], size, cap, flag(u8 at +48)}}` = 49 bytes padded to 56 (Opus review: 48 overran by one byte). The buffer is 64 for slack.
const TEXT_SIZE: usize = 64;
/// 40400 / 40401 are "Fullscreen" / "Windowed" (probe log 2026-10-26), not On / Off: On / Off are our own interned texts.
const LAYOUT_NAME: &str = "02_201_CommandList";
const ROW_TYPE_COMBO: i32 = 11;

macro_rules! native {
    ($name:expr, $rva:expr, [$($b:expr),+ $(,)?]) => {
        Native { name: $name, rva: $rva, expect: &[$($b),+] }
    };
}

/// Index 0 is the page builder (also checked by `hook::install_detour`).
static CHECKS: [Native; 13] = [
    native!("page builder 0x140CCDED0", BUILDER_RVA, [0x40, 0x57, 0xB8, 0x20, 0x10, 0x00, 0x00]),
    native!("combo row helper 0x140CADBA0", 0xCADBA0, [0x48, 0x8B, 0xC4, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8D, 0xA8, 0x58]),
    native!("page ctor 0x140CB8980", 0xCB8980, [0x48, 0x89, 0x4C, 0x24, 0x08, 0x57, 0x48, 0x81, 0xEC, 0x90, 0x00, 0x00, 0x00, 0x48, 0xC7, 0x44]),
    native!("finalise 0x140CA0AB0", 0xCA0AB0, [0x48, 0x8B, 0xC4, 0x55, 0x57, 0x41, 0x54, 0x41, 0x56, 0x41, 0x57, 0x48, 0x8D, 0xA8, 0x48, 0xFF]),
    native!("label getter 0x140CC5D10", 0xCC5D10, [0x48, 0x89, 0x4C, 0x24, 0x08, 0x53, 0x48, 0x83, 0xEC, 0x30, 0x48, 0xC7, 0x44, 0x24, 0x28, 0xFE]),
    native!("label free 0x140C609A0", 0xC609A0, [0x48, 0x89, 0x4C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x30, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE]),
    native!("alloc thunk 0x1419DE730", 0x19DE730, [0x49, 0x8B, 0x00, 0x4D, 0x8B, 0xC8, 0x4C, 0x8B, 0xC2, 0x48, 0x8B, 0xD1, 0x49, 0x8B, 0xC9, 0x48]),
    native!("text getter 0x140C7B910", 0xC7B910, [0x48, 0x89, 0x4C, 0x24, 0x08, 0x53, 0x48, 0x83, 0xEC, 0x40, 0x48, 0xC7, 0x44, 0x24, 0x38, 0xFE]),
    native!("wstring copy 0x1401C7130", 0x1C7130, [0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x4C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20, 0x44]),
    native!("wstring new 0x1401C8720", 0x1C8720, [0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x74, 0x24, 0x18, 0x48, 0x89, 0x4C, 0x24, 0x08, 0x57]),
    native!("list append 0x140CFF3C0", 0xCFF3C0, [0x40, 0x57, 0x48, 0x83, 0xEC, 0x30, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE, 0xFF, 0xFF, 0xFF, 0x48]),
    native!("item free 0x140CBBC30", 0xCBBC30, [0x48, 0x89, 0x4C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x30, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE]),
    // `mov rax, [rip+0x031200C1]; ret` = the load of the heap global at 0x143D9CE28 (proves the global's address).
    native!("heap global load 0x140C7CD60", 0xC7CD60, [0x48, 0x8B, 0x05, 0xC1, 0x00, 0x12, 0x03, 0xC3]),
];

/// Logged but never called (the deallocation, only in the original's exception path).
const FREE_INFO: Native = native!("dealloc 0x1419DE750 (not called)", 0x19DE750, [0x48, 0x85, 0xC9, 0x74, 0x41, 0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20, 0x49]);

/// Pure: the first check whose found bytes differ from the expected ones. `read(rva, len)` -> found bytes.
pub fn first_mismatch<'a>(checks: &'a [Native], read: &dyn Fn(usize, usize) -> Vec<u8>) -> Option<&'a Native> {
    checks.iter().find(|c| read(c.rva, c.expect.len()) != c.expect)
}

// ---------- natives table ----------

/// The natives the list handler uses (a separate struct so tests can fake it).
#[derive(Clone, Copy)]
pub struct ListNatives {
    /// `0x140C7B910(text48*, id) -> text48*`
    pub text: unsafe extern "C" fn(*mut u8, u32) -> *mut u8,
    /// `0x1401C7130(dst48*, src48*) -> dst`
    pub str_copy: unsafe extern "C" fn(*mut u8, *const u8) -> *mut u8,
    /// `0x1401C8720(dst48*, const wchar_t*, heap) -> dst`
    pub str_new: unsafe extern "C" fn(*mut u8, *const u16, usize) -> *mut u8,
    /// `0x140CFF3C0(list*, item120*)`
    pub append: unsafe extern "C" fn(*mut u8, *const u8) -> usize,
    /// `0x140CBBC30(item120*)`
    pub item_free: unsafe extern "C" fn(*mut u8),
    /// `*0x143D9CE28`
    pub heap: usize,
}

#[derive(Clone, Copy)]
pub struct Natives {
    /// `0x1419DE730(size, align, heap) -> void*`
    pub alloc: unsafe extern "C" fn(usize, usize, usize) -> usize,
    /// `0x140CB8980(page, ptr a1 (64-bit: the parent scene, not an int), i64 a2) -> page`
    pub ctor: unsafe extern "C" fn(usize, usize, usize) -> usize,
    /// `0x140CC5D10(label176*) -> label176*`
    pub label_get: unsafe extern "C" fn(*mut u8) -> *mut u8,
    /// `0x140C609A0(label176*)`
    pub label_free: unsafe extern "C" fn(*mut u8),
    /// `0x140CADBA0(page, label, &type, &layout, &getter, &setter, &list, &extra)`
    pub row: unsafe extern "C" fn(usize, *const u8, *const i32, *const Layout, *const u8, *const u8, *const u8, *const u8) -> usize,
    /// `0x140CA0AB0(page, 0)`
    pub finalise: unsafe extern "C" fn(usize, u32) -> usize,
    pub list: ListNatives,
}

static NATIVES: OnceLock<Natives> = OnceLock::new();

fn resolve(base: usize, heap: usize) -> Natives {
    let at = |rva: usize| base + rva;
    // SAFETY: every address was prologue-checked against the IDA bytes before this runs.
    unsafe {
        Natives {
            alloc: std::mem::transmute(at(0x19DE730)),
            ctor: std::mem::transmute(at(0xCB8980)),
            label_get: std::mem::transmute(at(0xCC5D10)),
            label_free: std::mem::transmute(at(0xC609A0)),
            row: std::mem::transmute(at(0xCADBA0)),
            finalise: std::mem::transmute(at(0xCA0AB0)),
            list: ListNatives {
                text: std::mem::transmute(at(0xC7B910)),
                str_copy: std::mem::transmute(at(0x1C7130)),
                str_new: std::mem::transmute(at(0x1C8720)),
                append: std::mem::transmute(at(0xCFF3C0)),
                item_free: std::mem::transmute(at(0xCBBC30)),
                heap,
            },
        }
    }
}

// ---------- structs handed to the game ----------

/// The row layout name struct (`{u32 7; u8 0; wchar_t* name}`), 16 bytes.
#[repr(C)]
pub struct Layout {
    pub kind: u32,
    pub flag: u8,
    pub pad: [u8; 3],
    pub name: *const u16,
}

const fn utf16<const N: usize>(s: &str) -> [u16; N] {
    let b = s.as_bytes();
    let mut out = [0u16; N];
    let mut i = 0;
    while i < b.len() {
        out[i] = b[i] as u16;
        i += 1;
    }
    out
}

static LAYOUT_NAME_W: [u16; LAYOUT_NAME.len() + 1] = utf16(LAYOUT_NAME);
static EMPTY_W: [u16; 1] = [0];

#[repr(C, align(16))]
struct Fn64([u8; 64]);
#[repr(C, align(16))]
struct Label([u8; LABEL_SIZE]);
#[repr(C, align(16))]
struct Text64([u8; TEXT_SIZE]);
#[repr(C, align(8))]
struct Item120([u8; ITEM_SIZE]);

// ---------- list items ----------

/// Frees the heap buffer of a `0x140C7B910` text object and resets it (the inline tail of `0x140CFDA60`):
/// `if cap >= 8 { alloc->vtbl[13](alloc, buf) }; cap = 7; size = 0; buf[0] = 0`.
///
/// # Safety
/// `t` points to a constructed 48-byte text object.
pub unsafe fn free_text(t: *mut u8) {
    unsafe {
        let cap = *(t.add(40) as *const u64);
        if cap >= 8 {
            let alloc = *(t.add(8) as *const usize);
            let buf = *(t.add(16) as *const usize);
            if alloc != 0 {
                let vt = *(alloc as *const usize);
                let f: unsafe extern "C" fn(usize, usize) = std::mem::transmute(*((vt + 104) as *const usize));
                f(alloc, buf);
            }
        }
        *(t.add(40) as *mut u64) = 7;
        *(t.add(32) as *mut u64) = 0;
        *(t.add(16) as *mut u16) = 0;
    }
}

/// Builds the game's `ComboItemList<uint8_t>` in `ret` from `(value, text id)` pairs, following the
/// per-item sequence of `0x140CFDA60`. At most [`MAX_ITEMS`] (the list's capacity) are added.
///
/// # Safety
/// `ret` points to a list buffer of the game (>= 3856 bytes); `n` holds working natives.
pub unsafe fn build_items(n: &ListNatives, ret: *mut u8, items: &[(u8, u32)]) {
    unsafe {
        *(ret.add(LIST_COUNT_OFFSET) as *mut u64) = 0;
        for &(value, id) in items.iter().take(MAX_ITEMS) {
            let mut tmp = Text64([0; TEXT_SIZE]);
            let mut t = (n.text)(tmp.0.as_mut_ptr(), id);
            if t.is_null() {
                t = tmp.0.as_mut_ptr();
            }
            let mut item = Item120([0; ITEM_SIZE]);
            let ip = item.0.as_mut_ptr();
            *ip = value;
            std::ptr::copy_nonoverlapping(t, ip.add(8), 8);
            (n.str_copy)(ip.add(16), t.add(8));
            // item +64 stays 0
            (n.str_new)(ip.add(72), EMPTY_W.as_ptr(), crate::game::read::<usize>(crate::game::module_base() + 0x3D9CE28).unwrap_or(n.heap));
            (n.append)(ret, ip);
            (n.item_free)(ip);
            free_text(t);
        }
    }
}
/// The text id of one list label: always our interned text.
pub fn text_id(label: &str) -> u32 {
    nativetext::intern(label)
}

// ---------- the page ----------

/// Builds the page. Returns the page pointer. Nothing is allocated when it returns `Err`.
///
/// # Safety
/// `n` holds working natives (or fakes whose behaviour matches).
pub unsafe fn build_page(n: &Natives, heap: usize, a1: usize, a2: usize) -> Result<usize, String> {
    unsafe {
        let rows = page_slots();
        if rows.is_empty() || rows.len() > model::MAX_ROWS {
            return Err(format!("{} rows", rows.len()));
        }
        if heap == 0 {
            return Err("game heap pointer is null".into());
        }
        let mem = (n.alloc)(PAGE_SIZE, PAGE_ALIGN, heap);
        if mem == 0 {
            return Err("page allocation failed".into());
        }
        let page = (n.ctor)(mem, a1, a2);
        if page == 0 {
            return Err("page constructor returned null".into());
        }
        let ty: i32 = ROW_TYPE_COMBO;
        let layout = Layout { kind: 7, flag: 0, pad: [0; 3], name: LAYOUT_NAME_W.as_ptr() };
        let empty = Fn64([0; 64]);
        for &(id, label_text, help_text) in rows.iter() {
            let mut label = Label([0; LABEL_SIZE]);
            nativetext::arm_override(label_text, help_text);
            (n.label_get)(label.0.as_mut_ptr());
            nativetext::disarm_override();
            let (mut g, mut s, mut l) = (Fn64([0; 64]), Fn64([0; 64]), Fn64([0; 64]));
            msfunc::init_function(g.0.as_mut_ptr(), msfunc::FnKind::Getter, id);
            msfunc::init_function(s.0.as_mut_ptr(), msfunc::FnKind::Setter, id);
            msfunc::init_function(l.0.as_mut_ptr(), msfunc::FnKind::List, id);
            (n.row)(page, label.0.as_ptr(), &ty, &layout, g.0.as_ptr(), s.0.as_ptr(), l.0.as_ptr(), empty.0.as_ptr());
            (n.label_free)(label.0.as_mut_ptr());
        }
        (n.finalise)(page, 0);
        Ok(page)
    }
}

static TRAMP: AtomicUsize = AtomicUsize::new(0);

fn tramp() -> usize {
    // In the install window (patch in, trampoline not stored yet) wait for it.
    loop {
        match TRAMP.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => return t,
        }
    }
}

fn active() -> bool {
    tuning::NATIVE_MOD_MENU.load(Ordering::Relaxed) && !msfunc::failed() && crate::mod_alive() && NATIVES.get().is_some()
}

/// Latches the page off for later builds and logs why.
fn fail(why: &str) {
    msfunc::latch_failed();
    log::line(&format!("optmenu: OFF ({why})"));
}

extern "C" fn builder_detour(a1: usize, a2: usize) -> usize {
    let t = tramp();
    // SAFETY: the trampoline runs the relocated prologue and continues in the original builder.
    let orig: extern "C" fn(usize, usize) -> usize = unsafe { std::mem::transmute(t) };
    if !active() {
        return orig(a1, a2);
    }
    let Some(n) = NATIVES.get() else { return orig(a1, a2) };
    let inworld = IN_WORLD.load(Ordering::Relaxed);
    log::line(&format!("optmenu: page builder called (a1 {a1:#x} (was truncated to 32 bit until v8.42), a2 {a2:#x}, in a world {inworld})"));
    if inworld && !tuning::NATIVE_MOD_MENU_INWORLD.load(Ordering::Relaxed) {
        log::line("optmenu: in a world, the game's own page is used (native_mod_menu_inworld = 0)");
        return orig(a1, a2);
    }
    let heap = game::read::<usize>(game::module_base() + HEAP_GLOBAL_RVA).unwrap_or(0);
    match catch_unwind(AssertUnwindSafe(|| unsafe { build_page(n, heap, a1, a2) })) {
        Ok(Ok(page)) => {
            LAST_PAGE.store(page, Ordering::Relaxed);
            log::line(&format!("optmenu: Network Settings page replaced: {} rows", model::page_rows().len()));
            page
        }
        Ok(Err(why)) => {
            fail(&why);
            orig(a1, a2)
        }
        Err(_) => {
            fail("panic while building the page");
            orig(a1, a2)
        }
    }
}

// ---------- values ----------

pub fn role_name(r: Role) -> &'static str {
    match r {
        Role::Solo => "Solo",
        Role::Host => "Host",
        Role::Client => "Client",
    }
}

/// `(role, connected)`: connected = in a session at all.
fn env() -> (Role, bool) {
    let role = menu::snapshot().0;
    (role, role != Role::Solo)
}

/// key -> value of an ini text (same rules as settings.rs: `;` / `#` comments, sections ignored, keys
/// lower-cased). A trailing ` ; comment` is cut (settings.rs would warn instead).
pub fn parse_ini(text: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = [v.find(" ;"), v.find("\t;")].into_iter().flatten().min().map_or(v, |i| &v[..i]);
            m.insert(k.trim().to_lowercase(), v.trim().to_owned());
        }
    }
    m
}

/// One ini value as the row's number. `None` = empty / unreadable (the caller then uses the default).
fn parse_row(row: &RowDef, v: &str) -> Option<i64> {
    if v.is_empty() {
        return None;
    }
    match row.kind {
        RowKind::OnOff => match v.to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(1),
            "0" | "false" | "no" | "off" => Some(0),
            _ => None,
        },
        RowKind::Choice(_) if !row.ini_names.is_empty() => row.ini_names.iter().position(|o| o.eq_ignore_ascii_case(v)).map(|i| i as i64),
        _ => v.parse::<i64>().ok(),
    }
}

/// The row's value from an ini map, then the built-in defaults, then 0; clamped to the row's range.
pub fn row_from_ini(row: &RowDef, ini: &HashMap<String, String>, defaults: &HashMap<String, String>) -> i64 {
    let get = |m: &HashMap<String, String>| m.get(row.key).and_then(|v| parse_row(row, v));
    model::clamp_for(row, get(ini).or_else(|| get(defaults)).unwrap_or(0))
}

fn defaults() -> &'static HashMap<String, String> {
    static D: OnceLock<HashMap<String, String>> = OnceLock::new();
    D.get_or_init(|| parse_ini(&settings::default_ini_text()))
}

const INI_CACHE_MS: u64 = 1_000;
static INI_CACHE: Mutex<Option<(u64, Arc<HashMap<String, String>>)>> = Mutex::new(None);

fn ini_map(now: u64) -> Arc<HashMap<String, String>> {
    let mut g = INI_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((t, m)) = g.as_ref()
        && now.saturating_sub(*t) < INI_CACHE_MS
    {
        return m.clone();
    }
    let m = Arc::new(settings::ini_text().map(|t| parse_ini(&t)).unwrap_or_default());
    *g = Some((now, m.clone()));
    m
}

fn invalidate_ini() {
    *INI_CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A value just set stays shown until the ini reload / game task has caught up.
pub const PENDING_MS: u64 = 5_000;

#[derive(Default)]
pub struct Pending {
    map: HashMap<&'static str, (i64, u64)>,
}

impl Pending {
    pub fn set(&mut self, key: &'static str, v: i64, now: u64) {
        self.map.insert(key, (v, now));
    }

    pub fn get(&mut self, key: &str, now: u64) -> Option<i64> {
        match self.map.get(key) {
            Some(&(v, t)) if now.saturating_sub(t) < PENDING_MS => Some(v),
            Some(_) => {
                self.map.remove(key);
                None
            }
            None => None,
        }
    }
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

fn pending_set(key: &'static str, v: i64, now: u64) {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Pending::default).set(key, v, now);
}

fn pending_get(key: &str, now: u64) -> Option<i64> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Pending::default).get(key, now)
}

/// The five rows that travel in the host's ScaleCfg and are edited through `menu::post_edit`.
pub const VALUES_KEYS: [&str; 5] = ["enemy_health_scaling", "enemy_posture_scaling", "boss_health_scaling", "boss_posture_scaling", "death_debuffs"];

pub fn is_values_key(key: &str) -> bool {
    VALUES_KEYS.contains(&key)
}

pub fn values_field(v: &Values, key: &str) -> Option<i64> {
    Some(match key {
        "enemy_health_scaling" => v.enemy_hp as i64,
        "enemy_posture_scaling" => v.enemy_posture as i64,
        "boss_health_scaling" => v.boss_hp as i64,
        "boss_posture_scaling" => v.boss_posture as i64,
        "death_debuffs" => v.death_debuffs as i64,
        _ => return None,
    })
}

/// `v` with the field of `key` replaced (`None` for any other key).
pub fn with_field(v: Values, key: &str, value: i64) -> Option<Values> {
    let pct = menu::clamp_pct(value);
    let mut v = v;
    match key {
        "enemy_health_scaling" => v.enemy_hp = pct,
        "enemy_posture_scaling" => v.enemy_posture = pct,
        "boss_health_scaling" => v.boss_hp = pct,
        "boss_posture_scaling" => v.boss_posture = pct,
        "death_debuffs" => v.death_debuffs = value != 0,
        _ => return None,
    }
    Some(v)
}

/// This machine's live scaling values (the tuning atomics the game task keeps current); a Client
/// sees the host's values once they arrived.
fn live_values(role: Role) -> Values {
    let p = |a: &std::sync::atomic::AtomicU32| menu::clamp_pct(a.load(Ordering::Relaxed) as i64);
    let local = Values {
        enemy_hp: p(&tuning::ENEMY_HEALTH_SCALING),
        enemy_posture: p(&tuning::ENEMY_POSTURE_SCALING),
        boss_hp: p(&tuning::BOSS_HEALTH_SCALING),
        boss_posture: p(&tuning::BOSS_POSTURE_SCALING),
        death_debuffs: tuning::DEATH_DEBUFFS.load(Ordering::Relaxed),
    };
    if role == Role::Client { menu::snapshot().2.unwrap_or(local) } else { local }
}

/// The values with edits that are still pending applied.
fn values_with_pending(role: Role, now: u64) -> Values {
    let mut v = live_values(role);
    for k in VALUES_KEYS {
        if let Some(p) = pending_get(k, now)
            && let Some(nv) = with_field(v, k, p)
        {
            v = nv;
        }
    }
    v
}

/// Pure value resolution: a pending edit, else the live scaling value, else the ini.
pub fn resolve_value(
    row: &RowDef,
    pending: Option<i64>,
    live: &Values,
    ini: &HashMap<String, String>,
    defaults: &HashMap<String, String>,
) -> i64 {
    if let Some(p) = pending {
        return model::clamp_for(row, p);
    }
    if let Some(v) = values_field(live, row.key) {
        return model::clamp_for(row, v);
    }
    row_from_ini(row, ini, defaults)
}

/// The current value of a row on this machine.
pub fn current_value(row: &RowDef) -> i64 {
    let (role, _) = env();
    let now = game::now_ms();
    let live = live_values(role);
    resolve_value(row, pending_get(row.key, now), &live, &ini_map(now), defaults())
}

// ---------- handlers ----------

/// What a chosen list index does.
#[derive(Debug, PartialEq, Eq)]
pub enum SetOutcome {
    /// Change the row to this value.
    Apply(i64),
    /// Same value as now: nothing to do.
    Unchanged,
    /// A client tried a host row.
    ClientRefused,
    /// Locked for another reason (a connected host on a start-of-session rule).
    Locked,
    /// Not a valid index for the list shown.
    BadIndex,
}

pub fn decide_set(row: &RowDef, index: u8, current: i64, role: Role, connected: bool) -> SetOutcome {
    match model::value_at(row, index, current, role, connected) {
        Some(v) if v == model::clamp_for(row, current) => SetOutcome::Unchanged,
        Some(v) => SetOutcome::Apply(v),
        None if !model::can_change(row, role, connected) => {
            if role == Role::Client && row.authority != Authority::Local {
                SetOutcome::ClientRefused
            } else {
                SetOutcome::Locked
            }
        }
        None => SetOutcome::BadIndex,
    }
}

/// `(value index, text id)` pairs of a row's list.
pub fn list_pairs(row: &RowDef, current: i64, role: Role, connected: bool, id_of: &dyn Fn(&str) -> u32) -> Vec<(u8, u32)> {
    model::items(row, current, role, connected).iter().map(|it| (it.value_index, id_of(&it.label))).collect()
}

pub fn format_set(key: &str, v: i64, role: Role) -> String {
    format!("optmenu: set {key} = {v} (role {})", role_name(role))
}

fn row_by_id(row_id: u64) -> Option<&'static RowDef> {
    model::rows().get(row_id as usize)
}

fn h_get(row_id: u64) -> u8 {
    if row_id == SELECTOR_ID {
        return PAGE.load(Ordering::Relaxed).min(page_count() as u8 - 1);
    }
    let Some(row) = row_by_id(row_id) else {
        fail(&format!("getter for unknown row {row_id}"));
        return 0;
    };
    let (role, connected) = env();
    model::index_of(row, current_value(row), role, connected)
}

fn h_set(row_id: u64, index: u8) {
    if row_id == SELECTOR_ID {
        let new = (index as usize).min(page_count() - 1) as u8;
        if new != PAGE.load(Ordering::Relaxed) && PAGING.load(Ordering::Relaxed) {
            PAGE.store(new, Ordering::Relaxed);
            REBUILD_AT.store(game::now_ms() + 100, Ordering::Relaxed);
            log::line(&format!("optmenu: page {} of {}", new + 1, page_count()));
        }
        return;
    }
    let Some(row) = row_by_id(row_id) else {
        fail(&format!("setter for unknown row {row_id}"));
        return;
    };
    let (role, connected) = env();
    match decide_set(row, index, current_value(row), role, connected) {
        SetOutcome::Apply(v) => apply(row, v, role),
        SetOutcome::Unchanged => {}
        SetOutcome::ClientRefused => log::line(&format!("optmenu: client tried host row {}: refused", row.key)),
        SetOutcome::Locked => log::line(&format!("optmenu: row {} is locked (role {}): refused", row.key, role_name(role))),
        SetOutcome::BadIndex => log::line(&format!("optmenu: row {} got index {index} outside its list: ignored", row.key)),
    }
}

/// # Safety
/// `ret` is the list buffer the game passed to the `std::function`.
unsafe fn h_list(row_id: u64, ret: *mut u8) -> *mut u8 {
    unsafe {
        if ret.is_null() {
            return ret;
        }
        let Some(n) = NATIVES.get() else {
            // Natives are resolved before the page can exist; without them there is nothing safe to build.
            *(ret.add(LIST_COUNT_OFFSET) as *mut u64) = 0;
            fail("list requested but natives missing");
            return ret;
        };
        if row_id == SELECTOR_ID {
            let total = page_count();
            let pairs: Vec<(u8, u32)> = (0..total).map(|i| (i as u8, text_id(&format!("Page {}/{}", i + 1, total)))).collect();
            build_items(&n.list, ret, &pairs);
            return ret;
        }
        let Some(row) = row_by_id(row_id) else {
            // The game's display lambda dereferences item 0: a list must never be empty (Opus review).
            fail(&format!("list for unknown row {row_id}"));
            build_items(&n.list, ret, &[(0u8, text_id("Off"))]);
            return ret;
        };
        let (role, connected) = env();
        let mut pairs = list_pairs(row, current_value(row), role, connected, &text_id);
        if pairs.is_empty() {
            pairs.push((0u8, text_id("Off")));
        }
        build_items(&n.list, ret, &pairs);
        ret
    }
}

/// Applies a changed value: the scaling rows through the existing edit task (like the ImGui panel),
/// everything else straight into the ini (the 2 s reload applies it).
fn apply(row: &'static RowDef, v: i64, role: Role) {
    let now = game::now_ms();
    pending_set(row.key, v, now);
    // post_edit is only taken by puppet.rs while the ImGui panel is on (ingame_menu = 1); with it
    // off the ini route below is the only one that works.
    if is_values_key(row.key)
        && tuning::ingame_menu()
        && let Some(nv) = with_field(values_with_pending(role, now), row.key, v)
    {
        menu::post_edit(nv);
    } else if let Err(e) = settings::write_keys(&[model::ini_update(row, v)]) {
        log::line(&format!("optmenu: could not save {}: {e}", row.key));
    } else {
        invalidate_ini();
    }
    log::line(&format_set(row.key, v, role));
}

// ---------- paging ----------

/// Detour of `0x140CC4B50(unit)`: remembers the Options unit so the page can be rebuilt after a page switch.
extern "C" fn open_detour(unit: usize) -> usize {
    UNIT.store(unit, Ordering::Relaxed);
    let t = OPEN_TRAMP.load(Ordering::Acquire);
    if t == 0 {
        return 0;
    }
    // SAFETY: the trampoline runs the relocated prologue and continues in the original routine.
    let orig: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(t) };
    orig(unit)
}


/// The page of the Co-op Settings page that is open right now as `(page, pages)` (1-based), None when the page is
/// not on screen. The Options unit keeps the page it shows at `unit + 608 * 8`; that must be the page we built last.
pub fn page_hint() -> Option<(usize, usize)> {
    if !PAGING.load(Ordering::Relaxed) {
        return None;
    }
    let (unit, last) = (UNIT.load(Ordering::Relaxed), LAST_PAGE.load(Ordering::Relaxed));
    if unit == 0 || last == 0 || game::read::<usize>(unit + 608 * 8) != Some(last) {
        return None;
    }
    Some((PAGE.load(Ordering::Relaxed) as usize + 1, page_count()))
}

/// Steps the page by `delta` (wraps) and queues the rebuild.
fn change_page(delta: i32) {
    let total = page_count() as i32;
    let new = (PAGE.load(Ordering::Relaxed) as i32 + delta).rem_euclid(total) as u8;
    PAGE.store(new, Ordering::Relaxed);
    REBUILD_AT.store(game::now_ms() + 100, Ordering::Relaxed);
    log::line(&format!("optmenu: page {} of {} (trigger)", new + 1, total));
}

/// L2 = previous page, R2 = next page (rising edge of the analogue trigger past half way).
fn pad_pages() {
    static L2: AtomicBool = AtomicBool::new(false);
    static R2: AtomicBool = AtomicBool::new(false);
    let (lt, rt) = match crate::spectate_input::pad_state() {
        Some(p) => (p.lt >= 128, p.rt >= 128),
        None => (false, false),
    };
    let (l_was, r_was) = (L2.swap(lt, Ordering::Relaxed), R2.swap(rt, Ordering::Relaxed));
    if page_hint().is_none() || REBUILD_AT.load(Ordering::Relaxed) != 0 {
        return;
    }
    if lt && !l_was {
        change_page(-1);
    } else if rt && !r_was {
        change_page(1);
    }
}
/// Runs the deferred page rebuild (the page switch happens inside a setter of the page being replaced, so the
/// rebuild waits ~100 ms and runs from the frame task).
pub fn on_frame() {
    pad_pages();
    let at = REBUILD_AT.load(Ordering::Relaxed);
    if at == 0 || game::now_ms() < at {
        return;
    }
    REBUILD_AT.store(0, Ordering::Relaxed);
    let (unit, t) = (UNIT.load(Ordering::Relaxed), OPEN_TRAMP.load(Ordering::Acquire));
    if unit == 0 || t == 0 || !active() {
        return;
    }
    log::line("optmenu: rebuilding the page for the new page number");
    let r = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `t` is the trampoline of the game's page (re)build routine, `unit` the unit it was last called with.
        let f: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(t) };
        f(unit)
    }));
    if r.is_err() {
        fail("panic in the page rebuild");
    }
}

// ---------- install ----------

fn read_found(base: usize, rva: usize, len: usize) -> Vec<u8> {
    (0..len).filter_map(|i| game::read::<u8>(base + rva + i)).collect()
}

/// Installs the page builder detour (always, the flag is read live). Any prologue mismatch installs nothing.
pub fn install() {
    let base = game::module_base();
    let read = |rva: usize, len: usize| read_found(base, rva, len);
    for c in CHECKS.iter().chain(std::iter::once(&FREE_INFO)) {
        log::line(&format!("optmenu: prologue {} = {}", c.name, crate::optprobe::hex(&read(c.rva, c.expect.len()))));
    }
    if let Some(bad) = first_mismatch(&CHECKS, &read) {
        log::line(&format!("optmenu: OFF ({} prologue unverified)", bad.name));
        return;
    }
    // Our labels need the text-lookup detour (quitblock.rs): without it every label would show the native
    // text or "FDP_MenuText(id)" (Opus review).
    if !crate::quitblock::msg_hooked() {
        log::line("optmenu: OFF (text lookup hook missing)");
        return;
    }
    let heap = game::read::<usize>(base + HEAP_GLOBAL_RVA).unwrap_or(0);
    if heap == 0 {
        log::line("optmenu: OFF (game heap pointer 0x143D9CE28 is null)");
        return;
    }
    let _ = NATIVES.set(resolve(base, heap));
    msfunc::set_handlers(h_get, h_set, h_list);
    let det: extern "C" fn(usize, usize) -> usize = builder_detour;
    match hook::install_detour(BUILDER_RVA, &BUILDER_PROLOGUE, det as usize, "Network Settings page builder (0xCCDED0)") {
        Some(t) => {
            TRAMP.store(t, Ordering::Release);
            log::line(&format!("optmenu: {0}/{0} prologues ok, page builder detoured", CHECKS.len()));
            let open: extern "C" fn(usize) -> usize = open_detour;
            match hook::install_detour(OPEN_RVA, &OPEN_PROLOGUE, open as usize, "options page (re)build (0xCC4B50)") {
                Some(ot) => {
                    OPEN_TRAMP.store(ot, Ordering::Release);
                    PAGING.store(true, Ordering::Relaxed);
                    log::line("optmenu: paging on (page selector row + rebuild)");
                }
                None => log::line("optmenu: paging off (rebuild routine not hooked), first 11 rows only"),
            }
        }
        None => log::line("optmenu: OFF (detour not installed)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn row(key: &str) -> &'static RowDef {
        model::rows().iter().find(|r| r.key == key).unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // ----- prologue table -----

    #[test]
    fn checks_are_well_formed() {
        assert_eq!(CHECKS[0].rva, BUILDER_RVA);
        assert_eq!(CHECKS[0].expect, &BUILDER_PROLOGUE[..]);
        let mut rvas: Vec<usize> = CHECKS.iter().map(|c| c.rva).collect();
        rvas.sort();
        rvas.dedup();
        assert_eq!(rvas.len(), CHECKS.len());
        for c in CHECKS.iter() {
            assert!((7..=16).contains(&c.expect.len()), "{}", c.name);
        }
        // the row helper bytes the design gives (12) are a prefix of ours
        assert_eq!(&CHECKS[1].expect[..12], &[0x48, 0x8B, 0xC4, 0x55, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57]);
        // the heap load decodes to the global: rip (after the 7-byte mov) + disp32
        let disp = i32::from_le_bytes([0xC1, 0x00, 0x12, 0x03]) as i64;
        assert_eq!(0x140C7CD60i64 + 7 + disp, 0x143D9CE28);
        assert_eq!(0x140000000 + HEAP_GLOBAL_RVA as i64, 0x143D9CE28);
    }

    #[test]
    fn first_mismatch_finds_the_bad_function() {
        let good = |rva: usize, len: usize| CHECKS.iter().find(|c| c.rva == rva).map(|c| c.expect[..len].to_vec()).unwrap_or_default();
        assert!(first_mismatch(&CHECKS, &good).is_none());
        let bad_ctor = |rva: usize, len: usize| {
            let mut v = good(rva, len);
            if rva == 0xCB8980 {
                v[3] ^= 1;
            }
            v
        };
        assert_eq!(first_mismatch(&CHECKS, &bad_ctor).unwrap().rva, 0xCB8980);
        let unreadable = |_rva: usize, _len: usize| Vec::new();
        assert_eq!(first_mismatch(&CHECKS, &unreadable).unwrap().rva, BUILDER_RVA);
    }

    // ----- ini parsing and values -----

    #[test]
    fn ini_parse_rules() {
        let m = parse_ini("\u{feff}[GAMEPLAY]\nShare_Loot = 1 ; comment\n; vsync = 1\n# x = 2\nparty_hp_position=TOP\nbad line\n");
        assert_eq!(m.get("share_loot").map(String::as_str), Some("1"));
        assert_eq!(m.get("party_hp_position").map(String::as_str), Some("TOP"));
        assert!(!m.contains_key("vsync") && !m.contains_key("x"));
    }

    #[test]
    fn defaults_cover_every_row_and_are_in_range() {
        let d = parse_ini(&settings::default_ini_text());
        let empty = HashMap::new();
        for r in model::rows() {
            assert!(d.get(r.key).is_some_and(|v| parse_row(r, v).is_some()), "default of {} unparsable", r.key);
            let v = row_from_ini(r, &empty, &d);
            assert_eq!(v, model::clamp_for(r, v), "{}", r.key);
        }
        assert_eq!(row_from_ini(row("enemy_health_scaling"), &empty, &d), 60);
        assert_eq!(row_from_ini(row("boss_health_scaling"), &empty, &d), 50);
        assert_eq!(row_from_ini(row("vsync"), &empty, &d), 0);
        assert_eq!(row_from_ini(row("fps_unlock"), &empty, &d), 1);
        assert_eq!(row_from_ini(row("party_hp_position"), &empty, &d), 0);
        assert_eq!(row_from_ini(row("f11_pad_button"), &empty, &d), 0);
        assert_eq!(row_from_ini(row("party_hp_scale"), &empty, &d), 100);
    }

    #[test]
    fn row_values_from_the_ini() {
        let d = parse_ini(&settings::default_ini_text());
        let ini = map(&[
            ("party_hp_position", "Top"),
            ("f11_pad_button", "r3"),
            ("merge_quests", "2"),
            ("vsync", "yes"),
            ("party_hp_scale", "500"),
            ("share_loot", "maybe"),
            ("spectate_orbit", ""),
        ]);
        assert_eq!(row_from_ini(row("party_hp_position"), &ini, &d), 2);
        assert_eq!(row_from_ini(row("f11_pad_button"), &ini, &d), 3);
        assert_eq!(row_from_ini(row("merge_quests"), &ini, &d), 2);
        assert_eq!(row_from_ini(row("vsync"), &ini, &d), 1);
        assert_eq!(row_from_ini(row("party_hp_scale"), &ini, &d), 200); // clamped
        assert_eq!(row_from_ini(row("share_loot"), &ini, &d), 1); // unparsable -> default
        assert_eq!(row_from_ini(row("spectate_orbit"), &ini, &d), 1); // empty -> default
        // the written word round-trips through the model's ini_update
        let (k, v) = model::ini_update(row("party_hp_position"), 2);
        assert_eq!(row_from_ini(row("party_hp_position"), &map(&[(k, &v)]), &d), 2);
        let (k, v) = model::ini_update(row("f11_pad_button"), 4);
        assert_eq!(row_from_ini(row("f11_pad_button"), &map(&[(k, &v)]), &d), 4);
    }

    #[test]
    fn resolve_order_pending_live_ini() {
        let d = parse_ini(&settings::default_ini_text());
        let live = Values { enemy_hp: 80, enemy_posture: 20, boss_hp: 30, boss_posture: 40, death_debuffs: false };
        let ini = map(&[("enemy_health_scaling", "10"), ("share_loot", "0")]);
        // live wins over the ini for the five scaling rows
        assert_eq!(resolve_value(row("enemy_health_scaling"), None, &live, &ini, &d), 80);
        assert_eq!(resolve_value(row("death_debuffs"), None, &live, &ini, &d), 0);
        // pending wins over everything (clamped)
        assert_eq!(resolve_value(row("enemy_health_scaling"), Some(150), &live, &ini, &d), 150);
        assert_eq!(resolve_value(row("death_debuffs"), Some(7), &live, &ini, &d), 1);
        // the others come from the ini
        assert_eq!(resolve_value(row("share_loot"), None, &live, &ini, &d), 0);
        assert_eq!(resolve_value(row("share_exp"), None, &live, &ini, &d), 1);
    }

    #[test]
    fn pending_map_expires() {
        let mut p = Pending::default();
        assert_eq!(p.get("a", 100), None);
        p.set("a", 5, 100);
        assert_eq!(p.get("a", 100), Some(5));
        assert_eq!(p.get("a", 100 + PENDING_MS - 1), Some(5));
        assert_eq!(p.get("a", 100 + PENDING_MS), None);
        assert_eq!(p.get("a", 100), None, "expired entry is gone");
        p.set("a", 6, 200);
        p.set("a", 7, 300);
        assert_eq!(p.get("a", 301), Some(7));
    }

    #[test]
    fn values_edit_helpers() {
        let v = Values { enemy_hp: 1, enemy_posture: 2, boss_hp: 3, boss_posture: 4, death_debuffs: true };
        assert_eq!(values_field(&v, "boss_health_scaling"), Some(3));
        assert_eq!(values_field(&v, "death_debuffs"), Some(1));
        assert_eq!(values_field(&v, "share_loot"), None);
        let w = with_field(v, "enemy_posture_scaling", 90).unwrap();
        assert_eq!((w.enemy_hp, w.enemy_posture, w.boss_hp, w.boss_posture), (1, 90, 3, 4));
        assert!(!with_field(v, "death_debuffs", 0).unwrap().death_debuffs);
        assert_eq!(with_field(v, "boss_health_scaling", 5000).unwrap().boss_hp, 1000);
        assert_eq!(with_field(v, "vsync", 1), None);
        for k in VALUES_KEYS {
            assert!(model::rows().iter().any(|r| r.key == k && r.authority == Authority::Host), "{k}");
        }
        assert_eq!(model::rows().iter().filter(|r| r.authority == Authority::Host).count(), VALUES_KEYS.len());
    }

    // ----- set decisions and lists over the model -----

    #[test]
    fn set_decisions_by_role() {
        let pct = row("enemy_health_scaling");
        let lock = row("share_loot");
        let local = row("vsync");
        // Host rows: Solo / Host change, a Client is refused
        assert_eq!(decide_set(pct, 9, 60, Role::Host, true), SetOutcome::Apply(90));
        assert_eq!(decide_set(pct, 6, 60, Role::Host, true), SetOutcome::Unchanged);
        assert_eq!(decide_set(pct, 9, 60, Role::Solo, false), SetOutcome::Apply(90));
        assert_eq!(decide_set(pct, 9, 60, Role::Client, true), SetOutcome::ClientRefused);
        assert_eq!(decide_set(pct, 40, 60, Role::Host, true), SetOutcome::BadIndex);
        // start-of-session rules: editable only when not connected
        assert_eq!(decide_set(lock, 0, 1, Role::Solo, false), SetOutcome::Apply(0));
        assert_eq!(decide_set(lock, 0, 1, Role::Host, true), SetOutcome::Locked);
        assert_eq!(decide_set(lock, 0, 1, Role::Client, true), SetOutcome::ClientRefused);
        // local rows always
        for role in [Role::Solo, Role::Host, Role::Client] {
            assert_eq!(decide_set(local, 1, 0, role, role != Role::Solo), SetOutcome::Apply(1));
        }
        // off-grid slot keeps the value
        assert_eq!(decide_set(pct, 31, 65, Role::Host, true), SetOutcome::Unchanged);
    }

    #[test]
    fn list_pairs_use_the_games_on_off_text() {
        let ids = |s: &str| match s {
            "On" => 1001,
            "Off" => 1000,
            _ => 7,
        };
        let on_off = list_pairs(row("share_loot"), 1, Role::Solo, false, &ids);
        assert_eq!(on_off, vec![(0, 1000), (1, 1001)]);
        // a locked row is one item with the current value's text
        assert_eq!(list_pairs(row("share_loot"), 1, Role::Client, true, &ids), vec![(0, 1001)]);
        let pct = list_pairs(row("boss_health_scaling"), 65, Role::Host, true, &ids);
        assert_eq!(pct.len(), 32);
        assert_eq!(pct[31], (31, 7));
        assert_eq!(text_id("On"), text_id("On"));
        assert_ne!(text_id("On"), text_id("Off"));
        assert!(text_id("On") >= nativetext::OUR_ID_BASE);
        let a = text_id("60 %");
        assert!((nativetext::OUR_ID_BASE..nativetext::OUR_ID_BASE + nativetext::OUR_ID_COUNT).contains(&a));
        assert_eq!(a, text_id("60 %"));
    }

    #[test]
    fn set_log_line() {
        assert_eq!(format_set("vsync", 1, Role::Host), "optmenu: set vsync = 1 (role Host)");
        assert_eq!(role_name(Role::Client), "Client");
    }

    // ----- struct layouts -----

    #[test]
    fn layout_struct_matches_the_games() {
        assert_eq!(std::mem::size_of::<Layout>(), 16);
        let l = Layout { kind: 7, flag: 0, pad: [0; 3], name: LAYOUT_NAME_W.as_ptr() };
        let p = &l as *const Layout as *const u8;
        unsafe {
            assert_eq!(*(p as *const u32), 7);
            assert_eq!(*p.add(4), 0);
            assert_eq!(*(p.add(8) as *const usize), LAYOUT_NAME_W.as_ptr() as usize);
        }
        let name: Vec<u16> = LAYOUT_NAME.encode_utf16().chain(std::iter::once(0)).collect();
        assert_eq!(&LAYOUT_NAME_W[..], &name[..]);
        assert_eq!(std::mem::size_of::<Fn64>(), 64);
        assert_eq!(std::mem::size_of::<Label>(), LABEL_SIZE);
        assert_eq!(std::mem::size_of::<Text64>(), TEXT_SIZE);
        assert_eq!(std::mem::size_of::<Item120>(), ITEM_SIZE);
        // 32 items of 120 bytes + alignment slack end at the count field
        assert!(MAX_ITEMS * ITEM_SIZE + 7 <= LIST_COUNT_OFFSET);
    }

    // ----- free_text -----

    thread_local! {
        static FREED: RefCell<Vec<(usize, usize)>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn fake_free(alloc: usize, buf: usize) {
        FREED.with(|f| f.borrow_mut().push((alloc, buf)));
    }

    #[repr(C)]
    struct FakeAlloc {
        vtbl: *const [usize; 16],
    }

    #[test]
    fn free_text_frees_only_heap_strings() {
        let mut slots = [0usize; 16];
        slots[13] = fake_free as unsafe extern "C" fn(usize, usize) as usize;
        let alloc = FakeAlloc { vtbl: &slots };
        let a = &alloc as *const FakeAlloc as usize;
        FREED.with(|f| f.borrow_mut().clear());
        // heap string (cap 20)
        let mut t = Text64([0; TEXT_SIZE]);
        let p = t.0.as_mut_ptr();
        unsafe {
            *(p.add(8) as *mut usize) = a;
            *(p.add(16) as *mut usize) = 0xBEEF0;
            *(p.add(32) as *mut u64) = 5;
            *(p.add(40) as *mut u64) = 20;
            free_text(p);
            assert_eq!(*(p.add(40) as *const u64), 7);
            assert_eq!(*(p.add(32) as *const u64), 0);
            assert_eq!(*(p.add(16) as *const u16), 0);
        }
        assert_eq!(FREED.with(|f| f.borrow().clone()), vec![(a, 0xBEEF0)]);
        // inline string (cap 7): no call, state reset
        FREED.with(|f| f.borrow_mut().clear());
        unsafe {
            *(p.add(8) as *mut usize) = a;
            *(p.add(16) as *mut u16) = 0x41;
            *(p.add(32) as *mut u64) = 1;
            *(p.add(40) as *mut u64) = 7;
            free_text(p);
            assert_eq!(*(p.add(16) as *const u16), 0);
        }
        assert!(FREED.with(|f| f.borrow().is_empty()));
    }

    // ----- build_items with fake natives -----

    thread_local! {
        static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        static APPENDED: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
        static TEXT_CAP: RefCell<u64> = const { RefCell::new(7) };
    }

    fn note(s: String) {
        LOG.with(|l| l.borrow_mut().push(s));
    }

    unsafe extern "C" fn f_text(t: *mut u8, id: u32) -> *mut u8 {
        unsafe {
            note(format!("text {id}"));
            *(t as *mut u64) = 0x1000 + id as u64;
            *(t.add(8) as *mut usize) = 0x77;
            *(t.add(40) as *mut u64) = TEXT_CAP.with(|c| *c.borrow());
        }
        t
    }
    unsafe extern "C" fn f_copy(d: *mut u8, s: *const u8) -> *mut u8 {
        unsafe {
            note("copy".into());
            // the source is the text object + 8
            std::ptr::copy_nonoverlapping(s, d, 40);
            *d.add(40) = 1;
        }
        d
    }
    unsafe extern "C" fn f_new(d: *mut u8, w: *const u16, heap: usize) -> *mut u8 {
        unsafe {
            assert_eq!(*w, 0, "empty string");
            note(format!("new heap={heap:#x}"));
            *(d as *mut usize) = heap;
        }
        d
    }
    unsafe extern "C" fn f_append(list: *mut u8, item: *const u8) -> usize {
        unsafe {
            let c = list.add(LIST_COUNT_OFFSET) as *mut u64;
            *c += 1;
            note(format!("append count={}", *c));
            APPENDED.with(|a| a.borrow_mut().push(std::slice::from_raw_parts(item, ITEM_SIZE).to_vec()));
        }
        0
    }
    unsafe extern "C" fn f_item_free(_i: *mut u8) {
        note("item_free".into());
    }

    fn fake_list() -> ListNatives {
        ListNatives { text: f_text, str_copy: f_copy, str_new: f_new, append: f_append, item_free: f_item_free, heap: 0xABC0 }
    }

    #[test]
    fn build_items_follows_the_native_sequence() {
        LOG.with(|l| l.borrow_mut().clear());
        APPENDED.with(|a| a.borrow_mut().clear());
        TEXT_CAP.with(|c| *c.borrow_mut() = 7);
        #[repr(C, align(16))]
        struct ListBuf([u8; 4096]);
        let mut buf = ListBuf([0xCC; 4096]);
        let p = buf.0.as_mut_ptr();
        unsafe { build_items(&fake_list(), p, &[(0, 40401), (1, 40400), (5, 0x3B9A_0007)]) };
        let log = LOG.with(|l| l.borrow().clone());
        let one = ["text", "copy", "new heap=0xabc0", "append count=", "item_free"];
        assert_eq!(log.len(), 15, "{log:?}");
        assert_eq!(log[0], "text 40401");
        assert_eq!(log[5], "text 40400");
        assert_eq!(log[10], format!("text {}", 0x3B9A_0007u32));
        for (i, l) in log.iter().enumerate() {
            let want = one[i % 5];
            assert!(l.starts_with(want), "{i}: {l}");
        }
        assert_eq!(unsafe { *(p.add(LIST_COUNT_OFFSET) as *const u64) }, 3);
        let items = APPENDED.with(|a| a.borrow().clone());
        assert_eq!(items.len(), 3);
        assert_eq!(items[1][0], 1);
        assert_eq!(items[2][0], 5);
        assert_eq!(u64::from_le_bytes(items[0][8..16].try_into().unwrap()), 0x1000 + 40401);
        assert_eq!(u64::from_le_bytes(items[1][8..16].try_into().unwrap()), 0x1000 + 40400);
        // +16 is the copy (marker byte at +16+40), +64 is zero, +72 holds the heap
        assert_eq!(items[0][16 + 40], 1);
        assert_eq!(u64::from_le_bytes(items[0][64..72].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(items[0][72..80].try_into().unwrap()), 0xABC0);
    }

    #[test]
    fn build_items_caps_at_32_and_frees_heap_text() {
        LOG.with(|l| l.borrow_mut().clear());
        APPENDED.with(|a| a.borrow_mut().clear());
        FREED.with(|f| f.borrow_mut().clear());
        TEXT_CAP.with(|c| *c.borrow_mut() = 7);
        #[repr(C, align(16))]
        struct ListBuf([u8; 4096]);
        let mut buf = ListBuf([0; 4096]);
        let items: Vec<(u8, u32)> = (0..40).map(|i| (i as u8, 100 + i)).collect();
        unsafe { build_items(&fake_list(), buf.0.as_mut_ptr(), &items) };
        assert_eq!(unsafe { *(buf.0.as_ptr().add(LIST_COUNT_OFFSET) as *const u64) }, 32);
        assert_eq!(APPENDED.with(|a| a.borrow().len()), 32);
        // an empty list still zeroes the count
        unsafe { build_items(&fake_list(), buf.0.as_mut_ptr(), &[]) };
        assert_eq!(unsafe { *(buf.0.as_ptr().add(LIST_COUNT_OFFSET) as *const u64) }, 0);
    }

    // ----- build_page with fake natives -----

    thread_local! {
        static PAGE_LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        static ROWS_SEEN: RefCell<Vec<(u64, u64, u64)>> = const { RefCell::new(Vec::new()) };
    }

    static PAGE_MEM: Mutex<usize> = Mutex::new(0);

    fn pnote(s: &str) {
        PAGE_LOG.with(|l| l.borrow_mut().push(s.to_string()));
    }

    unsafe extern "C" fn p_alloc(size: usize, align: usize, heap: usize) -> usize {
        pnote(&format!("alloc {size} {align} {heap:#x}"));
        let mem = Box::leak(vec![0u64; size / 8 + 1].into_boxed_slice()).as_mut_ptr() as usize;
        *PAGE_MEM.lock().unwrap() = mem;
        mem
    }
    unsafe extern "C" fn p_ctor(page: usize, a1: usize, a2: usize) -> usize {
        pnote(&format!("ctor {} {a1} {a2}", page == *PAGE_MEM.lock().unwrap()));
        page
    }
    unsafe extern "C" fn p_label_get(buf: *mut u8) -> *mut u8 {
        // the override is armed while the getter runs
        pnote("label_get");
        assert_eq!(buf as usize % 16, 0);
        buf
    }
    unsafe extern "C" fn p_label_free(_b: *mut u8) {
        pnote("label_free");
    }
    unsafe extern "C" fn p_row(
        page: usize,
        label: *const u8,
        ty: *const i32,
        layout: *const Layout,
        get: *const u8,
        set: *const u8,
        list: *const u8,
        extra: *const u8,
    ) -> usize {
        unsafe {
            assert_eq!(page, *PAGE_MEM.lock().unwrap());
            assert!(!label.is_null());
            assert_eq!(*ty, 11);
            assert_eq!((*layout).kind, 7);
            assert_eq!((*layout).flag, 0);
            assert_eq!(*(*layout).name, '0' as u16);
            // std::function objects: impl pointer at +56 = own address, our vtable, row id inline
            for (p, vt) in [
                (get, &msfunc::VTBL_GETTER as *const msfunc::FnVtbl),
                (set, &msfunc::VTBL_SETTER as *const _),
                (list, &msfunc::VTBL_LIST as *const _),
            ] {
                assert_eq!(*(p.add(56) as *const usize), p as usize);
                let imp = &*(p as *const msfunc::FnImpl);
                assert_eq!(imp.vtbl, vt);
            }
            let id = |p: *const u8| (*(p as *const msfunc::FnImpl)).row_id;
            ROWS_SEEN.with(|r| r.borrow_mut().push((id(get), id(set), id(list))));
            // the extra function is empty (impl pointer null)
            assert_eq!(*(extra.add(56) as *const usize), 0);
            assert!(std::slice::from_raw_parts(extra, 64).iter().all(|&b| b == 0));
        }
        pnote("row");
        0
    }
    unsafe extern "C" fn p_finalise(page: usize, a: u32) -> usize {
        pnote(&format!("finalise {a}"));
        page
    }

    fn page_natives() -> Natives {
        Natives {
            alloc: p_alloc,
            ctor: p_ctor,
            label_get: p_label_get,
            label_free: p_label_free,
            row: p_row,
            finalise: p_finalise,
            list: fake_list(),
        }
    }

    #[test]
    fn build_page_runs_the_addendum_sequence() {
        PAGE_LOG.with(|l| l.borrow_mut().clear());
        ROWS_SEEN.with(|r| r.borrow_mut().clear());
        let n = page_natives();
        let page = unsafe { build_page(&n, 0x1234, 5, 0x9999) }.unwrap();
        assert_eq!(page, *PAGE_MEM.lock().unwrap());
        let log = PAGE_LOG.with(|l| l.borrow().clone());
        let rows = model::page_rows().len();
        assert_eq!(log[0], "alloc 4696 8 0x1234");
        assert_eq!(log[1], "ctor true 5 39321");
        assert_eq!(log.len(), 2 + 3 * rows + 1);
        for r in 0..rows {
            assert_eq!(&log[2 + 3 * r..5 + 3 * r], ["label_get", "row", "label_free"], "row {r}");
        }
        assert_eq!(log[log.len() - 1], "finalise 0");
        let seen = ROWS_SEEN.with(|r| r.borrow().clone());
        assert_eq!(seen.len(), rows);
        for (i, s) in seen.iter().enumerate() {
            assert_eq!(*s, (i as u64, i as u64, i as u64));
        }
    }

    #[test]
    fn build_page_validates_before_allocating() {
        PAGE_LOG.with(|l| l.borrow_mut().clear());
        let n = page_natives();
        assert!(unsafe { build_page(&n, 0, 1, 2) }.is_err());
        assert!(PAGE_LOG.with(|l| l.borrow().is_empty()), "nothing allocated");
    }
}

/// Title screen or a new save loaded: the Options unit and the page the old world built are gone, so forget them
/// (a pending rebuild or a stale unit pointer must not be used by the next Options screen).
pub fn on_title() {
    UNIT.store(0, Ordering::Relaxed);
    LAST_PAGE.store(0, Ordering::Relaxed);
    REBUILD_AT.store(0, Ordering::Relaxed);
}

static IN_WORLD: AtomicBool = AtomicBool::new(false);

/// items.rs: a player exists (we are inside a world), not at the title screen.
pub fn set_in_world(v: bool) {
    IN_WORLD.store(v, Ordering::Relaxed);
}
