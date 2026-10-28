//! Script-driven item grants reach the friend, and a key-item check on connect
//! (docs/IMPLEMENTATION-NOTES.md §23, NOT verified live). Setting `share_key_items`
//! ([GAMEPLAY], default 1).
//!
//! Quest / world flags are shared, inventories are not. An item a talk script or an
//! event gives (Healing Gourd from Emma, the Mortal Blade from the Divine Child, the
//! esoteric texts, the Lotus / Shelter Stone / Mibu items, prosthetic tools from the
//! Sculptor, Prayer Beads / Gourd Seeds from minibosses...) is given once, in the world
//! of the player who triggered it; the friend's NPC then has the shared "already given"
//! flags and never gives it again: a soft-lock for the friend.
//!
//! IDA (copy `F:\Temp\ida_grant\db.i64`, outputs `o1`..`o5`):
//! - `0x918810 AwardItemLot(MapItemMan*, lot, bool)` is only reached from script code:
//!   `0x689FF0` (EMEVD 2003[4] = case 4 of the EMEVD dispatcher `0x904EF0`, ESD talk command
//!   104 in `0x11C63A0`, the Lua binding registered at `0x8FBB0`), `0x68A010` (EMEVD case 36),
//!   `0x68A030` / `0x688360` (lot of an event chr; `0x688360` also gives the boss experience),
//!   `0x68D240` (one fixed lot). It runs the lottery, calls GrantItems once per result
//!   (`0x918A69`) and sets the lot's get flag (`0x6AAAB0`).
//! - `0x91C970 GrantItems(MapItemMan*, ItemBuffer*, void*, char silent, u32* out, bool)`:
//!   the third argument is never read. Its other callers are not scripts: `0x918550`
//!   (OnGoodsUsed `use_get_item` and the lot variant `0x918690` from `0x7C0EF0` / `0x918480`),
//!   `0x91C6E0` (MapItemMan frame update `0x91A440`), `0x91D620` / `0x91FCF0` / `0x91FF30`
//!   (world pickups / treasure from `0x668CC0`, `0x920D20`, `0x923190`). Talk gives are
//!   AwardItemLot too (ESD command 104), so the replay hooks AwardItemLot (bossrules.rs) and
//!   reads the item buffer of the GrantItems call nested in it. World pickups, shops and our
//!   co-op items never go through AwardItemLot.
//! - Buffer: `{u32 count; count x {u32 fullId, u32 num, i32 -1, u32 popup}}`, AwardItemLot
//!   writes popup = 2 (`0x84E7E0`: dialog unless the goods' `first_get_event_flag_id` (row
//!   `+0x7C`) is on; that function only reads the flag, nothing in the grant path sets it).
//!
//! Rule (`classify` / `shareable`): weapons (prosthetic tools), protectors, accessories and
//! every goods row that is not consumed on use (key items, memories, esoteric texts, Prayer
//! Beads, Gourd Seeds, quest / upgrade materials) always; consumables (`is_consume`) only from
//! a one-time lot (the award switched on one of the per-player item-lot flags); never our
//! co-op items. Boss lots inside the 15 s after HandleBossDefeat stay with the boss reward
//! (bossrules.rs) while `boss_reward = 1`.
//!
//! Receiver: 5 s later, when no menu / talk / death blocks it (`game::safe_for_reward`),
//! skipped when all of the lot's flags are already on here; unique items (not goods, or
//! goods with `max_num <= 1`) already owned are left out; the rest through GrantItems with
//! the sender's own entries (popup), then the lot flags are set here (per player, never
//! sent). A lot given that way is remembered for 120 s: our own AwardItemLot of the same lot
//! (our world's script ran too) is then skipped, so nothing arrives twice. Our own GrantItems
//! call is not inside an AwardItemLot, so it is not captured again (no loop).
//!
//! Doctor: on every connection each side sends its owned key items (named goods, not
//! consumed on use, `max_num <= 1`). Differences are logged and shown ("You are missing:
//! ..." / "<friend> is missing: ..."). Only repair: an item the friend owns that we lack,
//! whose `first_get_event_flag_id` is a shared flag that is on in our world, and that no
//! item lot of ours ever gave us (all its per-player lot flags off) is given once (+ its
//! first lot flag set here, so a later doctor never gives it again).
//!
//! Enemy drops (`Drops 0x9FFF50`, detoured): miniboss Prayer Beads / Gourd Seeds are NpcParam
//! item lots dropped at the corpse, not AwardItemLot. For our own loot-step call (the
//! friend's kill) the item lot is dropped at the corpse even when the game's kill record
//! (`+0xC8` of the chr's modules: `+21 == 0 && +108 != -1`) would skip it. Boss / miniboss
//! drops (NpcParam deathblows >= 2) are announced to the friend (`BULK_BOSS_DROP`); a friend
//! whose own drop of that enemy did not happen within 15 s gets the lot's sure goods with
//! the normal pickup log, once (lot flags).
//!
//! Reward reconcile (docs/IMPLEMENTATION-NOTES.md §33, NOT verified live):
//! - Pending queue: a captured one-time grant (per-player lot flags switched on) is kept in
//!   `%LOCALAPPDATA%\sekiro-coop\pending_grants.txt` until the friend acks it
//!   (`BULK_GRANT_ACK`); a grant made while nobody was connected is sent on the next
//!   connect. Before sending, a queued grant must still be ours: its lot flags on here and
//!   every entry an item of that lot in our own ItemLotParam.
//! - Ledger: on every connect and after every rest each side sends the one-time lot rows
//!   whose flag is on here (`BULK_LOT_LEDGER`, only rows `classify_lot` calls script / boss
//!   or enemy drop, with one sure goods slot). A row on at the friend and off here is given
//!   once (its sure goods through GrantItems, its flag set): `reconcile: got lot L ...`.
//! - Doubles: a friend's lot whose flags are already on counts as owned only when our own
//!   award of it ran or we own every unique item of it (`owned_check`); boss lots of the
//!   friend are skipped when our own AwardItemLot of the same lot ran (`boss_lot_check`).

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fromsoftware_shared::FromStatic;
use sekiro::param::{EQUIP_PARAM_GOODS_ST, ITEMLOT_PARAM_ST, NPC_PARAM_ST, ParamDef, SHOP_LINEUP_PARAM};
use sekiro::sprj::SoloParamRepository;

use crate::game::{self, module_base, read, read_ptr};
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::{bossrules, hook, items, log, tuning, world};

/// Settings file `share_key_items` ([GAMEPLAY], default 1).
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// `[GAMEPLAY] reward_reconcile` (v7.7 §33): the lot ledger and the queued grants on
/// connect / after a rest. 0 = off (v7.6). Default 1.
pub static RECONCILE: AtomicBool = AtomicBool::new(true);

/// Bulk kind: one script grant of the sender (`encode_grant`). (30 is doctor.rs, 0x4E
/// nsession.rs; an older build leaves these unread.)
pub const BULK_ITEM_GRANT: u8 = 41;
/// Bulk kind: the sender's owned key items (`encode_keys`), once per connection.
pub const BULK_KEY_ITEMS: u8 = 42;
/// Bulk kind: a boss / miniboss item drop at its corpse on the sender (`{u8 1, i32 entity, i32 lot}`).
pub const BULK_BOSS_DROP: u8 = 43;
/// Bulk kind: the event ids of the friend's grants we handled (`encode_ack`). A v7.6 peer
/// leaves it unread in its inbox (dropped on the next link, at most 24 kept).
pub const BULK_GRANT_ACK: u8 = 44;
/// Bulk kind: our one-time lot rows that are on (`encode_ledger`), on connect and after a
/// rest. A v7.6 peer leaves it unread like 44.
pub const BULK_LOT_LEDGER: u8 = 45;

/// Queued one-time grants kept at most (oldest dropped).
const MAX_PENDING: usize = 128;
const MAX_LEDGER: usize = 8192;
/// Reconcile rows given in one pass at most (the rest come with the next ledger).
const MAX_RECONCILE: usize = 64;
/// Our ledger goes out this long after a rest (its flag burst settles).
const LEDGER_AFTER_REST: Duration = Duration::from_secs(5);
/// The friend's ledger is applied this long after it arrived (queued grants land first).
const LEDGER_APPLY_AFTER: Duration = Duration::from_secs(8);
/// Our own AwardItemLot calls are remembered this long.
const LOCAL_AWARD_KEEP_MS: u64 = 600_000;
/// A friend's boss lot is ours already when our own award of it ran this recently.
pub(crate) const BOSS_LOCAL_AWARD_MS: u64 = 600_000;
/// ItemLotParam ids from here up are map lots (`AAxxxxx` per map: treasure, chests,
/// corpses) or, from 10 000 000, enemy lots (only those NpcParam refers to are drops).
const MAP_LOT_MIN: i32 = 1_000_000;
/// One-time lots below `MAP_LOT_MIN` that are map treasure (thefifthmatt's randomizer
/// `annotations.txt`): Iron Fortress (61400), Lapis Lazuli (63610, 63660).
const TREASURE_LOTS: [i32; 3] = [61400, 63610, 63660];

/// `void Drops(ChrIns*)`: the death drop of an enemy (game death path, and enemies.rs
/// for the friend's kills): resource lots at the corpse (`0x918D40`, NpcParam `+720/+772`),
/// then the item lot (`0x918480(MapItemMan, entity, NpcParamRef*)` -> `0x918090`, NpcParam
/// `item_lot_id_1 +44`) unless `0xB5E3A0(*(*(chr+0x1FF8)+0xC8))` (`+21 == 0 && +108 != -1`).
const DROPS_RVA: usize = 0x9FFF50;
// mov rax, rsp; push rdi; sub rsp, 0A0h (no RIP-relative code)
const DROPS_PROLOGUE: [u8; 11] = [0x48, 0x8B, 0xC4, 0x57, 0x48, 0x81, 0xEC, 0xA0, 0x00, 0x00, 0x00];
const ITEM_DROP_RVA: usize = 0x918480;
/// ChrIns vtable slot 47 (`+0x178`): the NpcParam ref `{.., +8 row}` (first call in `Drops`).
const CHR_NPC_PARAM_SLOT: usize = 0x178;
const NPC_ITEM_LOT_1: usize = 44;
/// NpcParam deathblow count (scaling.rs): >= 2 = miniboss / boss.
const NPC_DEATHBLOWS: usize = 0x126;
/// `+0x1A12`: bit 0 = dropped already, bit 4 = no drop (`0x9F2DE0`).
const CHR_DROP_BITS: usize = 0x1A12;
const CHR_MODULES: usize = 0x1FF8;
const MODULE_KILL: usize = 0xC8;
/// The friend's boss drop: our own drop has this long to show up first.
const DROP_APPLY_AFTER: Duration = Duration::from_secs(15);
const OWN_DROP_KEEP: Duration = Duration::from_secs(300);

const GRANT_RVA: usize = 0x91C970;
// mov rax, rsp; mov [rax+20h], r9b (no RIP-relative code)
const GRANT_PROLOGUE: [u8; 7] = [0x48, 0x8B, 0xC4, 0x44, 0x88, 0x48, 0x20];
const MAP_ITEM_MAN: usize = 0x3D6CDC0;
/// `i32 FindItem(EquipGameData*, u32* fullId)`: inventory index, < 0 = not owned (items.rs).
const FIND_ITEM_RVA: usize = 0x79D7E0;
const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
const PGD_EQUIP_GAME_DATA: usize = 0x278;
const SET_EVENT_FLAG_RVA: usize = 0x6C4520;

const CAT_WEAPON: u32 = 0;
const CAT_PROTECTOR: u32 = 1;
const CAT_ACCESSORY: u32 = 2;
const CAT_GOODS: u32 = 4;
const GOODS: u32 = 0x4000_0000;
/// AwardItemLot's popup mode for a GrantItems entry (dialog unless first-get flag on).
const POPUP_DIALOG: u32 = 2;

const MAX_ENTRIES: usize = 16;
const MAX_FLAGS: usize = 32;
const MAX_KEYS: usize = 2048;
const MAX_CAPTURED: usize = 64;
/// The receiver waits this long: its own script may give the item first.
const APPLY_AFTER: Duration = Duration::from_secs(5);
/// A lot given from the friend: our own AwardItemLot of it this soon after is a double.
const REPLAYED_FOR_MS: u64 = 120_000;
/// Doctor: in the world this long, and connected this long, before our list goes out.
const DOCTOR_IN_WORLD: Duration = Duration::from_secs(10);
const DOCTOR_AFTER_CONNECT: Duration = Duration::from_secs(5);

static GRANT_TRAMP: AtomicUsize = AtomicUsize::new(0);
/// AwardItemLot calls seen (the boss window's included), for the stats line.
static AWARDS_SEEN: AtomicU32 = AtomicU32::new(0);
/// `game::now_ms` when a table boss's defeat flag (bosses.rs) last turned on here (0 = never).
static DEFEAT_FLAG_AT_MS: AtomicU64 = AtomicU64::new(0);
/// A lot given this long after the defeat flag turned on is the boss's (2026-10-24 [ARV]
/// Gyoubu: the friend's lot came ~6 s before his HBD, our synced 9301 had started the reward
/// event first).
const DEFEAT_FLAG_WINDOW_MS: u64 = 15_000;
static LOCAL_SKIPPED: AtomicU32 = AtomicU32::new(0);
static CAPTURED: Mutex<Vec<Captured>> = Mutex::new(Vec::new());
/// (lot, `game::now_ms`) of lots given from the friend.
static REPLAYED: Mutex<Vec<(i32, u64)>> = Mutex::new(Vec::new());
/// (lot, `game::now_ms`) of our own AwardItemLot calls that gave something.
static LOCAL_AWARDS: Mutex<Vec<(i32, u64)>> = Mutex::new(Vec::new());
static DROPS_TRAMP: AtomicUsize = AtomicUsize::new(0);
/// Boss / miniboss drops (and the item drops we forced) seen by the detour.
static DROPS_SEEN: Mutex<Vec<DropSeen>> = Mutex::new(Vec::new());
static FORCED_DROPS: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug)]
struct DropSeen {
    entity: i32,
    lot: i32,
    deathblows: i8,
    /// The game's item-lot branch ran (or we ran it).
    item_drop: bool,
    /// Our own call (enemies.rs loot step for the friend's kill), not the game's.
    ours: bool,
    /// The game would have skipped the item lot (kill record `+21`, `+108`); we dropped it.
    forced: bool,
    kill21: u8,
    kill108: i32,
}

/// `sekiro.exe` image range (PE `SizeOfImage`), for "who called".
fn in_game_image(addr: usize) -> bool {
    let base = module_base();
    let size = read::<u32>(base + 0x3C).and_then(|nt| read::<u32>(base + nt as usize + 0x50)).unwrap_or(0) as usize;
    addr >= base && addr < base + size
}

/// `0x9FFF50(chr)`: an enemy's death drop. For our own calls (the friend's kill, enemies.rs)
/// the item lot is dropped at the corpse even when the game would skip it because its kill
/// record names another killer (live 2026-10-23: the friend killed General Naomori; on the
/// host our loot step ran, the resource drop came, the Prayer Bead / Gourd Seed lot did not).
extern "C" fn drops_detour(chr: usize) {
    let t = loop {
        match DROPS_TRAMP.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    };
    let ours = !in_game_image(hook::detour_caller(t));
    let before = inspect_drop(chr);
    let original: extern "C" fn(usize) = unsafe { std::mem::transmute(t) };
    original(chr);
    let Some((mut seen, param_ref)) = before else { return };
    seen.ours = ours;
    let gate_only = !seen.item_drop && seen.kill21 == 0 && seen.kill108 != -1;
    if ours && gate_only && let Some(mim) = read_ptr(module_base() + MAP_ITEM_MAN) {
        let drop: extern "C" fn(usize, u32, usize) = unsafe { std::mem::transmute(module_base() + ITEM_DROP_RVA) };
        drop(mim, seen.entity as u32, param_ref);
        seen.forced = true;
        seen.item_drop = true;
        FORCED_DROPS.fetch_add(1, Ordering::Relaxed);
    }
    if (seen.deathblows >= 2 || seen.forced)
        && let Ok(mut q) = DROPS_SEEN.lock()
        && q.len() < MAX_CAPTURED
    {
        q.push(seen);
    }
}

/// What `Drops` will do for `chr` (read before it runs: it sets the drop bits).
fn inspect_drop(chr: usize) -> Option<(DropSeen, usize)> {
    let getter = read_ptr(read_ptr(chr)? + CHR_NPC_PARAM_SLOT)?;
    if !in_game_image(getter) {
        return None;
    }
    let get: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(getter) };
    let param_ref = get(chr);
    let row = read_ptr(param_ref + 8).filter(|&r| r != 0)?;
    let bits = read::<u8>(chr + CHR_DROP_BITS)?;
    let kill = read_ptr(read_ptr(chr + CHR_MODULES)? + MODULE_KILL)?;
    let kill21 = read::<u8>(kill + 21)?;
    let kill108 = read::<i32>(kill + 108)?;
    let seen = DropSeen {
        entity: read::<i32>(chr + 8)?,
        lot: read::<i32>(row + NPC_ITEM_LOT_1)?,
        deathblows: read::<i8>(row + NPC_DEATHBLOWS)?,
        item_drop: bits & 0x11 == 0 && !(kill21 == 0 && kill108 != -1),
        ours: false,
        forced: false,
        kill21,
        kill108,
    };
    (bits & 0x11 == 0).then_some((seen, param_ref))
}

/// The goods an item lot surely gives (rows `lot`, `lot+1`.. while they exist, each row only
/// when one slot holds all of its weight), with the per-player flag of each: (entry, flag).
fn sure_goods_of_lot(lot: i32) -> Vec<([u32; 4], u32)> {
    let mut out = Vec::new();
    let Some(file) = lots_file() else { return out };
    for id in lot.max(1)..lot.max(1).saturating_add(8) {
        let Some(r) = (unsafe { file.get_row_by_id::<ITEMLOT_PARAM_ST>(id as u32) }) else { break };
        match sure_slot(&slots_of(r), |i| items::goods_name(i).is_some()) {
            Ok((item, num, flag)) => {
                out.push(([GOODS | item, num, u32::MAX, 0], primary_flag(flag, r.get_item_flag_id())));
            }
            Err(why) => log::line(&format!("reward: lot row {id}: {why}, skipped")),
        }
    }
    out
}

fn lots_file() -> Option<&'static sekiro::fd4::ParamFile> {
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    repo.params().find(|p| p.struct_name() == ITEMLOT_PARAM_ST::NAME).map(|p| &*p.data)
}

/// One ItemLotParam slot: (item id, category, weight, count, slot get flag).
type Slot = (i32, i32, u16, u16, i32);

fn slots_of(r: &ITEMLOT_PARAM_ST) -> [Slot; 8] {
    [
        (r.lot_item_id01(), r.lot_item_category01(), r.lot_item_base_point01(), r.lot_item_num01(), r.get_item_flag_id01()),
        (r.lot_item_id02(), r.lot_item_category02(), r.lot_item_base_point02(), r.lot_item_num02(), r.get_item_flag_id02()),
        (r.lot_item_id03(), r.lot_item_category03(), r.lot_item_base_point03(), r.lot_item_num03(), r.get_item_flag_id03()),
        (r.lot_item_id04(), r.lot_item_category04(), r.lot_item_base_point04(), r.lot_item_num04(), r.get_item_flag_id04()),
        (r.lot_item_id05(), r.lot_item_category05(), r.lot_item_base_point05(), r.lot_item_num05(), r.get_item_flag_id05()),
        (r.lot_item_id06(), r.lot_item_category06(), r.lot_item_base_point06(), r.lot_item_num06(), r.get_item_flag_id06()),
        (r.lot_item_id07(), r.lot_item_category07(), r.lot_item_base_point07(), r.lot_item_num07(), r.get_item_flag_id07()),
        (r.lot_item_id08(), r.lot_item_category08(), r.lot_item_base_point08(), r.lot_item_num08(), r.get_item_flag_id08()),
    ]
}

/// The one goods item a lot row surely gives (a single slot holds all of the weight):
/// `(goods id, count, slot flag)`, or why not. Goods by category (3, 4 or a full
/// `0x4000_0000` form) or, failing that, a named goods row (`named_goods`).
fn sure_slot(slots: &[Slot; 8], named_goods: impl Fn(u32) -> bool) -> Result<(u32, u32, i32), String> {
    let total: u32 = slots.iter().map(|s| u32::from(s.2)).sum();
    let real: Vec<&Slot> = slots.iter().filter(|s| s.0 > 0 && s.2 > 0).collect();
    let [slot] = real.as_slice() else {
        return Err(format!("{} possible items, not a sure drop", real.len()));
    };
    let (item, cat, point, num, flag) = **slot;
    let goods = matches!(cat, 3 | 4) || (cat as u32) >> 28 == CAT_GOODS || named_goods(item as u32);
    if u32::from(point) != total || !goods {
        return Err(format!("item {item} category {cat} weight {point}/{total}, not a sure goods drop"));
    }
    Ok((item as u32, u32::from(num.max(1)), flag))
}

/// The flag the game sets for a sure row: the slot's own, else the row's.
fn primary_flag(slot: i32, row: i32) -> u32 {
    if slot > 0 { slot as u32 } else { row.max(0) as u32 }
}

thread_local! {
    /// Some while an AwardItemLot runs on this thread: the GrantItems entries it made.
    static IN_AWARD: RefCell<Option<Vec<[u32; 4]>>> = const { RefCell::new(None) };
}

/// One AwardItemLot of ours, as the detours saw it.
struct Captured {
    lot: i32,
    entries: Vec<[u32; 4]>,
    /// Flags switched on during the call.
    flags: Vec<u32>,
    /// Within the boss-reward window after HandleBossDefeat.
    boss_window: bool,
}

/// One grant on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub id: u32,
    pub lot: i32,
    /// Every entry the lot gave is in `entries` (none was left out by the rule).
    pub complete: bool,
    /// Raw GrantItems entries `{fullId, num, -1, popup}`.
    pub entries: Vec<[u32; 4]>,
    /// The per-player item-lot flags the award switched on.
    pub flags: Vec<u32>,
}

/// `BULK_ITEM_GRANT` body: `{u8 1, u32 id, i32 lot, u8 complete, u8 n, n x 4 u32, u8 m, m x u32}`.
pub fn encode_grant(g: &Grant) -> Vec<u8> {
    let entries = &g.entries[..g.entries.len().min(MAX_ENTRIES)];
    let flags = &g.flags[..g.flags.len().min(MAX_FLAGS)];
    let mut out = vec![1u8];
    out.extend(g.id.to_le_bytes());
    out.extend(g.lot.to_le_bytes());
    out.push(u8::from(g.complete));
    out.push(entries.len() as u8);
    for e in entries {
        for v in e {
            out.extend(v.to_le_bytes());
        }
    }
    out.push(flags.len() as u8);
    for f in flags {
        out.extend(f.to_le_bytes());
    }
    out
}

fn take<'a>(d: &'a [u8], p: &mut usize, n: usize) -> Option<&'a [u8]> {
    let s = d.get(*p..p.checked_add(n)?)?;
    *p += n;
    Some(s)
}

fn u32_at(d: &[u8], p: &mut usize) -> Option<u32> {
    Some(u32::from_le_bytes(take(d, p, 4)?.try_into().ok()?))
}

pub fn decode_grant(d: &[u8]) -> Option<Grant> {
    let mut p = 0;
    if take(d, &mut p, 1)? != [1] {
        return None;
    }
    let id = u32_at(d, &mut p)?;
    let lot = u32_at(d, &mut p)? as i32;
    let complete = take(d, &mut p, 1)?[0] != 0;
    let n = take(d, &mut p, 1)?[0] as usize;
    if n > MAX_ENTRIES {
        return None;
    }
    let mut entries = Vec::with_capacity(n);
    for _ in 0..n {
        entries.push([u32_at(d, &mut p)?, u32_at(d, &mut p)?, u32_at(d, &mut p)?, u32_at(d, &mut p)?]);
    }
    let m = take(d, &mut p, 1)?[0] as usize;
    if m > MAX_FLAGS {
        return None;
    }
    let flags = (0..m).map(|_| u32_at(d, &mut p)).collect::<Option<Vec<u32>>>()?;
    (p == d.len()).then_some(Grant { id, lot, complete, entries, flags })
}

/// `BULK_KEY_ITEMS` body: `{u8 1, u8 is_host, u16 n, n x u32 goods id}`.
pub fn encode_keys(is_host: bool, ids: &[u32]) -> Vec<u8> {
    let ids = &ids[..ids.len().min(MAX_KEYS)];
    let mut out = vec![1u8, u8::from(is_host)];
    out.extend((ids.len() as u16).to_le_bytes());
    for i in ids {
        out.extend(i.to_le_bytes());
    }
    out
}

pub fn decode_keys(d: &[u8]) -> Option<(bool, Vec<u32>)> {
    let mut p = 0;
    if take(d, &mut p, 1)? != [1] {
        return None;
    }
    let host = take(d, &mut p, 1)?[0] != 0;
    let n = u16::from_le_bytes(take(d, &mut p, 2)?.try_into().ok()?) as usize;
    if n > MAX_KEYS {
        return None;
    }
    let ids = (0..n).map(|_| u32_at(d, &mut p)).collect::<Option<Vec<u32>>>()?;
    (p == d.len()).then_some((host, ids))
}

/// `BULK_GRANT_ACK` body: `{u8 1, u16 n, n x u32 event id}`.
pub fn encode_ack(ids: &[u32]) -> Vec<u8> {
    let ids = &ids[..ids.len().min(MAX_PENDING)];
    let mut out = vec![1u8];
    out.extend((ids.len() as u16).to_le_bytes());
    for i in ids {
        out.extend(i.to_le_bytes());
    }
    out
}

pub fn decode_ack(d: &[u8]) -> Option<Vec<u32>> {
    let mut p = 0;
    if take(d, &mut p, 1)? != [1] {
        return None;
    }
    let n = u16::from_le_bytes(take(d, &mut p, 2)?.try_into().ok()?) as usize;
    if n > MAX_PENDING {
        return None;
    }
    let ids = (0..n).map(|_| u32_at(d, &mut p)).collect::<Option<Vec<u32>>>()?;
    (p == d.len()).then_some(ids)
}

/// Why a ledger went out.
pub const LEDGER_CONNECT: u8 = 0;
pub const LEDGER_REST: u8 = 1;

/// `BULK_LOT_LEDGER` body: `{u8 1, u8 reason, u16 n, n x i32 lot row}`.
pub fn encode_ledger(reason: u8, rows: &[i32]) -> Vec<u8> {
    let rows = &rows[..rows.len().min(MAX_LEDGER)];
    let mut out = vec![1u8, reason];
    out.extend((rows.len() as u16).to_le_bytes());
    for r in rows {
        out.extend(r.to_le_bytes());
    }
    out
}

pub fn decode_ledger(d: &[u8]) -> Option<(u8, Vec<i32>)> {
    let mut p = 0;
    if take(d, &mut p, 1)? != [1] {
        return None;
    }
    let reason = take(d, &mut p, 1)?[0];
    let n = u16::from_le_bytes(take(d, &mut p, 2)?.try_into().ok()?) as usize;
    if n > MAX_LEDGER {
        return None;
    }
    let rows = (0..n).map(|_| u32_at(d, &mut p).map(|v| v as i32)).collect::<Option<Vec<i32>>>()?;
    (p == d.len()).then_some((reason, rows))
}

/// Ledger diff: the friend's rows (`theirs`) that we know as reconcilable (`flag_of`: the
/// row's lot flag, None = not a reconcile row here) and whose flag is off here. Rows that
/// share a flag (one gift, several lots: Young Lord's Bell Charm 51600 / 61600, flag
/// 50006160) are given once: the first in the friend's order wins.
pub fn ledger_plan(theirs: &[i32], flag_of: impl Fn(i32) -> Option<u32>, on_here: impl Fn(u32) -> bool) -> Vec<i32> {
    let mut taken: HashSet<u32> = HashSet::new();
    let mut out = Vec::new();
    for &row in theirs {
        let Some(flag) = flag_of(row).filter(|&f| f > 0) else { continue };
        if on_here(flag) || !taken.insert(flag) {
            continue;
        }
        out.push(row);
    }
    out
}

/// Where an item lot row comes from, by what the game data says of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LotKind {
    /// Below 1 000 000 and not known map treasure: EMEVD / talk (ESD) / boss lots
    /// (AwardItemLot), e.g. 2000 Mechanical Barrel (Gyoubu), 61700 Flame Barrel Memo.
    ScriptOrBoss,
    /// NpcParam `item_lot_id_1..6` (or the rows after it), e.g. 13604000 Herb Catalogue Scrap.
    EnemyDrop,
    /// Map lots (1 000 000 up, not an enemy's) and `TREASURE_LOTS`: per player by design.
    MapTreasure,
    /// Its flag is a ShopLineupParam flag too: per player by design.
    Shop,
    /// No lot flag: a repeatable lot, can never be told apart from a new one.
    Repeatable,
}

/// `row` = ItemLotParam id, `enemy` = an NpcParam item lot refers to it, `flag` = its lot
/// flag (0 = none), `shop_flag` = that flag is also a shop row's.
pub fn classify_lot(row: i32, enemy: bool, flag: u32, shop_flag: bool) -> LotKind {
    if flag == 0 {
        LotKind::Repeatable
    } else if shop_flag {
        LotKind::Shop
    } else if enemy {
        LotKind::EnemyDrop
    } else if row > 0 && row < MAP_LOT_MIN && !TREASURE_LOTS.contains(&row) {
        LotKind::ScriptOrBoss
    } else {
        LotKind::MapTreasure
    }
}

pub fn reconcilable(k: LotKind) -> bool {
    matches!(k, LotKind::ScriptOrBoss | LotKind::EnemyDrop)
}

// --- Pending queue file ----------------------------------------------------------

const PENDING_HEADER: &str = "# Co-op mod (keyitems.rs): one-time item grants of ours the friend has not acked yet.\n# event-id lot complete entries(fullId:num:x:popup,...) lot-flags(,...)\n";

/// One queue line: `id lot complete full:num:x:popup,... flag,...`.
pub fn pending_line(g: &Grant) -> String {
    let entries: Vec<String> = g.entries.iter().map(|e| format!("{}:{}:{}:{}", e[0], e[1], e[2], e[3])).collect();
    let flags: Vec<String> = g.flags.iter().map(u32::to_string).collect();
    format!("{} {} {} {} {}", g.id, g.lot, u8::from(g.complete), entries.join(","), flags.join(","))
}

fn parse_pending_line(line: &str) -> Option<Grant> {
    let mut it = line.split_whitespace();
    let id = it.next()?.parse().ok()?;
    let lot = it.next()?.parse().ok()?;
    let complete = match it.next()? {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    let entries = it
        .next()?
        .split(',')
        .map(|e| {
            let v: Vec<u32> = e.split(':').map(|x| x.parse().ok()).collect::<Option<Vec<u32>>>()?;
            <[u32; 4]>::try_from(v).ok()
        })
        .collect::<Option<Vec<[u32; 4]>>>()?;
    let flags = it.next()?.split(',').map(|f| f.parse().ok()).collect::<Option<Vec<u32>>>()?;
    let ok = it.next().is_none()
        && id != 0
        && lot > 0
        && !entries.is_empty()
        && entries.len() <= MAX_ENTRIES
        && !flags.is_empty()
        && flags.len() <= MAX_FLAGS
        && flags.iter().all(|&f| f > 0);
    ok.then_some(Grant { id, lot, complete, entries, flags })
}

/// The queue file's grants (comments, unreadable lines and repeats of a lot left out).
pub fn parse_pending(text: &str) -> Vec<Grant> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if let Some(g) = parse_pending_line(line) {
            pending_add(&mut out, g);
        }
    }
    out
}

/// Adds a one-time grant unless that lot (or event) is queued already; keeps the newest
/// `MAX_PENDING`. True = added.
pub fn pending_add(list: &mut Vec<Grant>, g: Grant) -> bool {
    if g.flags.is_empty() || list.iter().any(|q| q.lot == g.lot || q.id == g.id) {
        return false;
    }
    if list.len() >= MAX_PENDING {
        list.remove(0);
    }
    list.push(g);
    true
}

/// Drops the acked events; the grants removed.
pub fn pending_remove(list: &mut Vec<Grant>, acked: &[u32]) -> Vec<Grant> {
    let (gone, kept): (Vec<Grant>, Vec<Grant>) = std::mem::take(list).into_iter().partition(|g| acked.contains(&g.id));
    *list = kept;
    gone
}

fn pending_path() -> std::path::PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop").join("pending_grants.txt")
}

fn load_pending_from(path: &std::path::Path) -> Vec<Grant> {
    std::fs::read_to_string(path).map(|t| parse_pending(&t)).unwrap_or_default()
}

/// One new line at the end (header first when the file is new).
fn append_pending_to(path: &std::path::Path, g: &Grant) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if new {
        f.write_all(PENDING_HEADER.as_bytes())?;
    }
    writeln!(f, "{}", pending_line(g))
}

/// The whole queue (temp file + rename); no file when it is empty.
fn save_pending_to(path: &std::path::Path, list: &[Grant]) -> std::io::Result<()> {
    if list.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut s = PENDING_HEADER.to_owned();
    for g in list {
        s += &pending_line(g);
        s.push('\n');
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, s)?;
    std::fs::rename(&tmp, path)
}

/// Queued grants every present peer acked (none present: none, the queue waits).
pub fn fully_acked(acked_by: &HashMap<u32, BTreeSet<PeerId>>, present: &[PeerId]) -> Vec<u32> {
    if present.is_empty() {
        return Vec::new();
    }
    let mut ids: Vec<u32> = acked_by.iter().filter(|(_, by)| present.iter().all(|p| by.contains(p))).map(|(&id, _)| id).collect();
    ids.sort_unstable();
    ids
}

/// What the rule needs of a goods row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GoodsInfo {
    pub consume: bool,
    pub max_num: i16,
    pub goods_type: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Share {
    Always,
    /// Consumables: only from a one-time lot.
    OneTime,
    Never,
}

/// Rot Essences (goods 9500..=9526): non-consumed key items that show a player's own
/// dragonrot on NPCs. Never shared, else a client's deaths could hand them to the host
/// (2026-10-24). The Dragonrot Blood Sample (9560) is not one of them and stays shareable.
pub const ROT_ESSENCES: std::ops::RangeInclusive<u32> = 9500..=9526;

/// The sharing rule for one item (`full` = category << 28 | id).
pub fn classify(full: u32, goods: Option<GoodsInfo>, coop: bool) -> Share {
    if coop || full == u32::MAX {
        return Share::Never;
    }
    if full >> 28 == CAT_GOODS && ROT_ESSENCES.contains(&(full & 0x0FFF_FFFF)) {
        return Share::Never;
    }
    match full >> 28 {
        CAT_WEAPON | CAT_PROTECTOR | CAT_ACCESSORY => Share::Always,
        CAT_GOODS => match goods {
            None => Share::Never,
            Some(g) if !g.consume => Share::Always,
            Some(_) => Share::OneTime,
        },
        _ => Share::Never,
    }
}

pub fn shareable(s: Share, one_time_lot: bool) -> bool {
    match s {
        Share::Always => true,
        Share::OneTime => one_time_lot,
        Share::Never => false,
    }
}

/// An item of which one copy is enough: owned = nothing to give.
pub fn unique(full: u32, goods: Option<GoodsInfo>) -> bool {
    full >> 28 != CAT_GOODS || goods.is_some_and(|g| g.max_num <= 1)
}

fn goods_file(repo: &SoloParamRepository) -> Option<&sekiro::fd4::ParamFile> {
    repo.params().find(|p| p.struct_name() == EQUIP_PARAM_GOODS_ST::NAME).map(|p| &*p.data)
}

fn goods_info(id: u32) -> Option<GoodsInfo> {
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let row = unsafe { goods_file(repo)?.get_row_by_id::<EQUIP_PARAM_GOODS_ST>(id) }?;
    Some(GoodsInfo { consume: row.is_consume(), max_num: row.max_num(), goods_type: row.goods_type() })
}

fn info_of(full: u32) -> Option<GoodsInfo> {
    (full >> 28 == CAT_GOODS).then(|| goods_info(full & 0x0FFF_FFFF)).flatten()
}

fn share_of(full: u32) -> Share {
    let coop = full >> 28 == CAT_GOODS && items::is_coop_goods(full & 0x0FFF_FFFF);
    classify(full, info_of(full), coop)
}

/// "goods 2470 'Mortal Blade' x1 (type 1, kept)" for the log.
fn describe(e: &[u32; 4]) -> String {
    let full = e[0];
    let id = full & 0x0FFF_FFFF;
    match full >> 28 {
        CAT_GOODS => {
            let name = items::goods_name(id).unwrap_or_else(|| "?".into());
            let info = info_of(full).map_or_else(
                || "no row".to_owned(),
                |g| format!("type {}, {}max {}", g.goods_type, if g.consume { "consumable, " } else { "" }, g.max_num),
            );
            format!("goods {id} '{name}' x{} ({info})", e[1])
        }
        CAT_WEAPON => format!("weapon {id} x{}", e[1]),
        CAT_PROTECTOR => format!("protector {id} x{}", e[1]),
        CAT_ACCESSORY => format!("accessory {id} x{}", e[1]),
        c => format!("category {c} id {id} x{}", e[1]),
    }
}

fn owns(full: u32) -> Option<bool> {
    let egd = read_ptr(module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))? + PGD_EQUIP_GAME_DATA;
    let find: extern "C" fn(usize, *const u32) -> i32 = unsafe { std::mem::transmute(module_base() + FIND_ITEM_RVA) };
    Some(find(egd, &full) >= 0)
}

/// GrantItems with the normal pickup log / dialog (`silent = 0`), like AwardItemLot.
fn grant_now(entries: &[[u32; 4]]) -> bool {
    let Some(mim) = read_ptr(module_base() + MAP_ITEM_MAN) else { return false };
    let mut buf: Vec<u32> = vec![entries.len() as u32];
    for e in entries {
        buf.extend_from_slice(e);
    }
    let grant: extern "C" fn(usize, *const u32, usize, usize, *mut u32, usize) -> usize =
        unsafe { std::mem::transmute(module_base() + GRANT_RVA) };
    grant(mim, buf.as_ptr(), 0, 0, std::ptr::null_mut(), 1);
    true
}

/// Per-player flags (item lots): set here only, world.rs never sends them.
fn set_flag_on(id: u32) -> bool {
    let Some(fm) = world::real_world_flag_man() else { return false };
    let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(module_base() + SET_EVENT_FLAG_RVA) };
    set(fm, id, true, false);
    true
}

// --- Detours -----------------------------------------------------------------

type Fn6 = extern "C" fn(usize, usize, usize, usize, usize, usize) -> usize;

/// `0x91C970`: inside an AwardItemLot, the entries are copied for the frame task.
extern "C" fn grant_detour(a: usize, buf: usize, c: usize, d: usize, e: usize, f: usize) -> usize {
    IN_AWARD.with(|cell| {
        if let Ok(mut slot) = cell.try_borrow_mut()
            && let Some(v) = slot.as_mut()
            && let Some(n) = read::<u32>(buf)
        {
            for i in 0..(n as usize).min(MAX_ENTRIES) {
                let at = buf + 4 + 16 * i;
                if let (Some(id), Some(num), Some(x), Some(popup)) =
                    (read::<u32>(at), read::<u32>(at + 4), read::<u32>(at + 8), read::<u32>(at + 12))
                    && v.len() < MAX_ENTRIES
                {
                    v.push([id, num, x, popup]);
                }
            }
        }
    });
    // An item grant must run: in the install window (patch in, trampoline not stored
    // yet; microseconds on the init thread) wait for it.
    let t = loop {
        match GRANT_TRAMP.load(Ordering::Acquire) {
            0 => std::hint::spin_loop(),
            t => break t,
        }
    };
    let original: Fn6 = unsafe { std::mem::transmute(t) };
    original(a, buf, c, d, e, f)
}

/// From bossrules.rs `install`.
pub fn install() {
    let detour: Fn6 = grant_detour;
    match hook::install_detour(GRANT_RVA, &GRANT_PROLOGUE, detour as usize, "GrantItems (0x91C970)") {
        Some(t) => {
            GRANT_TRAMP.store(t, Ordering::Release);
            log::line("reward: GrantItems hook installed, script item grants can be shared");
        }
        None => log::line("reward: GrantItems hook missing, script item grants are NOT shared (boss rewards still are)"),
    }
    let drops: extern "C" fn(usize) = drops_detour;
    match hook::install_detour(DROPS_RVA, &DROPS_PROLOGUE, drops as usize, "enemy Drops (0x9FFF50)") {
        Some(t) => DROPS_TRAMP.store(t, Ordering::Release),
        None => log::line("reward: Drops hook missing: the friend's kills may lose their item lot here, boss drops not shared"),
    }
}

/// bossrules.rs `award_detour`, before the original: a lot the friend already gave us
/// (their script ran first, ours runs now) is skipped. True = do not run it.
pub fn skip_local_award(lot: i32) -> bool {
    let now = game::now_ms();
    let Ok(mut r) = REPLAYED.lock() else { return false };
    r.retain(|(_, t)| now.saturating_sub(*t) < REPLAYED_FOR_MS);
    match r.iter().position(|(l, _)| *l == lot) {
        Some(i) => {
            r.remove(i);
            LOCAL_SKIPPED.fetch_add(1, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

/// bossrules.rs `pump_flags`: a boss's defeat flag turned on in our world (synced or ours).
pub fn note_defeat_flag() {
    DEFEAT_FLAG_AT_MS.store(game::now_ms().max(1), Ordering::Release);
}

/// A boss defeat flag turned on here within the last 15 s: lots given now are boss rewards.
pub fn defeat_flag_window() -> bool {
    let at = DEFEAT_FLAG_AT_MS.load(Ordering::Acquire);
    at != 0 && game::now_ms().saturating_sub(at) < DEFEAT_FLAG_WINDOW_MS
}

/// bossrules.rs `award_detour`, before the original (not for our own reward calls).
pub fn award_begin() {
    AWARDS_SEEN.fetch_add(1, Ordering::Relaxed);
    IN_AWARD.with(|c| {
        if let Ok(mut s) = c.try_borrow_mut() {
            *s = Some(Vec::new());
        }
    });
}

/// bossrules.rs `award_detour`, after the original: `flags` = switched on meanwhile.
pub fn award_end(lot: i32, flags: Vec<u32>, boss_window: bool) {
    let boss_window = boss_window || defeat_flag_window();
    let entries = IN_AWARD.with(|c| c.try_borrow_mut().ok().and_then(|mut s| s.take())).unwrap_or_default();
    if lot <= 0 {
        return;
    }
    // Our own script gave this lot: the friend's copy of it arriving soon is a double.
    if !entries.is_empty()
        && let Ok(mut l) = LOCAL_AWARDS.lock()
    {
        let now = game::now_ms();
        l.retain(|(_, t)| now.saturating_sub(*t) < LOCAL_AWARD_KEEP_MS);
        if l.len() >= 128 {
            l.remove(0);
        }
        l.push((lot, now));
    }
    if let Ok(mut q) = CAPTURED.lock()
        && q.len() < MAX_CAPTURED
    {
        q.push(Captured { lot, entries, flags, boss_window });
    }
}

/// Our own AwardItemLot of `lot` gave something within the last `window_ms` (at most
/// `LOCAL_AWARD_KEEP_MS`): ms ago. Script lots use 120 s (both worlds ran the script),
/// boss lots `BOSS_LOCAL_AWARD_MS`.
pub(crate) fn local_award_age(lot: i32, window_ms: u64) -> Option<u64> {
    let now = game::now_ms();
    let l = LOCAL_AWARDS.lock().ok()?;
    l.iter().filter(|(x, t)| *x == lot && now.saturating_sub(*t) < window_ms).map(|(_, t)| now.saturating_sub(*t)).min()
}

/// bossrules.rs, after it gave the friend's boss lot through AwardItemLot: our own world's
/// award of the same lot (its copy of the kill) in the next 120 s is skipped.
pub(crate) fn note_replayed(lot: i32) {
    if let Ok(mut r) = REPLAYED.lock() {
        r.push((lot, game::now_ms()));
        if r.len() > 64 {
            r.remove(0);
        }
    }
}

/// What a friend's lot that may be ours already needs (`owned_check`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owned {
    /// Not ours: give it.
    Give,
    /// Ours already: our own award of it ran.
    OurAward,
    /// Ours already: its lot flags are on and we own every unique item of it.
    FlagsAndItems,
    /// Its lot flags are on here but a unique item of it is missing: give only those.
    Missing,
}

/// Fix 4 (§33): lot flags on alone do not prove we got the lot (a flag that came over by
/// another way, an older build's sync). `unique_owned` = for each unique entry of the
/// friend's real grant (or, for a boss lot, of the lot's sure goods): owned here.
pub fn owned_check(has_flags: bool, flags_on: bool, our_award: bool, unique_owned: &[bool]) -> Owned {
    if our_award {
        Owned::OurAward
    } else if has_flags && flags_on {
        if unique_owned.iter().all(|&o| o) { Owned::FlagsAndItems } else { Owned::Missing }
    } else {
        Owned::Give
    }
}

/// bossrules.rs `apply_reward`: one lot of the friend's boss reward.
pub(crate) enum BossLot {
    /// Run AwardItemLot (the game's own lottery and flags).
    Award,
    Skip(String),
    /// Give only these (unique sure goods we lack; the lot flags are on already).
    GiveMissing(Vec<[u32; 4]>),
}

/// Fixes 3 and 4 for a friend's boss lot: lot 2499 (Regenerative Power, no lot flags) came
/// twice because a lot without flags cannot be seen as owned; our own award of it is.
pub(crate) fn boss_lot_check(lot: i32, flags: &[u32]) -> BossLot {
    let ours = local_award_age(lot, BOSS_LOCAL_AWARD_MS);
    let flags_on = !flags.is_empty() && flags.iter().all(|&f| game::event_flag(f) == Some(true));
    let uniq: Vec<[u32; 4]> = if flags_on && ours.is_none() {
        sure_goods_of_lot(lot).into_iter().map(|(e, _)| e).filter(|e| unique(e[0], info_of(e[0])) && share_of(e[0]) != Share::Never).collect()
    } else {
        Vec::new()
    };
    let owned: Vec<bool> = uniq.iter().map(|e| owns(e[0]) == Some(true)).collect();
    match owned_check(!flags.is_empty(), flags_on, ours.is_some(), &owned) {
        Owned::Give => BossLot::Award,
        Owned::OurAward => BossLot::Skip(format!("our own world gave it {} s ago", ours.unwrap_or(0) / 1000)),
        Owned::FlagsAndItems => BossLot::Skip("lot flags on here and its unique items owned".into()),
        Owned::Missing => BossLot::GiveMissing(
            uniq.into_iter().zip(owned).filter(|(_, o)| !o).map(|(mut e, _)| {
                e[3] = POPUP_DIALOG;
                e
            }).collect(),
        ),
    }
}

/// The unique, shareable goods a boss lot surely gives that we do not own (v8.10: lot 2080
/// of Genichiro gave the friend both Memories but not Bloodsmoke Ninjutsu, a type 8 skill,
/// live 2026-10-25). Only unique items: a consumable we hold is never "missing", so a check
/// that runs after the award can not double a stack.
pub(crate) fn boss_lot_missing(lot: i32) -> Vec<[u32; 4]> {
    sure_goods_of_lot(lot)
        .into_iter()
        .map(|(e, _)| e)
        .filter(|e| unique(e[0], info_of(e[0])) && share_of(e[0]) != Share::Never && owns(e[0]) == Some(false))
        .map(|mut e| {
            e[3] = POPUP_DIALOG;
            e
        })
        .collect()
}

/// bossrules.rs: GrantItems with the pickup log / dialog. False = item manager unreadable.
pub(crate) fn give_entries(entries: &[[u32; 4]]) -> bool {
    grant_now(entries)
}

pub(crate) fn describe_all(entries: &[[u32; 4]]) -> Vec<String> {
    entries.iter().map(describe).collect()
}

// --- Frame task ----------------------------------------------------------------

/// Goods the doctor compares, built once from the params.
struct KeyIndex {
    /// (goods id, first_get_event_flag_id)
    keys: Vec<(u32, i32)>,
    /// goods id -> per-player item-lot flags of the lots that give it.
    lot_flags: HashMap<u32, Vec<u32>>,
}

fn build_index() -> Option<KeyIndex> {
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let goods = goods_file(repo)?;
    let mut keys = Vec::new();
    let mut offset_checked = false;
    for (id, row) in unsafe { goods.rows::<EQUIP_PARAM_GOODS_ST>() } {
        if !offset_checked && row.first_get_event_flag_id() > 0 {
            // `0x84E7E0` reads the flag at row +0x7C: check the vendor layout agrees once.
            let raw = read::<i32>(row as *const EQUIP_PARAM_GOODS_ST as usize + 0x7C);
            log::line(&format!("key items: goods {id} first_get_event_flag_id {} (row +0x7C reads {raw:?})", row.first_get_event_flag_id()));
            offset_checked = true;
        }
        if row.is_consume() || row.max_num() > 1 || items::is_coop_goods(id) || keys.len() >= MAX_KEYS {
            continue;
        }
        if items::goods_name(id).is_none() {
            continue;
        }
        // The same sharing rule as everywhere (Rot Essences are never shared: not listed,
        // not compared, not repaired).
        let info = GoodsInfo { consume: false, max_num: row.max_num(), goods_type: row.goods_type() };
        if classify(GOODS | id, Some(info), false) == Share::Never {
            continue;
        }
        keys.push((id, row.first_get_event_flag_id()));
    }
    let wanted: HashSet<u32> = keys.iter().map(|k| k.0).collect();
    let mut lot_flags: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Some(lots) = repo.params().find(|p| p.struct_name() == ITEMLOT_PARAM_ST::NAME).map(|p| &*p.data) {
        for (_, r) in unsafe { lots.rows::<ITEMLOT_PARAM_ST>() } {
            let slots = [
                (r.lot_item_id01(), r.get_item_flag_id01()),
                (r.lot_item_id02(), r.get_item_flag_id02()),
                (r.lot_item_id03(), r.get_item_flag_id03()),
                (r.lot_item_id04(), r.get_item_flag_id04()),
                (r.lot_item_id05(), r.get_item_flag_id05()),
                (r.lot_item_id06(), r.get_item_flag_id06()),
                (r.lot_item_id07(), r.get_item_flag_id07()),
                (r.lot_item_id08(), r.get_item_flag_id08()),
            ];
            for (item, flag) in slots {
                if item > 0 && wanted.contains(&(item as u32)) {
                    let e = lot_flags.entry(item as u32).or_default();
                    for f in [flag, r.get_item_flag_id()] {
                        if f > 0 && !e.contains(&(f as u32)) {
                            e.push(f as u32);
                        }
                    }
                }
            }
        }
    }
    let with_first = keys.iter().filter(|k| k.1 > 0).count();
    log::line(&format!(
        "key items: {} key items indexed (named goods, not consumed on use, max 1): {with_first} with an acquisition flag (first_get_event_flag_id), {} given by item lots",
        keys.len(),
        lot_flags.len()
    ));
    Some(KeyIndex { keys, lot_flags })
}

/// The key-item exchange and the reward reconcile (§33) with one peer link (its epoch: a
/// new link exchanges again). Host: one per client; a client: the host only.
struct KeyLink {
    epoch: u64,
    at: Instant,
    sent: bool,
    /// Its list: (it is the host, key items).
    list: Option<(bool, Vec<u32>)>,
    checked: bool,
    /// Our queued grants went out on this link.
    queue_sent: bool,
    /// Our ledger is due on this link (connect: after the doctor's wait).
    ledger_due: Option<(Instant, u8)>,
    /// Its ledger: (arrived, reason, rows), applied `LEDGER_APPLY_AFTER` later.
    ledger: Option<(Instant, u8, Vec<i32>)>,
}

impl KeyLink {
    fn new(epoch: u64, at: Instant) -> Self {
        Self {
            epoch,
            at,
            sent: false,
            list: None,
            checked: false,
            queue_sent: false,
            ledger_due: Some((at + DOCTOR_AFTER_CONNECT, LEDGER_CONNECT)),
            ledger: None,
        }
    }
}

/// Reconcile rows: one-time lot rows (script / boss / enemy drop) with one sure goods slot.
struct LotIndex {
    /// row id -> (its lot flag, the sure goods entry `{fullId, num, -1, popup 0}`)
    rows: HashMap<i32, (u32, [u32; 4])>,
}

fn build_lot_index() -> Option<LotIndex> {
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = |name: &str| repo.params().find(|p| p.struct_name() == name).map(|p| &*p.data);
    let lots = file(ITEMLOT_PARAM_ST::NAME)?;
    // Enemy lots: what NpcParam names, and the rows after it (one lot reads rows L, L+1..).
    let mut enemy_refs: HashSet<i32> = HashSet::new();
    let npc = file(NPC_PARAM_ST::NAME);
    if let Some(npc) = npc {
        for (_, r) in unsafe { npc.rows::<NPC_PARAM_ST>() } {
            for l in [r.item_lot_id_1(), r.item_lot_id_2(), r.item_lot_id_3(), r.item_lot_id_4(), r.item_lot_id_5(), r.item_lot_id_6()] {
                if l > 0 {
                    enemy_refs.insert(l);
                }
            }
        }
    }
    let mut enemy: HashSet<i32> = HashSet::new();
    for &l in &enemy_refs {
        for id in l..l.saturating_add(8) {
            if unsafe { lots.get_row_by_id::<ITEMLOT_PARAM_ST>(id as u32) }.is_none() {
                break;
            }
            enemy.insert(id);
        }
    }
    let shop_flags: HashSet<u32> = file(SHOP_LINEUP_PARAM::NAME)
        .map(|s| unsafe { s.rows::<SHOP_LINEUP_PARAM>() }.map(|(_, r)| r.event_flag()).filter(|&f| f > 0).map(|f| f as u32).collect())
        .unwrap_or_default();
    let mut rows = HashMap::new();
    let (mut script, mut drops, mut treasure, mut shop, mut repeat, mut not_sure, mut coop) = (0, 0, 0, 0, 0, 0, 0);
    for (id, r) in unsafe { lots.rows::<ITEMLOT_PARAM_ST>() } {
        let id = id as i32;
        let sure = sure_slot(&slots_of(r), |i| items::goods_name(i).is_some());
        // A lottery row: no slot flag to name, the row's flag says "this lot ran".
        let flag = match &sure {
            Ok((_, _, f)) => primary_flag(*f, r.get_item_flag_id()),
            Err(_) => r.get_item_flag_id().max(0) as u32,
        };
        let kind = classify_lot(id, enemy.contains(&id), flag, shop_flags.contains(&flag));
        if !reconcilable(kind) {
            match kind {
                LotKind::Repeatable => repeat += 1,
                LotKind::Shop => shop += 1,
                _ => treasure += 1,
            }
            continue;
        }
        match sure {
            Err(_) => not_sure += 1,
            Ok((item, ..)) if items::is_coop_goods(item) => coop += 1,
            Ok((item, num, _)) => {
                if kind == LotKind::EnemyDrop { drops += 1 } else { script += 1 }
                rows.insert(id, (flag, [GOODS | item, num, u32::MAX, 0]));
            }
        }
    }
    log::line(&format!(
        "reconcile: {} lot rows can be reconciled ({script} script / boss, {drops} enemy drop; one-time, one sure goods slot); left per player: {treasure} map treasure, {shop} shop, {repeat} repeatable (no lot flag), {not_sure} lottery / not goods, {coop} co-op; NpcParam {} ({} lots named)",
        rows.len(),
        if npc.is_some() { "read" } else { "MISSING (no enemy drops)" },
        enemy_refs.len()
    ));
    Some(LotIndex { rows })
}

/// Before a queued grant goes out: still ours in this save? (Its lot flags on here, every
/// entry an item of rows `lot`.. of our own ItemLotParam, every flag one of theirs.)
fn queued_is_ours(g: &Grant) -> Result<(), String> {
    let file = lots_file().ok_or("ItemLotParam not readable")?;
    let mut items: HashSet<u32> = HashSet::new();
    let mut flags: HashSet<u32> = HashSet::new();
    for id in g.lot.max(1)..g.lot.max(1).saturating_add(8) {
        let Some(r) = (unsafe { file.get_row_by_id::<ITEMLOT_PARAM_ST>(id as u32) }) else { break };
        if r.get_item_flag_id() > 0 {
            flags.insert(r.get_item_flag_id() as u32);
        }
        for s in slots_of(r) {
            if s.0 > 0 {
                items.insert(s.0 as u32);
            }
            if s.4 > 0 {
                flags.insert(s.4 as u32);
            }
        }
    }
    if let Some(e) = g.entries.iter().find(|e| !items.contains(&(e[0] & 0x0FFF_FFFF))) {
        return Err(format!("{} is not an item of lot {}", describe(e), g.lot));
    }
    if let Some(f) = g.flags.iter().find(|f| !flags.contains(f)) {
        return Err(format!("flag {f} is not a flag of lot {}", g.lot));
    }
    match g.flags.iter().map(|&f| game::event_flag(f)).collect::<Option<Vec<bool>>>() {
        None => Err("flags not readable yet".into()),
        Some(on) if on.iter().all(|&o| o) => Ok(()),
        Some(_) => Err("its lot flags are off in this save (another character?)".into()),
    }
}

#[derive(Default)]
pub struct KeyItems {
    next_id: u32,
    pending: Vec<(Instant, Grant)>,
    handled: HashSet<u32>,
    wait_log_at: Option<Instant>,
    stats_at: Option<Instant>,
    sent: u32,
    kept_here: u32,
    received: u32,
    given: u32,
    // Doctor: host <-> each client, a client with the host (PLAN-MULTIPLAYER P4), per link epoch.
    index: Option<Option<KeyIndex>>,
    links: BTreeMap<PeerId, KeyLink>,
    repaired: HashSet<u32>,
    // Boss / miniboss drops: ours (entity -> when, item lot dropped here), the friend's.
    own_drops: HashMap<i32, (Instant, bool)>,
    pending_drops: Vec<(Instant, i32, i32, PeerId)>,
    drops_handled: HashSet<(i32, i32)>,
    // Reward reconcile (§33), per link (the per-link state is in `links`): our unacked
    // one-time grants (pending_grants.txt) and which peers acked each, the peers' events
    // to ack (sender, event id) and who sent each event, the lot index.
    queue: Option<Vec<Grant>>,
    acked_by: HashMap<u32, BTreeSet<PeerId>>,
    acks: Vec<(PeerId, u32)>,
    grant_from: HashMap<u32, PeerId>,
    /// Every peer in the session (last frame): a queued grant is done when all acked it.
    present: Vec<PeerId>,
    lots: Option<Option<std::sync::Arc<LotIndex>>>,
    rest_seen: Option<Instant>,
    reconciled: u32,
}

impl KeyItems {
    /// bossrules.rs, once per frame in the world. Grants and boss drops go to everyone;
    /// the key-item check and the reward reconcile run host <-> each client.
    pub fn on_frame(&mut self, net: &mut Option<&mut Net>, peers: &PeerSet, connected: bool, in_world_for: Duration) {
        let now = Instant::now();
        if connected {
            self.update_links(peers, now);
        }
        self.outgoing(net, connected);
        if let Some(n) = net.as_deref_mut() {
            while let Some((from, d)) = n.take_bulk_from(BULK_ITEM_GRANT) {
                match decode_grant(&d) {
                    Some(g) => {
                        self.received += 1;
                        self.grant_from.insert(g.id, from);
                        log::line(&format!(
                            "reward: friend's script lot {} arrived (event {}, {} entries, lot flags {:?}), giving it in {} s",
                            g.lot,
                            g.id,
                            g.entries.len(),
                            g.flags,
                            APPLY_AFTER.as_secs()
                        ));
                        crate::audit::reward_received(crate::audit::Pipe::Script, g.id as u64, g.lot, from);
                        self.pending.push((now, g));
                    }
                    None => log::line(&format!("reward: unreadable grant ({} bytes), ignored", d.len())),
                }
            }
        }
        self.apply_due(in_world_for);
        self.send_queue(net, connected, in_world_for);
        self.send_acks(net, connected);
        self.doctor(net, peers, connected, in_world_for, now);
        self.boss_drops(net, connected, in_world_for, now);
        self.reconcile(net, connected, in_world_for, now);
        if self.stats_at.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(60)) {
            self.stats_at = Some(now);
            if self.sent + self.received + self.kept_here > 0 || AWARDS_SEEN.load(Ordering::Relaxed) > 0 {
                log::line(&format!(
                    "reward stats (script lots): awards seen {}, sent {}, kept here {}, received {}, given {}, local doubles skipped {}; item lots we dropped for the friend's kills {}; queued for the friend {}, reconciled here {}",
                    AWARDS_SEEN.load(Ordering::Relaxed),
                    self.sent,
                    self.kept_here,
                    self.received,
                    self.given,
                    LOCAL_SKIPPED.load(Ordering::Relaxed),
                    FORCED_DROPS.load(Ordering::Relaxed),
                    self.queue.as_ref().map_or(0, Vec::len),
                    self.reconciled
                ));
            }
        }
    }

    /// Our unacked one-time grants, loaded from pending_grants.txt the first time.
    fn queue(&mut self) -> &mut Vec<Grant> {
        self.queue.get_or_insert_with(|| {
            let q = load_pending_from(&pending_path());
            if !q.is_empty() {
                log::line(&format!(
                    "reward: {} queued grant(s) in pending_grants.txt for the friend: lots {:?}",
                    q.len(),
                    q.iter().map(|g| g.lot).collect::<Vec<_>>()
                ));
            }
            q
        })
    }

    fn new_id(&mut self) -> u32 {
        if self.next_id == 0 {
            // Unique enough across our restarts within one session of the friend.
            self.next_id = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_millis() as u32)) | 1;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.next_id
    }

    /// The link table (doctor and reconcile): host <-> each client, a client with the host
    /// only (`PeerSet::link_epochs`); a new epoch is a new link (exchange again).
    fn update_links(&mut self, peers: &PeerSet, now: Instant) {
        let links = peers.link_epochs();
        self.links.retain(|id, _| links.iter().any(|(l, _)| l == id));
        for &(id, e) in &links {
            let l = self.links.entry(id).or_insert_with(|| KeyLink::new(e, now));
            if l.epoch != e {
                *l = KeyLink::new(e, now);
            }
        }
        self.present = peers.iter().map(|p| p.id).collect();
    }

    /// On every new link (in the world 10 s, linked 5 s): our queued grants go out again;
    /// the receiver's `apply` skips what it has (lot flags) and acks. Host: to that client;
    /// a client (its only link is the host): to everyone (the host relays).
    fn send_queue(&mut self, net: &mut Option<&mut Net>, connected: bool, in_world_for: Duration) {
        if !connected || in_world_for < DOCTOR_IN_WORLD {
            return;
        }
        let Some(n) = net.as_deref_mut() else { return };
        let due: Vec<PeerId> =
            self.links.iter().filter(|(_, l)| !l.queue_sent && l.at.elapsed() >= DOCTOR_AFTER_CONNECT).map(|(&id, _)| id).collect();
        if due.is_empty() {
            return;
        }
        for id in &due {
            if let Some(l) = self.links.get_mut(id) {
                l.queue_sent = true;
            }
        }
        if !ENABLED.load(Ordering::Relaxed) || !RECONCILE.load(Ordering::Relaxed) {
            return;
        }
        let host = n.is_host();
        let queued: Vec<Grant> = self.queue().clone();
        for g in queued {
            match queued_is_ours(&g) {
                Ok(()) => {
                    for &id in &due {
                        let d = if host { Dest::Peer(id) } else { Dest::All };
                        let ok = n.send_bulk_to(d, BULK_ITEM_GRANT, &encode_grant(&g));
                        self.sent += 1;
                        log::line(&format!(
                            "reward: sent queued grant lot {} to {} (event {}, ok = {ok}): {:?}, lot flags {:?}",
                            g.lot,
                            if host { format!("peer {}", id.slot()) } else { "the friend".to_owned() },
                            g.id,
                            g.entries.iter().map(describe).collect::<Vec<_>>(),
                            g.flags
                        ));
                    }
                }
                Err(why) => log::line(&format!("reward: queued grant lot {} (event {}) not sent: {why}; kept", g.lot, g.id)),
            }
        }
    }

    /// Our acks, each to the peer whose grant it was (the host relays a client's).
    fn send_acks(&mut self, net: &mut Option<&mut Net>, connected: bool) {
        if self.acks.is_empty() {
            return;
        }
        let acks = std::mem::take(&mut self.acks);
        if let Some(n) = net.as_deref_mut().filter(|_| connected) {
            let mut by: BTreeMap<PeerId, Vec<u32>> = BTreeMap::new();
            for (from, id) in acks {
                by.entry(from).or_default().push(id);
            }
            for (from, ids) in by {
                let ok = n.send_bulk_to(Dest::Peer(from), BULK_GRANT_ACK, &encode_ack(&ids));
                log::line(&format!("reward: acked the friend's events {ids:?} (peer {}, ok = {ok})", from.slot()));
            }
        }
        // Not connected: the friend sends them again on the next connect, `apply` skips them.
    }

    /// One grant decided here: ack it to its sender.
    fn ack(&mut self, id: u32) {
        let from = self.grant_from.remove(&id).unwrap_or(PeerId::HOST);
        self.acks.push((from, id));
    }

    /// A queued grant leaves pending_grants.txt once every peer in the session acked it
    /// (two players: the friend's ack).
    fn take_acks(&mut self, net: &mut Option<&mut Net>) {
        let Some(n) = net.as_deref_mut() else { return };
        let mut acked = Vec::new();
        while let Some((from, d)) = n.take_bulk_from(BULK_GRANT_ACK) {
            match decode_ack(&d) {
                Some(ids) => acked.extend(ids.into_iter().map(|i| (from, i))),
                None => log::line(&format!("reward: unreadable ack ({} bytes), ignored", d.len())),
            }
        }
        if acked.is_empty() {
            return;
        }
        let queued: HashSet<u32> = self.queue().iter().map(|g| g.id).collect();
        for (from, id) in acked {
            if queued.contains(&id) {
                self.acked_by.entry(id).or_default().insert(from);
            }
        }
        let done = fully_acked(&self.acked_by, &self.present);
        if done.is_empty() {
            return;
        }
        for id in &done {
            self.acked_by.remove(id);
        }
        let gone = pending_remove(self.queue(), &done);
        if gone.is_empty() {
            return;
        }
        let saved = save_pending_to(&pending_path(), self.queue().as_slice());
        log::line(&format!(
            "reward: the friend acked lots {:?} (events {:?}), removed from pending_grants.txt ({} left, saved {})",
            gone.iter().map(|g| g.lot).collect::<Vec<_>>(),
            gone.iter().map(|g| g.id).collect::<Vec<_>>(),
            self.queue().len(),
            saved.as_ref().map_or_else(|e| format!("FAILED: {e}"), |_| "ok".into())
        ));
    }

    /// Ledger (§33), per link (host <-> each client, a client with the host): ours out on
    /// every new link and after every rest; each link's ledger applied here.
    fn reconcile(&mut self, net: &mut Option<&mut Net>, connected: bool, in_world_for: Duration, now: Instant) {
        self.take_acks(net);
        let Some(n) = net.as_deref_mut() else { return };
        while let Some((from, d)) = n.take_bulk_from(BULK_LOT_LEDGER) {
            match decode_ledger(&d) {
                Some((reason, rows)) => {
                    let Some(l) = self.links.get_mut(&from) else {
                        log::line(&format!("reconcile: a ledger from peer {} ignored (we reconcile with the host only)", from.slot()));
                        continue;
                    };
                    log::line(&format!(
                        "reconcile: the friend's ledger arrived (peer {}, {} one-time lots on there, after {}){}",
                        from.slot(),
                        rows.len(),
                        if reason == LEDGER_REST { "a rest" } else { "connecting" },
                        if ENABLED.load(Ordering::Relaxed) { "" } else { "; share_key_items = 0 here, ignored" }
                    ));
                    l.ledger = Some((now, reason, rows));
                }
                None => log::line(&format!("reconcile: unreadable ledger ({} bytes), ignored", d.len())),
            }
        }
        if !connected || !ENABLED.load(Ordering::Relaxed) || !RECONCILE.load(Ordering::Relaxed) {
            return;
        }
        // When ours goes out: a new link (after the doctor's wait, `KeyLink::new`), and 5 s
        // after every rest (on every link).
        let rest = crate::restsync::last_rest();
        if rest != self.rest_seen {
            self.rest_seen = rest;
            if rest.is_some() {
                for l in self.links.values_mut() {
                    l.ledger_due = Some((now + LEDGER_AFTER_REST, LEDGER_REST));
                }
            }
        }
        if self.links.values().any(|l| l.ledger_due.is_some() || l.ledger.is_some()) {
            if in_world_for < DOCTOR_IN_WORLD {
                return;
            }
            if self.lots.is_none() {
                self.lots = Some(build_lot_index().map(std::sync::Arc::new));
            }
        }
        let Some(Some(index)) = self.lots.clone() else { return };
        let due: Vec<(PeerId, u8)> =
            self.links.iter().filter_map(|(&id, l)| l.ledger_due.filter(|(t, _)| now >= *t).map(|(_, r)| (id, r))).collect();
        if !due.is_empty() {
            let mut on: Vec<i32> = Vec::new();
            let mut unreadable = false;
            for (&row, &(flag, _)) in &index.rows {
                match game::event_flag(flag) {
                    Some(true) => on.push(row),
                    Some(false) => {}
                    None => unreadable = true,
                }
            }
            if !unreadable {
                on.sort_unstable();
                for (id, reason) in due {
                    if let Some(l) = self.links.get_mut(&id) {
                        l.ledger_due = None;
                    }
                    let ok = n.send_bulk_to(Dest::Peer(id), BULK_LOT_LEDGER, &encode_ledger(reason, &on));
                    log::line(&format!(
                        "reconcile: sent our ledger to peer {} ({} of {} one-time lots on here, after {}, ok = {ok})",
                        id.slot(),
                        on.len(),
                        index.rows.len(),
                        if reason == LEDGER_REST { "a rest" } else { "connecting" }
                    ));
                }
            }
        }
        // One link's ledger per frame (each gives items).
        let ready = self
            .links
            .iter()
            .find(|(_, l)| l.ledger.as_ref().is_some_and(|(at, ..)| now.duration_since(*at) >= LEDGER_APPLY_AFTER))
            .map(|(&id, _)| id);
        let Some(id) = ready else { return };
        if game::safe_for_reward(in_world_for).is_err() {
            return;
        }
        let Some((at, reason, theirs)) = self.links.get_mut(&id).and_then(|l| l.ledger.take()) else { return };
        if !self.apply_ledger(&index, &theirs)
            && let Some(l) = self.links.get_mut(&id)
        {
            l.ledger.get_or_insert((at, reason, theirs));
        }
    }

    /// One peer's ledger: its rows on there and off here are given once. False = wait (a
    /// flag not readable yet).
    fn apply_ledger(&mut self, index: &LotIndex, theirs: &[i32]) -> bool {
        // Every flag must be readable, or "off" would give everything.
        let flags: Vec<u32> = theirs.iter().filter_map(|r| index.rows.get(r).map(|x| x.0)).collect();
        let Some(states) = flags.iter().map(|&f| game::event_flag(f).map(|on| (f, on))).collect::<Option<HashMap<u32, bool>>>() else {
            return false;
        };
        let unknown = theirs.iter().filter(|r| !index.rows.contains_key(r)).count();
        let plan = ledger_plan(theirs, |r| index.rows.get(&r).map(|x| x.0), |f| states.get(&f).copied().unwrap_or(true));
        // Rows our rules never share would come back in every ledger and starve the budget.
        let ruled_out = plan.iter().filter(|r| index.rows.get(r).is_some_and(|x| !shareable(share_of(x.1[0]), true))).count();
        let plan: Vec<_> = plan.into_iter().filter(|r| index.rows.get(r).is_some_and(|x| shareable(share_of(x.1[0]), true))).collect();
        if ruled_out > 0 {
            log::line(&format!("reconcile: {ruled_out} of the friend's lot rows not shared (rule)"));
        }
        let total = theirs.len();
        let (now_rows, later) = plan.split_at(plan.len().min(MAX_RECONCILE));
        let mut give: Vec<([u32; 4], i32, u32)> = Vec::new();
        for &row in now_rows {
            let Some(&(flag, entry)) = index.rows.get(&row) else { continue };
            if unique(entry[0], info_of(entry[0])) && owns(entry[0]) == Some(true) {
                let set = set_flag_on(flag);
                log::line(&format!(
                    "reconcile: lot {row} [{}] that the friend has: already owned here, lot flag {flag} set ({set})",
                    describe(&entry)
                ));
                continue;
            }
            give.push((entry, row, flag));
        }
        for chunk in give.chunks(MAX_ENTRIES) {
            let entries: Vec<[u32; 4]> = chunk.iter().map(|g| g.0).collect();
            if !grant_now(&entries) {
                log::line("reconcile: item manager not readable, the rest waits for the next ledger");
                return true;
            }
            for &(entry, row, flag) in chunk {
                let set = set_flag_on(flag);
                self.reconciled += 1;
                log::line(&format!("reconcile: got lot {row} [{}] that the friend has, lot flag {flag} set ({set})", describe(&entry)));
            }
        }
        log::line(&format!(
            "reconcile: friend's ledger done: {total} lots on there, {} off here and given / marked, {} unknown or per player here{}",
            now_rows.len(),
            unknown,
            if later.is_empty() { String::new() } else { format!(", {} more with the next ledger", later.len()) }
        ));
        true
    }

    /// Our script grants -> the friend.
    fn outgoing(&mut self, net: &mut Option<&mut Net>, connected: bool) {
        let captured = CAPTURED.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for c in captured {
            let shown: Vec<String> = c.entries.iter().map(describe).collect();
            if c.boss_window && tuning::boss_reward() {
                log::line(&format!("reward: lot {} {shown:?} is a boss reward (boss rules send it)", c.lot));
                continue;
            }
            if !ENABLED.load(Ordering::Relaxed) {
                log::line(&format!("reward: script lot {} {shown:?} not shared (share_key_items = 0)", c.lot));
                continue;
            }
            if c.entries.is_empty() {
                log::line(&format!("reward: script lot {} gave nothing (empty lottery or no GrantItems hook)", c.lot));
                continue;
            }
            let per = world::per_player_flags();
            let lot_flags: Vec<u32> =
                c.flags.iter().copied().filter(|f| per.is_some_and(|s| s.contains(f))).take(MAX_FLAGS).collect();
            let one_time = !lot_flags.is_empty();
            let kept: Vec<[u32; 4]> = c.entries.iter().copied().filter(|e| shareable(share_of(e[0]), one_time)).collect();
            if kept.is_empty() {
                self.kept_here += 1;
                log::line(&format!(
                    "reward: script lot {} {shown:?} stays ours ({} lot: consumables only from one-time lots, never co-op items)",
                    c.lot,
                    if one_time { "one-time" } else { "repeatable" }
                ));
                continue;
            }
            let id = self.new_id();
            let g = Grant { id, lot: c.lot, complete: kept.len() == c.entries.len(), entries: kept, flags: lot_flags };
            // Fix 1 (§33): a one-time grant waits in pending_grants.txt until the friend
            // acks it (sent now, or on the next connect when nobody is here).
            let queued = one_time && {
                let added = pending_add(self.queue(), g.clone());
                if added && let Err(e) = append_pending_to(&pending_path(), &g) {
                    log::line(&format!("reward: pending_grants.txt not written ({e}); lot {} waits in memory only", g.lot));
                }
                added
            };
            let Some(n) = net.as_deref_mut().filter(|_| connected) else {
                log::line(&format!(
                    "reward: script lot {} {shown:?} not sent (no friend connected){}",
                    c.lot,
                    if queued {
                        format!(": queued in pending_grants.txt (event {id}), sent on the next connect")
                    } else if one_time {
                        ": that lot is queued already".to_owned()
                    } else {
                        ": repeatable lot, not queued".to_owned()
                    }
                ));
                continue;
            };
            let ok = n.send_bulk(BULK_ITEM_GRANT, &encode_grant(&g));
            crate::audit::reward_sent(crate::audit::Pipe::Script, g.id as u64, g.lot);
            self.sent += 1;
            log::line(&format!(
                "reward: sent our script lot {} to the friend (event {}, ok = {ok}): {:?}{}, lot flags {:?}",
                g.lot,
                g.id,
                g.entries.iter().map(describe).collect::<Vec<_>>(),
                if g.complete { String::new() } else { format!(" (of {shown:?})") },
                g.flags
            ));
        }
    }

    fn apply_due(&mut self, in_world_for: Duration) {
        if !self.pending.iter().any(|(t, _)| t.elapsed() >= APPLY_AFTER) {
            return;
        }
        if let Err(why) = game::safe_for_reward(in_world_for) {
            if self.wait_log_at.is_none_or(|t| t.elapsed().as_secs() >= 5) {
                self.wait_log_at = Some(Instant::now());
                log::line(&format!("reward: waiting ({why})"));
            }
            return;
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(t, _)| t.elapsed() >= APPLY_AFTER);
        self.pending = later;
        for (_, g) in due {
            // The sender, read before apply (its ack takes it out of grant_from).
            let from = self.grant_from.get(&g.id).copied().unwrap_or(PeerId::HOST);
            self.apply(&g);
            crate::audit::reward_resolved(crate::audit::Pipe::Script, g.id as u64, g.lot, &g.flags, from);
        }
    }

    fn apply(&mut self, g: &Grant) {
        if !self.handled.insert(g.id) {
            log::line(&format!("reward: event {} (lot {}) was already handled", g.id, g.lot));
            self.ack(g.id);
            return;
        }
        if !ENABLED.load(Ordering::Relaxed) {
            log::line(&format!("reward: share_key_items = 0 here, friend's lot {} dropped", g.lot));
            self.ack(g.id);
            return;
        }
        if self.apply_inner(g) {
            self.ack(g.id);
        } else {
            // Not given and not decided (item manager): the friend sends it again next connect.
            self.handled.remove(&g.id);
        }
    }

    /// True = decided (given, or ours already): the friend may forget it.
    fn apply_inner(&mut self, g: &Grant) -> bool {
        let one_time = !g.flags.is_empty();
        let flags_on = one_time && g.flags.iter().all(|&f| game::event_flag(f) == Some(true));
        // Both worlds ran the same script (e.g. a miniboss death mirrored here): ours gave it.
        let ours = local_award_age(g.lot, REPLAYED_FOR_MS);
        let unique_entries: Vec<[u32; 4]> =
            g.entries.iter().copied().filter(|e| unique(e[0], info_of(e[0])) && shareable(share_of(e[0]), one_time)).collect();
        let owned: Vec<bool> = if flags_on { unique_entries.iter().map(|e| owns(e[0]) == Some(true)).collect() } else { Vec::new() };
        let check = owned_check(one_time, flags_on, ours.is_some(), &owned);
        match check {
            Owned::OurAward => {
                log::line(&format!("reward: friend's lot {} already given here by our own script {} s ago", g.lot, ours.unwrap_or(0) / 1000));
                return true;
            }
            Owned::FlagsAndItems => {
                log::line(&format!("reward: friend's lot {} already received here (lot flags {:?} on, its unique items owned)", g.lot, g.flags));
                return true;
            }
            Owned::Missing => log::line(&format!(
                "reward: friend's lot {} has its lot flags {:?} on here but we lack some of its unique items: giving only those",
                g.lot, g.flags
            )),
            Owned::Give => {}
        }
        let mut give = Vec::new();
        let mut skipped = Vec::new();
        for e in &g.entries {
            let info = info_of(e[0]);
            if !shareable(share_of(e[0]), one_time) {
                skipped.push(format!("{} (not shared here)", describe(e)));
            } else if unique(e[0], info) && owns(e[0]) == Some(true) {
                skipped.push(format!("{} (already owned)", describe(e)));
            } else if check == Owned::Missing && !unique(e[0], info) {
                skipped.push(format!("{} (lot flags on: not unique, so not given again)", describe(e)));
            } else {
                give.push(*e);
            }
        }
        if give.is_empty() {
            log::line(&format!("reward: friend's lot {} gave nothing new here: {skipped:?}", g.lot));
            return true;
        }
        if !grant_now(&give) {
            log::line(&format!("reward: item manager not readable, friend's lot {} not given (comes again on the next connect)", g.lot));
            return false;
        }
        let set: Vec<u32> = g.flags.iter().copied().filter(|&f| set_flag_on(f)).collect();
        if g.complete && give.len() == g.entries.len() && check == Owned::Give
            && let Ok(mut r) = REPLAYED.lock()
        {
            r.push((g.lot, game::now_ms()));
            if r.len() > 64 {
                r.remove(0);
            }
        }
        self.given += 1;
        log::line(&format!(
            "reward: gave the friend's script lot {} (event {}): {:?}{}, lot flags set {set:?}",
            g.lot,
            g.id,
            give.iter().map(describe).collect::<Vec<_>>(),
            if skipped.is_empty() { String::new() } else { format!(", left out {skipped:?}") }
        ));
        true
    }

    /// Boss / miniboss item drops: ours are announced to the friend; the friend's, when no
    /// drop of that enemy came here within 15 s (its corpse not loaded here, or no drop
    /// call), are given with the normal pickup log, once (lot flags).
    fn boss_drops(&mut self, net: &mut Option<&mut Net>, connected: bool, in_world_for: Duration, now: Instant) {
        let seen = DROPS_SEEN.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for d in seen {
            log::line(&format!(
                "reward: {} drop of {} (NpcParam item lot {}, deathblows {}): item lot {} (kill record +21 = {}, +108 = {})",
                if d.ours { "our loot-step" } else { "the game's" },
                d.entity,
                d.lot,
                d.deathblows,
                if d.forced {
                    "dropped by us at the corpse (the game skipped it: another killer)"
                } else if d.item_drop {
                    "dropped at the corpse"
                } else {
                    "NOT dropped"
                },
                d.kill21,
                d.kill108
            ));
            if d.deathblows < 2 {
                continue;
            }
            self.own_drops.insert(d.entity, (now, d.item_drop));
            if d.item_drop
                && d.lot > 0
                && let Some(n) = net.as_deref_mut().filter(|_| connected)
            {
                let mut body = vec![1u8];
                body.extend(d.entity.to_le_bytes());
                body.extend(d.lot.to_le_bytes());
                let ok = n.send_bulk(BULK_BOSS_DROP, &body);
                crate::audit::reward_sent(crate::audit::Pipe::BossDrop, crate::audit::key2(d.entity, d.lot), d.lot);
                log::line(&format!("reward: told the friend about the drop of {} (lot {}, ok = {ok})", d.entity, d.lot));
            }
        }
        self.own_drops.retain(|_, (t, _)| now.duration_since(*t) < OWN_DROP_KEEP);
        if let Some(n) = net.as_deref_mut() {
            while let Some((from, d)) = n.take_bulk_from(BULK_BOSS_DROP) {
                match (d.len(), d.first()) {
                    (9, Some(1)) => {
                        let entity = i32::from_le_bytes([d[1], d[2], d[3], d[4]]);
                        let lot = i32::from_le_bytes([d[5], d[6], d[7], d[8]]);
                        log::line(&format!("reward: the friend's game dropped lot {lot} for {entity}, checking ours in {} s", DROP_APPLY_AFTER.as_secs()));
                        crate::audit::reward_received(crate::audit::Pipe::BossDrop, crate::audit::key2(entity, lot), lot, from);
                        self.pending_drops.push((now, entity, lot, from));
                    }
                    _ => log::line(&format!("reward: unreadable boss drop ({} bytes)", d.len())),
                }
            }
        }
        if !self.pending_drops.iter().any(|(t, ..)| now.duration_since(*t) >= DROP_APPLY_AFTER) || game::safe_for_reward(in_world_for).is_err() {
            return;
        }
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending_drops).into_iter().partition(|(t, ..)| now.duration_since(*t) >= DROP_APPLY_AFTER);
        self.pending_drops = later;
        for (_, entity, lot, from) in due {
            crate::audit::reward_resolved(crate::audit::Pipe::BossDrop, crate::audit::key2(entity, lot), lot, &[], from);
            if !self.drops_handled.insert((entity, lot)) {
                continue;
            }
            if self.own_drops.get(&entity).is_some_and(|(_, item)| *item) {
                log::line(&format!("reward: the friend's drop of {entity} (lot {lot}): our own drop lies at the corpse here (pick it up)"));
                continue;
            }
            let goods = sure_goods_of_lot(lot);
            let give: Vec<([u32; 4], u32)> = goods.into_iter().filter(|(_, f)| *f == 0 || game::event_flag(*f) != Some(true)).collect();
            if give.is_empty() {
                log::line(&format!("reward: the friend's drop of {entity} (lot {lot}): nothing to give here (already picked up, or no sure goods)"));
                continue;
            }
            let entries: Vec<[u32; 4]> = give.iter().map(|g| g.0).collect();
            let ok = grant_now(&entries);
            let set: Vec<u32> = give.iter().map(|g| g.1).filter(|&f| f > 0 && set_flag_on(f)).collect();
            log::line(&format!(
                "reward: gave the friend's boss drop of {entity} (lot {lot}, no drop of it here) with the pickup log (ok = {ok}): {:?}, lot flags set {set:?}",
                entries.iter().map(describe).collect::<Vec<_>>()
            ));
        }
    }

    /// Session-start check of key items (log, notice, the one narrow repair): host <-> each
    /// client, a client with the host only, once per link (epoch).
    fn doctor(&mut self, net: &mut Option<&mut Net>, peers: &PeerSet, connected: bool, in_world_for: Duration, now: Instant) {
        let Some(n) = net.as_deref_mut() else { return };
        if !connected {
            return;
        }
        // The link table is current (`update_links`, start of `on_frame`).
        while let Some((from, d)) = n.take_bulk_from(BULK_KEY_ITEMS) {
            match decode_keys(&d) {
                Some((host, ids)) => match self.links.get_mut(&from) {
                    Some(l) => {
                        log::line(&format!(
                            "key items: peer {}'s key item list arrived ({} items, it is {})",
                            from.slot(),
                            ids.len(),
                            if host { "host" } else { "client" }
                        ));
                        l.list = Some((host, ids));
                    }
                    None => log::line(&format!("key items: a list from peer {} ignored (we check with the host only)", from.slot())),
                },
                None => log::line(&format!("key items: unreadable key item list ({} bytes)", d.len())),
            }
        }
        if !ENABLED.load(Ordering::Relaxed) || in_world_for < DOCTOR_IN_WORLD {
            return;
        }
        if self.index.is_none() {
            self.index = Some(build_index());
        }
        let Some(Some(index)) = &self.index else { return };
        let mut ours_cache: Option<Vec<u32>> = None;
        for (&id, l) in self.links.iter_mut() {
            if now.duration_since(l.at) < DOCTOR_AFTER_CONNECT {
                continue;
            }
            if !l.sent {
                l.sent = true;
                let ours = ours_cache.get_or_insert_with(|| owned_keys(index));
                let ok = n.send_bulk_to(Dest::Peer(id), BULK_KEY_ITEMS, &encode_keys(n.is_host(), ours));
                log::line(&format!("key items: sent our {} key items to peer {} (ok = {ok})", ours.len(), id.slot()));
            }
            // The repair gives an item (popup): not in a menu, a talk or while dead.
            if l.checked || game::safe_for_reward(in_world_for).is_err() {
                continue;
            }
            let Some((_, theirs)) = &l.list else { continue };
            l.checked = true;
            let friend = peers.by_id(id).map(|p| p.name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Friend".to_owned());
            check_keys(index, &mut self.repaired, theirs, &friend);
        }
    }
}

/// Compares our key items with one peer's list; logs, repairs the one narrow case, notices.
fn check_keys(index: &KeyIndex, repaired: &mut HashSet<u32>, theirs: &[u32], friend: &str) {
    {
        let ours: HashSet<u32> = owned_keys(index).into_iter().collect();
        let theirs: HashSet<u32> = theirs.iter().copied().collect();
        let mut missing_here: Vec<u32> = theirs.difference(&ours).copied().collect();
        let mut missing_there: Vec<u32> = ours.difference(&theirs).copied().collect();
        missing_here.sort_unstable();
        missing_there.sort_unstable();
        let name = |id: &u32| items::goods_name(*id).unwrap_or_else(|| format!("goods {id}"));
        log::line(&format!(
            "key items: we own {} key items, {friend} {}; we miss {:?}; {friend} misses {:?}",
            ours.len(),
            theirs.len(),
            missing_here.iter().map(|i| format!("{i} {}", name(i))).collect::<Vec<_>>(),
            missing_there.iter().map(|i| format!("{i} {}", name(i))).collect::<Vec<_>>()
        ));
        // The one repair: acquisition flag on in the shared world, never got it ourselves.
        let mut still_missing = Vec::new();
        for id in missing_here {
            let first = index.keys.iter().find(|k| k.0 == id).map_or(-1, |k| k.1);
            let lots = index.lot_flags.get(&id).cloned().unwrap_or_default();
            let shared_on = first > 0 && world::should_sync(first as u32) && game::event_flag(first as u32) == Some(true);
            let ours_on: Vec<u32> = lots.iter().copied().filter(|&f| game::event_flag(f) == Some(true)).collect();
            if shared_on && ours_on.is_empty() && !repaired.contains(&id) {
                repaired.insert(id);
                let ok = grant_now(&[[GOODS | id, 1, u32::MAX, POPUP_DIALOG]]);
                let marked = lots.first().copied().filter(|&f| set_flag_on(f));
                log::line(&format!(
                    "key items: gave {id} '{}' once (ok = {ok}): its acquisition flag {first} is on in the shared world, we did not own it and no item lot ever gave it to us; lot flag set {marked:?}",
                    name(&id)
                ));
            } else {
                log::line(&format!(
                    "key items: not giving {id} '{}': acquisition flag {first} {}, our lot flags on {ours_on:?}",
                    name(&id),
                    if first <= 0 { "unknown".to_owned() } else { format!("shared {} on {}", world::should_sync(first as u32), shared_on) }
                ));
                still_missing.push(id);
            }
        }
        let list = |ids: &[u32]| {
            let mut s: Vec<String> = ids.iter().take(3).map(name).collect();
            if ids.len() > 3 {
                s.push(format!("{} more", ids.len() - 3));
            }
            s.join(", ")
        };
        let mut text = Vec::new();
        if !still_missing.is_empty() {
            text.push(format!("You are missing: {} ({friend} has it)", list(&still_missing)));
        }
        if !missing_there.is_empty() {
            text.push(format!("{friend} is missing: {}", list(&missing_there)));
        }
        if !text.is_empty() {
            bossrules::notice(text.join(" / "), 10);
        }
    }
}

fn owned_keys(index: &KeyIndex) -> Vec<u32> {
    index.keys.iter().map(|k| k.0).filter(|&id| owns(GOODS | id) == Some(true)).collect()
}

/// The Rot Essences we hold (goods 9500..=9526), read like `owned_keys` does (`owns`), for the
/// doctor's `rot` line (doctor.rs). None = the inventory is not readable yet.
pub(crate) fn owned_rot_essences() -> Option<Vec<u32>> {
    owns(GOODS | *ROT_ESSENCES.start())?;
    Some(ROT_ESSENCES.filter(|&id| owns(GOODS | id) == Some(true)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: GoodsInfo = GoodsInfo { consume: false, max_num: 1, goods_type: 1 };
    const BEADS: GoodsInfo = GoodsInfo { consume: false, max_num: 99, goods_type: 2 };
    const PELLET: GoodsInfo = GoodsInfo { consume: true, max_num: 5, goods_type: 0 };

    #[test]
    fn grant_round_trip() {
        let g = Grant {
            id: 0xDEAD_BEEF,
            lot: 6_000_100,
            complete: false,
            entries: vec![[GOODS | 2470, 1, u32::MAX, 2], [700_000, 1, u32::MAX, 2]],
            flags: vec![50_000_100, 71_100_500],
        };
        let d = encode_grant(&g);
        assert_eq!(decode_grant(&d), Some(g));
        // Trailing byte, truncation and an unknown version are refused.
        let mut long = d.clone();
        long.push(0);
        assert_eq!(decode_grant(&long), None);
        assert_eq!(decode_grant(&d[..d.len() - 1]), None);
        let mut v2 = d;
        v2[0] = 2;
        assert_eq!(decode_grant(&v2), None);
    }

    #[test]
    fn grant_caps_entries_and_flags() {
        let g = Grant { id: 1, lot: 1, complete: true, entries: vec![[1, 1, 1, 1]; 40], flags: vec![7; 90] };
        let back = decode_grant(&encode_grant(&g)).unwrap();
        assert_eq!(back.entries.len(), MAX_ENTRIES);
        assert_eq!(back.flags.len(), MAX_FLAGS);
    }

    #[test]
    fn keys_round_trip() {
        let d = encode_keys(true, &[2470, 9_000, 1]);
        assert_eq!(decode_keys(&d), Some((true, vec![2470, 9_000, 1])));
        assert_eq!(decode_keys(&encode_keys(false, &[])), Some((false, vec![])));
        assert_eq!(decode_keys(&d[..d.len() - 2]), None);
    }

    #[test]
    fn rot_essences_never_shared() {
        for id in [9500, 9510, 9526] {
            assert_eq!(classify(GOODS | id, Some(KEY), false), Share::Never, "{id}");
            assert_eq!(classify(GOODS | id, None, false), Share::Never, "{id}");
            assert!(!shareable(classify(GOODS | id, Some(KEY), false), true), "{id}");
        }
        // The neighbours, and the Dragonrot Blood Sample, are ordinary key items.
        for id in [9499, 9527, 9560] {
            assert_eq!(classify(GOODS | id, Some(KEY), false), Share::Always, "{id}");
        }
        // A weapon with the same number is not a goods id.
        assert_eq!(classify(9500, None, false), Share::Always);
    }

    #[test]
    fn the_sharing_rule() {
        // Key items, beads / seeds, weapons (prosthetic tools): always.
        assert_eq!(classify(GOODS | 2470, Some(KEY), false), Share::Always);
        assert_eq!(classify(GOODS | 4000, Some(BEADS), false), Share::Always);
        assert_eq!(classify(700_000, None, false), Share::Always);
        assert_eq!(classify(0x1000_0000 | 5, None, false), Share::Always);
        // Consumables: only from a one-time lot.
        assert_eq!(classify(GOODS | 1000, Some(PELLET), false), Share::OneTime);
        assert!(shareable(Share::OneTime, true));
        assert!(!shareable(Share::OneTime, false));
        // Our own co-op items, unknown goods rows and empty entries: never.
        assert_eq!(classify(GOODS | 2470, Some(KEY), true), Share::Never);
        assert_eq!(classify(GOODS | 1, None, false), Share::Never);
        assert_eq!(classify(u32::MAX, None, false), Share::Never);
        assert_eq!(classify(0x3000_0000 | 5, None, false), Share::Never);
    }

    #[test]
    fn lot_classification() {
        // Flame Barrel Memo (Anayama talk / death, lot 61700, flag 50006170): script.
        assert_eq!(classify_lot(61_700, false, 50_006_170, false), LotKind::ScriptOrBoss);
        // Mechanical Barrel (Gyoubu, boss lot 2000) with a flag: script / boss.
        assert_eq!(classify_lot(2_000, false, 50_002_000, false), LotKind::ScriptOrBoss);
        // Regenerative Power (lot 2499, no lot flag): repeatable, never in a ledger.
        assert_eq!(classify_lot(2_499, false, 0, false), LotKind::Repeatable);
        // Herb Catalogue Scrap (13604000, NpcParam of the Senpou Assassin): enemy drop.
        assert_eq!(classify_lot(13_604_000, true, 51_360_400, false), LotKind::EnemyDrop);
        // The same id without an NpcParam reference: a map / corpse lot, per player.
        assert_eq!(classify_lot(13_604_000, false, 51_360_400, false), LotKind::MapTreasure);
        // Map treasure: 7-digit map lots, and the known 5-digit treasures.
        assert_eq!(classify_lot(1_100_050, false, 51_100_050, false), LotKind::MapTreasure);
        assert_eq!(classify_lot(61_400, false, 50_061_400, false), LotKind::MapTreasure);
        assert_eq!(classify_lot(63_610, false, 50_063_610, false), LotKind::MapTreasure);
        // A flag a shop row uses too: per player, even for an enemy lot.
        assert_eq!(classify_lot(2_010, true, 71_101_000, true), LotKind::Shop);
        assert!(reconcilable(LotKind::ScriptOrBoss) && reconcilable(LotKind::EnemyDrop));
        assert!(![LotKind::MapTreasure, LotKind::Shop, LotKind::Repeatable].into_iter().any(reconcilable));
    }

    #[test]
    fn sure_slots() {
        let empty: Slot = (0, 0, 0, 0, 0);
        let mut s = [empty; 8];
        s[0] = (9_000, 4, 100, 1, 50_006_170);
        assert_eq!(sure_slot(&s, |_| false), Ok((9_000, 1, 50_006_170)));
        // A count of 0 is one item; a named goods row counts without a goods category.
        s[0] = (9_000, 1, 100, 0, 0);
        assert_eq!(sure_slot(&s, |i| i == 9_000), Ok((9_000, 1, 0)));
        assert!(sure_slot(&s, |_| false).is_err());
        // A lottery (two slots with weight) is not sure.
        s[0] = (9_000, 4, 60, 1, 0);
        s[1] = (9_001, 4, 40, 1, 0);
        assert!(sure_slot(&s, |_| true).is_err());
        // One item but an empty slot holds weight: not sure either.
        s[1] = (0, 4, 40, 0, 0);
        assert!(sure_slot(&s, |_| true).is_err());
        assert_eq!(primary_flag(7, 9), 7);
        assert_eq!(primary_flag(0, 9), 9);
        assert_eq!(primary_flag(0, -1), 0);
    }

    fn grant(id: u32, lot: i32) -> Grant {
        Grant { id, lot, complete: true, entries: vec![[GOODS | 9_000, 1, u32::MAX, 2]], flags: vec![50_006_170] }
    }

    #[test]
    fn pending_lines_round_trip() {
        let g = Grant {
            id: 77,
            lot: 13_604_000,
            complete: false,
            entries: vec![[GOODS | 2_470, 1, u32::MAX, 2], [700_000, 3, 5, 0]],
            flags: vec![51_360_400, 51_360_401],
        };
        let line = pending_line(&g);
        assert_eq!(parse_pending(&line), vec![g.clone()]);
        // Comments, blank lines, garbage and a second line of the same lot are left out.
        let text = format!("{PENDING_HEADER}\n{line}\nnot a grant\n12 61700 1 1:2:3 50006170\n{}\n0 5 1 1:1:1:1 5\n9 5 1 1:1:1:1 \n", pending_line(&Grant { id: 78, ..g.clone() }));
        assert_eq!(parse_pending(&text), vec![g]);
    }

    #[test]
    fn pending_queue_dedupes_and_caps() {
        let mut q = Vec::new();
        assert!(pending_add(&mut q, grant(1, 61_700)));
        // The same one-time lot again (a resend, a second capture) or the same event: no.
        assert!(!pending_add(&mut q, grant(2, 61_700)));
        assert!(!pending_add(&mut q, grant(1, 13_604_000)));
        // A repeatable lot (no flags) is never queued.
        assert!(!pending_add(&mut q, Grant { flags: vec![], ..grant(3, 2_499) }));
        assert!(pending_add(&mut q, grant(4, 13_604_000)));
        assert_eq!(q.iter().map(|g| g.lot).collect::<Vec<_>>(), vec![61_700, 13_604_000]);
        let gone = pending_remove(&mut q, &[1, 999]);
        assert_eq!(gone.iter().map(|g| g.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(q.len(), 1);
        for i in 0..(MAX_PENDING as u32 + 5) {
            pending_add(&mut q, grant(100 + i, 100 + i as i32));
        }
        assert_eq!(q.len(), MAX_PENDING);
        assert_eq!(q.last().map(|g| g.id), Some(100 + MAX_PENDING as u32 + 4));
    }

    #[test]
    fn pending_file_persists() {
        let dir = std::env::temp_dir().join(format!("sekiro-coop-test-{}-{}", std::process::id(), game::now_ms()));
        let path = dir.join("pending_grants.txt");
        assert!(load_pending_from(&path).is_empty());
        append_pending_to(&path, &grant(1, 61_700)).unwrap();
        append_pending_to(&path, &grant(2, 13_604_000)).unwrap();
        // An appended repeat of a lot reads back once.
        append_pending_to(&path, &grant(3, 61_700)).unwrap();
        let mut q = load_pending_from(&path);
        assert_eq!(q.iter().map(|g| g.id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# Co-op mod"));
        pending_remove(&mut q, &[1]);
        save_pending_to(&path, &q).unwrap();
        assert_eq!(load_pending_from(&path), vec![grant(2, 13_604_000)]);
        // Empty: the file goes away.
        save_pending_to(&path, &[]).unwrap();
        assert!(!path.exists());
        save_pending_to(&path, &[]).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ack_and_ledger_round_trip() {
        assert_eq!(decode_ack(&encode_ack(&[1, 0xFFFF_FFFF])), Some(vec![1, 0xFFFF_FFFF]));
        assert_eq!(decode_ack(&encode_ack(&[])), Some(vec![]));
        let a = encode_ack(&[5]);
        assert_eq!(decode_ack(&a[..a.len() - 1]), None);
        let d = encode_ledger(LEDGER_REST, &[61_700, 13_604_000, -1]);
        assert_eq!(decode_ledger(&d), Some((LEDGER_REST, vec![61_700, 13_604_000, -1])));
        let mut v2 = d.clone();
        v2[0] = 2;
        assert_eq!(decode_ledger(&v2), None);
        let mut long = d;
        long.push(0);
        assert_eq!(decode_ledger(&long), None);
    }

    #[test]
    fn ledger_diff() {
        // Ours: rows -> flag. 51600 / 61600 share the Bell Charm flag; 1100050 is treasure
        // (not a reconcile row here); 2000 is on here already.
        let rows: HashMap<i32, u32> =
            [(51_600, 50_006_160), (61_600, 50_006_160), (61_700, 50_006_170), (2_000, 50_002_000), (13_604_000, 51_360_400)].into();
        let on: HashSet<u32> = [50_002_000].into();
        let theirs = [61_700, 51_600, 61_600, 1_100_050, 2_000, 13_604_000, 61_700];
        let plan = ledger_plan(&theirs, |r| rows.get(&r).copied(), |f| on.contains(&f));
        assert_eq!(plan, vec![61_700, 51_600, 13_604_000]);
        // Everything on here: nothing to do.
        assert!(ledger_plan(&theirs, |r| rows.get(&r).copied(), |_| true).is_empty());
        // A row whose flag is 0 here is never given.
        assert!(ledger_plan(&[7], |_| Some(0), |_| false).is_empty());
    }

    #[test]
    fn queued_grant_done_when_every_peer_acked() {
        let by: HashMap<u32, BTreeSet<PeerId>> = [(1, [PeerId(1)].into()), (2, [PeerId(1), PeerId(2)].into())].into();
        // Two players: the friend's ack is enough.
        assert_eq!(fully_acked(&by, &[PeerId(1)]), vec![1, 2]);
        // Three: both clients must ack.
        assert_eq!(fully_acked(&by, &[PeerId(1), PeerId(2)]), vec![2]);
        assert!(fully_acked(&by, &[]).is_empty());
    }

    #[test]
    fn owned_rule() {
        // Lot 2499 (no lot flags): our own award ran -> ours (the live double); else give.
        assert_eq!(owned_check(false, false, true, &[]), Owned::OurAward);
        assert_eq!(owned_check(false, false, false, &[]), Owned::Give);
        // Lot flags on: ours only when every unique item is owned (none unique = ours).
        assert_eq!(owned_check(true, true, false, &[true, true]), Owned::FlagsAndItems);
        assert_eq!(owned_check(true, true, false, &[]), Owned::FlagsAndItems);
        assert_eq!(owned_check(true, true, false, &[true, false]), Owned::Missing);
        // Our award beats everything; flags off: give.
        assert_eq!(owned_check(true, true, true, &[false]), Owned::OurAward);
        assert_eq!(owned_check(true, false, false, &[]), Owned::Give);
    }

    #[test]
    fn unique_items() {
        assert!(unique(GOODS | 2470, Some(KEY)));
        assert!(!unique(GOODS | 4000, Some(BEADS)));
        assert!(unique(700_000, None));
        assert!(!unique(GOODS | 1, None));
    }
}
