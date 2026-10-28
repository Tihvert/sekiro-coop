//! Text injection for the native Co-op Settings page.
//!
//! `quitblock::msg_detour` (the detour of the text lookup `0x1410AA310(repo, bank, id, default) -> const wchar_t*`)
//! asks [`lookup`] first with the ORIGINAL function's result `orig` (a `const wchar_t*` as usize, 0 when the id is
//! unknown). `lookup` returns `Some(ptr)` with a NUL-terminated UTF-16 pointer of the same kind (never freed), or `None`.
#![allow(dead_code)]

use crate::tuning;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

/// Reserved text ids: `OUR_ID_BASE..OUR_ID_BASE + OUR_ID_COUNT` (any bank).
pub const OUR_ID_BASE: u32 = 0x3B9A_0000;
pub const OUR_ID_COUNT: u32 = 0x1_0000;

const RELABEL_LABEL: &str = "Co-op Settings";
const RELABEL_HELP: &str = "Co-op mod settings. The host sets the gameplay rules.";

const ID_NET_LABEL: u32 = 110700;
const ID_NET_HELP: u32 = 110605;
const ID_ROW_LABEL: u32 = 110710;
const ID_ROW_HELP: u32 = 110701;

struct Cache {
    ptrs: Vec<usize>, // id - OUR_ID_BASE -> pointer
    by_text: HashMap<String, u32>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Labels/help of the override slot; each field is spent after one lookup.
struct Slot {
    label: Option<usize>,
    help: Option<usize>,
}
static SLOT: Mutex<Slot> = Mutex::new(Slot { label: None, help: None });

fn leak_utf16(text: &str) -> usize {
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    Box::leak(w.into_boxed_slice()).as_ptr() as usize
}

/// Caches `text` (deduplicated) and returns its id in our range. Returns the last id when the range is full.
pub fn intern(text: &str) -> u32 {
    let mut g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(|| Cache { ptrs: Vec::new(), by_text: HashMap::new() });
    if let Some(&id) = c.by_text.get(text) {
        return id;
    }
    if c.ptrs.len() as u32 >= OUR_ID_COUNT {
        return OUR_ID_BASE + OUR_ID_COUNT - 1;
    }
    let id = OUR_ID_BASE + c.ptrs.len() as u32;
    c.ptrs.push(leak_utf16(text));
    c.by_text.insert(text.to_string(), id);
    id
}

fn our_ptr(id: u32) -> Option<usize> {
    if !(OUR_ID_BASE..OUR_ID_BASE + OUR_ID_COUNT).contains(&id) {
        return None;
    }
    let g = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref()?.ptrs.get((id - OUR_ID_BASE) as usize).copied()
}

fn is_label_bank(bank: u32) -> bool {
    matches!(bank, 200 | 232 | 272)
}
fn is_help_bank(bank: u32) -> bool {
    matches!(bank, 201 | 233 | 273)
}

/// Pure relabel selection: the replacement text for (bank, id) while the native page is on.
pub fn relabel_text(bank: u32, id: u32) -> Option<&'static str> {
    if id == ID_NET_LABEL && is_label_bank(bank) {
        Some(RELABEL_LABEL)
    } else if id == ID_NET_HELP && is_help_bank(bank) {
        Some(RELABEL_HELP)
    } else {
        None
    }
}

/// Which override field a lookup targets: `Some(true)` = label, `Some(false)` = help.
fn override_kind(bank: u32, id: u32) -> Option<bool> {
    if id == ID_ROW_LABEL && is_label_bank(bank) {
        Some(true)
    } else if id == ID_ROW_HELP && is_help_bank(bank) {
        Some(false)
    } else {
        None
    }
}

/// Arms the one-shot override: the next lookup of 110710 (label banks) returns `label`, the next of
/// 110701 (help banks) returns `help`. Each is spent after one use.
pub fn arm_override(label: &str, help: &str) {
    let (l, h) = (leak_utf16_cached(label), leak_utf16_cached(help));
    let mut s = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    s.label = Some(l);
    s.help = Some(h);
}

pub fn disarm_override() {
    let mut s = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    s.label = None;
    s.help = None;
}

fn leak_utf16_cached(text: &str) -> usize {
    our_ptr(intern(text)).unwrap_or(0)
}

fn take_override(label: bool) -> Option<usize> {
    let mut s = SLOT.lock().unwrap_or_else(|e| e.into_inner());
    let f = if label { &mut s.label } else { &mut s.help };
    f.take().filter(|&p| p != 0)
}

/// Called by `quitblock::msg_detour` with the original result `orig`. `Some(p)` = return `p` instead.
pub fn lookup(bank: u32, id: u32, _orig: usize) -> Option<usize> {
    if let Some(p) = our_ptr(id) {
        return Some(p);
    }
    if !tuning::NATIVE_MOD_MENU.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(kind) = override_kind(bank, id)
        && let Some(p) = take_override(kind)
    {
        return Some(p);
    }
    relabel_text(bank, id).map(leak_utf16_cached).filter(|&p| p != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests share SLOT and the NATIVE_MOD_MENU flag: serialise them.
    static LOCK: Mutex<()> = Mutex::new(());

    unsafe fn read(p: usize) -> String {
        let mut v = Vec::new();
        let mut q = p as *const u16;
        unsafe {
            while *q != 0 {
                v.push(*q);
                q = q.add(1);
            }
        }
        String::from_utf16(&v).unwrap()
    }

    #[test]
    fn intern_dedups_and_terminates() {
        let a = intern("Hello \u{00e4}");
        let b = intern("Hello \u{00e4}");
        let c = intern("Other");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!((OUR_ID_BASE..OUR_ID_BASE + OUR_ID_COUNT).contains(&a));
        let p = our_ptr(a).unwrap();
        assert_eq!(unsafe { read(p) }, "Hello \u{00e4}");
        // NUL right after the last unit
        let n = "Hello \u{00e4}".encode_utf16().count();
        assert_eq!(unsafe { *(p as *const u16).add(n) }, 0);
    }

    #[test]
    fn our_ids_resolve_in_any_bank_even_when_off() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        tuning::NATIVE_MOD_MENU.store(false, Ordering::Relaxed);
        let id = intern("Row text");
        let p = lookup(7, id, 0).unwrap();
        assert_eq!(unsafe { read(p) }, "Row text");
        assert_eq!(lookup(7, OUR_ID_BASE + OUR_ID_COUNT - 2, 0), None);
    }

    #[test]
    fn relabel_selection() {
        assert_eq!(relabel_text(200, 110700), Some("Co-op Settings"));
        assert_eq!(relabel_text(232, 110700), Some("Co-op Settings"));
        assert_eq!(relabel_text(272, 110700), Some("Co-op Settings"));
        assert_eq!(relabel_text(201, 110700), None);
        assert!(relabel_text(201, 110605).unwrap().starts_with("Co-op mod settings."));
        assert!(relabel_text(233, 110605).is_some());
        assert!(relabel_text(273, 110605).is_some());
        assert_eq!(relabel_text(200, 110605), None);
        assert_eq!(relabel_text(200, 110701), None);
    }

    #[test]
    fn relabel_only_when_flag_on() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        tuning::NATIVE_MOD_MENU.store(false, Ordering::Relaxed);
        assert_eq!(lookup(200, 110700, 123), None);
        tuning::NATIVE_MOD_MENU.store(true, Ordering::Relaxed);
        let p = lookup(200, 110700, 123).unwrap();
        assert_eq!(unsafe { read(p) }, "Co-op Settings");
        assert_eq!(lookup(200, 5, 123), None);
        tuning::NATIVE_MOD_MENU.store(false, Ordering::Relaxed);
    }

    #[test]
    fn override_is_one_shot_per_kind() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        tuning::NATIVE_MOD_MENU.store(true, Ordering::Relaxed);
        disarm_override();
        assert_eq!(lookup(200, 110710, 0), None);
        arm_override("Enemy HP +%", "Help text");
        // wrong bank does not spend it
        assert_eq!(lookup(201, 110710, 0), None);
        let l = lookup(232, 110710, 0).unwrap();
        assert_eq!(unsafe { read(l) }, "Enemy HP +%");
        assert_eq!(lookup(200, 110710, 0), None, "label spent");
        let h = lookup(233, 110701, 0).unwrap();
        assert_eq!(unsafe { read(h) }, "Help text");
        assert_eq!(lookup(201, 110701, 0), None, "help spent");
        arm_override("A", "B");
        disarm_override();
        assert_eq!(lookup(200, 110710, 0), None);
        assert_eq!(lookup(201, 110701, 0), None);
        tuning::NATIVE_MOD_MENU.store(false, Ordering::Relaxed);
    }

    #[test]
    fn override_ignored_when_flag_off() {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        tuning::NATIVE_MOD_MENU.store(false, Ordering::Relaxed);
        arm_override("X", "Y");
        assert_eq!(lookup(200, 110710, 0), None);
        disarm_override();
    }
}
