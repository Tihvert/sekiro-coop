//! No Quit Game / return to title from the pause menu while in a co-op session
//! (`[GAMEPLAY] block_quit_in_coop = 1`, default 0 since v7.4, IMPLEMENTATION-NOTES §27). The player uses the
//! Severed Thread first; Alt+F4 cannot be blocked (fine).
//!
//! How the game returns to the title [VAR, IDA copy `F:\Temp\ida_v8\db.i64`, o2-o23]:
//! - The InGameStep update `0x140A71190` reads `0x1408BF330` = MenuMan menu state 135
//!   (`MenuMan+0x20+4*135 != 0`, setter `0x1408C68B0(id, v)`). Set, it starts the fade
//!   (`0x140F29860`), sets the step's exit reason `+193 = 1`, waits for pending saves and
//!   finishes; its parent `0x140A81930` then builds `TitleStep(1)` (`0x140A89DA0`), the
//!   constructor items.rs counts. So menu state 135 IS the "return to title" request; the
//!   TitleStep is far too late to cancel.
//! - Writers of state 135 = 1: `0x140DD9AC0` (the `_Do_call` of the std::function lambda
//!   `12343e8a...`, vtable `0x142B0B018`: `state135 = 1; byte_143D8A044 = 1`) and
//!   `0x1408BF370` (EMEVD dispatcher `0x140904EF0`, a script's own return: untouched).
//! - The lambda is packed into a menu job by `0x140DD7C30`, called from `0x140DD6920` /
//!   `0x140DD6C40` (job chains with dialog texts 110500 / 110501, then a wait while
//!   `byte_143B11A90` (saving) is set, then `0x1408322E0` = the ReturnTitle save) and from
//!   `0x1408C2B30` (a MenuMan system-message flow, not the pause menu). `0x140DD6920` /
//!   `0x140DD6C40` are only called by `0x140CC37D0`, a menu job factory (via `0x140CAE890`
//!   <- `0x140CB3690` <- `0x140CB0900` <- `0x140CD3CF0`, the do_call of a menu item lambda,
//!   vtable `0x142AC7520`) [ARV: the pause menu's "Quit Game" item].
//!
//! Hooks (hook::install_detour): `0x140CC37D0` only ARMS (notes the time, runs the
//! original unchanged); `0x140DD9AC0`, while armed in the last 60 s and the link is
//! connected (host or client), does nothing instead of setting state 135, so the
//! InGameStep never starts the return. The rest of the menu chain still runs (the save
//! it waits for, the ReturnTitle save request: harmless). The game's own returns (EMEVD
//! `0x1408BF370`, the system-message flow) are never armed, so never blocked; nothing
//! is blocked when not connected. A notice tells the player what to do.
//!
//! Greyed-out item (IMPLEMENTATION-NOTES §36) [VAR, IDA copy `F:\Temp\ida_quit\db.i64`, o1-o8]:
//! - The item is built by `0x140CB5E30` (`AppMenu::CSCommonOptionUnit` ctor, "OptionTop"),
//!   `case 5` of its item loop: `0x140CC64A0` / `0x140CC6440` give label FDP_MenuText 110500
//!   (help 110501 / 110500), `0x140C5F7F0` builds an `AppMenu::CommandSelectDialog::Command`,
//!   the job lambda (vtable `0x142AC7520`, only made here) goes into the std::function at
//!   `Command+192` (impl pointer `+248`).
//! - `Command+392` is a `std::function<bool()>` (impl pointer `+448`), default lambda vtable
//!   `0x142AA6500` whose `_Do_call` `0x140C63670` is `xor al, al; ret` (only xref: that vtable
//!   slot). The quit item keeps the default (cases 6-8 set their own via `0x140CC71F0`).
//! - Render `0x140DC9820`: `0x140C53ED0(widget, pred() || !(flags & 2))` = "Grayout" / "Normal".
//!   Select `0x140DCA850` (CommandSelectDialog vtable `0x142AC5AA8` slot 17): pred() true ->
//!   sound (600, 2, 41) and nothing else; false -> `0x140DCB580` runs the command.
//! - Label text: `0x140C7B910` -> `0x140C7CE90` -> `0x1410AA310(MsgRepository*, bank, id,
//!   default) -> wchar_t*` for banks 200, 232, 272 in turn; the string is copied when the item
//!   is built (`0x140C7DAA0`), so the label follows the state at the time the menu is built.
//!
//! Hooks: `0x140C63670` (detour over the 2 code bytes, the ret and 2 int3 of padding) returns
//! true only for a Command whose job lambda is the quit one, while [`should_grey`]; the
//! `0x1410AA310` detour returns our label for (FDP_MenuText, 110500) while [`should_grey`].
//! The state-135 block above stays as the safety net.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::game::{self, read, read_ptr};
use crate::net::{self, Link};
use crate::{dialog, hook, log};

/// `[GAMEPLAY] block_quit_in_coop` (default off while untested, v7.4).
pub static ENABLED: AtomicBool = AtomicBool::new(false);

const QUIT_MENU_RVA: usize = 0xCC37D0;
/// push rbp / rsi / rdi / r12 / r13 / r14 / r15 (12 bytes, no RIP-relative code).
const QUIT_MENU_PROLOGUE: [u8; 12] = [0x40, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57];
const TITLE_LAMBDA_RVA: usize = 0xDD9AC0;
/// sub rsp, 28h / mov edx, 1 (9 bytes; then mov ecx, 87h / call 0x1408C68B0).
const TITLE_LAMBDA_PROLOGUE: [u8; 9] = [0x48, 0x83, 0xEC, 0x28, 0xBA, 0x01, 0x00, 0x00, 0x00];
/// The pause-menu flow arms the block for this long (its yes/no box and the save wait).
const ARM_FOR_MS: u64 = 60_000;
const MENU_MAN_RVA: usize = 0x3D67408;
/// MenuMan menu state 135 (`0x1408BF330`): the return-to-title request.
const STATE_135: usize = 0x20 + 4 * 135;
pub const NOTICE: &str = "Use Severed Thread to leave co-op before quitting.";

/// `_Do_call` of the default "disabled" predicate of a CommandSelectDialog::Command:
/// `xor al, al; ret` and int3 padding (the next function starts 16 bytes on).
const PRED_RVA: usize = 0xC63670;
const PRED_PROLOGUE: [u8; 5] = [0x32, 0xC0, 0xC3, 0xCC, 0xCC];
/// The predicate's std::function sits at `Command+392`, its impl pointer at `+448`; the
/// job std::function's impl pointer is at `Command+248`.
const CMD_PRED: usize = 392;
const CMD_PRED_IMPL: usize = 448;
const CMD_JOB_IMPL: usize = 248;
/// `_Func_impl<lambda_f5822cd0...>` vtable: the Quit Game item's job.
const QUIT_JOB_VTABLE_RVA: usize = 0x2AC7520;
/// The select handler `0x140DCA850` (its predicate call = a refused press).
const SELECT_RVA: std::ops::Range<usize> = 0xDCA850..0xDCA970;
/// `0x1410AA310(MsgRepository*, u32 bank, u32 id, wchar_t* default)`: push rbx /
/// sub rsp, 20h / mov rbx, r9 (9 bytes, then a call).
const MSG_RVA: usize = 0x10AA310;
const MSG_PROLOGUE: [u8; 9] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x20, 0x49, 0x8B, 0xD9];
/// FDP_MenuText banks (`0x140C7B910`) and the Quit Game label id.
const MENU_TEXT_BANKS: [u32; 3] = [200, 232, 272];
const QUIT_LABEL_ID: u32 = 110500;
/// Short: the menu row cuts longer text (live 2026-10-24); the press notice explains.
const LABEL_SUFFIX: &str = " (in co-op)";

static PRED_TRAMP: AtomicUsize = AtomicUsize::new(0);
static PRED_HOOKED: AtomicBool = AtomicBool::new(false);

/// True when the text-lookup detour (0x1410AA310) is installed (the Co-op Settings page needs it).
pub fn msg_hooked() -> bool {
    MSG_TRAMP.load(Ordering::Acquire) != 0
}
/// `game::now_ms()` of the last predicate call for the Quit Game command (0 = never).
static SYSTEM_LIST_MS: AtomicU64 = AtomicU64::new(0);

/// How long ago the System list (the one with Quit Game) was last seen being drawn;
/// `None` = never. The 'Mod settings' panel keys on this (puppet.rs, menu.rs).
pub fn system_list_age_ms() -> Option<u64> {
    match SYSTEM_LIST_MS.load(Ordering::Relaxed) {
        0 => None,
        t => Some(game::now_ms().saturating_sub(t)),
    }
}
static MSG_TRAMP: AtomicUsize = AtomicUsize::new(0);
static GREYED: AtomicU32 = AtomicU32::new(0);
static REFUSED: AtomicU32 = AtomicU32::new(0);
static LABELLED: AtomicU32 = AtomicU32::new(0);
/// (original text pointer, our leaked UTF-16 label) for the last original seen.
static LABEL: Mutex<Option<(usize, usize)>> = Mutex::new(None);

static MENU_TRAMP: AtomicUsize = AtomicUsize::new(0);
static LAMBDA_TRAMP: AtomicUsize = AtomicUsize::new(0);
static ARMED_AT_MS: AtomicU64 = AtomicU64::new(0);
static MENU_CALLS: AtomicU32 = AtomicU32::new(0);
static BLOCKED: AtomicU32 = AtomicU32::new(0);
static PASSED: AtomicU32 = AtomicU32::new(0);
static PASSED_UNARMED: AtomicU32 = AtomicU32::new(0);

fn tramp(slot: &AtomicUsize) -> usize {
    // In the install window (patch in, trampoline not stored yet) wait for it.
    loop {
        match slot.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    }
}

fn in_session(link: Link) -> bool {
    matches!(link, Link::HostConnected | Link::ClientConnected)
}

/// The decision, kept pure for the tests: block only a pause-menu request (armed) while
/// a companion is connected, the option is on and the mod runs.
pub fn should_block(enabled: bool, mod_alive: bool, link: Link, armed_at_ms: u64, now_ms: u64) -> bool {
    enabled && mod_alive && in_session(link) && armed_at_ms != 0 && now_ms.saturating_sub(armed_at_ms) <= ARM_FOR_MS
}

/// Grey the Quit Game item out (and relabel it): the option is on, the mod runs and a
/// companion is connected.
pub fn should_grey(enabled: bool, mod_alive: bool, link: Link) -> bool {
    enabled && mod_alive && in_session(link)
}

fn grey_now() -> bool {
    should_grey(ENABLED.load(Ordering::Relaxed), crate::mod_alive(), net::link())
}

/// Is the predicate impl at `this` the one inside a Command whose job is Quit Game?
/// The impl must be stored inline (`Command+448 == Command+392`), the job's impl pointer
/// non-null with the quit lambda's vtable. `rd` reads a pointer (fault-safe in the game).
fn is_quit_command(this: usize, base: usize, rd: impl Fn(usize) -> Option<usize>) -> bool {
    let Some(cmd) = this.checked_sub(CMD_PRED) else { return false };
    rd(cmd + CMD_PRED_IMPL) == Some(this)
        && rd(cmd + CMD_JOB_IMPL).filter(|&j| j != 0).and_then(&rd) == Some(base + QUIT_JOB_VTABLE_RVA)
}

fn is_quit_label(bank: u32, id: u32) -> bool {
    id == QUIT_LABEL_ID && MENU_TEXT_BANKS.contains(&bank)
}

/// The game's own label (localized) with our hint after it.
fn quit_label(original: Option<&str>) -> String {
    format!("{}{LABEL_SUFFIX}", original.filter(|s| !s.is_empty()).unwrap_or("Quit Game"))
}

/// `0x140C63670`: the default "disabled" predicate (originally always false).
extern "C" fn pred_detour(this: usize) -> bool {
    // v8.31a: the Quit Game command exists only in the System list (OptionTop), so a call
    // for it means that list is being drawn. Stamp the time for the 'Mod settings' panel
    // (menu.rs); the return value below is unchanged. Off with `ingame_menu = 0`.
    let menu_on = crate::tuning::ingame_menu();
    let grey = grey_now();
    if (!grey && !menu_on) || !is_quit_command(this, game::module_base(), read_ptr) {
        return false;
    }
    if menu_on {
        SYSTEM_LIST_MS.store(game::now_ms(), Ordering::Relaxed);
    }
    if !grey {
        return false;
    }
    GREYED.fetch_add(1, Ordering::Relaxed);
    let caller = hook::detour_caller(PRED_TRAMP.load(Ordering::Acquire)).wrapping_sub(game::module_base());
    if SELECT_RVA.contains(&caller) {
        REFUSED.fetch_add(1, Ordering::Relaxed);
    }
    true
}

fn wide_to_string(p: usize) -> Option<String> {
    if p == 0 {
        return None;
    }
    let mut v = Vec::new();
    for i in 0..256 {
        match read::<u16>(p + 2 * i)? {
            0 => break,
            c => v.push(c),
        }
    }
    Some(String::from_utf16_lossy(&v))
}

/// Our label for the original text pointer `orig` (built once per original, leaked).
fn label_for(orig: usize) -> usize {
    let Ok(mut g) = LABEL.lock() else { return orig };
    if let Some((o, ours)) = *g
        && o == orig
    {
        return ours;
    }
    let text = quit_label(wide_to_string(orig).as_deref());
    let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let ours = Box::leak(w.into_boxed_slice()).as_ptr() as usize;
    *g = Some((orig, ours));
    ours
}

/// `0x1410AA310(MsgRepository*, bank, id, default) -> wchar_t*`.
extern "C" fn msg_detour(repo: usize, bank: u32, id: u32, default: usize) -> usize {
    let f: extern "C" fn(usize, u32, u32, usize) -> usize = unsafe { std::mem::transmute(tramp(&MSG_TRAMP)) };
    let orig = f(repo, bank, id, default);
    crate::optprobe::note_text(bank, id, orig);
    if let Some(p) = crate::nativetext::lookup(bank, id, orig) {
        return p;
    }
    if is_quit_label(bank, id) && orig != 0 && PRED_HOOKED.load(Ordering::Relaxed) && grey_now() {
        LABELLED.fetch_add(1, Ordering::Relaxed);
        return label_for(orig);
    }
    orig
}

/// `0x140CC37D0(i64* a1, MenuJobRef* out, a3)` (decompiled: 3 arguments; r9 passed through too).
extern "C" fn quit_menu_detour(a1: usize, out: usize, a3: usize, a4: usize) -> usize {
    ARMED_AT_MS.store(game::now_ms(), Ordering::Release);
    MENU_CALLS.fetch_add(1, Ordering::Relaxed);
    let f: extern "C" fn(usize, usize, usize, usize) -> usize = unsafe { std::mem::transmute(tramp(&MENU_TRAMP)) };
    f(a1, out, a3, a4)
}

extern "C" fn title_lambda_detour(this: usize) {
    let armed = ARMED_AT_MS.swap(0, Ordering::AcqRel);
    let now = game::now_ms();
    if should_block(ENABLED.load(Ordering::Relaxed), crate::mod_alive(), net::link(), armed, now) {
        BLOCKED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    if armed != 0 && now.saturating_sub(armed) <= ARM_FOR_MS {
        PASSED.fetch_add(1, Ordering::Relaxed);
    } else {
        PASSED_UNARMED.fetch_add(1, Ordering::Relaxed);
    }
    let f: extern "C" fn(usize) = unsafe { std::mem::transmute(tramp(&LAMBDA_TRAMP)) };
    f(this);
}

/// Installs both detours (at the title screen, from `init`).
pub fn install() {
    let menu: extern "C" fn(usize, usize, usize, usize) -> usize = quit_menu_detour;
    let lambda: extern "C" fn(usize) = title_lambda_detour;
    let mut ok = 0;
    if let Some(t) = hook::install_detour(QUIT_MENU_RVA, &QUIT_MENU_PROLOGUE, menu as usize, "quit menu flow (0xCC37D0)") {
        MENU_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    if let Some(t) = hook::install_detour(TITLE_LAMBDA_RVA, &TITLE_LAMBDA_PROLOGUE, lambda as usize, "return-to-title request (0xDD9AC0)") {
        LAMBDA_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    log::line(&format!(
        "quit: {ok}/2 hooks installed{}",
        if ok == 2 { "" } else { "; Quit Game is NOT blocked in co-op" }
    ));
    let pred: extern "C" fn(usize) -> bool = pred_detour;
    let msg: extern "C" fn(usize, u32, u32, usize) -> usize = msg_detour;
    let mut grey = 0;
    if let Some(t) = hook::install_detour(PRED_RVA, &PRED_PROLOGUE, pred as usize, "menu command disabled predicate (0xC63670)") {
        PRED_TRAMP.store(t, Ordering::Release);
        PRED_HOOKED.store(true, Ordering::Release);
        grey += 1;
    }
    // The label only when the item really greys out (no "leave co-op first" on a live item).
    if grey == 1
        && let Some(t) = hook::install_detour(MSG_RVA, &MSG_PROLOGUE, msg as usize, "menu text lookup (0x10AA310)")
    {
        MSG_TRAMP.store(t, Ordering::Release);
        grey += 1;
    }
    log::line(&format!(
        "quit: {grey}/2 grey-out hooks installed{}",
        match grey {
            2 => "",
            1 => "; Quit Game greys out in co-op but keeps its label",
            _ => "; Quit Game stays selectable in co-op (the return is still cancelled)",
        }
    ));
}

static STATE_135_SEEN: AtomicBool = AtomicBool::new(false);
static LABEL_LOGGED: AtomicBool = AtomicBool::new(false);
static GREY_LOGGED: AtomicBool = AtomicBool::new(false);

/// Per frame (our frame task, lib.rs): the notice and the log lines for what the
/// detours saw.
pub fn tick() {
    {
        let link = net::link();
        if MENU_CALLS.swap(0, Ordering::Relaxed) > 0 {
            log::line(&format!(
                "quit: the pause menu's return-to-title flow started (0xCC37D0), link {link:?}, block_quit_in_coop {}",
                ENABLED.load(Ordering::Relaxed)
            ));
        }
        let blocked = BLOCKED.swap(0, Ordering::Relaxed);
        if blocked > 0 {
            log::line(&format!(
                "quit: Quit Game / return to title cancelled while in co-op (link {link:?}): menu state 135 not set; notice shown"
            ));
            dialog::game_notice(NOTICE.to_string(), dialog::Importance::Important, 8);
        }
        if LABELLED.swap(0, Ordering::Relaxed) > 0 && !LABEL_LOGGED.swap(true, Ordering::Relaxed) {
            log::line(&format!("quit: Quit Game label replaced while in co-op (link {link:?})"));
        }
        if GREYED.swap(0, Ordering::Relaxed) > 0 && !GREY_LOGGED.swap(true, Ordering::Relaxed) {
            log::line(&format!("quit: Quit Game shown greyed out while in co-op (link {link:?})"));
        }
        if !in_session(link) {
            LABEL_LOGGED.store(false, Ordering::Relaxed);
            GREY_LOGGED.store(false, Ordering::Relaxed);
        }
        if REFUSED.swap(0, Ordering::Relaxed) > 0 {
            log::line(&format!("quit: greyed-out Quit Game pressed while in co-op (link {link:?}): refused, notice shown"));
            dialog::game_notice(NOTICE.to_string(), dialog::Importance::Info, 6);
        }
        if PASSED.swap(0, Ordering::Relaxed) > 0 {
            log::line(&format!("quit: return to title from the pause menu allowed (link {link:?}, not in a co-op session or the option is off)"));
        }
        if PASSED_UNARMED.swap(0, Ordering::Relaxed) > 0 {
            log::line(&format!("quit: return-to-title request (0xDD9AC0) not from the pause menu's flow (game's own), allowed (link {link:?})"));
        }
        // Evidence: the request set by any path (the EMEVD one is 0x1408BF370).
        let set = read_ptr(game::module_base() + MENU_MAN_RVA).and_then(|m| read::<i32>(m + STATE_135)).is_some_and(|v| v != 0);
        if set && !STATE_135_SEEN.swap(true, Ordering::Relaxed) {
            log::line(&format!("quit: menu state 135 (return to title) is set now, link {link:?}"));
        } else if !set {
            STATE_135_SEEN.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_only_an_armed_pause_menu_quit_while_connected() {
        let now = 1_000_000;
        assert!(should_block(true, true, Link::ClientConnected, now - 5_000, now));
        assert!(should_block(true, true, Link::HostConnected, now, now));
        // Not in a session: hosting with nobody there, searching, off.
        for l in [Link::Off, Link::Hosting, Link::Searching] {
            assert!(!should_block(true, true, l, now, now));
        }
        // The game's own returns are never armed; an old arm expires.
        assert!(!should_block(true, true, Link::ClientConnected, 0, now));
        assert!(!should_block(true, true, Link::ClientConnected, now - ARM_FOR_MS - 1, now));
        // Option off, mod disabled.
        assert!(!should_block(false, true, Link::ClientConnected, now, now));
        assert!(!should_block(true, false, Link::ClientConnected, now, now));
    }

    #[test]
    fn greys_only_while_connected_with_the_option_on() {
        assert!(should_grey(true, true, Link::HostConnected));
        assert!(should_grey(true, true, Link::ClientConnected));
        for l in [Link::Off, Link::Hosting, Link::Searching] {
            assert!(!should_grey(true, true, l));
        }
        assert!(!should_grey(false, true, Link::ClientConnected));
        assert!(!should_grey(true, false, Link::ClientConnected));
    }

    #[test]
    fn recognises_only_the_quit_command() {
        use std::collections::HashMap;
        let base = 0x1_4000_0000usize;
        let cmd = 0x5000_0000usize;
        let pred = cmd + CMD_PRED;
        let job = cmd + 192;
        let mem = |m: HashMap<usize, usize>| move |a: usize| m.get(&a).copied();
        let quit = HashMap::from([(cmd + CMD_PRED_IMPL, pred), (cmd + CMD_JOB_IMPL, job), (job, base + QUIT_JOB_VTABLE_RVA)]);
        assert!(is_quit_command(pred, base, mem(quit.clone())));
        // Another command's job lambda.
        let mut other = quit.clone();
        other.insert(job, base + 0x2AC86C0);
        assert!(!is_quit_command(pred, base, mem(other)));
        // No job, unreadable, or a predicate not stored inline in a Command.
        let mut empty = quit.clone();
        empty.insert(cmd + CMD_JOB_IMPL, 0);
        assert!(!is_quit_command(pred, base, mem(empty)));
        assert!(!is_quit_command(pred, base, mem(HashMap::new())));
        let mut moved = quit;
        moved.insert(cmd + CMD_PRED_IMPL, 0x1234_0000);
        assert!(!is_quit_command(pred, base, mem(moved)));
        assert!(!is_quit_command(8, base, |_| None));
    }

    #[test]
    fn quit_label_text() {
        assert!(is_quit_label(200, 110500) && is_quit_label(232, 110500) && is_quit_label(272, 110500));
        assert!(!is_quit_label(200, 110501) && !is_quit_label(101, 110500));
        assert_eq!(quit_label(Some("Quit Game")), "Quit Game (in co-op)");
        assert_eq!(quit_label(Some("")), "Quit Game (in co-op)");
        assert_eq!(quit_label(None), "Quit Game (in co-op)");
    }
}
