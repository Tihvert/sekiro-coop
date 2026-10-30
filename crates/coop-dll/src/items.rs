//! Co-op connection items, like Elden Ring Seamless Co-op's session items (no
//! invasion or PvP items): docs/SPEC-CUSTOM-ITEMS.md, plan A, reworked 2026-10-23.
//!
//! - Kindred Bell: open our world (host). Kindred Charm: look for a companion's world
//!   (join). Severed Thread: leave co-op / close our world. They only set
//!   `net::set_intent`; `net.rs` does the rest.
//! - Rows: three unnamed, unused vanilla EquipParamGoods rows, found at runtime (the
//!   three highest candidate ids, see `pick_rows`). A vanilla consumable ("Pellet") is
//!   copied into them as the template and ~25 fields are overwritten (never consumed,
//!   cannot be dropped or stored, no effect). Re-applied every 5 s if it changed.
//! - Names: detours on GoodsName / GoodsInfo / GoodsCaption (`0x10AA1F0`, `0x10AA170`,
//!   `0x10AA0F0`) return our text for our ids.
//! - Use: detour on `PlayerIns::OnGoodsUsed` (`0xA262D0`, PlayerIns vtable slot 125)
//!   sets a bit for our ids on the main player; the frame task acts on it.
//! - Give: on the player's own idol rest once the Dilapidated Temple idol is unlocked
//!   (the tutorial is over), each missing item through `0x91C970` with the normal
//!   "item received" popup. Never at game start. The mod only runs with the co-op save
//!   (`S0000.coop.sl2`, me3 profile), so the items only ever land there.
//! - Title screen: detours on the TitleStep and InGameStep constructors (`0xA89DA0`,
//!   `0xA64520`) mark that the game went back to the title / loads a save: co-op ends
//!   (`net::on_title`) and the items must be used again after loading.
//!
//! Detours touch atomics only; everything else runs on the frame task (`on_frame`).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fromsoftware_shared::FromStatic;
use sekiro::param::{BONFIRE_WARP_PARAM_ST, EQUIP_PARAM_GOODS_ST, ITEMLOT_PARAM_ST, ParamDef, SHOP_LINEUP_PARAM};
use sekiro::sprj::SoloParamRepository;

use crate::game::{self, module_base, read, read_ptr};
use crate::net::{self, Intent, Link};
use crate::{bossrules, hook, log, settings};

const GOODS_NAME_RVA: usize = 0x10AA1F0;
const GOODS_INFO_RVA: usize = 0x10AA170;
const GOODS_CAPTION_RVA: usize = 0x10AA0F0;
// mov [rsp+8], rbx; push rdi; sub rsp, 20h (same in all three, no RIP-relative code)
const TEXT_PROLOGUE: [u8; 10] = [0x48, 0x89, 0x5C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x20];
/// `const wchar_t* GetPlaceName(MsgRepository*, u32 id)` (FMG banks 120, 19, 216, 256).
const PLACE_NAME_RVA: usize = 0x10AA550;
/// The constants the text getters return when no FMG has the id.
const MISSING_NAME_RVA: usize = 0x2B955E8; // L"?GoodsName?"
const MISSING_INFO_RVA: usize = 0x2B95600; // L"?GoodsInfo?"
const MISSING_CAPTION_RVA: usize = 0x2B95618; // L"?GoodsCaption?"
const MSG_REPO: usize = 0x3D964C8;
/// `char OnGoodsUsed(PlayerIns*, u32* fullId)`, after the use animation; it consumes
/// the item (`0x79AFA0`) and gives `use_get_item`.
const USED_RVA: usize = 0xA262D0;
// push rdi; sub rsp, 40h
const USED_PROLOGUE: [u8; 6] = [0x40, 0x57, 0x48, 0x83, 0xEC, 0x40];
/// TitleStep constructor `(this, int)`: the game (re)enters the title screen.
const TITLE_STEP_RVA: usize = 0xA89DA0;
// mov [rsp+8], rcx; push rdi; sub rsp, 30h
const TITLE_STEP_PROLOGUE: [u8; 10] = [0x48, 0x89, 0x4C, 0x24, 0x08, 0x57, 0x48, 0x83, 0xEC, 0x30];
/// InGameStep constructor `(this)`: only built by the title steps (`0xA65100` from
/// `0xA89DA0` / `0xA86430`), i.e. once per trip through the title, never on area loads.
const IN_GAME_STEP_RVA: usize = 0xA64520;
// mov [rsp+8], rcx; push rsi; push rdi; push r14; sub rsp, 30h
const IN_GAME_STEP_PROLOGUE: [u8; 13] = [0x48, 0x89, 0x4C, 0x24, 0x08, 0x56, 0x57, 0x41, 0x56, 0x48, 0x83, 0xEC, 0x30];
/// `MapItemMan_GrantItems(MapItemMan*, ItemBuffer*, void*, bool silent, u32* out, bool)`.
/// `silent = 0` runs the normal "item received" log (`0x91FB70`, needs `is_notice_log`).
const GRANT_RVA: usize = 0x91C970;
const MAP_ITEM_MAN: usize = 0x3D6CDC0;
/// `i32 FindItem(EquipGameData*, u32* fullId)`: inventory index, < 0 = not owned.
const FIND_ITEM_RVA: usize = 0x79D7E0;
const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
const PGD_EQUIP_GAME_DATA: usize = 0x278;
const EVENT_FLAG_MAN: usize = 0x3D55FE8;
const GET_EVENT_FLAG_RVA: usize = 0x6C3E60;
/// The idol entity id during the frame a rest is processed, -1 otherwise (world.rs).
const REST_NOW: usize = 0x3AFEB9C;
const GOODS: u32 = 0x4000_0000;
/// Hard-coded special goods in `0xA262D0` / `0x7A0E50`.
const RESERVED: [u32; 2] = [102, 111];
const ROW_SIZE: usize = 0xA0;
const APPLY_EVERY: Duration = Duration::from_secs(5);
const CHECK_EVERY: Duration = Duration::from_secs(1);
const COOLDOWN: Duration = Duration::from_secs(3);
/// After a title trip: in the world this long before `auto_connect` may connect again.
const HOLD_RELEASE_AFTER: Duration = Duration::from_secs(3);
/// The Pellet's row. The template is found by id first: the names below only match on an
/// English game, and the fallback row on other languages (row 61 on a Chinese game) gave
/// items whose use never reached `OnGoodsUsed` (Nexus reports, 1.0.4).
const TEMPLATE_ID: u32 = 3020;
/// English names of the template consumables, in order of preference after `TEMPLATE_ID`.
const TEMPLATE_NAMES: [&str; 2] = ["Pellet", "Ako's Sugar"];
const TEMPLE_NAME: &str = "Dilapidated Temple";
/// Fallback if no idol is called "Dilapidated Temple" (the place names did not resolve
/// live): BonfireWarpParam row 100, the home idol of a new game, is the first idol of
/// map 11_00 (entity 1101950, flag 11100000) [ARV, idol dump 2026-10-23]. 1101952 was
/// a wrong guess: it is the Ashina Outskirts idol the host stood at, so the friend's
/// temple rest gave nothing.
const TEMPLE_ROW: i32 = 100;
const TEMPLE_ENTITY_GUESS: i32 = 1101950;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Host = 0,
    Join = 1,
    Leave = 2,
}

const KINDS: [Kind; 3] = [Kind::Host, Kind::Join, Kind::Leave];

/// `wide!("text")`: a NUL-terminated UTF-16 static (ASCII only), never freed.
macro_rules! wide {
    ($s:expr) => {{
        const S: &str = $s;
        const N: usize = S.len() + 1;
        static W: [u16; N] = {
            let b = S.as_bytes();
            let mut o = [0u16; N];
            let mut i = 0;
            while i < b.len() {
                o[i] = b[i] as u16;
                i += 1;
            }
            o
        };
        W.as_ptr()
    }};
}

const HOST_NAME: &str = "Kindred Bell";
const JOIN_NAME: &str = "Kindred Charm";
const LEAVE_NAME: &str = "Severed Thread";

/// Players in the session now (puppet.rs publishes it every frame; 1 = solo).
fn session_players() -> u8 {
    crate::peers::SESSION_PLAYERS.load(Ordering::Relaxed)
}

/// The Charm while connected to a full world of 3+ (`max` > 2): "This world is full (N/N)."
/// Two players keep the old "Already in co-op" text.
fn full_text(players: u8, max: u8) -> Option<String> {
    (max > 2 && players >= max).then(|| format!("This world is full ({players}/{max})."))
}

fn name_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Host => HOST_NAME,
        Kind::Join => JOIN_NAME,
        Kind::Leave => LEAVE_NAME,
    }
}

/// Icon id per item. `None` = borrow a vanilla icon (`ICON_SOURCES`). 251 / 552 / 962 are
/// unused vanilla icon cells that icons.rs repaints in memory while the menu TPF loads
/// (custom_icons = 1); otherwise they show the vanilla art.
const HOST_ICON: Option<u16> = Some(251);
const JOIN_ICON: Option<u16> = Some(552);
const LEAVE_ICON: Option<u16> = Some(962);
const ICON_OVERRIDE: [Option<u16>; 3] = [HOST_ICON, JOIN_ICON, LEAVE_ICON];

/// Vanilla goods whose icon each item borrows (English names, first found wins; the
/// template's icon if none is found). Logged at start.
const ICON_SOURCES: [&[&str]; 3] = [
    // Kindred Bell: a bell.
    &["Bell Demon", "Bell Charm"],
    // Kindred Charm: a charm / idol.
    &["Homeward Idol", "Kuro's Charm"],
    // Severed Thread: something broken.
    &["Ceramic Shard", "Fistful of Ash"],
];

/// (name, info, caption) for `kind`.
fn texts(kind: Kind) -> [*const u16; 3] {
    match kind {
        Kind::Host => [
            wide!("Kindred Bell"),
            wide!("Ring to open your world to a companion who shares your password."),
            wide!("A small bell bound by a shared vow.\nIts faint toll carries far, to one who knows the vow.\n\nRing it to open your world to a companion\nwho shares your password."),
        ],
        Kind::Join => [
            wide!("Kindred Charm"),
            wide!("Seek a companion's world that shares your password."),
            wide!("A worn charm, warm to the touch.\nIt tugs gently toward its twin, wherever it may be.\n\nUse to seek the world of a companion\nwho shares your password, and travel there."),
        ],
        Kind::Leave => [
            wide!("Severed Thread"),
            wide!("Leave the shared world / close your world to companions."),
            wide!("A red thread, cut clean.\nWhat was bound is bound no longer.\n\nUse to leave a companion's world, or to close\nyour own. When the host closes their world,\nthe companion returns to their own."),
        ],
    }
}

/// Param ids of the rows we use (0 = not chosen yet), by `Kind`.
static IDS: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
static NAME_TRAMP: AtomicUsize = AtomicUsize::new(0);
static INFO_TRAMP: AtomicUsize = AtomicUsize::new(0);
static CAPTION_TRAMP: AtomicUsize = AtomicUsize::new(0);
static USED_TRAMP: AtomicUsize = AtomicUsize::new(0);
static TITLE_TRAMP: AtomicUsize = AtomicUsize::new(0);
static IN_GAME_TRAMP: AtomicUsize = AtomicUsize::new(0);
/// Bit per `Kind`: used by the main player, not handled yet.
static USE_PENDING: AtomicU8 = AtomicU8::new(0);
/// Bumped by the TitleStep / InGameStep constructors.
static TITLE_COUNT: AtomicU32 = AtomicU32::new(0);

/// TitleStep / InGameStep constructions so far: a change = back at the title or a save
/// load, never an area load or idol warp (ownworld.rs tells its own reload apart with it).
pub fn title_count() -> u32 {
    TITLE_COUNT.load(Ordering::Acquire)
}
/// `connection_items` (settings, [DEBUG]): 0 = no items (`auto_connect = 1` instead).
pub static ENABLED: AtomicBool = AtomicBool::new(true);

const STAGE_UNKNOWN: u8 = 0;
const STAGE_TUTORIAL: u8 = 1;
const STAGE_REST: u8 = 2;
const STAGE_READY: u8 = 3;
const STAGE_FAILED: u8 = 4;
/// What the solo status line says (`solo_hint`).
static STAGE: AtomicU8 = AtomicU8::new(STAGE_UNKNOWN);

/// Overlay text while there is no session (after "Co-op: ").
pub fn solo_hint() -> String {
    if !ENABLED.load(Ordering::Relaxed) {
        return "solo (connection_items = 0: set auto_connect = 1 to connect)".into();
    }
    match STAGE.load(Ordering::Relaxed) {
        STAGE_TUTORIAL => "co-op unlocks after the tutorial, at the Dilapidated Temple idol".into(),
        STAGE_REST => "rest at an idol to receive the co-op items".into(),
        STAGE_READY => format!("use {HOST_NAME} to host or {JOIN_NAME} to join"),
        STAGE_FAILED => "co-op items unavailable (see log.txt), use auto_connect = 1".into(),
        _ => "solo".into(),
    }
}

/// How good goods row `id` called `name` is as the template (lower is better).
fn template_rank(id: u32, name: &str) -> Option<usize> {
    if id == TEMPLATE_ID {
        return Some(0);
    }
    TEMPLATE_NAMES.iter().position(|n| *n == name).map(|rank| rank + 1)
}

fn kind_of(param_id: u32) -> Option<Kind> {
    if param_id == 0 {
        return None;
    }
    KINDS.into_iter().find(|&k| IDS[k as usize].load(Ordering::Relaxed) == param_id)
}

type TextFn = extern "C" fn(usize, u32) -> *const u16;

fn call_text(tramp: &AtomicUsize, missing_rva: usize, repo: usize, id: u32) -> *const u16 {
    match tramp.load(Ordering::Acquire) {
        // Hooked but the trampoline is not stored yet (install window).
        0 => (module_base() + missing_rva) as *const u16,
        t => {
            let f: TextFn = unsafe { std::mem::transmute(t) };
            f(repo, id)
        }
    }
}

extern "C" fn name_detour(repo: usize, id: u32) -> *const u16 {
    match kind_of(id) {
        Some(k) => texts(k)[0],
        None => call_text(&NAME_TRAMP, MISSING_NAME_RVA, repo, id),
    }
}

extern "C" fn info_detour(repo: usize, id: u32) -> *const u16 {
    match kind_of(id) {
        Some(k) => texts(k)[1],
        None => call_text(&INFO_TRAMP, MISSING_INFO_RVA, repo, id),
    }
}

extern "C" fn caption_detour(repo: usize, id: u32) -> *const u16 {
    match kind_of(id) {
        Some(k) => texts(k)[2],
        None => call_text(&CAPTION_TRAMP, MISSING_CAPTION_RVA, repo, id),
    }
}

/// `0xA262D0(PlayerIns*, u32* fullId)`: the original first, then flag our items.
extern "C" fn used_detour(chr: usize, id: *const u32) -> u8 {
    let full = read::<u32>(id as usize).unwrap_or(0);
    // Every goods use goes through here: wait out the install window (microseconds).
    let t = loop {
        match USED_TRAMP.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    };
    let original: extern "C" fn(usize, *const u32) -> u8 = unsafe { std::mem::transmute(t) };
    let r = original(chr, id);
    // Only the main player: a native remote PlayerIns runs the same vtable slot.
    if full & 0xF000_0000 == GOODS
        && let Some(k) = kind_of(full & 0x0FFF_FFFF)
        && game::player().is_some_and(|p| p.0 == chr)
    {
        USE_PENDING.fetch_or(1 << k as u8, Ordering::AcqRel);
    }
    r
}

type CtorFn = extern "C" fn(usize, usize) -> usize;

fn call_ctor(tramp: &AtomicUsize, this: usize, a: usize) -> usize {
    TITLE_COUNT.fetch_add(1, Ordering::AcqRel);
    // A constructor must run: in the install window (the patch is in, the trampoline
    // not stored yet; microseconds on the init thread) wait for it.
    let t = loop {
        match tramp.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    };
    let f: CtorFn = unsafe { std::mem::transmute(t) };
    f(this, a)
}

extern "C" fn title_step_detour(this: usize, a: usize) -> usize {
    call_ctor(&TITLE_TRAMP, this, a)
}

extern "C" fn in_game_step_detour(this: usize, a: usize) -> usize {
    call_ctor(&IN_GAME_TRAMP, this, a)
}

/// Installs the detours (at the title screen, from `init`).
pub fn install() {
    let text: [(usize, TextFn, &AtomicUsize, &str); 3] = [
        (GOODS_NAME_RVA, name_detour, &NAME_TRAMP, "GoodsName (0x10AA1F0)"),
        (GOODS_INFO_RVA, info_detour, &INFO_TRAMP, "GoodsInfo (0x10AA170)"),
        (GOODS_CAPTION_RVA, caption_detour, &CAPTION_TRAMP, "GoodsCaption (0x10AA0F0)"),
    ];
    let mut ok = 0;
    for (rva, detour, slot, name) in text {
        if let Some(t) = hook::install_detour(rva, &TEXT_PROLOGUE, detour as usize, name) {
            slot.store(t, Ordering::Release);
            ok += 1;
        }
    }
    let used: extern "C" fn(usize, *const u32) -> u8 = used_detour;
    if let Some(t) = hook::install_detour(USED_RVA, &USED_PROLOGUE, used as usize, "OnGoodsUsed (0xA262D0)") {
        USED_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    let title: CtorFn = title_step_detour;
    if let Some(t) = hook::install_detour(TITLE_STEP_RVA, &TITLE_STEP_PROLOGUE, title as usize, "TitleStep ctor (0xA89DA0)") {
        TITLE_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    let in_game: CtorFn = in_game_step_detour;
    if let Some(t) = hook::install_detour(IN_GAME_STEP_RVA, &IN_GAME_STEP_PROLOGUE, in_game as usize, "InGameStep ctor (0xA64520)") {
        IN_GAME_TRAMP.store(t, Ordering::Release);
        ok += 1;
    }
    log::line(&format!("items: {ok}/6 hooks installed"));
}

fn items_hooked() -> bool {
    [&NAME_TRAMP, &INFO_TRAMP, &CAPTION_TRAMP, &USED_TRAMP].iter().all(|t| t.load(Ordering::Acquire) != 0)
}

/// Reads a game UTF-16 string (at most 200 units).
fn wide_text(p: *const u16) -> String {
    let mut out = Vec::new();
    let mut a = p as usize;
    while out.len() < 200 {
        match read::<u16>(a) {
            Some(0) | None => break,
            Some(c) => out.push(c),
        }
        a += 2;
    }
    String::from_utf16_lossy(&out)
}

fn msg_repo() -> Option<usize> {
    read_ptr(module_base() + MSG_REPO)
}

/// The vanilla GoodsName / GoodsInfo text pointer of `id` (through the trampolines).
fn vanilla_text(repo: usize, id: u32, info: bool) -> usize {
    if info {
        call_text(&INFO_TRAMP, MISSING_INFO_RVA, repo, id) as usize
    } else {
        call_text(&NAME_TRAMP, MISSING_NAME_RVA, repo, id) as usize
    }
}

/// The vanilla English (game language) name of goods `id`, None for an unnamed row
/// (keyitems.rs: logs, notices, the key-item list). Works with or without our text hook.
pub(crate) fn goods_name(id: u32) -> Option<String> {
    let repo = msg_repo()?;
    let p = match NAME_TRAMP.load(Ordering::Acquire) {
        // Not hooked (or not yet): the function itself is unmodified.
        0 => {
            let f: TextFn = unsafe { std::mem::transmute(module_base() + GOODS_NAME_RVA) };
            f(repo, id) as usize
        }
        _ => vanilla_text(repo, id, false),
    };
    if p == 0 || p == module_base() + MISSING_NAME_RVA {
        return None;
    }
    Some(wide_text(p as *const u16)).filter(|s| !s.is_empty())
}

/// One of our three co-op item rows (Kindred Bell / Charm / Severed Thread): never shared.
pub(crate) fn is_coop_goods(param_id: u32) -> bool {
    kind_of(param_id).is_some()
}

fn place_name(repo: usize, id: i32) -> String {
    if id <= 0 {
        return String::new();
    }
    let f: TextFn = unsafe { std::mem::transmute(module_base() + PLACE_NAME_RVA) };
    wide_text(f(repo, id as u32))
}

fn event_flag(id: i32) -> bool {
    let Some(fm) = read_ptr(module_base() + EVENT_FLAG_MAN) else { return false };
    if id <= 0 {
        return false;
    }
    let get: extern "C" fn(usize, u32, bool) -> bool = unsafe { std::mem::transmute(module_base() + GET_EVENT_FLAG_RVA) };
    get(fm, id as u32, false)
}

fn equip_game_data() -> Option<usize> {
    read_ptr(module_base() + GAME_DATA_MAN)
        .and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))
        .map(|pgd| pgd + PGD_EQUIP_GAME_DATA)
}

fn owns(param_id: u32) -> Option<bool> {
    if param_id == 0 {
        return None;
    }
    let egd = equip_game_data()?;
    let full = GOODS | param_id;
    let find: extern "C" fn(usize, *const u32) -> i32 = unsafe { std::mem::transmute(module_base() + FIND_ITEM_RVA) };
    Some(find(egd, &full) >= 0)
}

/// Our three rows, as they should be.
struct Rows {
    rows: Vec<(u32, EQUIP_PARAM_GOODS_ST)>,
}

/// The Dilapidated Temple idol: unlock flag and entity id.
#[derive(Clone, Copy, Debug)]
struct Temple {
    flag: i32,
    entity: i32,
}

#[derive(Default)]
struct State {
    /// None = not tried yet; Some(None) = failed (logged).
    rows: Option<Option<Rows>>,
    temple: Option<Option<Temple>>,
    last_apply: Option<Instant>,
    last_check: Option<Instant>,
    last_rest: i32,
    /// A rest was seen; the items are given once no menu is open (if the tutorial is over).
    give_pending: bool,
    /// That rest was at the temple idol itself.
    rested_at_temple: bool,
    in_world_since: Option<Instant>,
    cooldown: [Option<Instant>; 3],
    title_seen: u32,
    /// After a title trip: waiting for the next world (player gone once, then back).
    hold: Option<bool>,
    /// A use waiting for its yes/no answer (dialog.rs ticket), `confirm_items = 1`.
    confirm: Option<(Kind, u64)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn goods_file(repo: &mut SoloParamRepository) -> Option<&mut sekiro::fd4::ParamFile> {
    repo.params_mut().find(|p| p.struct_name() == EQUIP_PARAM_GOODS_ST::NAME).map(|p| &mut *p.data)
}

/// Every goods id an item lot or a shop gives (any category: excluding more is harmless).
fn referenced_ids(repo: &SoloParamRepository) -> Option<std::collections::HashSet<u32>> {
    let file = |name: &str| repo.params().find(|p| p.struct_name() == name).map(|p| &*p.data);
    let lots = file(ITEMLOT_PARAM_ST::NAME)?;
    let shops = file(SHOP_LINEUP_PARAM::NAME)?;
    let mut set = std::collections::HashSet::new();
    for (_, r) in unsafe { lots.rows::<ITEMLOT_PARAM_ST>() } {
        for id in [
            r.lot_item_id01(),
            r.lot_item_id02(),
            r.lot_item_id03(),
            r.lot_item_id04(),
            r.lot_item_id05(),
            r.lot_item_id06(),
            r.lot_item_id07(),
            r.lot_item_id08(),
        ] {
            if id > 0 {
                set.insert(id as u32);
            }
        }
    }
    for (_, r) in unsafe { shops.rows::<SHOP_LINEUP_PARAM>() } {
        if r.equip_id() > 0 {
            set.insert(r.equip_id() as u32);
        }
    }
    Some(set)
}

/// Finds our three rows, the template and the icons. Logs the choice; None = refused.
fn pick_rows() -> Option<Rows> {
    if std::mem::size_of::<EQUIP_PARAM_GOODS_ST>() != ROW_SIZE {
        log::line("items: ERROR EquipParamGoods row size is not 0xA0, items off");
        return None;
    }
    if !items_hooked() {
        log::line("items: ERROR text or use hooks missing (game version?), items off");
        return None;
    }
    let msg = msg_repo()?;
    let repo = unsafe { SoloParamRepository::instance_mut() }.ok()?;
    let used = referenced_ids(repo)?;
    let base = module_base();
    let file = goods_file(repo)?;
    let mut candidates: Vec<u32> = Vec::new();
    let mut template: Option<(usize, u32, EQUIP_PARAM_GOODS_ST)> = None;
    // (rank in ICON_SOURCES, source name, source id, icon) per kind.
    let mut icons: [Option<(usize, &str, u32, u16)>; 3] = [None, None, None];
    for (id, row) in unsafe { file.rows::<EQUIP_PARAM_GOODS_ST>() } {
        let name_p = vanilla_text(msg, id, false);
        if name_p == base + MISSING_NAME_RVA {
            if vanilla_text(msg, id, true) == base + MISSING_INFO_RVA
                && !used.contains(&id)
                && !RESERVED.contains(&id)
                && id < 0x0FFF_FFFF
            {
                candidates.push(id);
            }
            continue;
        }
        let name = wide_text(name_p as *const u16);
        if let Some(rank) = template_rank(id, &name)
            && template.as_ref().is_none_or(|(r, ..)| rank < *r)
        {
            template = Some((rank, id, row.clone()));
        }
        for (i, sources) in ICON_SOURCES.iter().enumerate() {
            if let Some(rank) = sources.iter().position(|n| *n == name)
                && icons[i].is_none_or(|(r, ..)| rank < r)
            {
                icons[i] = Some((rank, sources[rank], id, row.icon_id()));
            }
        }
    }
    log::line(&format!(
        "items: {} unnamed unused goods rows (highest {:?})",
        candidates.len(),
        candidates.iter().rev().take(8).collect::<Vec<_>>()
    ));
    // Row 3020 missing and not English: a consumable with an effect, as close as we can tell.
    if template.is_none() {
        for (id, row) in unsafe { file.rows::<EQUIP_PARAM_GOODS_ST>() } {
            if row.goods_type() == 0 && row.ref_category() == 2 && row.is_consume() && !row.is_bonfire_warp_item() && row.max_num() > 1 {
                log::line(&format!("items: no 'Pellet' by name, template is the first consumable row {id}"));
                template = Some((9, id, row.clone()));
                break;
            }
        }
    }
    let Some((_, template_id, template)) = template else {
        log::line("items: ERROR no template consumable found, items off");
        return None;
    };
    if candidates.len() < 3 {
        log::line("items: ERROR fewer than 3 unnamed unused goods rows, items off");
        return None;
    }
    // The three highest ids, deterministic for every player with the same game data.
    candidates.sort_unstable();
    let chosen = [candidates[candidates.len() - 3], candidates[candidates.len() - 2], candidates[candidates.len() - 1]];
    let mut rows = Vec::new();
    for (i, kind) in KINDS.into_iter().enumerate() {
        let id = chosen[i];
        let mut r = template.clone();
        r.set_ref_id_default(-1);
        r.set_ref_id_1(-1);
        r.set_ref_category(0);
        let icon = ICON_OVERRIDE[i].unwrap_or_else(|| icons[i].map_or(template.icon_id(), |(.., icon)| icon));
        r.set_icon_id(icon);
        r.set_sort_id(template.sort_id() + 1 + i as i32);
        r.set_max_num(1);
        r.set_max_repository_num(0);
        r.set_basic_price(0);
        r.set_sell_value(0);
        r.set_sale_value(-1);
        r.set_consume_hero_point(0);
        r.set_consume_hp_rate(0.0);
        r.set_consume_resource_item_a(0);
        r.set_consume_resource_item_b(0);
        r.set_consume_resource_item_c(0);
        r.set_enable_live(true);
        r.set_enable_gray(true);
        r.set_enable_white(true);
        r.set_enable_black(true);
        r.set_enable_multi(true);
        r.set_disable_offline(false);
        r.set_is_consume(false);
        r.set_is_only_one(true);
        r.set_is_discard(false);
        r.set_is_deposit(false);
        r.set_is_full_supple_item(false);
        r.set_is_enhance(false);
        r.set_is_bonfire_warp_item(false);
        r.set_is_use_ref_id1_for_enchant(false);
        r.set_is_drop(false);
        // The normal "item received" log when we give it (0x84C010 reads this bit).
        r.set_is_notice_log(true);
        r.set_is_notice_dialog(false);
        r.set_vagrant_item_lot_id(-1);
        r.set_vagrant_bonus_ene_drop_item_lot_id(-1);
        r.set_vagrant_item_ene_drop_item_lot_id(-1);
        r.set_first_get_event_flag_id(-1);
        r.set_use_get_item(0);
        r.set_use_limit_sp_effect_type0(0);
        r.set_use_limit_sp_effect_type1(0);
        r.set_use_limit_sp_effect_type2(0);
        r.set_use_limit_sp_effect_type3(0);
        r.set_use_limit_sp_effect_type4(0);
        log::line(&format!(
            "items: row id={id} ({}) template={template_id} icon={icon}{} consume=0 drop=0 refcat=0",
            name_of(kind),
            if ICON_OVERRIDE[i].is_some() {
                " (custom icon id)".to_owned()
            } else {
                icons[i].map_or_else(|| " (template's, no icon source found)".to_owned(), |(_, n, src, _)| format!(" (icon of '{n}' id {src})"))
            }
        ));
        rows.push((id, r));
    }
    Some(Rows { rows })
}

/// Writes our rows where they differ (idempotent); publishes the ids once written.
fn apply_rows(rows: &Rows) {
    let Ok(repo) = (unsafe { SoloParamRepository::instance_mut() }) else { return };
    let Some(file) = goods_file(repo) else { return };
    for (i, (id, want)) in rows.rows.iter().enumerate() {
        let Some(row) = (unsafe { file.get_row_by_id_mut::<EQUIP_PARAM_GOODS_ST>(*id) }) else { continue };
        let bytes = |r: &EQUIP_PARAM_GOODS_ST| unsafe { std::slice::from_raw_parts((r as *const EQUIP_PARAM_GOODS_ST).cast::<u8>(), ROW_SIZE) }.to_vec();
        if bytes(row) != bytes(want) {
            *row = want.clone();
            log::line(&format!("items: row {id} written"));
        }
        IDS[i].store(*id, Ordering::Release);
    }
}

/// The Dilapidated Temple idol from BonfireWarpParam (by its English place name, else
/// the entity id guess). Logs every idol row once.
fn find_temple() -> Option<Temple> {
    let msg = msg_repo()?;
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = repo.params().find(|p| p.struct_name() == BONFIRE_WARP_PARAM_ST::NAME).map(|p| &*p.data)?;
    let mut by_name = None;
    let mut by_guess = None;
    let mut n = 0;
    for (id, r) in unsafe { file.rows::<BONFIRE_WARP_PARAM_ST>() } {
        let names = [place_name(msg, r.msg_id()), place_name(msg, r.menu_text_id())];
        log::line(&format!(
            "items: idol row {id} flag {} entity {} msg {} menu {} names {:?}",
            r.eventflag_id(),
            r.bonfire_entity_id(),
            r.msg_id(),
            r.menu_text_id(),
            names
        ));
        n += 1;
        let t = Temple { flag: r.eventflag_id(), entity: r.bonfire_entity_id() };
        if by_name.is_none() && names.iter().any(|s| s == TEMPLE_NAME) {
            by_name = Some(t);
        }
        if by_guess.is_none() && (id as i32 == TEMPLE_ROW || r.bonfire_entity_id() == TEMPLE_ENTITY_GUESS) {
            by_guess = Some(t);
        }
    }
    let found = by_name.or(by_guess);
    log::line(&format!(
        "items: {n} idol rows, temple idol {found:?} ({})",
        if by_name.is_some() {
            "by name"
        } else if by_guess.is_some() {
            "by the entity id guess"
        } else {
            "NOT FOUND: the items are given on any rest"
        }
    ));
    found
}

impl State {
    /// The tutorial is over: the temple idol's unlock flag is on. If the idol row was
    /// not found at all, every rest counts (logged once by `find_temple`).
    fn temple_unlocked(&self) -> bool {
        match self.temple {
            Some(Some(t)) => event_flag(t.flag),
            Some(None) => true,
            None => false,
        }
    }

    /// Gives every missing item in one call, with the normal popup.
    fn give_missing(&mut self) {
        let mut entries: Vec<u32> = Vec::new();
        let mut names = Vec::new();
        for kind in KINDS {
            let id = IDS[kind as usize].load(Ordering::Acquire);
            match owns(id) {
                Some(false) if id != 0 => {
                    entries.extend_from_slice(&[GOODS | id, 1, u32::MAX, 0]);
                    names.push(name_of(kind));
                }
                Some(true) => log::line(&format!("items: has id={:#x} ({})", GOODS | id, name_of(kind))),
                _ => {}
            }
        }
        if entries.is_empty() {
            return;
        }
        let Some(mim) = read_ptr(module_base() + MAP_ITEM_MAN) else { return };
        let mut buf: Vec<u32> = vec![(entries.len() / 4) as u32];
        buf.extend_from_slice(&entries);
        let grant: extern "C" fn(usize, *const u32, usize, bool, *mut u32, bool) -> u64 =
            unsafe { std::mem::transmute(module_base() + GRANT_RVA) };
        grant(mim, buf.as_ptr(), 0, false, std::ptr::null_mut(), false);
        log::line(&format!("items: gave {names:?} (popup)"));
    }

    fn refresh_stage(&mut self) {
        let stage = match &self.rows {
            Some(None) => STAGE_FAILED,
            None => STAGE_UNKNOWN,
            Some(Some(_)) => {
                let have = [Kind::Host, Kind::Join].iter().all(|&k| owns(IDS[k as usize].load(Ordering::Acquire)) == Some(true));
                if have {
                    STAGE_READY
                } else if self.temple_unlocked() {
                    STAGE_REST
                } else {
                    STAGE_TUTORIAL
                }
            }
        };
        STAGE.store(stage, Ordering::Relaxed);
    }

    fn on_use(&mut self, kind: Kind, now: Instant) {
        // The Bell's second use confirms a resync (resync.rs, 0.7-5 s after the first).
        let resync_confirm = kind == Kind::Host && crate::resync::confirm_pending();
        if !resync_confirm && self.cooldown[kind as usize].is_some_and(|t| now.duration_since(t) < COOLDOWN) {
            return;
        }
        self.cooldown[kind as usize] = Some(now);
        if let Some((open, _)) = self.confirm {
            log::line(&format!("items: used {} while the question for {} is open, ignored", name_of(kind), name_of(open)));
            return;
        }
        // confirm_items = 1: a use that would act asks first, in the game's own message box
        // (dialog.rs); the answer comes in `tick`. No dialog possible: act as before.
        if let Some(question) = self.question(kind) {
            if let Some(ticket) = crate::dialog::ask(question) {
                log::line(&format!("items: used {} -> asking \"{question}\"", name_of(kind)));
                self.confirm = Some((kind, ticket));
                return;
            }
            log::line(&format!("items: used {}: no dialog possible, acting without confirmation", name_of(kind)));
        }
        self.act(kind, false);
    }

    /// The yes/no question for a use that would change the session (`confirm_items = 1`).
    /// None = the setting is off, or the use is refused anyway (its notice says why).
    fn question(&self, kind: Kind) -> Option<&'static str> {
        if !crate::dialog::CONFIRM_ITEMS.load(Ordering::Relaxed) {
            return None;
        }
        let link = net::link();
        let intent = net::intent();
        let connected = matches!(link, Link::HostConnected | Link::ClientConnected);
        let no_password = settings::net().password.is_empty();
        match kind {
            Kind::Host | Kind::Join if no_password => None,
            Kind::Host if connected => Some("Resync the world? Both of you will rest."),
            Kind::Join if connected => None,
            Kind::Host if link == Link::Hosting && intent == Some(Intent::Host) => None,
            Kind::Join if link == Link::Searching && intent == Some(Intent::Join) => None,
            Kind::Host => Some("Open your world to a companion?"),
            Kind::Join => Some("Seek a companion's world and join it?"),
            Kind::Leave if link == Link::Off && !matches!(intent, Some(Intent::Host | Intent::Join)) => None,
            Kind::Leave if matches!(link, Link::HostConnected | Link::Hosting) || intent == Some(Intent::Host) => {
                Some(if session_players() > 2 { "Close your world to your companions?" } else { "Close your world to your companion?" })
            }
            Kind::Leave => Some("Sever the bond and leave the shared world?"),
        }
    }

    /// What a use does (the state is read again: it may have changed while the question was
    /// open). `confirmed`: the yes/no dialog said yes (the Bell's resync needs no second use).
    fn act(&mut self, kind: Kind, confirmed: bool) {
        let link = net::link();
        let intent = net::intent();
        let connected = matches!(link, Link::HostConnected | Link::ClientConnected);
        let text: String = match kind {
            Kind::Host | Kind::Join if settings::net().password.is_empty() => {
                "Set a password in sekiro_coop_settings.ini first.".into()
            }
            // Already connected: the Bell resyncs the world (both players rest), after the
            // dialog's yes, or (no dialog) confirmed by a second use within 5 s.
            Kind::Host if connected && confirmed => {
                crate::resync::request_confirmed();
                log::line(&format!("items: used {} -> resync (dialog yes)", name_of(kind)));
                return;
            }
            Kind::Host if connected => match crate::resync::bell_used() {
                Some(t) => t,
                None => return,
            },
            // 3+ players (PLAN-MULTIPLAYER P4): a full world says so.
            Kind::Join if connected && let Some(t) = full_text(session_players(), crate::tuning::max_players()) => t,
            Kind::Host | Kind::Join if connected => format!("Already in co-op. Use {LEAVE_NAME} to leave first."),
            Kind::Host if link == Link::Hosting && intent == Some(Intent::Host) => "Your world is already open.".into(),
            Kind::Join if link == Link::Searching && intent == Some(Intent::Join) => "Already searching for a companion's world.".into(),
            Kind::Host => {
                net::set_intent(Intent::Host);
                "Your world is open. Waiting for a companion...".into()
            }
            Kind::Join => {
                net::set_intent(Intent::Join);
                "Searching for a companion's world...".into()
            }
            Kind::Leave if link == Link::Off && !matches!(intent, Some(Intent::Host | Intent::Join)) => {
                "You are not in co-op.".into()
            }
            Kind::Leave => {
                net::set_intent(Intent::Solo);
                // A client's Thread leaves alone (the others stay with the host); the
                // host's closes the world for everyone.
                match link {
                    Link::ClientConnected => "You left the shared world.".into(),
                    Link::HostConnected if session_players() > 2 => {
                        "Your world is closed. Your companions return to their own worlds.".into()
                    }
                    Link::HostConnected => "Your world is closed. Your companion returns to their own world.".into(),
                    Link::Hosting => "Your world is closed to companions.".into(),
                    Link::Searching => "Stopped searching.".into(),
                    Link::Off => "Co-op stopped.".into(),
                }
            }
        };
        log::line(&format!("items: used {} -> {text}", name_of(kind)));
        bossrules::notice(text, 5);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let player = game::player();
        // Back at the title (or a save is loading): co-op ends.
        let titles = TITLE_COUNT.load(Ordering::Acquire);
        if titles != self.title_seen {
            self.title_seen = titles;
            self.hold = Some(false);
            self.give_pending = false;
            self.rested_at_temple = false;
            // A question open across the title is forgotten (its late answer is dropped).
            self.confirm = None;
            net::on_title();
        }
        let Some(_) = player else {
            self.in_world_since = None;
            if self.hold == Some(false) {
                self.hold = Some(true);
            }
            return;
        };
        crate::optmenu::set_in_world(true);
        let in_world = now.duration_since(*self.in_world_since.get_or_insert(now));
        if self.hold == Some(true) && in_world >= HOLD_RELEASE_AFTER {
            self.hold = None;
            net::release_title_hold();
        }
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        if self.rows.is_none() && in_world >= Duration::from_secs(1) {
            self.rows = Some(pick_rows());
        }
        if self.temple.is_none() && in_world >= Duration::from_secs(1) {
            self.temple = Some(find_temple());
        }
        let Some(Some(rows)) = &self.rows else { return };
        if self.last_apply.is_none_or(|t| now.duration_since(t) >= APPLY_EVERY) {
            self.last_apply = Some(now);
            apply_rows(rows);
        }

        // Our own rest at an idol (also a mirrored one: harmless, it only gives missing items).
        let rest = read::<i32>(module_base() + REST_NOW).unwrap_or(-1);
        if rest != -1 && rest != self.last_rest {
            let temple = matches!(self.temple, Some(Some(t)) if t.entity == rest);
            log::line(&format!("items: rest at idol {rest} (temple idol {temple}, unlocked {})", self.temple_unlocked()));
            self.give_pending = true;
            self.rested_at_temple |= temple;
        }
        self.last_rest = rest;
        // After the rest menu: give if the tutorial is over (the temple idol is unlocked,
        // or this rest was at it: its first rest may set the flag a moment later).
        if self.give_pending && game::safe_for_reward(in_world).is_ok() {
            self.give_pending = false;
            if self.temple_unlocked() || std::mem::take(&mut self.rested_at_temple) {
                self.give_missing();
            } else {
                log::line("items: temple idol not unlocked yet (tutorial), no items");
            }
        }

        let used = USE_PENDING.swap(0, Ordering::AcqRel);
        for kind in KINDS {
            if used & (1 << kind as u8) != 0 {
                self.on_use(kind, now);
            }
        }
        // The answer to a co-op item's yes/no question (dialog.rs).
        if let Some((kind, ticket)) = self.confirm
            && let Some(answer) = crate::dialog::answer(ticket)
        {
            self.confirm = None;
            match answer {
                crate::dialog::Answer::Yes => {
                    log::line(&format!("items: {} confirmed (Yes)", name_of(kind)));
                    self.act(kind, true);
                }
                crate::dialog::Answer::No => log::line(&format!("items: {} declined (No), nothing done", name_of(kind))),
                crate::dialog::Answer::Unavailable(why) => {
                    log::line(&format!("items: {}: the dialog could not be shown ({why}), acting as before", name_of(kind)));
                    self.act(kind, false);
                }
            }
        }

        if self.last_check.is_none_or(|t| now.duration_since(t) >= CHECK_EVERY) {
            self.last_check = Some(now);
            self.refresh_stage();
        }
    }
}

/// Once per frame on the game thread, before the rest of the mod.
pub fn on_frame() {
    // The game's own message box (dialog.rs): open / poll ours before the items read answers.
    crate::dialog::tick();
    let Ok(mut guard) = STATE.lock() else { return };
    let state = guard.get_or_insert_with(|| State { last_rest: -1, title_seen: TITLE_COUNT.load(Ordering::Acquire), ..Default::default() });
    state.tick();
}

#[cfg(test)]
mod full_tests {
    use super::*;

    #[test]
    fn the_template_is_found_by_id_in_any_language() {
        assert_eq!(template_rank(3020, "\u{4e38}\u{836f}"), Some(0), "translated name");
        assert_eq!(template_rank(3020, "Pellet"), Some(0));
        assert_eq!(template_rank(61, "\u{4e38}\u{836f}"), None, "other rows only by English name");
        assert_eq!(template_rank(7, "Ako's Sugar"), Some(2));
    }

    #[test]
    fn charm_full_text_only_with_room_for_three_or_more() {
        assert_eq!(full_text(2, 2), None, "two players: the old text");
        assert_eq!(full_text(3, 4), None);
        assert_eq!(full_text(4, 4).as_deref(), Some("This world is full (4/4)."));
        assert_eq!(full_text(6, 6).as_deref(), Some("This world is full (6/6)."));
    }
}
