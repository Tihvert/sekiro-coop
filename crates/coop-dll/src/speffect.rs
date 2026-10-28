//! Own SpEffect rows for the death debuff (SPEC-DEBUFF-SCALING.md §2.4,
//! phase 2): five rows 2140000001..5 (one per stack); the row for the current
//! stack count is kept on the main player. Each row lowers max HP and max
//! posture by `pct` % per stack and attack power by `ATTACK_PCT_PER_STACK` %
//! per stack, and shows an existing debuff icon on the HUD.
//!
//! Version 2 (2026-10-23, after the live bug "Inspect on doors does nothing"
//! with version 1): the SpEffectParam file is NOT replaced any more. Version 1
//! built a new file buffer and switched `FD4ParamResCap.data` to it; at exit
//! the game/me3 freed that Rust-heap buffer as if it were a game allocation
//! (`me3_mod_host+0xaa95d`, rdx = our file - 0x10), and its rows were a copy
//! of row 4, of which only ~30 of the ~270 fields had been checked. Now:
//! - the game's only SpEffect row lookup `0x10CEC30(ParamRef* out, id)` is
//!   detoured; ids 2140000001..5 get our static rows (DLL memory, never freed
//!   by anyone), every other id goes to the original unchanged;
//! - rows start from the per-byte MODE of all vanilla rows (the default value
//!   of every field, so no behaviour / camouflage / session / target oddities
//!   of one particular row), take the effect-target bits from the icon donor
//!   (a vanilla player debuff), then get our values;
//! - a self-test runs before the rows are enabled: every vanilla id looked up
//!   through the hooked function must give exactly the row the file holds.
//!
//! IDA (quick_re7 copy `F:\Temp\ida_sp`, RVAs):
//! - `0x10CEC30`: holder 15 → `file = *(*(rescap+0x70)+0x70)`; binary search in
//!   `{u32 id, u32 index}` at `file + align16(u32 file-0x10)`; not found →
//!   row 0; writes `out{+0 id, +8 row, +0x10 u8 paramdef version}`; void.
//!   Prologue `40 57 48 83 EC 40 48 C7 44 24 20 FE FF FF FF` (15 B, no RIP use).
//!   Callers include add (`0x9F8320`), node init (`0xC03AF0`) and the ObjAct
//!   SpEffect qualification `0x6B3DE0`. The only other holder-15 reader is
//!   `0x10CE6E0`, used by the debug text `0xC03C30` only (not hooked).
//! - `0x9E9C00(chr, id, source, 0, 0, &{1,1,1,1}, 1.0, force, 0, 0)` adds,
//!   `0xBFEFB0(list, id)` tests, `0xC00340(list, id)` marks for removal;
//!   list = `*(chr+0x11D0)`; nodes cache the row pointer at `+0x60`.
//! - Max HP `0xC01940` → `0xC04E30` reads `+0x10`; max posture `0xBFD500` →
//!   `0xC04FB0` reads `+0x18` (maxStaminaRate); NG+ factor when `+0x168`
//!   bit 6. Attack power rates `0xC04280` kinds 0, 13..16 = `+0x48..+0x54`,
//!   `+0x1EC`.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::game::{module_base, read, read_bytes, read_ptr};
use crate::log;

/// Row ids are `ROW_ID_BASE + stacks` (1..=ROWS).
pub const ROW_ID_BASE: u32 = 2_140_000_000;
pub const ROWS: u32 = 5;
/// sizeof(SP_EFFECT_PARAM_ST) (vendor generated.rs).
pub const ROW_SIZE: usize = 0x440;
/// Attack power lost per stack, percent (spec: 1 - 0.05k).
pub const ATTACK_PCT_PER_STACK: u32 = 5;
/// Icon donor override (`None` = `pick_icon`). The live log of 2026-10-23
/// picked row 3361 (icon 2110).
pub const ICON_ROW_ID: Option<u32> = None;
const TICK: Duration = Duration::from_millis(500);

const STRUCT_NAME: &str = "SP_EFFECT_PARAM_ST";
/// Holder index the game's own lookup `0x10CEC30` uses.
const SP_EFFECT_HOLDER: usize = 15;
const CAP_DATA: usize = 0x70;

const LOOKUP_RVA: usize = 0x10CEC30;
const LOOKUP_PROLOGUE: [u8; 15] =
    [0x40, 0x57, 0x48, 0x83, 0xEC, 0x40, 0x48, 0xC7, 0x44, 0x24, 0x20, 0xFE, 0xFF, 0xFF, 0xFF];
const WORLD_CHR_MAN: usize = 0x3D7A1E0;
const WCM_MAIN_PLAYER: usize = 0x88;
const CHR_SP_LIST: usize = 0x11D0;
const CHR_MODULES: usize = 0x1FF8;
const MOD_DATA: usize = 0x18;
const DATA_HP: usize = 0x130;
const APPLY_RVA: usize = 0x9E9C00;
const HAS_RVA: usize = 0xBFEFB0;
const REMOVE_RVA: usize = 0xC00340;
/// `mov rax, rsp; mov [rax+8], rbx; mov [rax+10h], rbp` (IDA bytes at 0x9E9C00).
const APPLY_PROLOGUE: [u8; 11] = [0x48, 0x8B, 0xC4, 0x48, 0x89, 0x58, 0x08, 0x48, 0x89, 0x68, 0x10];

/// SP_EFFECT_PARAM_ST offsets (vendor generated.rs, computed from field order).
mod f {
    pub const ICON_ID: usize = 0x00;
    pub const EFFECT_ENDURANCE: usize = 0x08;
    pub const MAX_HP_RATE: usize = 0x10;
    pub const MAX_MP_RATE: usize = 0x14;
    pub const MAX_STAMINA_RATE: usize = 0x18;
    pub const ATTACK_POWER_RATES: usize = 0x48;
    pub const SP_CATEGORY: usize = 0x146;
    pub const CATEGORY_PRIORITY: usize = 0x148;
    pub const SAVE_CATEGORY: usize = 0x149;
    pub const STATE_INFO: usize = 0x15E;
    /// effectTargetSelf/Friend/Enemy/Player/AI/Live/Ghost/WhiteGhost.
    pub const BITS_167: usize = 0x167;
    /// bit 0 targetBlackGhost, 1 targetAttacker, 2 dispIconNonactive, 6 bGameClearBonus.
    pub const BITS_168: usize = 0x168;
    /// bit 1 bCurrHpIndependeMaxHp (0xC04E30 returns 1.0 when set).
    pub const BITS_16A: usize = 0x16A;
    /// bit 0 effectTargetPcOnlyTarget.
    pub const BITS_16C: usize = 0x16C;
    /// vowType0..15.
    pub const BITS_16D: usize = 0x16D;
    pub const BITS_16E: usize = 0x16E;
    /// bits 0..2 effectTargetOpposeTarget/FriendlyTarget/SelfTarget.
    pub const BITS_174: usize = 0x174;
    /// bit 3 effectTargetPcDeceased.
    pub const BITS_275: usize = 0x275;
    pub const DARK_ATTACK_POWER_RATE: usize = 0x1EC;
}

// ---------------------------------------------------------------------------
// Pure part (unit tested).

fn rd_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_i32(b: &[u8], o: usize) -> i32 {
    rd_u32(b, o) as i32
}
fn rd_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn rd_f32(b: &[u8], o: usize) -> f32 {
    f32::from_bits(rd_u32(b, o))
}
fn wr_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn wr_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn wr_f32(b: &mut [u8], o: usize, v: f32) {
    wr_u32(b, o, v.to_bits());
}

const fn align16(n: usize) -> usize {
    (n + 15) & !15
}

/// Bytes before the file: `{u32 after_name_offset, ...}` (vendor ParamFileMetadata).
pub const META: usize = 0x10;
const HDR: usize = 0x40;
const DESC: usize = 24;
const H_VERSION: usize = 0x08;
const H_ROW_COUNT: usize = 0x0A;
const H_FORMAT_2D: usize = 0x2D;
const H_FORMAT_2E: usize = 0x2E;

/// One row of a parsed snapshot (offsets are file relative).
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub id: u32,
    pub data: usize,
}

/// A param file snapshot: `[metadata 0x10][file .. after][pad][lookup 8*n]`.
pub struct Parsed<'a> {
    snap: &'a [u8],
    row_size: usize,
    pub rows: Vec<Row>,
    /// `{id, index}` in file order (sorted by id).
    pub lookup: Vec<(u32, u32)>,
}

impl<'a> Parsed<'a> {
    pub fn count(&self) -> usize {
        self.rows.len()
    }
    pub fn row_bytes(&self, r: &Row) -> &'a [u8] {
        &self.snap[META + r.data..META + r.data + self.row_size]
    }
    pub fn find(&self, id: u32) -> Option<&Row> {
        let i = self.lookup.binary_search_by(|e| e.0.cmp(&id)).ok()?;
        self.rows.get(self.lookup[i].1 as usize)
    }
    /// Rows in id order.
    pub fn by_id(&self) -> impl Iterator<Item = &Row> + '_ {
        self.lookup.iter().map(|&(_, i)| &self.rows[i as usize])
    }
    pub fn version(&self) -> u16 {
        rd_u16(self.snap, META + H_VERSION)
    }
}

/// Snapshot length for a file whose `after_name_offset` and row count are known.
pub fn snapshot_len(after: usize, count: usize) -> usize {
    META + align16(after) + 8 * count
}

/// Parses a snapshot the way `0x10CEC30` reads the file (format 5, 0x2D bit 7,
/// 0x2E bit 1: 64-bit descriptors) and checks every offset it relies on.
pub fn parse(snap: &[u8], row_size: usize) -> Result<Parsed<'_>, String> {
    if snap.len() < META + HDR {
        return Err("snapshot too short".into());
    }
    let after = rd_u32(snap, 0) as usize;
    if after < HDR || after > snap.len() - META {
        return Err(format!("after_name_offset {after:#x} out of range"));
    }
    let file = &snap[META..META + after];
    let (f2d, f2e) = (file[H_FORMAT_2D], file[H_FORMAT_2E]);
    if f2d & 0x7F != 5 || f2d & 0x80 == 0 || f2e & 2 == 0 {
        return Err(format!("unexpected param format {f2d:#x}/{f2e:#x}"));
    }
    let n = rd_u16(file, H_ROW_COUNT) as usize;
    if snap.len() < snapshot_len(after, n) {
        return Err("snapshot shorter than its lookup table".into());
    }
    let desc_end = HDR + DESC * n;
    if desc_end > after {
        return Err("row descriptors run past the file".into());
    }
    let mut rows = Vec::with_capacity(n);
    for i in 0..n {
        let d = HDR + DESC * i;
        let r = Row { id: rd_u32(file, d), data: rd_u64(file, d + 8) as usize };
        if r.data < desc_end || r.data + row_size > after {
            return Err(format!("row {} data {:#x} out of range", r.id, r.data));
        }
        rows.push(r);
    }
    let lt = META + align16(after);
    let mut lookup = Vec::with_capacity(n);
    for i in 0..n {
        let (id, idx) = (rd_u32(snap, lt + 8 * i), rd_u32(snap, lt + 8 * i + 4));
        if idx as usize >= n || rows[idx as usize].id != id {
            return Err(format!("lookup entry {i} ({id}, {idx}) does not match the descriptors"));
        }
        if lookup.last().is_some_and(|&(p, _)| p >= id) {
            return Err(format!("lookup table not sorted at {i}"));
        }
        lookup.push((id, idx));
    }
    Ok(Parsed { snap, row_size, rows, lookup })
}

/// The most common value of every byte over all rows: each field's default
/// (nearly every row leaves nearly every field at its default).
pub fn mode_row<'a>(rows: impl Iterator<Item = &'a [u8]>, row_size: usize) -> Vec<u8> {
    let mut counts = vec![[0u32; 256]; row_size];
    for r in rows {
        for (c, &b) in counts.iter_mut().zip(r) {
            c[b as usize] += 1;
        }
    }
    // Ties go to the lower byte value (max_by_key keeps the last max, so reverse).
    counts.iter().map(|c| (0..256usize).rev().max_by_key(|&v| c[v]).unwrap() as u8).collect()
}

/// Byte offsets where `a` and `b` differ, as `off:aa>bb` (at most `max`).
pub fn diff(a: &[u8], b: &[u8], max: usize) -> String {
    let all: Vec<usize> = (0..a.len().min(b.len())).filter(|&i| a[i] != b[i]).collect();
    let mut s: Vec<String> = all.iter().take(max).map(|&i| format!("{i:x}:{:02x}>{:02x}", a[i], b[i])).collect();
    if all.len() > max {
        s.push(format!("(+{} more)", all.len() - max));
    }
    format!("{} bytes [{}]", all.len(), s.join(" "))
}

/// Icon donor: the lowest-id row with an icon that lowers max HP, max posture
/// or physical attack power (a debuff), else the lowest-id row with an icon.
pub fn pick_icon(p: &Parsed) -> Option<(u32, i32)> {
    let with_icon = || p.by_id().map(|r| (r.id, p.row_bytes(r))).filter(|(_, b)| rd_i32(b, f::ICON_ID) > 0);
    let debuff = |b: &[u8]| {
        [f::MAX_HP_RATE, f::MAX_STAMINA_RATE, f::ATTACK_POWER_RATES].iter().any(|&o| {
            let v = rd_f32(b, o);
            v > 0.0 && v < 1.0
        })
    };
    with_icon().find(|(_, b)| debuff(b)).or_else(|| with_icon().next()).map(|(id, b)| (id, rd_i32(b, f::ICON_ID)))
}

/// Percent kept after `k` stacks of `pct` % (never below 5 %, like phase 1).
pub fn kept(pct: u32, k: u32) -> f32 {
    (100i64 - pct as i64 * k as i64).max(5) as f32 / 100.0
}

/// Our row for `k` stacks: the default row (`mode_row`), the effect-target
/// bits of `donor` (a vanilla player debuff, so the add path accepts the
/// player the same way), then our values.
pub fn make_row(template: &[u8], donor: &[u8], k: u32, pct: u32, icon: i32) -> Vec<u8> {
    let mut r = template.to_vec();
    for o in [f::BITS_167, f::BITS_16D, f::BITS_16E] {
        r[o] = donor[o];
    }
    let take = |r: &mut Vec<u8>, o: usize, mask: u8| r[o] = (r[o] & !mask) | (donor[o] & mask);
    take(&mut r, f::BITS_168, 0x07);
    take(&mut r, f::BITS_16C, 0x01);
    take(&mut r, f::BITS_174, 0x07);
    take(&mut r, f::BITS_275, 0x08);
    let keep = kept(pct, k);
    let atk = kept(ATTACK_PCT_PER_STACK, k);
    wr_u32(&mut r, f::ICON_ID, icon as u32);
    wr_f32(&mut r, f::EFFECT_ENDURANCE, -1.0);
    wr_f32(&mut r, f::MAX_HP_RATE, keep);
    wr_f32(&mut r, f::MAX_STAMINA_RATE, keep);
    wr_f32(&mut r, f::MAX_MP_RATE, 1.0);
    for i in 0..4 {
        wr_f32(&mut r, f::ATTACK_POWER_RATES + 4 * i, atk);
    }
    wr_f32(&mut r, f::DARK_ATTACK_POWER_RATE, atk);
    // No replacement rules against other effects, never saved, no state.
    wr_u16(&mut r, f::SP_CATEGORY, 0);
    r[f::CATEGORY_PRIORITY] = 0;
    r[f::SAVE_CATEGORY] = -1i8 as u8;
    wr_u16(&mut r, f::STATE_INFO, 0);
    r[f::BITS_168] &= !0x40;
    r[f::BITS_16A] &= !0x02;
    r
}

// ---------------------------------------------------------------------------
// Lookup detour (any thread: the game looks params up from loader threads too).

/// `ParamRef` as `0x10CEC30` writes it.
#[repr(C)]
struct ParamRef {
    id: i32,
    _pad: u32,
    row: usize,
    version: u8,
}

#[repr(C, align(16))]
struct RowMem(UnsafeCell<[[u8; ROW_SIZE]; ROWS as usize]>);
// Written only before ROWS_ON is set (Release), read only after (Acquire).
unsafe impl Sync for RowMem {}
static ROW_MEM: RowMem = RowMem(UnsafeCell::new([[0; ROW_SIZE]; ROWS as usize]));
static ROWS_ON: AtomicBool = AtomicBool::new(false);
static ROW_VERSION: AtomicUsize = AtomicUsize::new(0);
static LOOKUP_TRAMP: AtomicUsize = AtomicUsize::new(0);

type LookupFn = extern "C" fn(*mut ParamRef, i32);

/// One of our death debuff ids (only this machine has these rows; native.rs
/// hides them from the game's SpEffect list packet).
pub fn is_own_row(id: i32) -> bool {
    (1..=ROWS).contains(&(id as u32).wrapping_sub(ROW_ID_BASE))
}

fn our_row(id: i32) -> Option<usize> {
    let k = (id as u32).wrapping_sub(ROW_ID_BASE);
    if !(1..=ROWS).contains(&k) || !ROWS_ON.load(Ordering::Acquire) {
        return None;
    }
    Some(unsafe { (*ROW_MEM.0.get())[k as usize - 1].as_ptr() } as usize)
}

extern "C" fn lookup_detour(out: *mut ParamRef, id: i32) {
    if let Some(row) = our_row(id) {
        unsafe {
            (*out).id = id;
            (*out).row = row;
            (*out).version = ROW_VERSION.load(Ordering::Relaxed) as u8;
        }
        return;
    }
    let orig: LookupFn = unsafe { std::mem::transmute(LOOKUP_TRAMP.load(Ordering::Acquire)) };
    orig(out, id);
}

/// At startup (title screen, before any character exists), only when
/// `death_debuff_icon` is on: our ids pass through until the rows are built.
pub fn install_hook() {
    if crate::tuning::DEATH_DEBUFF_ICON.load(Ordering::Relaxed) == 0 {
        return;
    }
    let detour: LookupFn = lookup_detour;
    if let Some(page) =
        crate::hook::install_detour(LOOKUP_RVA, &LOOKUP_PROLOGUE, detour as usize, "SpEffect lookup (0x10CEC30)")
    {
        LOOKUP_TRAMP.store(page, Ordering::Release);
    }
}

/// The game's lookup as every caller sees it (through our detour).
fn game_lookup(id: u32) -> (usize, u8) {
    let f: LookupFn = unsafe { std::mem::transmute(module_base() + LOOKUP_RVA) };
    let mut out = ParamRef { id: 0, _pad: 0, row: 0, version: 0 };
    f(&mut out, id as i32);
    (out.row, out.version)
}

// ---------------------------------------------------------------------------
// Runtime part (game thread only).

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Unknown,
    Ready,
    Failed,
}

/// 16-byte aligned like `{1,1,1,1}` at 0x328AE90 (the game loads it with movaps).
#[repr(C, align(16))]
struct Vec4([f32; 4]);
static ONE4: Vec4 = Vec4([1.0; 4]);

type ApplyFn = extern "C" fn(usize, i32, usize, u8, u8, *const [f32; 4], f32, u8, i32, u8) -> u8;
type ListFn = extern "C" fn(usize, i32) -> u8;
type RemoveFn = extern "C" fn(usize, i32) -> usize;

pub struct Effects {
    state: State,
    next_tick: Option<Instant>,
    /// Stack row currently kept on the player (0 = none).
    pub active: u32,
    last_log: Option<(u32, u8)>,
}

impl Default for Effects {
    fn default() -> Self {
        Effects { state: State::Unknown, next_tick: None, active: 0, last_log: None }
    }
}

enum Error {
    /// Params not loaded yet: try again later, quietly.
    NotYet,
    Fatal(String),
}

impl Effects {
    /// True while our rows resolve (builds and self-tests them on the first call).
    /// `icon_setting`: 1 = picked icon, > 1 = that icon id.
    pub fn ready(&mut self, pct: u32, icon_setting: u32) -> bool {
        match self.state {
            State::Failed => return false,
            State::Ready => return true,
            State::Unknown => {}
        }
        match build(pct, icon_setting) {
            Ok(()) => {
                self.state = State::Ready;
                true
            }
            Err(Error::NotYet) => false,
            Err(Error::Fatal(e)) => {
                ROWS_ON.store(false, Ordering::Release);
                log::line(&format!("speffect: rows not enabled: {e}; death debuff falls back to base value writes"));
                self.state = State::Failed;
                false
            }
        }
    }

    /// Every frame while phase 2 is on: every 0.5 s keeps row `k` (0 = none)
    /// on the main player and removes the other stack rows.
    pub fn tick(&mut self, k: u32) {
        let now = Instant::now();
        if self.next_tick.is_some_and(|t| now < t) {
            return;
        }
        self.next_tick = Some(now + TICK);
        let base = module_base();
        let Some(player) = read_ptr(base + WORLD_CHR_MAN).and_then(|w| read_ptr(w + WCM_MAIN_PLAYER)) else { return };
        let Some(list) = read_ptr(player + CHR_SP_LIST) else { return };
        let alive = read_ptr(player + CHR_MODULES)
            .and_then(|m| read_ptr(m + MOD_DATA))
            .and_then(|d| read::<i32>(d + DATA_HP))
            .is_some_and(|hp| hp > 0);
        let has: ListFn = unsafe { std::mem::transmute(base + HAS_RVA) };
        let remove: RemoveFn = unsafe { std::mem::transmute(base + REMOVE_RVA) };
        for j in 1..=ROWS {
            let id = (ROW_ID_BASE + j) as i32;
            if j != k && has(list, id) != 0 {
                remove(list, id);
            }
        }
        self.active = 0;
        if k == 0 || k > ROWS || self.state != State::Ready {
            return;
        }
        let id = ROW_ID_BASE + k;
        if has(list, id as i32) != 0 {
            self.active = k;
            return;
        }
        if !alive {
            return;
        }
        let apply: ApplyFn = unsafe { std::mem::transmute(base + APPLY_RVA) };
        let r = apply(player, id as i32, player, 0, 0, &ONE4.0, 1.0, 0, 0, 0);
        if has(list, id as i32) != 0 {
            self.active = k;
        }
        // Every successful apply (after loads and deaths), but a refused one only
        // once per (id, result): it would otherwise log twice a second.
        if self.last_log != Some((id, r)) || self.active == k {
            self.last_log = Some((id, r));
            log::line(&format!("debuff: applied sp {id} (ret {r}, present {})", self.active == k));
        }
    }

    /// Removes every stack row from the player (phase 2 turned off or failed).
    pub fn clear(&mut self) {
        if self.active != 0 {
            self.next_tick = None;
            self.tick(0);
        }
    }
}

/// Builds our rows from the loaded SpEffectParam, self-tests the hooked
/// lookup and enables the rows.
fn build(pct: u32, icon_setting: u32) -> Result<(), Error> {
    use fromsoftware_shared::FromStatic;
    use sekiro::sprj::SoloParamRepository;
    let fatal = |s: String| Error::Fatal(s);
    if LOOKUP_TRAMP.load(Ordering::Acquire) == 0 {
        return Err(fatal("lookup hook not installed (see the hook: line at startup)".into()));
    }
    let base = module_base();
    let mut prologue = [0u8; APPLY_PROLOGUE.len()];
    if !read_bytes(base + APPLY_RVA, &mut prologue) || prologue != APPLY_PROLOGUE {
        return Err(fatal(format!("0x9E9C00 prologue {prologue:02x?} does not match")));
    }
    let repo = unsafe { SoloParamRepository::instance() }.map_err(|_| Error::NotYet)?;
    let holder = repo.solo_param_holders.get(SP_EFFECT_HOLDER).ok_or(Error::NotYet)?;
    let rc = holder.get_res_cap(0).ok_or(Error::NotYet)?;
    let cap_ref: &sekiro::fd4::FD4ParamResCap = &rc.param_res_cap;
    if cap_ref.struct_name() != STRUCT_NAME {
        return Err(fatal(format!("holder {SP_EFFECT_HOLDER} is {:?}, not {STRUCT_NAME}", cap_ref.struct_name())));
    }
    let cap = cap_ref as *const _ as usize;
    let file = read_ptr(cap + CAP_DATA).ok_or(Error::NotYet)?;
    let after = read::<u32>(file - META).ok_or_else(|| fatal("metadata unreadable".into()))? as usize;
    let count = read::<u16>(file + H_ROW_COUNT).ok_or_else(|| fatal("header unreadable".into()))? as usize;
    let len = snapshot_len(after, count);
    if len > 256 << 20 {
        return Err(fatal(format!("file size {after:#x} is implausible")));
    }
    let mut snap = vec![0u8; len];
    if !read_bytes(file - META, &mut snap) {
        return Err(fatal(format!("could not read {len} bytes at {:#x}", file - META)));
    }
    let p = parse(&snap, ROW_SIZE).map_err(fatal)?;
    for k in 1..=ROWS {
        if p.find(ROW_ID_BASE + k).is_some() {
            return Err(fatal(format!("row {} already exists", ROW_ID_BASE + k)));
        }
    }
    log::line(&format!("speffect: SpEffectParam {} rows at {file:#x} (file not modified)", p.count()));
    log_icon_rows(&p);

    let template = mode_row(p.by_id().map(|r| p.row_bytes(r)), ROW_SIZE);
    let (donor_id, mut icon) = match ICON_ROW_ID {
        Some(id) => p.find(id).map(|r| (id, rd_i32(p.row_bytes(r), f::ICON_ID))),
        None => pick_icon(&p),
    }
    .ok_or_else(|| fatal("no icon donor row".into()))?;
    if icon_setting > 1 {
        icon = icon_setting as i32;
    }
    let donor = p.row_bytes(p.find(donor_id).unwrap());
    log::line(&format!("speffect: template = per-byte mode of {} rows; icon {icon}, target bits from row {donor_id}", p.count()));
    log::line(&format!("speffect: row {donor_id} vs template: {}", diff(&template, donor, 48)));
    if let Some(r4) = p.find(4) {
        // Version 1 copied row 4 (suspected of breaking ObjAct): what it carried.
        log::line(&format!("speffect: row 4 vs template: {}", diff(&template, p.row_bytes(r4), 48)));
    }

    // Self-test 1: before our rows exist, every vanilla id resolves through the
    // hooked lookup to exactly its row in the file, and ours to nothing.
    let version = (p.version() & 0xFF) as u8;
    let check_vanilla = |when: &str| -> Result<(), Error> {
        for r in &p.rows {
            let (row, v) = game_lookup(r.id);
            if row != file + r.data || v != version {
                return Err(fatal(format!(
                    "self-test ({when}): id {} -> {row:#x} v{v}, file has {:#x} v{version}",
                    r.id,
                    file + r.data
                )));
            }
        }
        Ok(())
    };
    check_vanilla("hook passthrough")?;
    for k in 1..=ROWS {
        if game_lookup(ROW_ID_BASE + k).0 != 0 {
            return Err(fatal(format!("self-test: id {} resolves before our rows exist", ROW_ID_BASE + k)));
        }
    }

    let mem = unsafe { &mut *ROW_MEM.0.get() };
    for k in 1..=ROWS {
        mem[k as usize - 1].copy_from_slice(&make_row(&template, donor, k, pct, icon));
    }
    log::line(&format!("speffect: our row vs template: {}", diff(&template, &mem[0], 48)));
    ROW_VERSION.store(version as usize, Ordering::Relaxed);
    ROWS_ON.store(true, Ordering::Release);

    // Self-test 2: ours resolve to our memory, vanilla still unchanged.
    for k in 1..=ROWS {
        let want = mem[k as usize - 1].as_ptr() as usize;
        let (row, v) = game_lookup(ROW_ID_BASE + k);
        if row != want || v != version {
            return Err(fatal(format!("self-test: id {} -> {row:#x}, expected {want:#x}", ROW_ID_BASE + k)));
        }
    }
    check_vanilla("rows on")?;
    log::line(&format!(
        "speffect: rows {}..{} enabled through the lookup hook; self-test passed for {} vanilla ids",
        ROW_ID_BASE + 1,
        ROW_ID_BASE + ROWS,
        p.count()
    ));
    Ok(())
}

/// Candidate icons for a live pick (SPEC §5): id:icon of rows with an icon,
/// debuff-like rows first, at most 120 of each.
fn log_icon_rows(p: &Parsed) {
    let mut debuffs = Vec::new();
    let mut others = Vec::new();
    for r in p.by_id() {
        let b = p.row_bytes(r);
        let icon = rd_i32(b, f::ICON_ID);
        if icon <= 0 {
            continue;
        }
        let low = |o: usize| {
            let v = rd_f32(b, o);
            v > 0.0 && v < 1.0
        };
        let s = format!("{}:{icon}", r.id);
        if low(f::MAX_HP_RATE) || low(f::MAX_STAMINA_RATE) || low(f::ATTACK_POWER_RATES) {
            debuffs.push(s);
        } else {
            others.push(s);
        }
    }
    for (what, v) in [("debuff", &debuffs), ("other", &others)] {
        for chunk in v.chunks(20).take(6) {
            log::line(&format!("speffect: icon rows ({what}, {} total): {}", v.len(), chunk.join(" ")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RS: usize = 16;

    /// A format-5 file: header, descriptors, pad, rows, struct name, lookup
    /// table (rows given out of id order on purpose).
    fn synthetic(ids: &[u32]) -> Vec<u8> {
        let n = ids.len();
        let data = align16(HDR + DESC * n);
        let mut file = vec![0u8; data + RS * n];
        file[H_FORMAT_2D] = 0x85;
        file[H_FORMAT_2E] = 0x02;
        wr_u16(&mut file, H_VERSION, 7);
        wr_u16(&mut file, H_ROW_COUNT, n as u16);
        for (i, id) in ids.iter().enumerate() {
            let row = &mut file[data + RS * i..data + RS * (i + 1)];
            wr_u32(row, 0, *id);
            row[4] = 0xAB;
            let d = HDR + DESC * i;
            wr_u32(&mut file, d, *id);
            file[d + 8..d + 16].copy_from_slice(&((data + RS * i) as u64).to_le_bytes());
        }
        file.extend_from_slice(b"TEST_PARAM_ST\0");
        let after = file.len();
        let mut snap = vec![0u8; META];
        wr_u32(&mut snap, 0, after as u32);
        snap.extend_from_slice(&file);
        snap.resize(META + align16(after), 0);
        let mut lt: Vec<(u32, u32)> = ids.iter().enumerate().map(|(i, id)| (*id, i as u32)).collect();
        lt.sort_unstable();
        for (id, i) in lt {
            snap.extend_from_slice(&id.to_le_bytes());
            snap.extend_from_slice(&i.to_le_bytes());
        }
        snap
    }

    #[test]
    fn parse_reads_rows_like_the_game() {
        let snap = synthetic(&[300, 100, 200]);
        let p = parse(&snap, RS).unwrap();
        assert_eq!(p.count(), 3);
        assert_eq!(p.version(), 7);
        for id in [100, 200, 300] {
            let b = p.row_bytes(p.find(id).unwrap());
            assert_eq!((rd_u32(b, 0), b[4]), (id, 0xAB));
        }
        assert!(p.find(150).is_none());
        let ids: Vec<u32> = p.by_id().map(|r| r.id).collect();
        assert_eq!(ids, [100, 200, 300]);
    }

    #[test]
    fn parse_rejects_bad_formats_and_tables() {
        let snap = synthetic(&[1, 2]);
        let mut bad = snap.clone();
        bad[META + H_FORMAT_2D] = 0x05;
        assert!(parse(&bad, RS).is_err());
        let mut unsorted = snap.clone();
        let lt = snap.len() - 16;
        unsorted.swap(lt, lt + 8);
        assert!(parse(&unsorted, RS).is_err());
        assert!(parse(&snap[..snap.len() - 1], RS).is_err());
    }

    #[test]
    fn mode_row_takes_the_common_value() {
        let a = [1u8, 2, 3, 4];
        let b = [1u8, 9, 3, 5];
        let c = [7u8, 9, 3, 6];
        let m = mode_row([&a[..], &b[..], &c[..]].into_iter(), 4);
        assert_eq!(m, [1, 9, 3, 4]); // last byte: three-way tie -> lowest value
        assert_eq!(diff(&a, &m, 8), "1 bytes [1:02>09]");
    }

    #[test]
    fn rows_scale_take_target_bits_and_sanitize() {
        let mut template = vec![0u8; ROW_SIZE];
        template[f::BITS_168] = 0xF8;
        template[f::BITS_16A] = 0xFF;
        wr_u16(&mut template, f::SP_CATEGORY, 140);
        let mut donor = vec![0u8; ROW_SIZE];
        donor[f::BITS_167] = 0x29;
        donor[f::BITS_168] = 0x05;
        donor[f::BITS_174] = 0xFF;
        donor[0x20] = 0x55; // anything else of the donor is not taken
        let r = make_row(&template, &donor, 3, 10, 77);
        assert_eq!(r[f::BITS_167], 0x29);
        assert_eq!(r[f::BITS_168], 0xB8 | 0x05); // bit 6 (NG+) cleared, low 3 from donor
        assert_eq!(r[f::BITS_174], 0x07);
        assert_eq!(r[0x20], 0);
        assert_eq!(rd_i32(&r, f::ICON_ID), 77);
        assert_eq!(rd_f32(&r, f::EFFECT_ENDURANCE), -1.0);
        assert!((rd_f32(&r, f::MAX_HP_RATE) - 0.7).abs() < 1e-6);
        assert!((rd_f32(&r, f::MAX_STAMINA_RATE) - 0.7).abs() < 1e-6);
        assert!((rd_f32(&r, f::ATTACK_POWER_RATES + 12) - 0.85).abs() < 1e-6);
        assert!((rd_f32(&r, f::DARK_ATTACK_POWER_RATE) - 0.85).abs() < 1e-6);
        assert_eq!(r[f::SAVE_CATEGORY] as i8, -1);
        assert_eq!(rd_u16(&r, f::SP_CATEGORY), 0);
        assert_eq!(r[f::BITS_16A] & 0x02, 0);
        assert_eq!(kept(10, 5), 0.5);
        assert_eq!(kept(30, 5), 0.05);
    }

    #[test]
    fn our_ids_only_when_enabled() {
        assert!(our_row((ROW_ID_BASE + 1) as i32).is_none());
        assert!(our_row(ROW_ID_BASE as i32).is_none());
        assert!(our_row((ROW_ID_BASE + ROWS + 1) as i32).is_none());
        assert!(our_row(-1).is_none());
    }
}
