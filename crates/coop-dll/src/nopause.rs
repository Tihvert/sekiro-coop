//! No pausing while a companion is connected (`[GAMEPLAY] no_pause_in_coop = 1`,
//! IMPLEMENTATION-NOTES §28, NOT verified live). Solo play pauses as before.
//!
//! How Sekiro pauses [VAR, IDA copy `F:\Temp\ida_v73\db.i64`, jobs o1-o8]:
//! - `NS_SPRJ::CSPauseManagerImp` (instance `0x143E9DA30`, built in `0x140A6F060` by
//!   `0x1411731C0`) keeps a list of `CSPauseIns` objects: `_Instant` (vtable `0x142BBEC18`,
//!   pauses one frame), `_MenuHandle` (`0x142BBEC40`, made by `0x141173830` for every menu
//!   opened with "pause": the in-game menus `0x140D884F0`, `0x140E13740` from the EMEVD /
//!   Lua message boxes `0x140909870` and the system message flows, our GenDialog included),
//!   `_DebugBool` (`0x142BBEC68`).
//! - `0x141173910` (called once per frame by the InGameStep update `0x140A6D490` at
//!   `0x140A6DAF6`) sets `+8 = any ins->IsPause()` (vtable slot 1) and drops those whose slot
//!   2 says finished.
//! - The InGameStep then clears its "world runs this frame" byte `+0x318` when `+8` is set
//!   (`0x140A6DB35`); that byte gates the world / chr / map update (`0x140A76BB0`,
//!   `0x140A75790`, `0x140A75180`), which is why WorldChrMan's update counter `+0x678`
//!   stands still in a menu (nsession.rs `world_running`). `+8` is also read by MenuMan
//!   `0x1408CB250`, MapItemMan `0x14091A440` (pickups), `0x140D603B0`, `0x140E8D0E0` and
//!   `0x1411CC0B0` (sound): all of them follow it.
//! - `CSPauseIns_MenuHandle::IsPause` = `0x141172EE0`: false when `0x140D87CF0` (menu system
//!   `+216`) or `0x140E13390`; otherwise true while its menu handle is open (`0x140E136C0` /
//!   `0x140E16380`).
//!
//! The fix: a detour on `0x141172EE0` returns false while connected (host or client), the
//! option is on and the mod runs, so menus, the inventory, item pickup popups and our own
//! yes/no boxes leave the world running, as FromSoft's online play does. The Instant and
//! debug pauses are untouched; nothing changes solo.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::game::{self, read, read_ptr};
use crate::net::{self, Link};
use crate::{hook, log};

/// `[GAMEPLAY] no_pause_in_coop` (default on).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const MENU_PAUSE_RVA: usize = 0x1172EE0;
/// push rdi / sub rsp, 60h / mov qword [rsp+20h], -2 (15 bytes, nothing RIP-relative).
const MENU_PAUSE_PROLOGUE: [u8; 15] = [0x40, 0x57, 0x48, 0x83, 0xEC, 0x60, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE, 0xFF, 0xFF, 0xFF];
/// `CSPauseManagerImp*`; `+8` = the world is paused this frame.
const PAUSE_MAN_RVA: usize = 0x3E9DA30;
const PM_PAUSED: usize = 8;

static TRAMP: AtomicUsize = AtomicUsize::new(0);
static SUPPRESSED: AtomicU32 = AtomicU32::new(0);
static PASSED: AtomicU32 = AtomicU32::new(0);

/// Pure, for the tests: keep the world running for an open menu?
pub fn keep_running(enabled: bool, mod_alive: bool, link: Link) -> bool {
    enabled && mod_alive && matches!(link, Link::HostConnected | Link::ClientConnected)
}

extern "C" fn menu_pause_detour(this: usize) -> u8 {
    let f: extern "C" fn(usize) -> u8 = match TRAMP.load(Ordering::Acquire) {
        0 => return 0,
        t => unsafe { std::mem::transmute(t) },
    };
    let r = f(this);
    if r == 0 {
        return 0;
    }
    if keep_running(ENABLED.load(Ordering::Relaxed), crate::mod_alive(), net::link()) {
        // Safety: while there is no player, the player is dead or a load is pending, the
        // game's own pause stays (a suppressed pause there could stall the death / load flow).
        let unsafe_now = game::player().and_then(|p| p.hp()).is_none_or(|hp| hp <= 0) || game::load_pending();
        if PASS_THROUGH.swap(unsafe_now, Ordering::Relaxed) != unsafe_now && unsafe_now {
            log::line("nopause: pass-through (dead/loading)");
        }
        if unsafe_now {
            PASSED.fetch_add(1, Ordering::Relaxed);
            return r;
        }
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
        return 0;
    }
    PASSED.fetch_add(1, Ordering::Relaxed);
    r
}

/// The last call passed the pause through because of death / loading (log on change only).
static PASS_THROUGH: AtomicBool = AtomicBool::new(false);

/// Installs the detour (at the title screen, from `init`).
pub fn install() {
    let d: extern "C" fn(usize) -> u8 = menu_pause_detour;
    match hook::install_detour(MENU_PAUSE_RVA, &MENU_PAUSE_PROLOGUE, d as usize, "menu pause (CSPauseIns_MenuHandle::IsPause 0x1172EE0)") {
        Some(t) => {
            TRAMP.store(t, Ordering::Release);
            log::line("nopause: menu pause hooked: menus do not pause the world while a companion is connected");
        }
        None => log::line("nopause: menu pause NOT hooked: menus still pause the game in co-op"),
    }
}

#[derive(Default)]
struct Seen {
    menu_open: bool,
    other_pause: bool,
}

static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Seen { menu_open: false, other_pause: false });

/// Per frame (our frame task): log lines on edges (a menu kept running / paused, another
/// kind of pause while connected).
pub fn tick() {
    let kept = SUPPRESSED.swap(0, Ordering::Relaxed);
    let passed = PASSED.swap(0, Ordering::Relaxed);
    let link = net::link();
    let paused = read_ptr(game::module_base() + PAUSE_MAN_RVA).and_then(|pm| read::<u8>(pm + PM_PAUSED)).is_some_and(|v| v != 0);
    let Ok(mut s) = SEEN.lock() else { return };
    let open = kept > 0 || passed > 0;
    if open && !s.menu_open {
        if kept > 0 {
            log::line(&format!("nopause: a menu / message box is open, the world keeps running (link {link:?})"));
        } else {
            log::line(&format!("nopause: a menu pauses the world (link {link:?}, no_pause_in_coop {})", ENABLED.load(Ordering::Relaxed)));
        }
    } else if !open && s.menu_open {
        log::line("nopause: menu closed");
    }
    s.menu_open = open;
    let other = paused && kept > 0 && passed == 0;
    if other && !s.other_pause {
        log::line("nopause: the world is paused by something other than a menu (an instant / debug pause) while connected");
    }
    s.other_pause = other;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_running_only_while_connected() {
        assert!(keep_running(true, true, Link::HostConnected));
        assert!(keep_running(true, true, Link::ClientConnected));
        for l in [Link::Off, Link::Hosting, Link::Searching] {
            assert!(!keep_running(true, true, l));
        }
        assert!(!keep_running(false, true, Link::ClientConnected));
        assert!(!keep_running(true, false, Link::ClientConnected));
    }
}
