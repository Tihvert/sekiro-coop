//! No New Game+ while in a co-op session (`[GAMEPLAY] block_ng_in_coop = 1`, default 1,
//! IMPLEMENTATION-NOTES §38, 2026-10-24, NOT verified live). NG+ resets every event flag and
//! reloads the world: for a connected pair that is a desync and a lost world.
//!
//! How the game gets to NG+ [VAR, IDA, image base 0x140000000]:
//! - InGameStep `0x140A71190`: event flags 20-23 on -> RequestToEnding (GameMan+0xB6C) and the
//!   ending kind into GameDataMan+0x74, then clears them; flag 30 on -> RequestToNextLap
//!   (GameMan+0xB6E), clears 30. world.rs `should_sync` never shares ids 20..=30 any more, so
//!   the companion does not get the ending or the NG+ request.
//! - The post-credits prompt (EndingStep, texts 450000 / 450001): the YES lambda `_Do_call`
//!   `0x1411DADA0` is exactly `C6 05 F9 63 CC 02 01 C3` (mov byte [rip+...], 1; ret) and writes
//!   `byte_143EA11A0` (RVA `0x3EA11A0`); NO (`0x1411DAE10`) writes 0. State 7 `0x140A8B650` reads
//!   the byte: set -> state 8 (NG+); clear -> `0x140A8D030`, the vanilla "No" path (clears
//!   NextLap, loads map `0x0B000000` entry 1100970).
//! - The NG+ commit `0x140A8D130` (only caller: state 8 `0x140A8BFD0`): resets all event flags,
//!   GameDataMan+0x70 += 1, loads map `0x0B020000` entry 1120999.
//!
//! Hooks (hook::install_detour):
//! (a) the YES lambda. The function is 8 bytes and ends the 8 bytes: `install_detour` writes a
//!     5-byte `jmp` and 3 int3 over exactly those 8 bytes (the next function is untouched), the
//!     "trampoline" holds RIP-relative code and is never called: the detour does the original's
//!     job itself (`module_base + 0x3EA11A0 = 1`). Blocked: it writes nothing, the byte stays 0 and
//!     the game takes its own "No" path.
//! (b) the commit, the safety net: blocked -> calls `0x140A8D030(a1)` instead; else the original.
//! (c) the "No" path `0x140A8D030`, only to clear the latch.
//!
//! LATCH: set when the ending prompt is answered / the commit runs while connected, so a block
//! still holds if the link drops between the two; cleared when the "No" path or the commit runs.
//! Blocked NG+ is not lost: after leaving co-op the player uses the Dilapidated Temple idol.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::net::{self, Link};
use crate::{dialog, game, hook, log};

/// `[GAMEPLAY] block_ng_in_coop`.
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const YES_RVA: usize = 0x11DADA0;
/// `mov byte [rip+0x2CC63F9], 1 ; ret`.
const YES_PROLOGUE: [u8; 8] = [0xC6, 0x05, 0xF9, 0x63, 0xCC, 0x02, 0x01, 0xC3];
/// The byte State 7 reads (`byte_143EA11A0`).
const ANSWER_BYTE_RVA: usize = 0x3EA11A0;
const COMMIT_RVA: usize = 0xA8D130;
/// push-prologue of the commit (15 bytes, no RIP-relative code).
const COMMIT_PROLOGUE: [u8; 15] = [0x48, 0x8B, 0xC4, 0x57, 0x41, 0x56, 0x41, 0x57, 0x48, 0x81, 0xEC, 0x90, 0x00, 0x00, 0x00];
const NO_RVA: usize = 0xA8D030;
/// push rbx / sub rsp, 30h.
const NO_PROLOGUE: [u8; 6] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x30];
pub const NOTICE: &str = "New Game+ is off in co-op. Leave co-op, then use the Dilapidated Temple idol.";

static COMMIT_TRAMP: AtomicUsize = AtomicUsize::new(0);
static NO_TRAMP: AtomicUsize = AtomicUsize::new(0);
static NO_HOOKED: AtomicBool = AtomicBool::new(false);
static LATCH: AtomicBool = AtomicBool::new(false);
static BLOCKED_YES: AtomicU32 = AtomicU32::new(0);
static BLOCKED_COMMIT: AtomicU32 = AtomicU32::new(0);
static PASSED: AtomicU32 = AtomicU32::new(0);
static NO_RAN: AtomicU32 = AtomicU32::new(0);

fn in_session(link: Link) -> bool {
    matches!(link, Link::HostConnected | Link::ClientConnected)
}

/// The decision, pure for the tests: block while the option is on, the mod runs and a
/// companion is connected, or the ending started while one was (the latch).
pub fn should_block(enabled: bool, mod_alive: bool, link: Link, latched: bool) -> bool {
    enabled && mod_alive && (in_session(link) || latched)
}

fn block_now() -> bool {
    should_block(ENABLED.load(Ordering::Relaxed), crate::mod_alive(), net::link(), LATCH.load(Ordering::Acquire))
}

fn tramp(slot: &AtomicUsize) -> usize {
    loop {
        match slot.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    }
}

/// The YES lambda `0x1411DADA0(this)`: does the original's job itself.
extern "C" fn yes_detour(_this: usize) {
    if in_session(net::link()) {
        LATCH.store(true, Ordering::Release);
    }
    if block_now() {
        BLOCKED_YES.fetch_add(1, Ordering::Relaxed);
        return;
    }
    PASSED.fetch_add(1, Ordering::Relaxed);
    game::write::<u8>(game::module_base() + ANSWER_BYTE_RVA, 1);
}

/// The NG+ commit `0x140A8D130(a1)`; blocked: the vanilla "No" path `0x140A8D030(a1)`.
extern "C" fn commit_detour(a1: usize) -> usize {
    if in_session(net::link()) {
        LATCH.store(true, Ordering::Release);
    }
    if block_now() {
        BLOCKED_COMMIT.fetch_add(1, Ordering::Relaxed);
        LATCH.store(false, Ordering::Release);
        // Through the "No" hook when installed (it then trampolines to the original).
        let no: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(game::module_base() + NO_RVA) };
        return no(a1);
    }
    PASSED.fetch_add(1, Ordering::Relaxed);
    LATCH.store(false, Ordering::Release);
    let f: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(tramp(&COMMIT_TRAMP)) };
    f(a1)
}

/// The "No" path `0x140A8D030(a1)`: clears the latch, then the original.
extern "C" fn no_detour(a1: usize) -> usize {
    LATCH.store(false, Ordering::Release);
    NO_RAN.fetch_add(1, Ordering::Relaxed);
    let f: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(tramp(&NO_TRAMP)) };
    f(a1)
}

/// Installs the detours (at the title screen, from `init`).
pub fn install() {
    let yes: extern "C" fn(usize) = yes_detour;
    let commit: extern "C" fn(usize) -> usize = commit_detour;
    let no: extern "C" fn(usize) -> usize = no_detour;
    let mut ok = 0;
    if hook::install_detour(YES_RVA, &YES_PROLOGUE, yes as usize, "NG+ prompt YES lambda (0x1411DADA0)").is_some() {
        ok += 1;
    } else {
        log::line("ngblock: YES lambda not hooked; relying on the commit hook");
    }
    // The "No" hook first: the commit detour calls it by address.
    if let Some(t) = hook::install_detour(NO_RVA, &NO_PROLOGUE, no as usize, "NG+ declined path (0xA8D030)") {
        NO_TRAMP.store(t, Ordering::Release);
        NO_HOOKED.store(true, Ordering::Release);
    }
    if let Some(t) = hook::install_detour(COMMIT_RVA, &COMMIT_PROLOGUE, commit as usize, "NG+ commit (0xA8D130)") {
        COMMIT_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    log::line(&format!(
        "ngblock: {ok}/2 hooks installed{}; declined-path hook {}",
        if ok == 2 { "" } else { "; NG+ is NOT fully blocked in co-op" },
        if NO_HOOKED.load(Ordering::Relaxed) { "on" } else { "off (latch only cleared by the commit)" }
    ));
}

static LATCH_LOGGED: AtomicBool = AtomicBool::new(false);

/// Per frame (lib.rs): the notice and the log lines for what the detours saw.
pub fn tick() {
    let link = net::link();
    let latched = LATCH.load(Ordering::Relaxed);
    if latched && !LATCH_LOGGED.swap(true, Ordering::Relaxed) {
        log::line(&format!("ngblock: ending seen while connected, latch set (link {link:?})"));
    } else if !latched {
        LATCH_LOGGED.store(false, Ordering::Relaxed);
    }
    let y = BLOCKED_YES.swap(0, Ordering::Relaxed);
    let c = BLOCKED_COMMIT.swap(0, Ordering::Relaxed);
    if y + c > 0 {
        log::line(&format!(
            "ngblock: NG+ refused (link {link:?}; {}): notice shown",
            if c > 0 { "commit 0xA8D130 redirected to the No path" } else { "prompt answer YES dropped" }
        ));
        dialog::game_notice(NOTICE.to_string(), dialog::Importance::Important, 8);
    }
    if PASSED.swap(0, Ordering::Relaxed) > 0 {
        log::line(&format!("ngblock: NG+ step allowed (link {link:?}, not in a co-op session or the option is off)"));
    }
    if NO_RAN.swap(0, Ordering::Relaxed) > 0 {
        log::line("ngblock: the declined path (0xA8D030) ran, latch cleared");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_while_connected_or_latched() {
        for l in [Link::HostConnected, Link::ClientConnected] {
            assert!(should_block(true, true, l, false));
            assert!(should_block(true, true, l, true));
        }
        // Link dropped (hosting alone, searching, off): only the latch holds the block.
        for l in [Link::Off, Link::Hosting, Link::Searching] {
            assert!(!should_block(true, true, l, false));
            assert!(should_block(true, true, l, true));
        }
        // Option off, mod disabled.
        assert!(!should_block(false, true, Link::ClientConnected, true));
        assert!(!should_block(true, false, Link::HostConnected, true));
    }

    #[test]
    fn prologue_constants() {
        assert_eq!(YES_PROLOGUE.len(), 8);
        assert_eq!(YES_PROLOGUE[0..2], [0xC6, 0x05]);
        assert_eq!(*YES_PROLOGUE.last().unwrap(), 0xC3);
        assert_eq!(YES_PROLOGUE[6], 1, "writes 1");
        // mov byte [rip+0x02CC63F9], 1 at 0x1411DADA0 (+7 = next ip) -> 0x143EA11A0.
        let rel = u32::from_le_bytes([YES_PROLOGUE[2], YES_PROLOGUE[3], YES_PROLOGUE[4], YES_PROLOGUE[5]]) as usize;
        assert_eq!(YES_RVA + 7 + rel, ANSWER_BYTE_RVA);
        assert_eq!(COMMIT_PROLOGUE.len(), 15);
        assert_eq!(NO_PROLOGUE.len(), 6);
        assert_eq!(&COMMIT_PROLOGUE[..3], &[0x48, 0x8B, 0xC4]);
    }
}
