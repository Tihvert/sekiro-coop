//! Animation sync.
//!
//! Playing (RE-FINDINGS-2 § Ghost animation playback, 2026-10-22): the
//! EMEVD route `chr->vtbl[247]` returns the EMEVD entity adapter at
//! ChrIns+0x2118, which only exists for characters registered with the event
//! system, so it was null for our ghost. The adapter just forwards to
//! ForceAnimationByChrEventModule (0xBD84C0) on the real event module
//! (modules table +0x58), which every character has; we call that directly.
//! The game then raises the behavior event `W_Event<id>` on the next update,
//! so only animations that have such an event play (event-style ones; plain
//! attacks/locomotion may not). `raised` tells which did.
//!
//! Reading: SprjChrTimeActModule ring (see `Probe::current`). It holds the
//! full TAE id `category * 1_000_000 + anim`; `W_Event%04d` wants the bare
//! anim number, so we play `id % 1_000_000` (inferred, logged for checking).

use crate::game::{Chr, module_base, read, read_ptr};
use crate::log;

const CHR_MODULES: usize = 0x1FF8;
const MOD_EVENT: usize = 0x58;
const FORCE_BY_EVENT_MODULE_RVA: usize = 0xBD84C0;
/// Event module +8 = its owner ChrIns.
const EVM_OWNER: usize = 0x8;
const RAISE_BY_NAME_RVA: usize = 0xFF1CD0;

/// Plays `anim` (full TAE id or bare number) on `chr` through its event module.
pub fn play(chr: Chr, anim: i32) -> Result<(), &'static str> {
    let evm = module(chr, MOD_EVENT).filter(|&p| p >= 0x10000).ok_or("event module null")?;
    if read_ptr(evm + EVM_OWNER) != Some(chr.0) {
        return Err("event module owner mismatch");
    }
    let force: extern "C" fn(usize, i32) = unsafe { std::mem::transmute(module_base() + FORCE_BY_EVENT_MODULE_RVA) };
    force(evm, anim % 1_000_000);
    Ok(())
}

/// Raises the behavior event `W_Event<anim>` straight on the character's
/// graph (RE-FINDINGS-2 Path A', 0xFF1CD0): the last step of `play` without
/// its gameplay gates (throws, busy). For the ghost, which has no gameplay.
/// Ok(false) = the graph has no such event.
pub fn play_wevent(chr: Chr, anim: i32) -> Result<bool, &'static str> {
    let bm = module(chr, 0x28).filter(|&p| p >= 0x10000).ok_or("behavior module null")?;
    let x = read_ptr(bm + 0x10).filter(|&p| p >= 0x10000).ok_or("hk character null")?;
    let graph = read_ptr(x + 0x28).filter(|&p| p >= 0x10000).ok_or("graph null")?;
    let pair = [graph, x + 0x38];
    let name: Vec<u16> = format!("W_Event{:04}", anim % 1_000_000).encode_utf16().chain([0]).collect();
    // MSVC bool: only al is defined; a Rust `bool` return must be exactly 0/1.
    let raise: extern "C" fn(*const [usize; 2], *const u16) -> u8 =
        unsafe { std::mem::transmute(module_base() + RAISE_BY_NAME_RVA) };
    Ok(raise(&pair, name.as_ptr()) != 0)
}

fn module(chr: Chr, slot: usize) -> Option<usize> {
    read_ptr(read_ptr(chr.0 + CHR_MODULES)? + slot)
}

/// SprjChrTimeActModule keeps a ring of the last 10 TimeActs (RE-FINDINGS-2
/// §5, from its debug printer 0xB5C9B0): entries at +0x20, stride 0x14, the
/// id first; write index (head) at +0xE8, read index (tail) at +0xEC. The
/// newest entry is `(head + 9) % 10`. The earlier probe picked +0x44/+0x6C…,
/// which are these entries' start frames (always growing), not ids.
const MODULE_TIME_ACT: usize = 0x10;
const RING: usize = 0x20;
const RING_STRIDE: usize = 0x14;
const RING_HEAD: usize = 0xE8;
const RING_TAIL: usize = 0xEC;

#[derive(Default)]
pub struct Probe {
    logged: u32,
    frame: u32,
}

impl Probe {
    /// Logs the player's current animation now and then, for verification.
    pub fn on_frame(&mut self, player: Chr) {
        self.frame += 1;
        if self.logged < 20 && self.frame % 120 == 0 {
            self.logged += 1;
            let ta = module(player, MODULE_TIME_ACT);
            let raw = ta.map(|t| (read::<i32>(t + RING_HEAD), read::<i32>(t + RING_TAIL), read::<i32>(t + RING)));
            // (head, tail, entry 0): public DS3/ER tables read entry 0 as "current".
            log::line(&format!("anim: player current animation {:?} raw {raw:?}", self.current(player)));
        }
    }

    /// The character's newest TimeAct id (player, ghost or enemy).
    pub fn current(&self, chr: Chr) -> Option<i32> {
        let ta = module(chr, MODULE_TIME_ACT)?;
        let head = read::<i32>(ta + RING_HEAD)?;
        let tail = read::<i32>(ta + RING_TAIL)?;
        if !(0..10).contains(&head) || head == tail {
            return None;
        }
        let idx = ((head + 9) % 10) as usize;
        read::<i32>(ta + RING + RING_STRIDE * idx).filter(|&id| id > 0)
    }
}

/// Logs which module slots a character has (RTTI names), e.g. to see what a
/// ghost lacks compared with the player (DS3 order: 0x58 = event module).
pub fn log_modules(label: &str, chr: Chr) {
    let Some(table) = read_ptr(chr.0 + CHR_MODULES) else {
        log::line(&format!("anim: {label} has no module table"));
        return;
    };
    let mut parts = Vec::new();
    for slot in (0..=0x90).step_by(8) {
        let name = read_ptr(table + slot)
            .filter(|&p| p >= 0x10000)
            .map(|p| crate::rtti::class_name(p).unwrap_or_else(|| "?".into()));
        if let Some(n) = name {
            let short = n.trim_start_matches(".?AV").split('@').next().unwrap_or("?").to_string();
            parts.push(format!("{slot:#x}={short}"));
        } else {
            parts.push(format!("{slot:#x}=null"));
        }
    }
    log::line(&format!("anim: {label} modules {}", parts.join(" ")));
}
