//! The joiner keeps their own world (2026-10-23, NOT verified live; IMPLEMENTATION-NOTES §17).
//!
//! The join snapshot (world.rs `apply_dump`) zeroes the joiner's shared event-flag
//! records and copies the host's in; the game autosaves, so before this module the
//! joiner's co-op save kept the host's world for good (live bug: a friend joined a host
//! on an old save and lost bosses in their own world). Now, like Elden Ring Seamless
//! Co-op, the joiner plays in the host's world during the session and gets their own
//! world back when it ends, plus the progress made in the session.
//!
//! - Backup: right before the first join snapshot of a session, every non-zero shared
//!   record of ours (exact bytes, keyed by its base id, which `resolve` maps to the same
//!   (group, area, block, k) record again) goes to memory and to
//!   `%LOCALAPPDATA%\sekiro-coop\own_world_slot<N>.bin` (temp file + rename).
//! - Baseline: right after each join snapshot, the host's world as it is now (same
//!   format). Session progress = a flag of a merged category that is on now and was off
//!   in both the baseline and the backup. Folded into `progress` every 5 s and before a
//!   later snapshot (reconnect, crash), and persisted when it grew.
//! - Restore: when we are no longer a connected client (Severed Thread, the host closed
//!   its world, title screen, link lost for 45 s, or we host now), at a moment the warp
//!   gate allows: zero our shared records, write the backup, OR the progress in, keep the
//!   current per-player flags (items, shops, skills, 9500-9599), raw writes (no event
//!   fires, our SetEventFlag hook sees nothing, nothing is sent to anyone). Since §25 the
//!   restore is armed with an idol warp and written on a loading frame of that warp
//!   (`on_loading`), so only a freshly loaded map ever sees it; a warp that does not load
//!   is retried, never leaving a restored world un-reloaded. The file is deleted afterwards.
//! - Crash / quit: the file survives. The next load of the same save slot without a
//!   session restores it (after 8 s in the world); a join snapshot that comes first
//!   continues from it instead of backing up the host's world as ours.
//! - Key: the save slot index `GameMan(0x3D5C0D0)+0xAE4` (0..9, -1 at the title), set by
//!   the character load job and used by the autosave (`0x140833950`: slot -1 = this
//!   field, slot <= 9; IDA 2026-10-23). Sekiro has no character name to hash (the
//!   character is always Wolf), so the file also stores `total_sen_earned`
//!   (PlayerGameData +0x80, vendor `PlayerInfo` at +0x10, field +0x70 [I]) as a
//!   fingerprint: it only grows for one character, so a smaller value means the slot
//!   now holds another character (the file is set aside as `.stale.bin`, never applied).
//! - The host's world is never backed up: `apply_dump` only runs on the client.
//! - NPC quests (2026-10-23, `merge_quests`, NOT verified live; IMPLEMENTATION-NOTES §19):
//!   quest state is merged per unit, never per flag, so an NPC is never left half in
//!   one world and half in the other. Units (`quest_unit`): the NPC state machines
//!   1000-1999 in blocks of 20 (one NPC each, its death flag is the block's last one)
//!   and the NPC dialogue flags `7xxxxxxx` in blocks of 100 (`merge_quests = 2`: whole
//!   1000-flag records). A quest baseline is taken 10 s after the join load (the load's
//!   own flag resets are in it); a unit one of whose flags was written later (by us
//!   after the 5 s load settle, or by the friend) and that differs from that baseline
//!   when the session ends is taken whole, at its session state; every other unit keeps
//!   our own state. Per-player flags (items, shops, skills, 9500-9599) are never part of
//!   a unit.
//! - Mirror mode (2026-10-23, `own_world_mode = auto|mirror|merge`, NOT verified live;
//!   IMPLEMENTATION-NOTES §22): two players who play the whole game together. At every
//!   join snapshot our own world (backup + progress so far) is compared with the host's:
//!   when every flag on in ours is on in the host's (per-player flags and the time of day
//!   ignored), `auto` ends the session with MIRROR: the shared world stays as it is in
//!   memory (nothing written, no reload; per-player flags were never synced, so they are
//!   ours). Otherwise, and always with `merge`, the merge above.
//! - Restore safety (file version 3): each backup record's resolve tuple (group, block
//!   entry, k, size; world.rs `record_tuple`) is stored; a record that resolves
//!   differently at the restore is not written (it keeps its live bytes, logged). Before
//!   the write: a copy of the co-op save (backups.rs `pre-restore`, waited for up to 20 s)
//!   and a dry-run summary line.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::game::{self, Chr, WarpGate, read, read_ptr};
use crate::hook::{self, FlagEntry};
use crate::net::{self, Intent, Link};
use crate::peers::PeerSet;
use crate::{log, world};

/// What kind of session progress a merged flag is. Everything not in a merged category
/// and not in a quest unit taken from the session (`quest_unit`) goes back to the
/// joiner's own value: global flags outside the param lists (group 0: time of day
/// 9800-9802, the Demon Bell 9830, ...), group 6, and map flags that changed outside a
/// boss-defeat or door window (NPC placement, cutscene state, enemy event states).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Category {
    /// BonfireWarpParam `eventFlagId`: idol unlocked = listed in the travel menu
    /// (RE-FINDINGS-2 §1, SekiroTool's `IdolEventFlags` = `1AABB000n` [V]).
    Idol = 1,
    /// GameProgressParam `eventFlagId`: the game's own progress milestones (enemy
    /// scaling by bosses defeated) [I: from the field names, not traced in IDA].
    GameProgress = 2,
    /// Shared map flags (groups 1/2) first switched on from 20 s before to 15 s after a
    /// boss defeat: our HandleBossDefeat (bossrules.rs), the friend's `EV_BOSS(2)`, or
    /// our boss gauge going away with the boss dead (minibosses, if they use it) [I].
    BossDefeat = 3,
    /// Shared map flags (groups 1/2) first switched on from 2 s before to 10 s after us or
    /// the friend playing an ObjActParam `playerAnimId` (doors, gates, levers, shortcut
    /// ladders) [I: TimeAct id vs param id format not checked live].
    ObjAct = 4,
    /// The rest of an idol's activation: shared flags (groups 0/1/2, not per-player, not
    /// the time of day) that changed, on OR off, from 3 s before to 5 s after an idol's
    /// unlock flag switched on in the session. At the restore they take their session
    /// value, so the idol is left as its activation left it. Live 2026-10-23: activating
    /// idol 1101956 sent `11100006` on, `11100016` off, `236` and `232` on within 3 s;
    /// merging only `11100006` left the idol half activated (sitting threw the player out).
    IdolActivation = 5,
}

/// The categories merged into the restored own world. To extend: add a variant, give it
/// a source (`Lists` for param data, `State::pump` for a time window) and list it here;
/// removing one here stops merging it.
pub const MERGED: &[Category] =
    &[Category::Idol, Category::GameProgress, Category::BossDefeat, Category::ObjAct, Category::IdolActivation];

fn merged(c: Category) -> bool {
    MERGED.contains(&c)
}

fn category_of(v: u8) -> Option<Category> {
    match v {
        1 => Some(Category::Idol),
        2 => Some(Category::GameProgress),
        3 => Some(Category::BossDefeat),
        4 => Some(Category::ObjAct),
        5 => Some(Category::IdolActivation),
        _ => None,
    }
}

const GAME_MAN: usize = 0x3D5C0D0;
/// Save slot of the loaded character (0..9), -1 at the title.
const GM_SAVE_SLOT: usize = 0xAE4;
const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PGD: usize = 0x08;
/// `PlayerInfo.total_sen_earned` (u64): PlayerInfo at PGD+0x10, field at +0x70.
const PGD_TOTAL_SEN: usize = 0x80;
const DEATH_FLAGS: std::ops::RangeInclusive<u32> = 9500..=9599;

const DEFEAT_BEFORE: Duration = Duration::from_secs(20);
const DEFEAT_AFTER: Duration = Duration::from_secs(15);
const OBJACT_BEFORE: Duration = Duration::from_secs(2);
const OBJACT_AFTER: Duration = Duration::from_secs(10);
const RECENT_KEEP: Duration = Duration::from_secs(30);
const FOLD_EVERY: Duration = Duration::from_secs(5);
const SAVE_MIN_GAP: Duration = Duration::from_secs(5);
/// Crash / quit recovery looks for a file this long after a load.
const RECOVER_AFTER_LOAD: Duration = Duration::from_secs(8);
/// No restore before the player has been in the world this long after a load.
const RESTORE_MIN_IN_WORLD: Duration = Duration::from_secs(5);
/// Not a connected client any more for this long before the restore: we left (Thread,
/// host closed its world, title) / no session object / the link was lost (it may come back).
const GRACE_LEFT: Duration = Duration::from_secs(2);
const GRACE_OFF: Duration = Duration::from_secs(10);
const GRACE_LOST: Duration = Duration::from_secs(45);
/// The restore waits this long for its copy of the co-op save (backups.rs).
const PRE_BACKUP_WAIT: Duration = Duration::from_secs(20);
const MAX_RECORD_BYTES: usize = 1024;
const MAGIC: &[u8; 4] = b"SKOW";
/// 2: + quest baseline, touched quest flags, quest flags taken (version 1 still reads).
/// 3: + the resolve tuple of every backed-up record, the mirror/merge subset result
/// (versions 1 and 2 still read: no tuples = the resolve check is skipped).
/// 4: + the story phase masks (`own_phase`, `base_phase`; a v3 file reads with none: the
/// phase is then not restored).
const VERSION: u32 = 4;

/// Where a record resolves (world.rs `record_tuple`): group, block entry index the
/// resolver picked for the id's area/block, k slot, byte size.
pub type Tuple = (u8, u32, u8, u32);

/// `own_world_mode` ([GAMEPLAY], settings.rs): how the joiner's own world comes back.
pub const MODE_AUTO: u8 = 0;
/// The joiner adopts the shared world wholesale (per-player flags stay theirs).
pub const MODE_MIRROR: u8 = 1;
/// Backup + session progress (the §17/§19 merge).
pub const MODE_MERGE: u8 = 2;
pub static OWN_WORLD_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(MODE_AUTO);

/// `auto` / `mirror` / `merge` (empty = auto); anything else: auto and a warning.
pub fn parse_mode(v: &str, warn: &mut Vec<String>) -> u8 {
    match v.trim().to_lowercase().as_str() {
        "" | "auto" | "0" => MODE_AUTO,
        "mirror" | "1" => MODE_MIRROR,
        "merge" | "2" => MODE_MERGE,
        other => {
            warn.push(format!("own_world_mode = {other:?} is not auto/mirror/merge, using auto"));
            MODE_AUTO
        }
    }
}

fn mode() -> u8 {
    OWN_WORLD_MODE.load(Ordering::Relaxed)
}

pub fn mode_name() -> &'static str {
    match mode() {
        MODE_MIRROR => "mirror",
        MODE_MERGE => "merge",
        _ => "auto",
    }
}

/// Time-of-day flags (RE-FINDINGS-2 §1, shared group 0): they follow the story and the
/// host's world, so a difference there never makes the joiner's world "not a subset" [I].
const NOISE: std::ops::RangeInclusive<u32> = 9800..=9802;

/// `merge_quests` values (tuning.rs).
pub const QUEST_OFF: u8 = 0;
pub const QUEST_UNITS: u8 = 1;
pub const QUEST_RECORDS: u8 = 2;
/// NPC state machines: 20 flags per NPC (one-hot states, the last one = dead).
const NPC_STATES: std::ops::RangeInclusive<u32> = 1000..=1999;
const NPC_STATE_BLOCK: u32 = 20;
/// NPC dialogue / quest flags `7AABBkxxx`: 100 flags per block.
const NPC_TALK_BLOCK: u32 = 100;
/// Flag writes in the first seconds after a load are the game's own init (world.rs
/// `SETTLE_AFTER_LOAD`): they never mark a quest unit as touched.
const QUEST_LOAD_SETTLE: Duration = Duration::from_secs(5);
/// The quest baseline is taken this long after the load that follows a join snapshot...
const QUEST_BASE_AFTER_LOAD: Duration = Duration::from_secs(10);
/// ...or this long after the snapshot when no load came (a join without a warp).
const QUEST_BASE_NO_LOAD: Duration = Duration::from_secs(60);

/// When the last boss defeat was seen (`game::now_ms`), set by `note_defeat`.
static DEFEAT_MS: AtomicU64 = AtomicU64::new(0);

/// A boss fell (ours: bossrules.rs HandleBossDefeat; the friend's: world.rs `EV_BOSS(2)`).
/// The flags switched on around it are session progress (`Category::BossDefeat`).
pub fn note_defeat(who: &'static str) {
    DEFEAT_MS.store(game::now_ms(), Ordering::Release);
    log::line(&format!("own world: {who} defeated, flags around it count as session progress"));
}

// ---------------------------------------------------------------- file format (pure)

/// The persisted state of one joiner session (one save slot).
#[derive(Debug, Clone, PartialEq, Default)]
struct Saved {
    slot: i32,
    /// `total_sen_earned` when the backup was taken (fingerprint, see the module doc).
    sen_total: u64,
    /// Unix seconds of the backup.
    created: u64,
    /// Join snapshots applied over this backup.
    snapshots: u32,
    /// Our own world: the non-zero shared records before the first snapshot.
    backup: Vec<(u32, Vec<u8>)>,
    /// The host's world right after the latest snapshot (non-zero records).
    baseline: Vec<(u32, Vec<u8>)>,
    /// Flags to switch on in our own world.
    progress: BTreeSet<u32>,
    /// Flags put in a window category during the session: id -> `Category as u8`.
    classified: BTreeMap<u32, u8>,
    /// The quest records (`quest_record`) as they were settled after the latest join
    /// load (non-zero records); None until taken.
    quest_base: Option<Vec<(u32, Vec<u8>)>>,
    /// Quest flags written in the session after the quest baseline (ids).
    touched: BTreeSet<u32>,
    /// Quest units taken from the session: every flag of them (per-player ones left
    /// out) at its session value.
    quest_taken: BTreeMap<u32, bool>,
    /// Version 3: where each backup record resolved when it was taken (base id -> tuple).
    tuples: BTreeMap<u32, Tuple>,
    /// Version 3: at the latest join snapshot, was our own world (backup + progress) a
    /// subset of the host's? None = not checked yet. With the count of our flags that the
    /// host's world lacked.
    subset: Option<bool>,
    subset_extra: u32,
    /// Version 4: our story phase mask (world.rs `STORY_PHASE`) before the first join
    /// snapshot. The phase flags live outside the shared records, so the backup lacks them.
    own_phase: Option<u32>,
    /// Version 4: the mask right after the latest snapshot applied the host's phase.
    base_phase: Option<u32>,
}

/// FNV-1a, 32 bit: a torn or edited file is rejected, not half applied.
fn checksum(data: &[u8]) -> u32 {
    data.iter().fold(0x811C_9DC5u32, |h, &b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

fn put_records(out: &mut Vec<u8>, recs: &[(u32, Vec<u8>)]) {
    out.extend((recs.len() as u32).to_le_bytes());
    for (base, b) in recs {
        out.extend(base.to_le_bytes());
        out.extend((b.len() as u32).to_le_bytes());
        out.extend(b);
    }
}

fn encode(s: &Saved) -> Vec<u8> {
    encode_version(s, VERSION)
}

/// Version 1 has no quest fields (kept for the test that old files still read).
fn encode_version(s: &Saved, version: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + 140 * (s.backup.len() + s.baseline.len()));
    out.extend(MAGIC);
    out.extend(version.to_le_bytes());
    out.extend(s.slot.to_le_bytes());
    out.extend(s.sen_total.to_le_bytes());
    out.extend(s.created.to_le_bytes());
    out.extend(s.snapshots.to_le_bytes());
    put_records(&mut out, &s.backup);
    put_records(&mut out, &s.baseline);
    out.extend((s.progress.len() as u32).to_le_bytes());
    for id in &s.progress {
        out.extend(id.to_le_bytes());
    }
    out.extend((s.classified.len() as u32).to_le_bytes());
    for (id, c) in &s.classified {
        out.extend(id.to_le_bytes());
        out.push(*c);
    }
    if version >= 2 {
        out.push(s.quest_base.is_some() as u8);
        if let Some(q) = &s.quest_base {
            put_records(&mut out, q);
        }
        out.extend((s.touched.len() as u32).to_le_bytes());
        for id in &s.touched {
            out.extend(id.to_le_bytes());
        }
        out.extend((s.quest_taken.len() as u32).to_le_bytes());
        for (id, on) in &s.quest_taken {
            out.extend(id.to_le_bytes());
            out.push(*on as u8);
        }
    }
    if version >= 3 {
        out.extend((s.tuples.len() as u32).to_le_bytes());
        for (base, (g, entry, k, size)) in &s.tuples {
            out.extend(base.to_le_bytes());
            out.push(*g);
            out.extend(entry.to_le_bytes());
            out.push(*k);
            out.extend(size.to_le_bytes());
        }
        out.push(match s.subset {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        });
        out.extend(s.subset_extra.to_le_bytes());
    }
    if version >= 4 {
        for m in [s.own_phase, s.base_phase] {
            out.push(m.is_some() as u8);
            out.extend(m.unwrap_or(0).to_le_bytes());
        }
    }
    let sum = checksum(&out);
    out.extend(sum.to_le_bytes());
    out
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    fn records(&mut self) -> Option<Vec<(u32, Vec<u8>)>> {
        let n = self.u32()? as usize;
        if n > 100_000 {
            return None;
        }
        let mut v = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            let base = self.u32()?;
            let len = self.u32()? as usize;
            if len > MAX_RECORD_BYTES {
                return None;
            }
            v.push((base, self.take(len)?.to_vec()));
        }
        Some(v)
    }
}

fn decode(data: &[u8]) -> Option<Saved> {
    let (body, sum) = data.split_at(data.len().checked_sub(4)?);
    if checksum(body) != u32::from_le_bytes(sum.try_into().ok()?) {
        return None;
    }
    let mut r = Reader(body);
    if r.take(4)? != MAGIC {
        return None;
    }
    let version = r.u32()?;
    if !(1..=VERSION).contains(&version) {
        return None;
    }
    let mut s = Saved {
        slot: r.u32()? as i32,
        sen_total: r.u64()?,
        created: r.u64()?,
        snapshots: r.u32()?,
        backup: r.records()?,
        baseline: r.records()?,
        ..Default::default()
    };
    for _ in 0..r.u32()? {
        s.progress.insert(r.u32()?);
    }
    for _ in 0..r.u32()? {
        let id = r.u32()?;
        s.classified.insert(id, r.u8()?);
    }
    if version >= 2 {
        if r.u8()? != 0 {
            s.quest_base = Some(r.records()?);
        }
        for _ in 0..r.u32()? {
            s.touched.insert(r.u32()?);
        }
        for _ in 0..r.u32()? {
            let id = r.u32()?;
            s.quest_taken.insert(id, r.u8()? != 0);
        }
    }
    if version >= 3 {
        let n = r.u32()?;
        if n > 100_000 {
            return None;
        }
        for _ in 0..n {
            let base = r.u32()?;
            let t = (r.u8()?, r.u32()?, r.u8()?, r.u32()?);
            s.tuples.insert(base, t);
        }
        s.subset = match r.u8()? {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return None,
        };
        s.subset_extra = r.u32()?;
    }
    if version >= 4 {
        let mut masks = [None, None];
        for m in &mut masks {
            let present = r.u8()?;
            let v = r.u32()?;
            *m = match present {
                0 => None,
                1 => Some(v),
                _ => return None,
            };
        }
        [s.own_phase, s.base_phase] = masks;
    }
    r.0.is_empty().then_some(s)
}

/// The story phase mask to write at the end of the session (world.rs `STORY_PHASE`). The
/// phase flags are not part of the shared records, so the join snapshot never took our own
/// away; the host's was applied separately. `mirror`: the shared world stays, so the live
/// phase does. Otherwise: the session moved the phase (live differs from the host's mask
/// we started the session with) -> the whole live mask, else our own.
fn phase_at_restore(own: u32, base: u32, live: u32, mirror: bool) -> u32 {
    if mirror || live != base { live } else { own }
}

// ---------------------------------------------------------------- flag bits (pure)

/// Base id of the record that holds `id` (a record covers 1000 flags).
fn record_base(id: u32) -> u32 {
    id - id % 1000
}

/// Byte and mask of bit `idx` (= id % 1000) in a record: SetEventFlag sets
/// `dword[idx >> 5] |= 1 << (31 - (idx & 31))` on little-endian dwords (RE-FINDINGS-2 §1).
fn bit_pos(idx: u32) -> (usize, u8) {
    let p = 31 - (idx & 31);
    ((idx >> 5) as usize * 4 + (p / 8) as usize, 1 << (p % 8))
}

fn get_bit(bytes: &[u8], idx: u32) -> bool {
    let (i, m) = bit_pos(idx);
    bytes.get(i).is_some_and(|b| b & m != 0)
}

fn set_bit(bytes: &mut [u8], idx: u32, on: bool) -> bool {
    let (i, m) = bit_pos(idx);
    let Some(b) = bytes.get_mut(i) else { return false };
    if on {
        *b |= m;
    } else {
        *b &= !m;
    }
    true
}

fn index(recs: &[(u32, Vec<u8>)]) -> HashMap<u32, &[u8]> {
    recs.iter().map(|(b, v)| (*b, v.as_slice())).collect()
}

fn flag_in(recs: &HashMap<u32, &[u8]>, id: u32) -> bool {
    recs.get(&record_base(id)).is_some_and(|b| get_bit(b, id % 1000))
}

/// Candidates that are on now but were off in both the host's baseline and our backup.
fn fresh_progress(
    candidates: impl IntoIterator<Item = u32>,
    on_now: impl Fn(u32) -> bool,
    baseline: &[(u32, Vec<u8>)],
    backup: &[(u32, Vec<u8>)],
) -> Vec<u32> {
    let (base, own) = (index(baseline), index(backup));
    candidates.into_iter().filter(|&id| !flag_in(&base, id) && !flag_in(&own, id) && on_now(id)).collect()
}

/// Is our own world (the backup's flags + the progress kept so far) inside the host's
/// world (`baseline`)? Flags `ignore` says (per-player, noise) do not count. Returns
/// (our flags that count, the ones the host's world lacks: first 40, and how many).
fn subset_check(
    backup: &[(u32, Vec<u8>)],
    progress: &BTreeSet<u32>,
    baseline: &[(u32, Vec<u8>)],
    ignore: impl Fn(u32) -> bool,
) -> (usize, Vec<u32>, usize) {
    let host = index(baseline);
    let mut own: BTreeSet<u32> = progress.iter().copied().collect();
    for (base, bytes) in backup {
        for idx in 0..(bytes.len() as u32 * 8).min(1000) {
            if get_bit(bytes, idx) {
                own.insert(base + idx);
            }
        }
    }
    own.retain(|&id| !ignore(id));
    let missing: Vec<u32> = own.iter().copied().filter(|&id| !flag_in(&host, id)).collect();
    let n = missing.len();
    (own.len(), missing.into_iter().take(40).collect(), n)
}

/// Mirror (adopt the shared world as it is) or merge, for the setting and the subset
/// result of the latest join snapshot.
fn use_mirror(setting: u8, subset: Option<bool>) -> bool {
    match setting {
        MODE_MIRROR => true,
        MODE_MERGE => false,
        _ => subset == Some(true),
    }
}

/// Restore safety: keeps the records whose base id resolves now (`now`) where it did when
/// it was backed up (`stored`). A record with no stored tuple (an older file, or a record
/// the backup did not have) is kept unchecked. Returns (records to write, refused (base,
/// then, now), unchecked count).
#[allow(clippy::type_complexity)]
fn resolve_filter(
    recs: Vec<(u32, Vec<u8>)>,
    stored: &BTreeMap<u32, Tuple>,
    now: impl Fn(u32) -> Option<Tuple>,
) -> (Vec<(u32, Vec<u8>)>, Vec<(u32, Tuple, Option<Tuple>)>, usize) {
    let (mut keep, mut refused, mut unchecked) = (Vec::new(), Vec::new(), 0);
    for (base, bytes) in recs {
        match stored.get(&base) {
            None => {
                unchecked += 1;
                keep.push((base, bytes));
            }
            Some(&t) => {
                let n = now(base);
                if n == Some(t) {
                    keep.push((base, bytes));
                } else {
                    refused.push((base, t, n));
                }
            }
        }
    }
    (keep, refused, unchecked)
}

/// The records to write back: the backup, the progress flags switched on, the quest
/// units taken from the session (`quest`: every flag of them, on or off), then the
/// per-player flags (`keep`) at their current value. `size_of` gives the live size of a
/// record the backup does not have (all zero then); a flag whose record has no size is
/// skipped (counted). Returns (records, progress flags set, quest flags written, flags
/// skipped).
fn merge(
    backup: &[(u32, Vec<u8>)],
    progress: &BTreeSet<u32>,
    quest: &BTreeMap<u32, bool>,
    keep: &[(u32, bool)],
    size_of: impl Fn(u32) -> Option<usize>,
) -> (Vec<(u32, Vec<u8>)>, usize, usize, usize) {
    let mut map: BTreeMap<u32, Vec<u8>> = backup.iter().cloned().collect();
    let (mut set, mut quest_set, mut skipped) = (0, 0, 0);
    let record = |map: &mut BTreeMap<u32, Vec<u8>>, base: u32| -> bool {
        if !map.contains_key(&base) {
            let Some(n) = size_of(base).filter(|&n| n > 0 && n <= MAX_RECORD_BYTES) else { return false };
            map.insert(base, vec![0; n]);
        }
        true
    };
    for &id in progress {
        if record(&mut map, record_base(id)) && set_bit(map.get_mut(&record_base(id)).unwrap(), id % 1000, true) {
            set += 1;
        } else {
            skipped += 1;
        }
    }
    // Shared by the quest units and the per-player flags: one flag at an exact value.
    // Some(on) when done, None when its record has no size.
    let put = |map: &mut BTreeMap<u32, Vec<u8>>, id: u32, on: bool| -> Option<bool> {
        let base = record_base(id);
        // Off in a record we do not write = off after the zeroing anyway.
        if !on && !map.contains_key(&base) {
            return Some(false);
        }
        (record(map, base) && set_bit(map.get_mut(&base).unwrap(), id % 1000, on)).then_some(on)
    };
    for (&id, &on) in quest {
        match put(&mut map, id, on) {
            Some(true) => quest_set += 1,
            Some(false) => {}
            None => skipped += 1,
        }
    }
    for &(id, on) in keep {
        if put(&mut map, id, on).is_none() {
            skipped += 1;
        }
    }
    let out = map.into_iter().filter(|(_, b)| b.iter().any(|&x| x != 0)).collect();
    (out, set, quest_set, skipped)
}

/// The quest unit that holds `id` as (first id, flag count), or None when `id` is not
/// NPC quest state or quests are not merged (`mode`, tuning.rs `merge_quests`).
/// Evidence (IMPLEMENTATION-NOTES §19): the NPC state machines live in 1000-1999 in
/// blocks of 20 (Inosuke's mother 1120-1139, set 1139 when her HP hits 0; Kotaro
/// 1440-1459; Doujun 1460-1479; the Exiled Memorial Mob batch-clears 1580-1594 and sets
/// one, 1599 = gone); the NPC dialogue flags are group 7, where FromSoftware's talk
/// scripts give one NPC a block of 100 (Isshin 711005xx, Emma 711100xx, Kuro 711201xx,
/// Anayama's shop conditions 711002xx; the one-hot resets seen in our logs, e.g.
/// 71100449-71100461, never cross a block of 100) [I for group 7].
fn quest_unit(id: u32, mode: u8) -> Option<(u32, u32)> {
    let size = match mode {
        QUEST_UNITS if NPC_STATES.contains(&id) => NPC_STATE_BLOCK,
        QUEST_UNITS if id / 10_000_000 == 7 => NPC_TALK_BLOCK,
        QUEST_RECORDS if NPC_STATES.contains(&id) || id / 10_000_000 == 7 => 1000,
        _ => return None,
    };
    Some((id - id % size, size))
}

/// A record that holds quest units (any mode): the NPC state record 1000 or group 7.
fn quest_record(base: u32) -> bool {
    base == *NPC_STATES.start() || base / 10_000_000 == 7
}

fn quest_records(recs: Vec<(u32, Vec<u8>)>) -> Vec<(u32, Vec<u8>)> {
    recs.into_iter().filter(|(b, bytes)| quest_record(*b) && bytes.iter().any(|&x| x != 0)).collect()
}

/// One quest unit taken from the session.
#[derive(Debug, PartialEq)]
struct TakenUnit {
    start: u32,
    len: u32,
    /// Every flag of the unit except the per-player ones, at its session value.
    flags: Vec<(u32, bool)>,
    /// Flags of it whose session value differs from our own world's.
    differ_own: usize,
    on: usize,
}

/// The quest units whose flags changed in the session: a flag of the unit was written
/// (`touched`) and the unit is different now (`live`) from the quest baseline (`base`),
/// counting a flag that went on or off. Per-player flags are neither compared nor
/// taken. Returns the units to take and the number of touched units that ended as they
/// started (they keep our own state).
fn changed_quest_units(
    touched: &BTreeSet<u32>,
    mode: u8,
    base: &[(u32, Vec<u8>)],
    live: &[(u32, Vec<u8>)],
    own: &[(u32, Vec<u8>)],
    per_player: impl Fn(u32) -> bool,
) -> (Vec<TakenUnit>, usize) {
    let units: BTreeSet<(u32, u32)> = touched.iter().filter_map(|&id| quest_unit(id, mode)).collect();
    let (base, live, own) = (index(base), index(live), index(own));
    let mut out = Vec::new();
    let mut same = 0;
    for (start, len) in units {
        let ids: Vec<u32> = (start..start + len).filter(|&id| !per_player(id)).collect();
        if !ids.iter().any(|&id| flag_in(&live, id) != flag_in(&base, id)) {
            same += 1;
            continue;
        }
        let flags: Vec<(u32, bool)> = ids.iter().map(|&id| (id, flag_in(&live, id))).collect();
        let differ_own = flags.iter().filter(|&&(id, on)| on != flag_in(&own, id)).count();
        let on = flags.iter().filter(|(_, on)| *on).count();
        out.push(TakenUnit { start, len, flags, differ_own, on });
    }
    (out, same)
}

/// Puts `units` into `taken`, replacing what an earlier fold took for the same flags.
fn take_units(taken: &mut BTreeMap<u32, bool>, units: &[TakenUnit]) {
    for u in units {
        let stale: Vec<u32> = taken.range(u.start..u.start + u.len).map(|(id, _)| *id).collect();
        for id in stale {
            taken.remove(&id);
        }
        taken.extend(u.flags.iter().copied());
    }
}

/// Map flags a time window may classify: group 1/2 (`1AABBkxxx`) with a shared `k`
/// (world.rs `should_sync`; the runtime check adds the per-player sets).
fn window_eligible(id: u32) -> bool {
    let top = id / 10_000_000 % 10;
    let k = id / 1000 % 10;
    id >= 10_000_000 && id < 30_000_000 && matches!(top, 1 | 2) && matches!(k, 0 | 1 | 4 | 5 | 6 | 7)
}

/// Flags an idol-activation window may take (on or off): the map flags of
/// `window_eligible` and the shared global ones (group 0, `k` in 0/1/5/9: the live
/// activation set `232` / `236`), never the time of day 9800-9802 or the death range.
fn idol_window_eligible(id: u32) -> bool {
    let k = id / 1000 % 10;
    window_eligible(id)
        || (id > 0 && id < 10_000_000 && matches!(k, 0 | 1 | 5 | 9) && !NOISE.contains(&id) && !DEATH_FLAGS.contains(&id))
}

/// Where the reload goes: our last idol if it is unlocked in the world we write back,
/// else an unlocked idol in the same map (`entity / 10000`), else any unlocked one, else
/// the last idol anyway. `rows`: (entity, unlock flag); `unlocked`: in the restored world.
fn pick_reload_idol(last: i32, rows: &[(i32, u32)], unlocked: impl Fn(u32) -> bool) -> (i32, &'static str) {
    let flag_of = |e: i32| rows.iter().find(|(r, _)| *r == e).map(|(_, f)| *f);
    if flag_of(last).is_none_or(&unlocked) {
        return (last, "our last idol");
    }
    let open: Vec<i32> = rows.iter().filter(|(_, f)| unlocked(*f)).map(|(e, _)| *e).collect();
    if let Some(&e) = open.iter().find(|&&e| e / 10000 == last / 10000) {
        return (e, "an unlocked idol in the same map (our last one is not unlocked in our own world)");
    }
    match open.first() {
        Some(&e) => (e, "an unlocked idol (our last one is not unlocked in our own world)"),
        None => (last, "our last idol (no unlocked idol found)"),
    }
}

/// An animation is an ObjAct's player animation (TimeAct ids carry the anim category
/// in the millions: compare both the full id and the id within its category).
fn objact_anim(anim: i32, set: &HashSet<u32>) -> bool {
    anim > 0 && (set.contains(&(anim as u32)) || set.contains(&(anim as u32 % 1_000_000)))
}

// ---------------------------------------------------------------- game data

/// Param-driven flag lists, read once the params are loaded.
struct Lists {
    idols: Vec<u32>,
    /// (idol entity, unlock flag) of every BonfireWarpParam row.
    idol_rows: Vec<(i32, u32)>,
    progress: Vec<u32>,
    objact_anims: HashSet<u32>,
}

static LISTS: OnceLock<Lists> = OnceLock::new();

fn lists() -> Option<&'static Lists> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{BONFIRE_WARP_PARAM_ST, GAME_PROGRESS_PARAM_ST, OBJ_ACT_PARAM_ST, ParamDef};
    use sekiro::sprj::SoloParamRepository;
    if let Some(l) = LISTS.get() {
        return Some(l);
    }
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    // By struct name, like world.rs `item_flags` (the vendor's table indices are off in 1.06).
    let file = |name: &str| repo.params().find(|p| p.struct_name() == name).map(|p| &*p.data);
    let idol_file = file(BONFIRE_WARP_PARAM_ST::NAME)?;
    let idols: Vec<u32> = unsafe { idol_file.rows::<BONFIRE_WARP_PARAM_ST>() }
        .filter(|(_, r)| r.eventflag_id() > 0)
        .map(|(_, r)| r.eventflag_id() as u32)
        .collect();
    let idol_rows: Vec<(i32, u32)> = unsafe { idol_file.rows::<BONFIRE_WARP_PARAM_ST>() }
        .filter(|(_, r)| r.eventflag_id() > 0 && r.bonfire_entity_id() > 0)
        .map(|(_, r)| (r.bonfire_entity_id(), r.eventflag_id() as u32))
        .collect();
    let progress: Vec<u32> = file(GAME_PROGRESS_PARAM_ST::NAME)
        .map(|f| unsafe { f.rows::<GAME_PROGRESS_PARAM_ST>() }.filter(|(_, r)| r.event_flag_id() > 0).map(|(_, r)| r.event_flag_id() as u32).collect())
        .unwrap_or_default();
    let objact_anims: HashSet<u32> = file(OBJ_ACT_PARAM_ST::NAME)
        .map(|f| {
            unsafe { f.rows::<OBJ_ACT_PARAM_ST>() }
                .map(|(_, r)| r.player_anim_id())
                .filter(|&a| a > 0 && a != u32::MAX)
                .collect()
        })
        .unwrap_or_default();
    let mut sample: Vec<u32> = progress.clone();
    sample.truncate(40);
    let mut anims: Vec<u32> = objact_anims.iter().copied().collect();
    anims.sort_unstable();
    anims.truncate(40);
    log::line(&format!(
        "own world: {} idol flags, {} game progress flags {sample:?}, {} ObjAct player anims {anims:?}",
        idols.len(),
        progress.len(),
        objact_anims.len()
    ));
    Some(LISTS.get_or_init(|| Lists { idols, idol_rows, progress, objact_anims }))
}

fn save_slot() -> Option<i32> {
    read_ptr(game::module_base() + GAME_MAN)
        .and_then(|gm| read::<i32>(gm + GM_SAVE_SLOT))
        .filter(|s| (0..=9).contains(s))
}

fn sen_total() -> Option<u64> {
    read_ptr(game::module_base() + GAME_DATA_MAN)
        .and_then(|g| read_ptr(g + GDM_PGD))
        .and_then(|pgd| read::<u64>(pgd + PGD_TOTAL_SEN))
}

/// The loaded save slot and its `total_sen_earned` (doctor.rs: the slot fingerprint).
pub(crate) fn fingerprint() -> (Option<i32>, Option<u64>) {
    (save_slot(), sen_total())
}

fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop")
}

fn path_for(slot: i32) -> PathBuf {
    dir().join(format!("own_world_slot{slot}.bin"))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Temp file + rename, so a crash mid-write leaves the old file whole.
fn write_file(s: &Saved) -> bool {
    let path = path_for(s.slot);
    let tmp = path.with_extension("bin.tmp");
    let data = encode(s);
    let result = std::fs::create_dir_all(dir())
        .and_then(|_| std::fs::write(&tmp, &data))
        .and_then(|_| std::fs::rename(&tmp, &path));
    match result {
        Ok(()) => true,
        Err(e) => {
            log::line(&format!("own world: could not write {}: {e}", path.display()));
            false
        }
    }
}

fn read_file(slot: i32) -> Option<Saved> {
    let path = path_for(slot);
    let data = std::fs::read(&path).ok()?;
    match decode(&data) {
        Some(s) if s.slot == slot => Some(s),
        _ => {
            log::line(&format!("own world: {} is unreadable ({} B), set aside", path.display(), data.len()));
            set_aside(slot);
            None
        }
    }
}

/// A file we must not apply (another character in the slot, or unreadable).
fn set_aside(slot: i32) {
    let path = path_for(slot);
    let to = path.with_extension("stale.bin");
    let _ = std::fs::rename(&path, &to);
    log::line(&format!("own world: {} moved to {}", path.display(), to.display()));
}

/// The file belongs to the character in the slot now: its total sen never shrinks.
fn same_character(s: &Saved, now: Option<u64>) -> bool {
    now.is_none_or(|n| n >= s.sen_total)
}

fn non_zero(recs: Vec<(u32, Vec<u8>)>) -> Vec<(u32, Vec<u8>)> {
    recs.into_iter().filter(|(_, b)| b.iter().any(|&x| x != 0)).collect()
}

fn bytes(recs: &[(u32, Vec<u8>)]) -> usize {
    recs.iter().map(|(_, b)| b.len()).sum()
}

// ---------------------------------------------------------------- runtime

#[derive(Default)]
struct State {
    active: Option<Saved>,
    dirty: bool,
    saved_at: Option<Instant>,
    fold_at: Option<Instant>,
    ring: Option<u32>,
    /// Window-eligible flags switched on recently (id, when seen).
    recent: VecDeque<(u32, Instant)>,
    defeat_seen: u64,
    defeat_until: Option<Instant>,
    objact_until: Option<Instant>,
    last_anims: [i32; 1 + crate::peers::MAX_PLAYERS],
    gauge: Option<i32>,
    no_client_since: Option<Instant>,
    /// The crash / quit check ran for this load.
    checked_load: bool,
    last_in_world: Duration,
    warp: game::WarpWait,
    classified_logged: u32,
    /// The latest join snapshot (for the quest baseline) and whether a load came since.
    quest_snap_at: Option<Instant>,
    quest_loaded: bool,
    touched_logged: u32,
    /// The pre-restore save copy: (backups.rs ticket, when asked).
    pre_backup: Option<(u64, Instant)>,
    pre_backup_late_logged: bool,
    /// Shared flags that changed recently, on or off (idol-activation window).
    recent_changes: VecDeque<(u32, Instant)>,
    idol_until: Option<Instant>,
    /// Idol flags whose activation window was opened this session (once each).
    idols_opened: HashSet<u32>,
    /// A merge restore waiting for the loading screen of its reload (`on_loading`).
    armed: Option<Armed>,
    /// Reloads that did not lead to a write (no load, or no ready flag manager in it).
    failed_reloads: u32,
}

/// A merge restore armed together with its reload warp (2026-10-23, after the live
/// restore of v6.1: write only where a real load follows).
struct Armed {
    slot: i32,
    /// `items::title_count` when armed: a change = a title trip / save load, not our reload.
    titles: u32,
    idol: i32,
    at: Instant,
    loading_seen: bool,
    mode: ArmMode,
}

enum ArmMode {
    /// Normal: nothing written yet; written on a loading frame of the reload.
    DuringLoad,
    /// Fallback after `FALLBACK_AFTER` reloads without a write: written right before the
    /// warp (the v6.1 order); the live shared records are kept to roll back to if no load
    /// follows within `WARP_START_WAIT`.
    BeforeWarp { rollback: Vec<(u32, Vec<u8>)> },
}

/// A reload warp must show a loading screen within this long (puppet hands off for 20 s).
const WARP_START_WAIT: Duration = Duration::from_secs(25);
/// Reloads without a write before the restore falls back to writing before the warp.
const FALLBACK_AFTER: u32 = 2;
const IDOL_BEFORE: Duration = Duration::from_secs(3);
const IDOL_AFTER: Duration = Duration::from_secs(5);
/// GameMan +0x10: a map load is requested (game.rs `safe_to_warp`, logged only).
const GM_LOAD_REQUEST: usize = 0x10;

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

/// world.rs `apply_dump`, before our shared records are zeroed (client, game thread).
pub fn before_join_snapshot(fm: usize) {
    with_state(|st| {
        let Some(slot) = save_slot() else {
            log::line("own world: save slot unknown, our own world is NOT backed up for this session");
            return;
        };
        let sen = sen_total();
        if st.active.as_ref().is_some_and(|a| a.slot != slot) {
            st.drop_active("another save is loaded");
        }
        if st.active.is_none()
            && let Some(f) = read_file(slot)
        {
            if same_character(&f, sen) {
                log::line(&format!(
                    "own world: an unfinished session of save slot {slot} is on disk (game closed or crashed), continuing from its backup"
                ));
                st.active = Some(f);
            } else {
                log::line(&format!("own world: the file of slot {slot} belongs to another character (sen {} > {sen:?})", f.sen_total));
                set_aside(slot);
            }
        }
        if let Some(a) = st.active.as_mut() {
            // A reconnect (or a session continued after a crash): what changed since the
            // previous snapshot is progress; the backup stays the one of our own world.
            let added = fold(fm, a);
            // Quest units changed since the previous snapshot: taken now, the next
            // snapshot replaces the world they are compared in.
            let (units, _) = fold_quests(fm, a);
            a.snapshots += 1;
            log::line(&format!(
                "own world: join snapshot #{} over the backup of {} (slot {slot}), {added} progress flags folded in ({} in total), {units} quest units taken ({} quest flags in total)",
                a.snapshots,
                a.created,
                a.progress.len(),
                a.quest_taken.len()
            ));
        } else {
            let backup = non_zero(world::read_shared_records(fm));
            log::line(&format!(
                "own world: backed up our own world before the join snapshot: {} non-zero shared records ({} B), save slot {slot}, sen total {sen:?}",
                backup.len(),
                bytes(&backup)
            ));
            // Restore safety: where each record resolves now (checked again before writing it back).
            let tuples: BTreeMap<u32, Tuple> = backup.iter().filter_map(|(b, _)| world::record_tuple(fm, *b).map(|t| (*b, t))).collect();
            log::line(&format!("own world: {} of {} backup records with their resolve tuple", tuples.len(), backup.len()));
            let own_phase = Some(world::read_phase(fm));
            log::line(&format!("own world: our own story phase: {}", world::phase_names(own_phase.unwrap_or(0))));
            let s = Saved {
                slot,
                sen_total: sen.unwrap_or(0),
                created: unix_now(),
                snapshots: 1,
                backup,
                tuples,
                own_phase,
                ..Default::default()
            };
            // On disk before anything is zeroed (the baseline follows right after the copy).
            write_file(&s);
            st.active = Some(s);
            st.idols_opened.clear();
        }
        st.no_client_since = None;
        st.warp.reset();
        // Flag writes before the snapshot are not session progress.
        st.ring = Some(hook::flag_ring_count());
        st.recent.clear();
    });
}

/// world.rs `apply_dump`, after the host's records were copied in: the new baseline.
pub fn after_join_snapshot(fm: usize) {
    with_state(|st| {
        let Some(a) = st.active.as_mut() else { return };
        a.baseline = non_zero(world::read_shared_records(fm));
        // The story phase as the host's snapshot left it (world.rs `apply_phase` ran first).
        a.base_phase = Some(world::read_phase(fm));
        // Mirror or merge (own_world_mode = auto): is our own world inside the host's?
        let (counted, missing, n_missing) = subset_check(&a.backup, &a.progress, &a.baseline, |id| per_player(id) || NOISE.contains(&id));
        a.subset = Some(n_missing == 0);
        a.subset_extra = n_missing as u32;
        log::line(&format!(
            "own world: own_world_mode = {}: our world ({counted} flags on, per-player and time-of-day ignored) {} the host's world{}; at the end of the session: {}",
            mode_name(),
            if n_missing == 0 { "is a subset of".to_string() } else { format!("has {n_missing} flags not in") },
            if missing.is_empty() { String::new() } else { format!(" (first {:?})", missing) },
            if use_mirror(mode(), a.subset) { "mirror (keep the shared world)" } else { "merge (our own world + session progress)" }
        ));
        // The quest baseline follows once the join load has settled (`quest_baseline`).
        a.quest_base = None;
        a.touched.clear();
        st.quest_snap_at = Some(Instant::now());
        st.quest_loaded = false;
        let ok = write_file(a);
        log::line(&format!(
            "own world: baseline = the host's world ({} records, {} B); saved to {} (ok = {ok})",
            a.baseline.len(),
            bytes(&a.baseline),
            path_for(a.slot).display()
        ));
        st.dirty = !ok;
        st.saved_at = Some(Instant::now());
    });
}

/// Progress flags on now and off in the baseline and the backup: added to `progress`.
fn fold(fm: usize, a: &mut Saved) -> usize {
    // No baseline yet (a crash between the backup and the copy): nothing can be told
    // apart from the host's world, so nothing counts.
    if a.baseline.is_empty() {
        return 0;
    }
    let mut cands: Vec<u32> = Vec::new();
    if let Some(l) = lists() {
        if merged(Category::Idol) {
            cands.extend(&l.idols);
        }
        if merged(Category::GameProgress) {
            cands.extend(&l.progress);
        }
    }
    // Idol activation flags are not "on = progress": they take their session value at the
    // restore (`State::restore`, on or off).
    cands.extend(
        a.classified
            .iter()
            .filter(|(_, c)| category_of(**c).is_some_and(|c| merged(c) && c != Category::IdolActivation))
            .map(|(id, _)| *id),
    );
    // Only shared records were replaced by the host's; per-player flags never count.
    cands.retain(|&id| world::should_sync(id) && !a.progress.contains(&id));
    let fresh = fresh_progress(cands, |id| world::get_flag(fm, id), &a.baseline, &a.backup);
    let n = fresh.len();
    a.progress.extend(fresh);
    n
}

/// Items, shops, skills and the death-penalty flags: never part of a quest unit.
fn per_player(id: u32) -> bool {
    DEATH_FLAGS.contains(&id) || world::per_player_state(id) || world::per_player_flags().is_some_and(|s| s.contains(&id))
}

/// Quest units changed in the session -> `quest_taken`. Returns (units taken now,
/// touched units that ended as they started).
fn fold_quests(fm: usize, a: &mut Saved) -> (usize, usize) {
    let mode = crate::tuning::merge_quests();
    let Some(base) = a.quest_base.as_ref() else { return (0, 0) };
    if mode == QUEST_OFF || a.touched.is_empty() {
        return (0, 0);
    }
    let live = quest_records(world::read_shared_records(fm));
    let (units, same) = changed_quest_units(&a.touched, mode, base, &live, &a.backup, per_player);
    for u in &units {
        log::line(&format!(
            "own world: quest unit {}-{} changed in session -> taken ({} flags differ from our own world, {} on)",
            u.start,
            u.start + u.len - 1,
            u.differ_own,
            u.on
        ));
    }
    take_units(&mut a.quest_taken, &units);
    (units.len(), same)
}

impl State {
    fn drop_active(&mut self, why: &str) {
        if let Some(a) = self.active.take() {
            if self.dirty {
                write_file(&a);
            }
            log::line(&format!("own world: {why}; the backup of slot {} stays on disk", a.slot));
        }
        self.dirty = false;
    }

    fn classify(&mut self, id: u32, cat: Category) {
        if !merged(cat) {
            return;
        }
        let Some(a) = self.active.as_mut() else { return };
        if a.classified.insert(id, cat as u8).is_none() {
            self.dirty = true;
            self.classified_logged += 1;
            if self.classified_logged <= 60 {
                log::line(&format!("own world: flag {id} is session progress ({cat:?})"));
            }
        }
    }

    /// A quest flag was written in the session (by our game after the load settle, or
    /// the friend's change applied here): its unit is checked when the session ends.
    fn note_quest_write(&mut self, id: u32) {
        let Some(a) = self.active.as_mut() else { return };
        // Before the baseline the write is part of it.
        if a.quest_base.is_none() || quest_unit(id, crate::tuning::merge_quests()).is_none() || per_player(id) {
            return;
        }
        if a.touched.insert(id) {
            self.dirty = true;
            self.touched_logged += 1;
            if self.touched_logged <= 40 {
                log::line(&format!("own world: quest flag {id} written in the session ({} touched)", a.touched.len()));
            }
        }
    }

    /// The quest baseline: our quest records once the join load has settled (its own
    /// flag resets are in it, so they never count as session changes).
    fn quest_baseline(&mut self, fm: usize, in_world_for: Duration, now: Instant) {
        let Some(at) = self.quest_snap_at else { return };
        let Some(a) = self.active.as_mut() else { return };
        if a.quest_base.is_some() || crate::tuning::merge_quests() == QUEST_OFF {
            return;
        }
        let since = now.duration_since(at);
        let ready = if self.quest_loaded { in_world_for >= QUEST_BASE_AFTER_LOAD } else { since >= QUEST_BASE_NO_LOAD };
        if !ready {
            return;
        }
        let q = quest_records(world::read_shared_records(fm));
        log::line(&format!(
            "own world: quest baseline taken {:.0} s after the join snapshot ({} quest records, {} B, merge_quests = {})",
            since.as_secs_f32(),
            q.len(),
            bytes(&q),
            crate::tuning::merge_quests()
        ));
        a.quest_base = Some(q);
        a.touched.clear();
        self.quest_snap_at = None;
        self.dirty = true;
    }

    /// One flag write (ours or the friend's applied here): an idol's unlock flag switching
    /// on for the first time in the session opens its activation window, and every
    /// eligible change in it is classified `IdolActivation`.
    fn idol_activation(&mut self, id: u32, on: bool, settled: bool, now: Instant) {
        let Some(l) = lists() else { return };
        let is_idol = l.idols.contains(&id);
        if !is_idol && idol_window_eligible(id) && world::should_sync(id) {
            self.recent_changes.push_back((id, now));
            if self.idol_until.is_some_and(|t| now <= t) {
                self.classify(id, Category::IdolActivation);
            }
        }
        // A real activation: in the world (not the load's own rewrites), and the idol was
        // off both in the host's world at the snapshot and in our own.
        if !(is_idol && on && settled) || self.idols_opened.contains(&id) {
            return;
        }
        let Some(a) = self.active.as_ref() else { return };
        if a.baseline.is_empty() || flag_in(&index(&a.baseline), id) || flag_in(&index(&a.backup), id) {
            return;
        }
        self.idols_opened.insert(id);
        log::line(&format!("own world: idol flag {id} switched on in the session: the flags changed around it are its activation"));
        let ids: Vec<u32> =
            self.recent_changes.iter().filter(|(_, t)| now.duration_since(*t) <= IDOL_BEFORE).map(|(i, _)| *i).collect();
        for i in ids {
            self.classify(i, Category::IdolActivation);
        }
        self.idol_until = Some(now + IDOL_AFTER);
    }

    fn open_window(&mut self, cat: Category, before: Duration, after: Duration, now: Instant) {
        let ids: Vec<u32> = self.recent.iter().filter(|(_, t)| now.duration_since(*t) <= before).map(|(id, _)| *id).collect();
        for id in ids {
            self.classify(id, cat);
        }
        let until = Some(now + after);
        match cat {
            Category::BossDefeat => self.defeat_until = until,
            _ => self.objact_until = until,
        }
    }

    /// Boss defeats, ObjActs and the flags switched on around them; quest flag writes
    /// (`settled`: past the first seconds after a load).
    fn pump(&mut self, player: Chr, anims: &[(Option<crate::peers::PeerId>, Option<i32>)], settled: bool, now: Instant) {
        let d = DEFEAT_MS.load(Ordering::Acquire);
        if d != 0 && d != self.defeat_seen {
            self.defeat_seen = d;
            self.open_window(Category::BossDefeat, DEFEAT_BEFORE, DEFEAT_AFTER, now);
        }
        // Minibosses: the gauge goes away with its boss dead while we live [I].
        let gauge = game::boss_state().map(|b| b.entity);
        if let (Some(e), None) = (self.gauge, gauge)
            && e > 0
            && player.hp().is_some_and(|h| h > 0)
            // Still loaded (the corpse stays until a reload) and at 0 HP: not a load or
            // an escape from the arena.
            && game::chr_by_entity_id(e).and_then(|c| c.hp()).is_some_and(|h| h <= 0)
        {
            log::line(&format!("own world: boss gauge of {e} went away with the boss dead, flags around it count as progress"));
            self.open_window(Category::BossDefeat, DEFEAT_BEFORE, DEFEAT_AFTER, now);
        }
        self.gauge = gauge;
        if let Some(l) = lists() {
            // Ours (index 0) and every peer's (1 + slot): progress windows accept any player.
            for &(who, anim) in anims {
                let i = who.map_or(0, |p| 1 + p.index());
                let a = anim.unwrap_or(0);
                if a != self.last_anims[i] && objact_anim(a, &l.objact_anims) {
                    let who = who.map_or_else(|| "we".to_owned(), |p| format!("peer {}", p.slot()));
                    log::line(&format!("own world: {who} played ObjAct animation {a}, flags right after it count as progress"));
                    self.open_window(Category::ObjAct, OBJACT_BEFORE, OBJACT_AFTER, now);
                }
                self.last_anims[i] = a;
            }
        }
        // Flags switched on since the last frame (our own SetEventFlag hook ring).
        let count = hook::flag_ring_count();
        let mut n = *self.ring.get_or_insert(count);
        if count.wrapping_sub(n) > hook::FLAG_RING {
            n = count.wrapping_sub(hook::FLAG_RING);
        }
        while n != count {
            match hook::flag_ring_entry(n) {
                FlagEntry::Pending => break,
                FlagEntry::Ready(id, on, 0) => {
                    if settled {
                        self.note_quest_write(id);
                    }
                    self.idol_activation(id, on, settled, now);
                    if on && window_eligible(id) && world::should_sync(id) {
                        self.recent.push_back((id, now));
                        if self.defeat_until.is_some_and(|t| now <= t) {
                            self.classify(id, Category::BossDefeat);
                        }
                        if self.objact_until.is_some_and(|t| now <= t) {
                            self.classify(id, Category::ObjAct);
                        }
                    }
                }
                _ => {}
            }
            n = n.wrapping_add(1);
        }
        self.ring = Some(n);
        while self.recent.front().is_some_and(|(_, t)| now.duration_since(*t) > RECENT_KEEP) {
            self.recent.pop_front();
        }
        while self.recent_changes.front().is_some_and(|(_, t)| now.duration_since(*t) > RECENT_KEEP) {
            self.recent_changes.pop_front();
        }
        if self.recent_changes.len() > 4096 {
            self.recent_changes.drain(..2048);
        }
        if self.recent.len() > 4096 {
            self.recent.drain(..2048);
        }
    }

    fn fold_and_save(&mut self, fm: usize, now: Instant) {
        if self.fold_at.is_none_or(|t| now.duration_since(t) >= FOLD_EVERY) {
            self.fold_at = Some(now);
            if let Some(a) = self.active.as_mut() {
                let n = fold(fm, a);
                if n > 0 {
                    self.dirty = true;
                    log::line(&format!("own world: {n} new progress flags ({} in total)", a.progress.len()));
                }
            }
        }
        if self.dirty && self.saved_at.is_none_or(|t| now.duration_since(t) >= SAVE_MIN_GAP) {
            if let Some(a) = &self.active {
                self.dirty = !write_file(a);
            }
            self.saved_at = Some(now);
        }
    }

    /// Our own world back, plus the progress (merge), or the shared world kept as it is
    /// (mirror). None = nothing to restore; Some(true) = flags were written (merge: call it
    /// only on a loading frame of a reload, or right before a reload warp with `finish =
    /// false`); Some(false) = mirror, nothing written, no reload needed. `finish = false`
    /// keeps the session (file and backup) until `confirm_restore`.
    fn restore(&mut self, fm: usize, finish: bool) -> Option<bool> {
        crate::invguard::checkpoint("before own-world restore");
        let r = self.restore_inner(fm, finish);
        crate::invguard::checkpoint("after own-world restore");
        r
    }

    fn restore_inner(&mut self, fm: usize, finish: bool) -> Option<bool> {
        let mut a = self.active.take()?;
        let t0 = Instant::now();
        let mirror = use_mirror(mode(), a.subset);
        log::line(&format!(
            "own world: restore mode {}: setting own_world_mode = {}, our world {} at the last join snapshot ({} flags of ours not in the host's world)",
            if mirror { "MIRROR" } else { "MERGE" },
            mode_name(),
            match a.subset {
                Some(true) => "was a subset of the host's",
                Some(false) => "was NOT a subset of the host's",
                None => "was not compared (no baseline)",
            },
            a.subset_extra
        ));
        if mirror {
            // The shared world stays: our shared records already hold it (plus everything
            // the session changed), and the per-player flags (items, shops, skills, death
            // penalty) were never synced, so they are ours. Nothing to write, no reload.
            if a.subset == Some(false) {
                log::line(&format!(
                    "own world: WARNING own_world_mode = mirror with {} flags of our own world not in the host's: they are given up",
                    a.subset_extra
                ));
            }
            log::line(&format!(
                "own world: MIRRORED save slot {}: the shared world is kept as our own ({} join snapshots, our backup of {} records dropped), nothing written",
                a.slot,
                a.snapshots,
                a.backup.len()
            ));
            self.finish_restore(a.slot);
            crate::dialog::game_notice("Back in your own world. It now matches the shared world.".into(), crate::dialog::Importance::Membership, 8);
            return Some(false);
        }
        fold(fm, &mut a);
        let mode = crate::tuning::merge_quests();
        let (_, same) = fold_quests(fm, &mut a);
        let no_quests = BTreeMap::new();
        let quest = if mode == QUEST_OFF { &no_quests } else { &a.quest_taken };
        let quest_units: BTreeSet<u32> = quest.keys().filter_map(|&id| quest_unit(id, mode).map(|u| u.0)).collect();
        log::line(&format!(
            "own world: quests (merge_quests = {mode}): {} units taken from the session ({} flags), {same} touched units ended as they started, {} flags touched; {}",
            quest_units.len(),
            quest.len(),
            a.touched.len(),
            if mode == QUEST_OFF {
                "off: NPC quests as in our own world".to_string()
            } else if a.quest_base.is_none() && a.quest_taken.is_empty() {
                "no quest baseline (session ended within seconds of the join load): NPC quests as in our own world".to_string()
            } else {
                format!("other NPC quests as in our own world; units {:?}", quest_units.iter().take(30).collect::<Vec<_>>())
            }
        ));
        // Per-player flags at their current value (items and skills got this session,
        // our death-penalty state): the records are rewritten whole.
        let live = world::read_shared_records(fm);
        let bases: HashSet<u32> = live.iter().map(|(b, _)| *b).chain(a.backup.iter().map(|(b, _)| *b)).collect();
        let mut keep: Vec<(u32, bool)> = DEATH_FLAGS.map(|id| (id, world::get_flag(fm, id))).collect();
        // The other per-player state (9100, 9110, ..., the dragonrot cure 627 / 70-75): live too.
        keep.extend(
            world::PER_PLAYER_STATE
                .iter()
                .filter(|&&id| bases.contains(&record_base(id)))
                .map(|&id| (id, world::get_flag(fm, id))),
        );
        if let Some(items) = world::per_player_flags() {
            keep.extend(items.iter().filter(|&&id| bases.contains(&record_base(id))).map(|&id| (id, world::get_flag(fm, id))));
        }
        // Idol activations of the session: every flag of the activation at its session value
        // (on or off), applied after the progress like the per-player flags.
        let idol_act: Vec<(u32, bool)> = a
            .classified
            .iter()
            .filter(|(id, c)| **c == Category::IdolActivation as u8 && !per_player(**id))
            .map(|(&id, _)| (id, world::get_flag(fm, id)))
            .collect();
        if !idol_act.is_empty() {
            log::line(&format!(
                "own world: {} idol activation flags take their session value: {:?}",
                idol_act.len(),
                idol_act.iter().take(40).collect::<Vec<_>>()
            ));
        }
        keep.extend(&idol_act);
        let (recs, set, quest_set, skipped) = merge(&a.backup, &a.progress, quest, &keep, |b| world::record_size(fm, b));
        // Restore safety: a record whose base id resolves elsewhere now than when it was
        // backed up is not ours to write. It keeps its live (shared-world) bytes instead
        // (`write_records_raw` zeroes every shared record first).
        let (mut recs, refused, unchecked) = resolve_filter(recs, &a.tuples, |b| world::record_tuple(fm, b));
        for (base, then, now) in refused.iter().take(20) {
            log::line(&format!("own world: record {base} resolved to {then:?} at the backup, now {now:?}: NOT restored, kept as it is"));
        }
        let live_map: HashMap<u32, &Vec<u8>> = live.iter().map(|(b, v)| (*b, v)).collect();
        recs.extend(refused.iter().filter_map(|(b, _, _)| live_map.get(b).map(|v| (*b, (*v).clone()))));
        log::line(&format!(
            "own world: dry run (merge): {} records ({} B) to write after zeroing {} shared records, {set} progress flags, {quest_set} quest flags on, {} per-player flags kept, {skipped} skipped, {} refused by the resolve check, {unchecked} without a stored tuple ({} stored); save copied first",
            recs.len(),
            bytes(&recs),
            live.len(),
            keep.len(),
            refused.len(),
            a.tuples.len()
        ));
        let (written, missing) = world::write_records_raw(fm, &recs);
        // The story phase (outside the shared records): the session's if it moved it, else ours.
        match (a.own_phase, a.base_phase) {
            (Some(own), Some(base)) => {
                let live = world::read_phase(fm);
                let mask = phase_at_restore(own, base, live, false);
                world::write_phase(fm, mask);
                log::line(&format!(
                    "own world: story phase {}: {} (mask {mask:#x}; own {}, host's at the join {}, live {})",
                    if live != base { "taken from session" } else { "kept own" },
                    world::phase_names(mask),
                    world::phase_names(own),
                    world::phase_names(base),
                    world::phase_names(live)
                ));
            }
            _ => log::line("own world: no story phase in the session file (older build): the phase is left as it is"),
        }
        let mut by_cat: BTreeMap<&str, u32> = BTreeMap::new();
        let l = lists();
        for id in &a.progress {
            let name = match a.classified.get(id).copied().and_then(category_of) {
                Some(Category::BossDefeat) => "boss",
                Some(Category::ObjAct) => "objact",
                _ if l.is_some_and(|l| l.idols.contains(id)) => "idol",
                _ if l.is_some_and(|l| l.progress.contains(id)) => "game progress",
                _ => "other",
            };
            *by_cat.entry(name).or_insert(0) += 1;
        }
        log::line(&format!(
            "own world: RESTORED save slot {}: {written} records written ({missing} not resolvable), {set} progress flags merged {by_cat:?} {:?}, {} quest units ({quest_set} flags on), {} per-player flags kept, {skipped} skipped, {} join snapshots, {:.1} ms",
            a.slot,
            a.progress.iter().take(40).collect::<Vec<_>>(),
            quest_units.len(),
            keep.len(),
            a.snapshots,
            t0.elapsed().as_secs_f32() * 1000.0
        ));
        if finish {
            self.finish_restore(a.slot);
            crate::dialog::game_notice("Back in your own world. Progress from the shared world was kept.".into(), crate::dialog::Importance::Membership, 8);
        } else {
            // Written before the warp (fallback): the session stays until its load is seen.
            self.active = Some(a);
        }
        Some(true)
    }

    /// The fallback's load was seen: the written world is the one the map loads.
    fn confirm_restore(&mut self) {
        if let Some(a) = self.active.take() {
            self.finish_restore(a.slot);
            crate::dialog::game_notice("Back in your own world. Progress from the shared world was kept.".into(), crate::dialog::Importance::Membership, 8);
        }
    }

    /// Where the reload goes (`pick_reload_idol`), with the restored world's idols.
    fn reload_idol(&self, fm: usize) -> (i32, &'static str) {
        let last = game::last_idol();
        let (Some(a), Some(l)) = (self.active.as_ref(), lists()) else { return (last, "our last idol") };
        let own = index(&a.backup);
        let unlocked = |f: u32| {
            flag_in(&own, f)
                || a.progress.contains(&f)
                || (a.classified.get(&f) == Some(&(Category::IdolActivation as u8)) && world::get_flag(fm, f))
        };
        pick_reload_idol(last, &l.idol_rows, unlocked)
    }

    /// In the world with a restore armed. True = still waiting for its load (the caller
    /// does nothing else this frame). A reload that ended without a write, or a warp that
    /// never loaded, disarms (nothing of ours is in memory then, or the fallback's write is
    /// rolled back) and the restore is tried again through the warp gate.
    fn armed_check(&mut self, loaded: bool, now: Instant) -> bool {
        let Some(arm) = self.armed.as_ref() else { return false };
        let waited = now.duration_since(arm.at);
        let load_passed = arm.loading_seen || loaded;
        if net::link() == Link::ClientConnected && !load_passed {
            // Back in a session before the reload: no restore (the backup carries on).
            log::line("own world: connected again before the restore's reload, restore cancelled");
            if let ArmMode::BeforeWarp { rollback } = &arm.mode
                && let Some(fm) = world::real_world_flag_man()
            {
                world::write_records_raw(fm, rollback);
            }
            self.armed = None;
            return false;
        }
        match &arm.mode {
            ArmMode::DuringLoad if load_passed => {
                self.failed_reloads += 1;
                log::line(&format!(
                    "own world: the reload at idol {} ended with nothing written (no loading frame with a ready flag manager, {:.1} s after the warp; try {}): our world is still the shared one, trying again",
                    arm.idol,
                    waited.as_secs_f32(),
                    self.failed_reloads
                ));
            }
            ArmMode::BeforeWarp { .. } if load_passed => {
                // `on_loading` confirms it; a load without a loading frame did too.
                log::line("own world: the fallback's reload happened (player re-created), restore confirmed");
                self.confirm_restore();
                return false;
            }
            _ if waited < WARP_START_WAIT => return true,
            mode => {
                self.failed_reloads += 1;
                let request = read_ptr(game::module_base() + GAME_MAN).and_then(|gm| read::<u8>(gm + GM_LOAD_REQUEST));
                log::line(&format!(
                    "own world: our reload warp to idol {} showed no loading screen within {} s (GameMan+0x10 load request = {request:?}; left alone, a pending load is the game's), try {}",
                    arm.idol,
                    WARP_START_WAIT.as_secs(),
                    self.failed_reloads
                ));
                if let ArmMode::BeforeWarp { rollback } = mode
                    && let Some(fm) = world::real_world_flag_man()
                {
                    // Never leave the player in an un-reloaded restored world.
                    let (w, m) = world::write_records_raw(fm, rollback);
                    log::line(&format!("own world: fallback write rolled back to the shared world ({w} records, {m} not resolvable)"));
                }
            }
        }
        self.armed = None;
        self.warp.reset();
        false
    }

    /// The session's file is used up: delete it and forget the session.
    fn finish_restore(&mut self, slot: i32) {
        let path = path_for(slot);
        match std::fs::remove_file(&path) {
            Ok(()) => log::line(&format!("own world: {} deleted", path.display())),
            Err(e) => log::line(&format!("own world: could not delete {}: {e}", path.display())),
        }
        self.dirty = false;
        self.no_client_since = None;
        self.recent.clear();
        self.defeat_until = None;
        self.objact_until = None;
        self.quest_snap_at = None;
        self.pre_backup = None;
        self.pre_backup_late_logged = false;
        self.recent_changes.clear();
        self.idol_until = None;
        self.idols_opened.clear();
        self.armed = None;
        self.failed_reloads = 0;
    }
}

/// A session backup is held or a restore is armed (leave.rs waits for it before its own
/// reload: a merge reloads by itself, a mirror finishes without one).
pub fn restore_pending() -> bool {
    with_state(|st| st.active.is_some() || st.armed.is_some()).unwrap_or(false)
}

/// world.rs `apply_flag`: the friend's flag change, applied here. Its quest unit counts
/// as changed in the session even when it arrived during our load settle (where our
/// own hook ring writes are ignored).
pub fn note_friend_flag(id: u32) {
    with_state(|st| st.note_quest_write(id));
}

/// Every loading-screen frame (puppet.rs, no player). An armed merge restore is written
/// here, on the first frame its flag manager is ready, so the map that loads next is the
/// first thing that reads our own world. Not on a title trip or a save load (another
/// slot, or `items::title_count` changed): the save's own flags are loaded then.
pub fn on_loading() {
    with_state(|st| {
        let Some(arm) = st.armed.as_mut() else { return };
        let first = !arm.loading_seen;
        arm.loading_seen = true;
        let ours = save_slot() == Some(arm.slot) && crate::items::title_count() == arm.titles;
        match arm.mode {
            ArmMode::BeforeWarp { .. } => {
                if ours {
                    log::line(&format!("own world: the reload's loading screen is up ({:.1} s after the warp), restore confirmed", arm.at.elapsed().as_secs_f32()));
                    st.confirm_restore();
                } else {
                    // A save load replaces every flag from the save: the file stays.
                    log::line("own world: a title trip / save load instead of our reload; the backup stays on disk");
                    st.armed = None;
                }
            }
            ArmMode::DuringLoad => {
                if !ours {
                    log::line("own world: a title trip / save load instead of our reload: nothing written, the backup stays on disk");
                    st.armed = None;
                    return;
                }
                if net::link() == Link::ClientConnected {
                    log::line("own world: connected again during the reload, restore cancelled");
                    st.armed = None;
                    return;
                }
                let Some(fm) = world::real_world_flag_man() else {
                    if first {
                        log::line("own world: loading screen of the reload, flag manager not ready yet");
                    }
                    return;
                };
                log::line(&format!(
                    "own world: writing our own world during the reload's loading screen ({:.1} s after the warp to idol {})",
                    arm.at.elapsed().as_secs_f32(),
                    arm.idol
                ));
                st.armed = None;
                st.restore(fm, true);
            }
        }
    });
}

/// Every in-world frame (puppet.rs, not while a warp is pending). `anim`: our current
/// animation; `peers`: every other player's from its latest visible snapshot (the host and
/// the other joiners on a joiner): an ObjAct of any of them opens a progress window
/// (PLAN-MULTIPLAYER P4). Returns true on the frame we started the reload warp (the
/// caller lets go of every character, like after the join warp).
pub fn on_frame(player: Chr, anim: Option<i32>, peers: &PeerSet, in_world_for: Duration) -> bool {
    let anims: Vec<(Option<crate::peers::PeerId>, Option<i32>)> =
        std::iter::once((None, anim)).chain(peers.iter().map(|p| (Some(p.id), p.snapshot.map(|s| s.anim)))).collect();
    // Rotating co-op save copies (session start / end, every 10 min in co-op).
    crate::backups::tick();
    crate::invguard::tick();
    with_state(|st| {
        let now = Instant::now();
        let loaded = in_world_for < st.last_in_world;
        if loaded {
            // A load: check for a file again; the old gauge belongs to the old map.
            st.checked_load = false;
            st.gauge = None;
            st.quest_loaded = true;
        }
        st.last_in_world = in_world_for;
        // An armed restore whose reload did not write (or, in the fallback, did not load).
        if st.armed_check(loaded, now) {
            return false;
        }
        // Loading or in a Remnant memory (bank 1): nothing to read or write.
        let Some(fm) = world::real_world_flag_man() else { return false };
        let Some(slot) = save_slot() else { return false };
        if st.active.as_ref().is_some_and(|a| a.slot != slot) {
            st.drop_active("another save is loaded");
        }
        // Crash / quit recovery: a file of this slot with no session holding it.
        if st.active.is_none() && !st.checked_load && in_world_for >= RECOVER_AFTER_LOAD {
            st.checked_load = true;
            if let Some(f) = read_file(slot) {
                if same_character(&f, sen_total()) {
                    log::line(&format!(
                        "own world: found an unfinished session of save slot {slot} (backup of {}, {} progress flags): the game was closed or crashed during co-op, restoring",
                        f.created,
                        f.progress.len()
                    ));
                    st.active = Some(f);
                } else {
                    log::line(&format!("own world: the file of slot {slot} belongs to another character (sen {} > {:?})", f.sen_total, sen_total()));
                    set_aside(slot);
                }
            }
        }
        if st.active.is_none() {
            st.ring = None;
            return false;
        }
        st.quest_baseline(fm, in_world_for, now);
        st.pump(player, &anims, in_world_for >= QUEST_LOAD_SETTLE, now);
        st.fold_and_save(fm, now);
        if net::link() == Link::ClientConnected {
            st.no_client_since = None;
            st.warp.reset();
            st.pre_backup = None;
            st.pre_backup_late_logged = false;
            st.failed_reloads = 0;
            return false;
        }
        // The session is over for us (or paused): restore at a safe moment.
        let since = *st.no_client_since.get_or_insert(now);
        let grace = match (net::intent(), net::link()) {
            (Some(Intent::Solo | Intent::Host), _) => GRACE_LEFT,
            (_, Link::Off) => GRACE_OFF,
            _ => GRACE_LOST,
        };
        if now.duration_since(since) < grace || in_world_for < RESTORE_MIN_IN_WORLD {
            return false;
        }
        // A copy of the co-op save before anything is written (backups.rs, off the game
        // thread): wait for it, at most PRE_BACKUP_WAIT.
        let (ticket, asked) = *st.pre_backup.get_or_insert_with(|| {
            let t = crate::backups::request("pre-restore");
            log::line(&format!("own world: restore due; copying the co-op save first (backup #{t})"));
            (t, now)
        });
        if !crate::backups::done(ticket) {
            if now.duration_since(asked) < PRE_BACKUP_WAIT {
                return false;
            }
            if !st.pre_backup_late_logged {
                st.pre_backup_late_logged = true;
                log::line("own world: the save copy is taking long, restoring without waiting for it");
            }
        }
        // Never mid-boss-fight (2026-10-23, §26): wait until the fight ends or its
        // death / rest load comes.
        if game::boss_state().is_some() || crate::bossrules::spectating() {
            static BOSS_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !BOSS_LOGGED.swap(true, Ordering::Relaxed) {
                log::line("own world: restore due during a boss fight, waiting until it ends");
            }
            st.warp.reset();
            return false;
        }
        match st.warp.poll(in_world_for) {
            WarpGate::Go => {
                let Some(a) = st.active.as_mut() else { return false };
                if use_mirror(mode(), a.subset) {
                    // Mirror: nothing is written, so no reload is needed.
                    st.restore(fm, true);
                    return false;
                }
                // Merge: written only where a real load follows (live v6.1: written in
                // the world, then the reload; the result was an idol that threw the player
                // out). Normally on a loading frame of this reload (`on_loading`).
                fold(fm, a);
                let (idol, why) = st.reload_idol(fm);
                let titles = crate::items::title_count();
                let arm_mode = if st.failed_reloads >= FALLBACK_AFTER {
                    let rollback = world::read_shared_records(fm);
                    log::line(&format!(
                        "own world: {} reloads gave no loading frame with a ready flag manager: writing our own world right before the warp (fallback; rolled back if no load follows within {} s)",
                        st.failed_reloads,
                        WARP_START_WAIT.as_secs()
                    ));
                    if st.restore(fm, false).is_none() {
                        return false;
                    }
                    ArmMode::BeforeWarp { rollback }
                } else {
                    ArmMode::DuringLoad
                };
                log::line(&format!(
                    "own world: restore armed: reloading at idol {idol} ({why}); our own world is written {}",
                    if matches!(arm_mode, ArmMode::DuringLoad) { "during its loading screen" } else { "already (fallback)" }
                ));
                st.armed = Some(Armed { slot, titles, idol, at: now, loading_seen: false, mode: arm_mode });
                game::warp_to_idol(idol);
                true
            }
            WarpGate::Wait => false,
            WarpGate::GaveUp => {
                log::line("own world: no safe moment for the reload yet, trying again");
                false
            }
        }
    })
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Saved {
        let mut s = Saved {
            slot: 3,
            sen_total: 123_456,
            created: 1_760_000_000,
            snapshots: 2,
            backup: vec![(11_100_000, vec![0x80, 0, 0, 1]), (9000, vec![0; 128])],
            baseline: vec![(11_105_000, (0..128).map(|i| i as u8).collect())],
            ..Default::default()
        };
        s.progress.extend([11_100_001, 11_105_800]);
        s.classified.insert(11_105_800, Category::BossDefeat as u8);
        s
    }

    #[test]
    fn file_round_trip() {
        let s = sample();
        assert_eq!(decode(&encode(&s)), Some(s));
        let empty = Saved::default();
        assert_eq!(decode(&encode(&empty)), Some(empty));
    }

    #[test]
    fn file_rejects_damage() {
        let data = encode(&sample());
        assert_eq!(decode(&data[..data.len() - 1]), None, "truncated");
        assert_eq!(decode(&data[..10]), None, "much too short");
        assert_eq!(decode(&[]), None);
        let mut bad = data.clone();
        bad[20] ^= 1;
        assert_eq!(decode(&bad), None, "checksum");
        let mut magic = data;
        magic[0] = b'X';
        assert_eq!(decode(&magic), None, "magic");
    }

    #[test]
    fn bit_layout_matches_set_event_flag() {
        // dword[idx >> 5] |= 1 << (31 - (idx & 31)), little-endian dwords.
        for idx in 0..1000u32 {
            let mut b = vec![0u8; 128];
            assert!(set_bit(&mut b, idx, true));
            let w = (idx >> 5) as usize;
            let dword = u32::from_le_bytes(b[w * 4..w * 4 + 4].try_into().unwrap());
            assert_eq!(dword, 1 << (31 - (idx & 31)), "idx {idx}");
            assert_eq!(b.iter().filter(|&&x| x != 0).count(), 1);
            assert!(get_bit(&b, idx));
            assert!(set_bit(&mut b, idx, false));
            assert!(b.iter().all(|&x| x == 0));
        }
        let mut short = vec![0u8; 4];
        assert!(!set_bit(&mut short, 40, true));
        assert!(!get_bit(&short, 40));
    }

    #[test]
    fn merge_restores_backup_ors_progress_keeps_per_player() {
        let mut own = vec![0u8; 128];
        set_bit(&mut own, 5, true); // our own boss kill
        set_bit(&mut own, 700, true); // an item flag we had
        let backup = vec![(11_100_000, own)];
        let progress: BTreeSet<u32> = [11_100_010, 11_105_801, 12_000_123].into_iter().collect();
        // 11_100_700 picked up again this session? It is per player: its value now wins (off).
        // 11_105_500 an item picked up in the host's world this session (on).
        let keep = vec![(11_100_700, false), (11_105_500, true), (11_107_001, false)];
        let size_of = |base: u32| (base != 12_000_000).then_some(128);
        let (recs, set, quest_set, skipped) = merge(&backup, &progress, &BTreeMap::new(), &keep, size_of);
        assert_eq!(quest_set, 0);
        let m: HashMap<u32, &[u8]> = index(&recs);
        assert!(flag_in(&m, 11_100_005), "own flag kept");
        assert!(flag_in(&m, 11_100_010), "progress in a backed-up record");
        assert!(flag_in(&m, 11_105_801), "progress in a record the backup did not have");
        assert!(!flag_in(&m, 11_100_700), "per-player flag takes its current value");
        assert!(flag_in(&m, 11_105_500), "per-player flag on now stays on");
        assert!(!m.contains_key(&11_107_000), "off flag in an unwritten record writes nothing");
        assert!(!m.contains_key(&12_000_000), "no size: skipped");
        assert_eq!((set, skipped), (2, 1));
    }

    #[test]
    fn merge_with_nothing_is_the_backup() {
        let mut own = vec![0u8; 128];
        set_bit(&mut own, 1, true);
        let backup = vec![(11_100_000, own.clone()), (11_101_000, vec![0; 128])];
        let (recs, set, quest_set, skipped) = merge(&backup, &BTreeSet::new(), &BTreeMap::new(), &[], |_| Some(128));
        assert_eq!(recs, vec![(11_100_000, own)], "all-zero records are dropped (the restore zeroes first)");
        assert_eq!((set, quest_set, skipped), (0, 0, 0));
    }

    fn rec(base: u32, on: &[u32]) -> (u32, Vec<u8>) {
        let mut b = vec![0u8; 128];
        for &id in on {
            assert_eq!(record_base(id), base);
            set_bit(&mut b, id % 1000, true);
        }
        (base, b)
    }

    #[test]
    fn file_round_trip_with_quests_and_old_version() {
        let mut s = sample();
        s.quest_base = Some(vec![rec(71_100_000, &[71_100_450])]);
        s.touched.extend([71_100_451, 1_441]);
        s.quest_taken.extend([(71_100_450, false), (71_100_451, true)]);
        assert_eq!(decode(&encode(&s)), Some(s.clone()));
        // A version 1 file (written by the previous build, e.g. after a crash) still reads.
        let old = sample();
        assert_eq!(decode(&encode_version(&old, 1)), Some(old));
        // Version 2 (the previous build) still reads; it has no tuples / subset result.
        assert_eq!(decode(&encode_version(&s, 2)), Some(s.clone()));
        assert_eq!(decode(&encode_version(&s, 5)), None, "unknown version");
    }

    #[test]
    fn file_v3_keeps_resolve_tuples_and_the_subset_result() {
        let mut s = sample();
        s.tuples.insert(11_100_000, (1, 37, 0, 128));
        s.tuples.insert(9000, (0, 0, 9, 128));
        s.subset = Some(false);
        s.subset_extra = 7;
        let data = encode(&s);
        assert_eq!(decode(&data), Some(s.clone()));
        for v in [None, Some(true)] {
            let t = Saved { subset: v, ..s.clone() };
            assert_eq!(decode(&encode(&t)), Some(t));
        }
        // Written as version 2, the v3 fields are dropped (not misread).
        let v2 = decode(&encode_version(&s, 2)).unwrap();
        assert!(v2.tuples.is_empty() && v2.subset.is_none() && v2.subset_extra == 0);
        // A damaged subset byte is rejected as a whole (checksum recomputed on purpose).
        let mut bad = data[..data.len() - 4].to_vec();
        let at = bad.len() - 5;
        bad[at] = 9;
        let sum = checksum(&bad);
        bad.extend(sum.to_le_bytes());
        assert_eq!(decode(&bad), None);
    }

    #[test]
    fn phase_taken_whole_when_session_changed() {
        // Own 8301 only; the host's world at the join was 8306+8310; the session moved it on
        // to 8302+8306+8310: the whole live mask, none of ours.
        let (own, base, live) = (0b00001, 0b11000, 0b11010);
        assert_eq!(phase_at_restore(own, base, live, false), live);
        // Switched off in the session counts as a change too.
        assert_eq!(phase_at_restore(own, base, 0b01000, false), 0b01000);
    }

    #[test]
    fn phase_own_kept_when_unchanged() {
        assert_eq!(phase_at_restore(0b00001, 0b11000, 0b11000, false), 0b00001);
        assert_eq!(phase_at_restore(0, 0b00010, 0b00010, false), 0, "the host's phase goes, ours comes back");
    }

    #[test]
    fn phase_mirror_keeps_live() {
        assert_eq!(phase_at_restore(0b00001, 0b11000, 0b11000, true), 0b11000);
        assert_eq!(phase_at_restore(0b00001, 0b11000, 0b11010, true), 0b11010);
    }

    #[test]
    fn file_v3_reads_without_phase() {
        let mut s = sample();
        s.own_phase = Some(0b01001);
        s.base_phase = Some(0b11000);
        assert_eq!(decode(&encode(&s)), Some(s.clone()));
        // Written as version 3 the phase masks are dropped, and the file still reads.
        let v3 = decode(&encode_version(&s, 3)).unwrap();
        assert!(v3.own_phase.is_none() && v3.base_phase.is_none());
        assert_eq!(v3.backup, s.backup);
        // One mask only.
        let t = Saved { base_phase: None, ..s };
        assert_eq!(decode(&encode(&t)), Some(t));
    }

    #[test]
    fn subset_check_ignores_per_player_and_noise() {
        let own = vec![rec(11_100_000, &[11_100_005, 11_100_700]), rec(9000, &[9_801, 9_500])];
        let host = vec![rec(11_100_000, &[11_100_005, 11_100_006]), rec(9000, &[9_802])];
        let ignore = |id: u32| id == 11_100_700 || (9500..=9599).contains(&id) || NOISE.contains(&id);
        let (counted, missing, n) = subset_check(&own, &BTreeSet::new(), &host, ignore);
        assert_eq!((counted, n), (1, 0), "only 11_100_005 counts, and the host has it");
        assert!(missing.is_empty());
        // Our own boss kill the host lacks: not a subset. Progress counts as ours too.
        let own2 = vec![rec(11_100_000, &[11_100_005, 11_100_010])];
        let progress: BTreeSet<u32> = [12_000_001].into_iter().collect();
        let (counted, missing, n) = subset_check(&own2, &progress, &host, ignore);
        assert_eq!((counted, n), (3, 2));
        assert_eq!(missing, vec![11_100_010, 12_000_001]);
        // Nothing of ours at all: trivially a subset.
        assert_eq!(subset_check(&[], &BTreeSet::new(), &host, ignore).2, 0);
    }

    #[test]
    fn mirror_or_merge() {
        assert!(use_mirror(MODE_AUTO, Some(true)));
        assert!(!use_mirror(MODE_AUTO, Some(false)));
        assert!(!use_mirror(MODE_AUTO, None), "no baseline: merge");
        assert!(use_mirror(MODE_MIRROR, Some(false)));
        assert!(!use_mirror(MODE_MERGE, Some(true)));
        let mut w = Vec::new();
        assert_eq!(parse_mode("Mirror", &mut w), MODE_MIRROR);
        assert_eq!(parse_mode(" merge ", &mut w), MODE_MERGE);
        assert_eq!(parse_mode("", &mut w), MODE_AUTO);
        assert_eq!(parse_mode("auto", &mut w), MODE_AUTO);
        assert!(w.is_empty());
        assert_eq!(parse_mode("both", &mut w), MODE_AUTO);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn resolve_filter_refuses_moved_records() {
        let recs = vec![rec(11_100_000, &[11_100_001]), rec(11_105_000, &[11_105_001]), rec(12_000_000, &[12_000_001])];
        let stored: BTreeMap<u32, Tuple> = [(11_100_000, (1, 3, 0, 128)), (11_105_000, (1, 3, 5, 128))].into_iter().collect();
        let now = |b: u32| match b {
            11_100_000 => Some((1, 3, 0, 128)),
            11_105_000 => Some((1, 4, 5, 128)), // another block entry now
            _ => None,
        };
        let (keep, refused, unchecked) = resolve_filter(recs, &stored, now);
        assert_eq!(keep.iter().map(|(b, _)| *b).collect::<Vec<_>>(), vec![11_100_000, 12_000_000]);
        assert_eq!(refused, vec![(11_105_000, (1, 3, 5, 128), Some((1, 4, 5, 128)))]);
        assert_eq!(unchecked, 1, "no stored tuple: kept unchecked");
        // A record that does not resolve at all any more is refused too.
        let (keep, refused, _) = resolve_filter(vec![rec(11_100_000, &[])], &stored, |_| None);
        assert!(keep.is_empty() && refused.len() == 1 && refused[0].2.is_none());
    }

    #[test]
    fn quest_units_are_npc_blocks() {
        // NPC state machines: 20 per NPC (Kotaro 1440-1459, Doujun 1460-1479).
        assert_eq!(quest_unit(1_459, QUEST_UNITS), Some((1_440, 20)));
        assert_eq!(quest_unit(1_460, QUEST_UNITS), Some((1_460, 20)));
        assert_eq!(quest_unit(1_000, QUEST_UNITS), Some((1_000, 20)));
        assert_eq!(quest_unit(1_999, QUEST_UNITS), Some((1_980, 20)));
        // Group 7 dialogue flags: 100 per block, any k.
        assert_eq!(quest_unit(71_100_509, QUEST_UNITS), Some((71_100_500, 100)));
        assert_eq!(quest_unit(70_009_115, QUEST_UNITS), Some((70_009_100, 100)));
        // Whole records in mode 2.
        assert_eq!(quest_unit(71_100_509, QUEST_RECORDS), Some((71_100_000, 1000)));
        assert_eq!(quest_unit(1_459, QUEST_RECORDS), Some((1_000, 1000)));
        // Not quest state: global flags, time of day, death penalty, map flags, off.
        for id in [999, 2_000, 9_801, 9_500, 6_911, 11_100_000, 61_000_000, 81_000_000] {
            assert_eq!(quest_unit(id, QUEST_UNITS), None, "{id}");
            assert_eq!(quest_unit(id, QUEST_RECORDS), None, "{id}");
        }
        assert_eq!(quest_unit(71_100_509, QUEST_OFF), None);
        assert!(quest_record(1_000) && quest_record(71_100_000) && quest_record(70_009_000));
        assert!(!quest_record(0) && !quest_record(11_100_000) && !quest_record(2_000));
    }

    #[test]
    fn quest_unit_changed_on_or_off_is_taken_whole() {
        // Our own world: Kotaro at state 1441, the NPC block 711004xx at stage 450.
        let own = vec![rec(1_000, &[1_441]), rec(71_100_000, &[71_100_450, 71_100_120])];
        // The host's world settled after the join load: Kotaro at 1440, stage 452.
        let base = vec![rec(1_000, &[1_440]), rec(71_100_000, &[71_100_452, 71_100_120])];
        // The session: Kotaro moved on to 1443 (1440 off, 1443 on); in 711004xx only a
        // flag went OFF (452 cleared by the quest script).
        let live = vec![rec(1_000, &[1_443]), rec(71_100_000, &[71_100_120])];
        let touched: BTreeSet<u32> = [1_440, 1_443, 71_100_452].into_iter().collect();
        let (units, same) = changed_quest_units(&touched, QUEST_UNITS, &base, &live, &own, |_| false);
        assert_eq!(same, 0);
        assert_eq!(units.iter().map(|u| (u.start, u.len)).collect::<Vec<_>>(), vec![(1_440, 20), (71_100_400, 100)]);
        let kotaro = &units[0];
        assert_eq!(kotaro.flags.len(), 20);
        assert_eq!(kotaro.on, 1);
        assert_eq!(kotaro.differ_own, 2, "1441 off, 1443 on");
        assert_eq!(units[1].on, 0);
        assert_eq!(units[1].differ_own, 1, "450 was ours, the session cleared the stage");
        let mut taken = BTreeMap::new();
        take_units(&mut taken, &units);
        // Restored: our own world + the two units at their session state.
        let (recs, _, quest_set, skipped) = merge(&own, &BTreeSet::new(), &taken, &[], |_| Some(128));
        let m = index(&recs);
        assert!(flag_in(&m, 1_443) && !flag_in(&m, 1_441) && !flag_in(&m, 1_440), "no mixed Kotaro state");
        assert!(!flag_in(&m, 71_100_450) && !flag_in(&m, 71_100_452), "our stage flag is cleared with the unit");
        assert!(flag_in(&m, 71_100_120), "an untouched unit keeps our own state");
        assert_eq!((quest_set, skipped), (1, 0));
    }

    #[test]
    fn untouched_or_unchanged_units_keep_our_own_state() {
        let own = vec![rec(71_100_000, &[71_100_450, 71_100_120])];
        let base = vec![rec(71_100_000, &[71_100_452, 71_100_125])];
        // The session wrote 452 off and on again (dialogue): no net change. 125 changed
        // but nobody wrote it after the baseline (the host's world differs from ours).
        let live = vec![rec(71_100_000, &[71_100_452])];
        let touched: BTreeSet<u32> = [71_100_452, 9_801, 11_100_000].into_iter().collect();
        let (units, same) = changed_quest_units(&touched, QUEST_UNITS, &base, &live, &own, |_| false);
        assert!(units.is_empty());
        assert_eq!(same, 1, "the touched quest unit ended as it started; non-quest ids are ignored");
        let (recs, ..) = merge(&own, &BTreeSet::new(), &BTreeMap::new(), &[], |_| Some(128));
        let m = index(&recs);
        assert!(flag_in(&m, 71_100_450) && flag_in(&m, 71_100_120) && !flag_in(&m, 71_100_452));
    }

    #[test]
    fn per_player_flags_never_make_or_join_a_quest_unit() {
        // 71_100_910 a shop flag, bought in the session: per player.
        let own = vec![rec(71_100_000, &[71_100_950])];
        let base = vec![rec(71_100_000, &[])];
        let live = vec![rec(71_100_000, &[71_100_910])];
        let pp = |id: u32| id == 71_100_910;
        let touched: BTreeSet<u32> = [71_100_910].into_iter().collect();
        let (units, same) = changed_quest_units(&touched, QUEST_UNITS, &base, &live, &own, pp);
        assert!(units.is_empty() && same == 1, "a purchase alone does not take the unit");
        // A real change next to it: taken, but without the shop flag.
        let live = vec![rec(71_100_000, &[71_100_910, 71_100_901])];
        let touched: BTreeSet<u32> = [71_100_910, 71_100_901].into_iter().collect();
        let (units, _) = changed_quest_units(&touched, QUEST_UNITS, &base, &live, &own, pp);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].flags.len(), 99);
        assert!(units[0].flags.iter().all(|&(id, _)| id != 71_100_910));
        let mut taken = BTreeMap::new();
        take_units(&mut taken, &units);
        // The restore keeps the shop flag at its current value (keep list), whatever the unit says.
        let keep = vec![(71_100_910, true), (9_500, false)];
        let (recs, ..) = merge(&own, &BTreeSet::new(), &taken, &keep, |_| Some(128));
        let m = index(&recs);
        assert!(flag_in(&m, 71_100_901) && flag_in(&m, 71_100_910) && !flag_in(&m, 71_100_950));
    }

    #[test]
    fn a_later_fold_replaces_the_unit_and_records_mode_takes_more() {
        let mut taken: BTreeMap<u32, bool> = [(71_100_450, true), (71_100_451, false), (71_100_120, true)].into_iter().collect();
        let newer = TakenUnit { start: 71_100_400, len: 100, flags: vec![(71_100_451, true)], differ_own: 1, on: 1 };
        take_units(&mut taken, &[newer]);
        assert_eq!(taken, [(71_100_451, true), (71_100_120, true)].into_iter().collect());
        // Mode 2: one touched flag takes the whole record.
        let own = vec![rec(71_100_000, &[71_100_120])];
        let base = vec![rec(71_100_000, &[])];
        let live = vec![rec(71_100_000, &[71_100_455])];
        let touched: BTreeSet<u32> = [71_100_455].into_iter().collect();
        let (units, _) = changed_quest_units(&touched, QUEST_RECORDS, &base, &live, &own, |_| false);
        assert_eq!((units[0].start, units[0].len, units[0].differ_own), (71_100_000, 1000, 2));
    }

    #[test]
    fn fresh_progress_only_counts_new_flags() {
        let mut host = vec![0u8; 128];
        set_bit(&mut host, 1, true); // the host had idol 1 already
        let mut own = vec![0u8; 128];
        set_bit(&mut own, 2, true); // we had idol 2 in our own world
        let baseline = vec![(11_100_000, host)];
        let backup = vec![(11_100_000, own)];
        let on_now = |id: u32| matches!(id, 11_100_001 | 11_100_002 | 11_100_003);
        let fresh = fresh_progress([11_100_001, 11_100_002, 11_100_003, 11_100_004], on_now, &baseline, &backup);
        assert_eq!(fresh, vec![11_100_003]);
    }

    #[test]
    fn window_rule_is_map_flags_with_a_shared_k() {
        assert!(window_eligible(11_105_800));
        assert!(window_eligible(12_500_000));
        assert!(window_eligible(20_000_001));
        assert!(!window_eligible(11_102_800), "k = 2 is local");
        assert!(!window_eligible(71_100_500), "NPC");
        assert!(!window_eligible(9_801), "global time of day");
        assert!(!window_eligible(61_000_000), "group 6");
    }

    #[test]
    fn objact_anim_matches_full_or_category_id() {
        let set: HashSet<u32> = [60_070, 60_120].into_iter().collect();
        assert!(objact_anim(60_070, &set));
        assert!(objact_anim(100_060_120, &set));
        assert!(!objact_anim(60_071, &set));
        assert!(!objact_anim(0, &set));
        assert!(!objact_anim(-1, &set));
    }

    #[test]
    fn fingerprint() {
        let s = Saved { sen_total: 500, ..Default::default() };
        assert!(same_character(&s, Some(500)));
        assert!(same_character(&s, Some(9000)));
        assert!(same_character(&s, None));
        assert!(!same_character(&s, Some(10)));
    }

    #[test]
    fn idol_window_takes_the_live_activation_set() {
        // Live 2026-10-23: activating idol 1101956 changed these within 3 s.
        for id in [11_100_016, 11_100_120, 236, 232, 9110] {
            assert!(idol_window_eligible(id), "{id}");
        }
        assert!(!idol_window_eligible(9801), "time of day");
        assert!(!idol_window_eligible(9512), "death range");
        assert!(!idol_window_eligible(6911), "group 0 k = 6 is local");
        assert!(!idol_window_eligible(71_100_500), "NPC");
        assert!(!idol_window_eligible(0));
    }

    #[test]
    fn idol_activation_flags_take_their_session_value_in_the_merge() {
        // Own world: idol 6 never activated, its "not yet" flag 11100016 on.
        let backup = vec![rec(11_100_000, &[11_100_000, 11_100_016])];
        let progress: BTreeSet<u32> = [11_100_006].into_iter().collect();
        // The activation's other flags at their session values: 16 off, 232/236 on.
        let keep = vec![(11_100_016, false), (232, true), (236, true)];
        let (recs, set, ..) = merge(&backup, &progress, &BTreeMap::new(), &keep, |_| Some(128));
        let m = index(&recs);
        assert!(flag_in(&m, 11_100_006) && flag_in(&m, 11_100_000));
        assert!(!flag_in(&m, 11_100_016), "the activation's off flag");
        assert!(flag_in(&m, 232) && flag_in(&m, 236));
        assert_eq!(set, 1);
    }

    #[test]
    fn reload_idol_is_one_unlocked_in_the_restored_world() {
        let rows = [(1_101_950, 11_100_000), (1_101_952, 11_100_002), (1_101_956, 11_100_006), (1_111_953, 11_110_003)];
        let own = |f: u32| matches!(f, 11_100_000 | 11_110_003);
        // Live: the last idol 1101952 came from the join rest, never unlocked in our world.
        assert_eq!(pick_reload_idol(1_101_952, &rows, own).0, 1_101_950, "same map first");
        assert_eq!(pick_reload_idol(1_111_953, &rows, own).0, 1_111_953, "our own last idol");
        assert_eq!(pick_reload_idol(2_001_954, &rows, |f| f == 11_110_003).0, 2_001_954, "unknown row: as it is");
        assert_eq!(pick_reload_idol(1_101_956, &rows, |f| f == 11_110_003).0, 1_111_953, "another map");
        assert_eq!(pick_reload_idol(1_101_956, &rows, |_| false).0, 1_101_956, "nothing unlocked");
    }
}
