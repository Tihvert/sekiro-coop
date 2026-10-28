//! The game's own yes/no message box and notices (2026-10-23, NOT verified live;
//! IMPLEMENTATION-NOTES §26).
//!
//! Sekiro opens its generic dialog (`NS_SPRJ::GenDlgCtrl`, MenuMan `+0x1FB0`) from Lua
//! scripts with `ShowGenDialog(msg, buttons, type, bool)` (registered at `0x140092150`,
//! proxy `0x14068DF90`) and `CloseGenDialog()` (`0x1400921C0` -> `0x1408C4AC0`). The proxy
//! does exactly what we do here (IDA, copy `F:\Temp\ida_dlg\db.i64`, outputs o2-o16):
//! - `0x1408B6380 bool CanOpen()`: MenuMan menu states 17, 180, 182 all 0 and the global
//!   byte `0x143D67250` 0 (no generic dialog up). The open-menu mask `0x8C58E0` sets its
//!   bit 8 from it, so `game::menu_mask()` sees our dialog too.
//! - `0x1408C4B40 Open(u32 msg, i32 buttons, u32 type)`: buttons 0 = FDP_Dialogues ids
//!   3 / 4, 1 = ids 1 / 2 [I: Yes / No and OK / Cancel, as in DS3's ESD]; type 1 -> 7, 2 -> 8,
//!   6 -> 9. The dialog job (`0x1408B70C0` -> `0x140DEA010`) shows one button for 1/4/7,
//!   two for 2/3/5/8. It sets GenDlgCtrl `+0x10` = 1, so the text comes from
//!   `0x140C791F0` = "Event Text" = `0x1410A9E40 (MsgRepository*, u32 id)` (banks 101, 30,
//!   231, 271; "?ActionEventInfo?" when missing). We detour that lookup and return our own
//!   text for our own ids (`TEXT_ID_BASE`..). The same code path the game uses at
//!   `0x14075B9F5`: `Open(10030010, 0, 2)` = a two-button yes/no.
//! - Result: GenDlgCtrl `+0x18`: -1 while open (`0x1408B71C0` sets it), button index + 1
//!   when one is pressed (`0x140DE8100` -> `0x1408B6B90`), 0 when backed out
//!   (`0x140DEC990`, `0x140DF0620`). The menu state 17 is set to 1 / 11 by the dialog
//!   constructor (`0x140DE53B0`), so `CanOpen` is false while it is up. After the answer
//!   we call `Close` (`0x1408C4AC0`: state 17 back to 0), as the Lua scripts do.
//!
//! Default button (v8.27b, NOT verified live): `Open` ignores its 4th arg and sets menu
//! state 84 (`0x1408B6680` -> `0x1408B71F0` -> `0x1408C68B0(0x54, v)`) = 1 for yes/no; state 84
//! is the highlighted button (0 = Yes, 1 = No) [I]. Writing it before `Open` is overwritten,
//! so `open_default_yes` repeats Open's steps with state 84 = 0 (`[GAMEPLAY]
//! dialog_default_yes`, falls back to `Open`). Test: Kindred Bell, Yes highlighted; confirm at
//! once logs `answered Yes`.
//!
//! Only on the game thread (items.rs frame call), only when nothing else is open (no menu,
//! no talk, no other dialog, alive, 3 s in the world, no load pending). A question that
//! finds no quiet moment within 10 s, or a dialog the game does not show within 1.5 s,
//! answers `Unavailable`: the caller falls back to the old behaviour (and says so in the
//! log).
//!
//! Notices (owner 2026-10-23: never a box that needs OK) [VAR, IDA `F:\Temp\ida_v73`
//! o9-o19]: `Important` notices use the game's status message, the one EMEVD 2007[3]
//! (case 3 of `0x140909870` -> `0x14068CEC0`) shows: `0x140E13C30(u32 msg, bool)` runs a
//! NewMenuSystem lambda (vtable `0x142B1DE00`, slot 2 `0x140E186F0`) that calls
//! `0x140D87E40(menu, msg, flag)`, which copies the Event Text of `msg` (`0x140C791F0`, our
//! detour above) into the menu system (`+17016` / `+17024`, flag `+17072`). It needs no
//! input and opens no menu, so it never pauses. The text is copied, our slot buffers
//! (leaked) are only needed for the call. Banners (`0x1408C75F0(type 1..22, bool)`, EMEVD
//! 2007[2] / Lua `SetTextEffect`) are per-language images ("<lang>_TextEffect"), so no
//! custom text there; type 13 is the idol-found banner: the C++ imitation of the Lua event
//! `OnEvent_BonfireFirstLvUp_TextEffect` (`0x14069AFF0`) is `SetTextEffect(13)`, scheduled
//! by `OnEvent_BonfireFirstLvUp` (`0x14069AF20`) when a new idol is lit. `idol_banner`
//! plays it for an idol the friend unlocked for us.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, module_base, read, read_ptr};
use crate::{bossrules, hook, log};

/// `const wchar_t* EventText(MsgRepository*, u32 id)`.
const EVENT_TEXT_RVA: usize = 0x10A9E40;
/// `mov [rsp+8], rbx; push rdi; sub rsp, 20h` (same as GoodsName, read in IDA).
const EVENT_TEXT_PROLOGUE: [u8; 10] = [0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20];
const CAN_OPEN_RVA: usize = 0x8B6380;
const OPEN_RVA: usize = 0x8C4B40;
const CLOSE_RVA: usize = 0x8C4AC0;
const MENU_MAN_RVA: usize = 0x3D67408;
const MM_GEN_DLG: usize = 0x1FB0;
const DLG_RESULT: usize = 0x18;
const GAME_MAN_RVA: usize = 0x3D5C0D0;
const GM_LOAD_REQUEST: usize = 0x10;
/// `void (u32 msg, bool)`: the status message (EMEVD 2007[3]), Event Text `msg`.
const STATUS_MESSAGE_RVA: usize = 0xE13C30;
/// `bool (u32 type, bool)`: the big text banner (EMEVD 2007[2], Lua SetTextEffect).
const BANNER_RVA: usize = 0x8C75F0;
/// AppMenu::NewMenuSystem* (`0x140D872C0` returns it); both calls need it.
const NEW_MENU_SYSTEM_RVA: usize = 0x3D8A140;
/// Banner type of `OnEvent_BonfireFirstLvUp_TextEffect`: "SCULPTOR'S IDOL FOUND".
pub const BANNER_IDOL_FOUND: u32 = 13;
/// Two status messages at least this far apart (one replaces the other on screen).
const NOTICE_SPACING: Duration = Duration::from_secs(4);
/// Idols unlocked together (a join snapshot) get one banner.
const BANNER_SPACING: Duration = Duration::from_secs(10);

/// `void (GenDlgCtrl*, u32 msg, u32 id_a, u32 id_b)`: writes `+0/+4/+8` (thunk `0x1408B6B80`).
const GDC_SET_IDS_RVA: usize = 0x8B7200;
/// `void (GenDlgCtrl*, u8)`: writes byte `+0x10` = text source (thunk `0x1408B6B60`).
const GDC_SET_TEXT_SRC_RVA: usize = 0x8B71E0;
/// `void (u32 id, i32 v)`: MenuMan menu state `id` = v (`MenuMan + 0x20 + 4*id`).
const SET_MENU_STATE_RVA: usize = 0x8C68B0;
/// `void (GenDlgCtrl*)`: `+0x18` = -1, `+0x1C` = 0, menu state 136 = 0 (thunk `0x1408B6A80`).
const GDC_RESET_RESULT_RVA: usize = 0x8B71C0;
/// `void (GenDlgCtrl*, u32 type)`: `+0xC` = type, builds the dialog job (thunk `0x1408B6A70`).
const GDC_BUILD_RVA: usize = 0x8B70C0;
/// Menu state 84 = the initially highlighted button (0 = first = Yes, 1 = second = No) [I];
/// `Open` sets it to 1 for yes/no (`0x1408B6680` -> `0x1408B71F0`).
const MS_DEFAULT_BUTTON: u32 = 84;
/// MenuMan `+0x158` (344): `Open` ends with `-1` here.
const MM_PENDING_Q: usize = 0x158;
/// MenuMan menu state `id` lives at `MenuMan + MM_STATES + 4 * id`.
const MM_STATES: usize = 0x20;
/// GenDlgCtrl `+0x14`: `Open` (via `0x1408B66A0`) zeroes it.
const DLG_FIELD_14: usize = 0x14;
/// The `build` type `Open` uses for dialog type 2 (two buttons).
const BUILD_TYPE_TWO_BUTTONS: u32 = 8;
/// FDP_Dialogues ids of the yes/no labels (`Open` buttons 0).
const LABEL_YES: u32 = 3;
const LABEL_NO: u32 = 4;

const BUTTONS_YES_NO: i32 = 0;
const TYPE_TWO_BUTTONS: u32 = 2;

/// Our Event Text ids: far above the game's (8- and 9-digit ids), below i32::MAX.
const TEXT_ID_BASE: u32 = 1_999_990_000;
const SLOTS: usize = 8;

/// A question waits this long for a quiet moment, then answers `Unavailable`.
const ASK_WAIT: Duration = Duration::from_secs(10);
/// A notice waits this long, then goes to the overlay.
const NOTICE_WAIT: Duration = Duration::from_secs(8);
/// The dialog must be up (menu state 17) this soon after `Open`.
const SHOW_CHECK: Duration = Duration::from_millis(1500);
/// An unanswered dialog is closed after this long (answer No).
const ANSWER_MAX: Duration = Duration::from_secs(300);
/// Pause between two of our dialogs.
const GAP: Duration = Duration::from_millis(600);
const SETTLED: Duration = Duration::from_secs(3);

/// `[GAMEPLAY] confirm_items`: the co-op items ask first (yes/no).
pub static CONFIRM_ITEMS: AtomicBool = AtomicBool::new(true);
/// `[DISPLAY] native_notices`: important co-op notices as the game's own status message
/// (no input needed); 0 = the overlay.
pub static NATIVE_NOTICES: AtomicBool = AtomicBool::new(false);

static TEXT_TRAMP: AtomicUsize = AtomicUsize::new(0);
static EMPTY: [u16; 1] = [0];
/// UTF-16 text per slot (leaked buffers, a few dozen bytes per dialog, never freed:
/// the menu may still read the old pointer for a frame).
static SLOT_TEXT: [AtomicPtr<u16>; SLOTS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; SLOTS];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    Yes,
    No,
    /// Not shown: the caller does what it did before this feature.
    Unavailable(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Importance {
    /// The overlay (top left), as before.
    Info,
    /// The game's status message (fades by itself, no input), else the overlay.
    Important,
    /// Join / leave / "entered your world" / "back in your own world" notices. ALWAYS the
    /// overlay, whatever `native_notices` says (2026-10-24: v7.8 live, the game's status
    /// message for "A companion has entered your world." left the menu mask stuck at
    /// 0x800008, no attack or menu until death; `native_notices = 0` fixed it).
    Membership,
}

/// Whether a notice may go out as the game's status message (pure, for tests).
fn uses_native(importance: Importance, native_on: bool, hooked: bool) -> bool {
    importance == Importance::Important && native_on && hooked
}

/// The message box is only used for questions now (notices never need OK).
enum Job {
    Ask { ticket: u64 },
}

/// A non-blocking notice or banner waiting for a quiet moment.
enum Show {
    Status { text: String, secs: u64 },
    Banner { ty: u32, fallback: String },
}

struct Queued {
    job: Job,
    text: String,
    at: Instant,
}

struct Open {
    job: Job,
    text: String,
    at: Instant,
    shown: bool,
    gone_since: Option<Instant>,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Queued>,
    open: Option<Open>,
    answers: Vec<(u64, Answer)>,
    next_ticket: u64,
    next_slot: usize,
    closed_at: Option<Instant>,
    in_world_since: Option<Instant>,
    wait_logged: Option<Instant>,
    shows: VecDeque<(Show, Instant)>,
    status_at: Option<Instant>,
    banner_at: Option<Instant>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

extern "C" fn event_text_detour(repo: usize, id: u32) -> *const u16 {
    if let Some(slot) = id.checked_sub(TEXT_ID_BASE).map(|s| s as usize).filter(|&s| s < SLOTS) {
        let p = SLOT_TEXT[slot].load(Ordering::Acquire);
        if !p.is_null() {
            return p;
        }
    }
    match TEXT_TRAMP.load(Ordering::Acquire) {
        // Hooked, trampoline not stored yet (microseconds at install): an empty text is
        // safer than a spin on a menu thread.
        0 => EMPTY.as_ptr(),
        t => {
            let f: extern "C" fn(usize, u32) -> *const u16 = unsafe { std::mem::transmute(t) };
            f(repo, id)
        }
    }
}

/// Installs the Event Text detour (from `init`, at the title screen).
pub fn install() {
    let d: extern "C" fn(usize, u32) -> *const u16 = event_text_detour;
    match hook::install_detour(EVENT_TEXT_RVA, &EVENT_TEXT_PROLOGUE, d as usize, "EventText (0x10A9E40)") {
        Some(t) => {
            TEXT_TRAMP.store(t, Ordering::Release);
            log::line("dialog: EventText hooked, the game's own message box can show co-op texts");
        }
        None => log::line("dialog: EventText NOT hooked: co-op confirmations fall back to the old behaviour, notices stay on the overlay"),
    }
}

fn hooked() -> bool {
    TEXT_TRAMP.load(Ordering::Acquire) != 0
}

fn wide(s: &str) -> *mut u16 {
    let v: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    Box::leak(v.into_boxed_slice()).as_mut_ptr()
}

fn can_open() -> bool {
    let f: extern "C" fn() -> bool = unsafe { std::mem::transmute(module_base() + CAN_OPEN_RVA) };
    f()
}

fn ctrl() -> Option<usize> {
    read_ptr(module_base() + MENU_MAN_RVA).and_then(|mm| read_ptr(mm + MM_GEN_DLG))
}

/// MenuMan menu state `id`, read like `Open` writes it.
fn menu_state(id: u32) -> Option<i32> {
    read::<i32>(read_ptr(module_base() + MENU_MAN_RVA)? + MM_STATES + 4 * id as usize)
}

/// `Open(msg, 0, 2)` rebuilt with the default button = Yes: the steps of `0x1408C4B40`
/// (`set_ids 3/4`, text source 0 + `+0x14` = 0, text source 1, menu state 84, reset result,
/// build 8, MenuMan `+0x158` = -1), except menu state 84 = 0 where `Open` writes 1.
/// False when the control is missing (nothing was called).
fn open_default_yes(msg: u32) -> bool {
    let Some(c) = ctrl() else { return false };
    let Some(mm) = read_ptr(module_base() + MENU_MAN_RVA) else { return false };
    let base = module_base();
    unsafe {
        let set_ids: extern "C" fn(usize, u32, u32, u32) = std::mem::transmute(base + GDC_SET_IDS_RVA);
        let set_text_src: extern "C" fn(usize, u8) = std::mem::transmute(base + GDC_SET_TEXT_SRC_RVA);
        let set_state: extern "C" fn(u32, i32) = std::mem::transmute(base + SET_MENU_STATE_RVA);
        let reset: extern "C" fn(usize) = std::mem::transmute(base + GDC_RESET_RESULT_RVA);
        let build: extern "C" fn(usize, u32) = std::mem::transmute(base + GDC_BUILD_RVA);
        set_ids(c, msg, LABEL_YES, LABEL_NO);
        set_text_src(c, 0);
        *((c + DLG_FIELD_14) as *mut u32) = 0;
        set_text_src(c, 1);
        set_state(MS_DEFAULT_BUTTON, 0);
        reset(c);
        build(c, BUILD_TYPE_TWO_BUTTONS);
        *((mm + MM_PENDING_Q) as *mut i64) = -1;
    }
    true
}

fn result() -> Option<i32> {
    read::<i32>(ctrl()? + DLG_RESULT)
}

/// Why our dialog must not open now (`None` = fine).
fn blocker(in_world: Duration) -> Option<&'static str> {
    if !hooked() {
        return Some("EventText not hooked");
    }
    if in_world < SETTLED {
        return Some("just loaded");
    }
    if read_ptr(module_base() + GAME_MAN_RVA).and_then(|gm| read::<u8>(gm + GM_LOAD_REQUEST)) != Some(0) {
        return Some("load pending");
    }
    if let Err(why) = game::safe_for_reward(in_world) {
        return Some(why);
    }
    if !game::talk_idle() {
        return Some("talk");
    }
    if ctrl().is_none() {
        return Some("no dialog control");
    }
    if !can_open() {
        return Some("another dialog");
    }
    None
}

/// Asks a yes/no question in the game's own message box. `None` = cannot be asked at all
/// (setting off is the caller's business; hook missing here): act as before. Else a ticket
/// for `answer`.
pub fn ask(text: &str) -> Option<u64> {
    if !hooked() {
        log::line(&format!("dialog: cannot ask \"{text}\" (EventText not hooked)"));
        return None;
    }
    with(|st| {
        st.next_ticket += 1;
        let ticket = st.next_ticket;
        st.queue.push_back(Queued { job: Job::Ask { ticket }, text: text.to_string(), at: Instant::now() });
        log::line(&format!("dialog: question #{ticket} queued: \"{text}\""));
        ticket
    })
}

/// The answer to `ask`, once (None = not answered yet).
pub fn answer(ticket: u64) -> Option<Answer> {
    with(|st| {
        let i = st.answers.iter().position(|(t, _)| *t == ticket)?;
        Some(st.answers.swap_remove(i).1)
    })
    .flatten()
}

/// One co-op notice: `Important` becomes the game's own status message (it fades by
/// itself, nothing to press, no pause) when `native_notices = 1` and the world is ready
/// within a few seconds; otherwise (and `Info`) the overlay for `secs` seconds as before.
pub fn game_notice(text: String, importance: Importance, secs: u64) {
    if !uses_native(importance, NATIVE_NOTICES.load(Ordering::Relaxed), hooked()) {
        bossrules::notice(text, secs);
        return;
    }
    let queued = with(|st| {
        if st.shows.len() >= 6 {
            return false;
        }
        st.shows.push_back((Show::Status { text: text.clone(), secs }, Instant::now()));
        true
    })
    .unwrap_or(false);
    if !queued {
        bossrules::notice(text, secs);
    }
}

/// The friend unlocked a Sculptor's Idol for us: the game's own "SCULPTOR'S IDOL FOUND"
/// banner (type 13, as vanilla lights a new idol), nothing to press. Several idols at once
/// get one banner. `fallback` goes to the overlay if the banner cannot be shown.
pub fn idol_banner(fallback: &str) {
    let fallback = fallback.to_string();
    let queued = with(|st| {
        let recent = st.banner_at.is_some_and(|t| t.elapsed() < BANNER_SPACING)
            || st.shows.iter().any(|(s, _)| matches!(s, Show::Banner { .. }));
        if recent {
            return true;
        }
        st.shows.push_back((Show::Banner { ty: BANNER_IDOL_FOUND, fallback: fallback.clone() }, Instant::now()));
        true
    })
    .unwrap_or(false);
    if !queued {
        bossrules::notice(fallback, 6);
    }
}

fn new_menu_system() -> bool {
    read_ptr(module_base() + NEW_MENU_SYSTEM_RVA).is_some_and(|p| p >= 0x10000)
}

/// Why a status message / banner must wait (`None` = show it now).
fn show_blocker(in_world: Duration) -> Option<&'static str> {
    if in_world < SETTLED {
        return Some("just loaded");
    }
    if read_ptr(module_base() + GAME_MAN_RVA).and_then(|gm| read::<u8>(gm + GM_LOAD_REQUEST)) != Some(0) {
        return Some("load pending");
    }
    if !new_menu_system() {
        return Some("no menu system");
    }
    None
}

/// The non-blocking notices and banners (game thread, from `tick`).
fn show_step(st: &mut State, in_world: bool, in_world_for: Duration, now: Instant) {
    let Some((front, at)) = st.shows.front() else { return };
    let banner = matches!(front, Show::Banner { .. });
    if !banner && st.status_at.is_some_and(|t| now.duration_since(t) < NOTICE_SPACING) {
        return;
    }
    let blocked = if in_world { show_blocker(in_world_for) } else { Some("no player") };
    if let Some(why) = blocked {
        if now.duration_since(*at) >= NOTICE_WAIT {
            let (s, _) = st.shows.pop_front().expect("front");
            match s {
                Show::Status { text, secs } => {
                    log::line(&format!("dialog: notice on the overlay instead ({why}): {text}"));
                    bossrules::notice(text, secs);
                }
                Show::Banner { ty, fallback } => {
                    log::line(&format!("dialog: banner {ty} not shown ({why}): overlay notice instead"));
                    bossrules::notice(fallback, 6);
                }
            }
        }
        return;
    }
    let (s, at) = st.shows.pop_front().expect("front");
    match s {
        Show::Status { text, .. } => {
            let slot = st.next_slot % SLOTS;
            st.next_slot += 1;
            SLOT_TEXT[slot].store(wide(&text), Ordering::Release);
            let id = TEXT_ID_BASE + slot as u32;
            let status: extern "C" fn(u32, u8) = unsafe { std::mem::transmute(module_base() + STATUS_MESSAGE_RVA) };
            status(id, 0);
            st.status_at = Some(now);
            log::line(&format!(
                "dialog: notice as the game's status message (text id {id}, after {:.1} s): \"{text}\"",
                now.duration_since(at).as_secs_f32()
            ));
        }
        Show::Banner { ty, fallback } => {
            let banner: extern "C" fn(u32, u8) -> u8 = unsafe { std::mem::transmute(module_base() + BANNER_RVA) };
            let ok = banner(ty, 0) != 0;
            st.banner_at = Some(now);
            if ok {
                log::line(&format!("dialog: the game's banner {ty} shown (idol found)"));
            } else {
                log::line(&format!("dialog: the game refused banner {ty}: overlay notice instead"));
                bossrules::notice(fallback, 6);
            }
        }
    }
}

fn give_up(st: &mut State, q: Queued, why: &'static str) {
    let Job::Ask { ticket } = q.job;
    log::line(&format!("dialog: question #{ticket} not shown ({why}): the caller falls back"));
    st.answers.push((ticket, Answer::Unavailable(why)));
}

fn finish(st: &mut State, o: Open, answer: Answer) {
    // As the Lua scripts do after reading the result: menu state 17 back to 0.
    if !can_open() {
        let close: extern "C" fn() -> bool = unsafe { std::mem::transmute(module_base() + CLOSE_RVA) };
        close();
    }
    st.closed_at = Some(Instant::now());
    let Job::Ask { ticket } = o.job;
    log::line(&format!("dialog: question #{ticket} answered {answer:?} after {:.1} s", o.at.elapsed().as_secs_f32()));
    st.answers.push((ticket, answer));
}

/// Every frame (items.rs `on_frame`, game thread, first thing of the mod).
pub fn tick() {
    let in_world = game::player().is_some();
    with(|st| {
        let now = Instant::now();
        let in_world_for = if in_world { now.duration_since(*st.in_world_since.get_or_insert(now)) } else {
            st.in_world_since = None;
            Duration::ZERO
        };
        // Status messages and banners need no answer: whatever else is open.
        show_step(st, in_world, in_world_for, now);
        // Ours is up: wait for the answer.
        if let Some(o) = st.open.as_mut() {
            let r = result().unwrap_or(-1);
            if !o.shown {
                if !can_open() || r != -1 {
                    o.shown = true;
                    log::line(&format!("dialog: shown after {:.2} s: \"{}\"", o.at.elapsed().as_secs_f32(), o.text));
                } else if o.at.elapsed() >= SHOW_CHECK {
                    let o = st.open.take().expect("open");
                    st.closed_at = Some(now);
                    give_up(st, Queued { job: o.job, text: o.text, at: o.at }, "the game did not show it");
                    return;
                }
                return;
            }
            if r != -1 {
                let o = st.open.take().expect("open");
                let a = if r == 1 { Answer::Yes } else { Answer::No };
                if r != 1 && r != 2 {
                    log::line(&format!("dialog: result {r} (0 = backed out) counts as No"));
                }
                finish(st, o, a);
                return;
            }
            // Gone without a result (a load, another menu closed it): No.
            if can_open() {
                let since = *o.gone_since.get_or_insert(now);
                if now.duration_since(since) >= Duration::from_millis(500) {
                    let o = st.open.take().expect("open");
                    log::line("dialog: closed without a result, counts as No");
                    finish(st, o, Answer::No);
                }
                return;
            }
            o.gone_since = None;
            if o.at.elapsed() >= ANSWER_MAX {
                let o = st.open.take().expect("open");
                log::line("dialog: no answer for 5 minutes, closing it (No)");
                finish(st, o, Answer::No);
            }
            return;
        }
        let Some(front) = st.queue.front() else { return };
        let limit = ASK_WAIT;
        if st.closed_at.is_some_and(|t| now.duration_since(t) < GAP) {
            return;
        }
        if let Some(why) = if in_world { blocker(in_world_for) } else { Some("no player") } {
            if now.duration_since(front.at) >= limit {
                let q = st.queue.pop_front().expect("front");
                give_up(st, q, why);
            } else if st.wait_logged.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(2)) {
                st.wait_logged = Some(now);
                log::line(&format!("dialog: waiting ({why})"));
            }
            return;
        }
        let q = st.queue.pop_front().expect("front");
        let slot = st.next_slot % SLOTS;
        st.next_slot += 1;
        SLOT_TEXT[slot].store(wide(&q.text), Ordering::Release);
        let (buttons, ty) = (BUTTONS_YES_NO, TYPE_TWO_BUTTONS);
        let id = TEXT_ID_BASE + slot as u32;
        if crate::tuning::DIALOG_DEFAULT_YES.load(Ordering::Relaxed) && open_default_yes(id) {
            let n = menu_state(MS_DEFAULT_BUTTON).map_or_else(|| "?".to_string(), |v| v.to_string());
            log::line(&format!("dialog: opened (text id {id}, default=Yes, state84 read back {n}): \"{}\"", q.text));
        } else {
            let open: extern "C" fn(u32, i32, u32, u32) -> u64 = unsafe { std::mem::transmute(module_base() + OPEN_RVA) };
            open(id, buttons, ty, 0);
            log::line(&format!("dialog: opened (text id {id}, buttons {buttons}, type {ty}): \"{}\"", q.text));
        }
        st.open = Some(Open { job: q.job, text: q.text, at: now, shown: false, gone_since: None });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_ids_stay_positive_and_ours() {
        assert!(TEXT_ID_BASE + SLOTS as u32 <= i32::MAX as u32);
        assert!(TEXT_ID_BASE.checked_sub(TEXT_ID_BASE + SLOTS as u32).is_none());
    }

    #[test]
    fn membership_notices_never_use_the_native_status_message() {
        assert!(!uses_native(Importance::Membership, true, true));
        assert!(!uses_native(Importance::Info, true, true));
        assert!(!uses_native(Importance::Important, false, true));
        assert!(!uses_native(Importance::Important, true, false));
        assert!(uses_native(Importance::Important, true, true));
    }

    #[test]
    fn wide_text_is_nul_terminated() {
        let p = wide("Yes?");
        let s = unsafe { std::slice::from_raw_parts(p, 5) };
        assert_eq!(s, &[b'Y' as u16, b'e' as u16, b's' as u16, b'?' as u16, 0]);
    }
}
