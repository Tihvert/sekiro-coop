//! Shared world: event flags (doors, keys, bosses, shortcuts, NPC dialogue)
//! and resting at an idol (first test version, 2026-10-22).
//!
//! Flags: the SetEventFlag hook (hook.rs) records every flag write. Writes
//! that pass `should_sync`, happen in the real world (bank 0, not a Remnant
//! memory) and not during the first seconds after a load (the game's own
//! init scripts rewrite flags from the save then) go to the friend as
//! Event::Game{FLAG, id, on | version << 1}. Sends are coalesced per flag and
//! paced so the reliable queue never overflows; the version lets the friend
//! drop a value that arrives after a newer one. The friend applies them with
//! the game's own SetEventFlag. Flags we applied are not echoed back.
//! Rest: the game puts the idol's entity id at `REST_NOW` for one frame when
//! the player rests. We send it; the friend triggers a rest at the same idol
//! by writing that id to `REST_REQUEST` when in the same map block. Whether
//! that also respawns the friend's enemies is not verified yet (RE-FINDINGS-2
//! §2: it runs Rest + the idol effect).
//! Everything lands in the co-op save (S0000.coop.sl2), never the normal one.
//!
//! Join snapshot (2026-10-22, RE-FINDINGS-2 § Event flag storage): on every
//! new connection the host sends all its non-zero shared flag records (bank
//! 0) as one reliable bulk transfer; the client, before its join warp,
//! zeroes its own shared records and copies the host's bytes in (raw memory
//! copy, no SetEventFlag, so no event fires before the reload; the map
//! scripts read the state when the warp loads the area). Without the zeroing
//! a flag the client had set and the host had not (the client's "Chained
//! Ogre killed") survived the join and EMEVD disabled the Ogre on load.
//! The raw copy bypasses our SetEventFlag hook (it hooks the function entry;
//! a memory write never runs it), so nothing of it is echoed back to the
//! host. This deliberately overwrites the client's shared flags with the
//! host's: they are saved into the client's co-op save (S0000.coop.sl2),
//! which is separate from the normal save, so the solo game is untouched.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use coop_net::{FLAG_DUMP_DISABLED, FLAG_DUMP_OK, FLAG_DUMP_UNAVAILABLE, decode_flag_dump, encode_flag_dump};

use crate::game::{self, read, read_ptr};
use crate::hook::{self, FlagEntry};
use crate::log;
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::tuning;

pub const EV_FLAG: u8 = 1;
pub const EV_REST: u8 = 2;
/// "I died for real" (`base+0x3EA0D74` went 0 -> 1: penalty done, idol reload next).
pub const EV_DEATH: u8 = 3;
/// Host, a few seconds after its death reload: "I am back, send what you killed".
pub const EV_BACK: u8 = 4;
/// Bulk from the client after EV_BACK: i32 entity ids of enemies dead on its side.
pub const BULK_DEAD_LIST: u8 = 5;
/// Boss fight of the sender: a = 1 started, 2 boss fell (HandleBossDefeat), 3 gauge gone
/// without a fall; b = boss entity id (docs/SPEC-BOSS-RULES.md §0.2).
pub const EV_BOSS: u8 = 6;
/// a = 1: the sender died in a boss fight and waits on the death screen; 0: it stopped waiting.
pub const EV_DOWN: u8 = 7;
/// Host → joiner with the join snapshot: the host's last idol (game::last_idol), for the join warp.
pub const EV_HOST_IDOL: u8 = 41;
/// Bulk from the killer of a boss: the item lots the fight gave it (bossrules.rs).
pub const BULK_BOSS_REWARD: u8 = 6;
/// Bulk transfer kind of the host's flag dump.
pub const BULK_FLAGS: u8 = 1;
/// Host -> client bulk: the story phase mask (u32 LE, bit i = `STORY_PHASE[i]`). Sent after the
/// join dump's records and whenever the host's own mask changes. (Taken kinds checked: 1, 5, 6,
/// 7, 20-23, 30, 41-45, 50, 60, 0x4E and the roster's 0x52.)
pub const BULK_PHASE: u8 = 8;

const EVENT_FLAG_MAN: usize = 0x3D55FE8;
const FM_READY: usize = 0x228;
const FM_BANK: usize = 0x220;
const SET_EVENT_FLAG_RVA: usize = 0x6C4520;
/// Active bank's group descriptors: 10 x {block entries*, u32 count} (0x18).
const FM_GROUPS: usize = 0x218;
/// u64 byte sizes of the two bank buffers at `+0x00` / `+0x08`.
const FM_BUF_SIZES: usize = 0x10;
const GROUP_STRIDE: usize = 0x18;
/// Block entry = 10 records x {u8* bits, u64 size} (+8).
const BLOCK_ENTRY: usize = 168;
const RECORD: usize = 16;
/// `0x6C63F0(fm, id, 0) -> record*` (null if the group/block is unknown).
const RESOLVE_RVA: usize = 0x6C63F0;
/// Areas >= 90 map to the global block (0x6C6090), already covered by 0.
const AREAS: u32 = 90;
const MAX_RECORD_BYTES: u64 = 1024;
/// The client waits this long for the host's dump before warping anyway.
const DUMP_WAIT: Duration = Duration::from_secs(15);
/// The host waits this long for its own flags to be readable.
const DUMP_HOST_WAIT: Duration = Duration::from_secs(10);
/// MenuMan +0x20: 1 in the world, 0 on a loading screen (public SekiroTool
/// fact, not verified on 1.06): logged at apply time only.
const MENU_MAN: usize = 0x3D67408;
const MENU_IN_WORLD: usize = 0x20;
const REST_REQUEST: usize = 0x3AFEB98;
/// Idol mailbox for the respawn of all characters and objects (`OnEvent_BonfireRespawn`
/// `0x69B030` writes REST_REQUEST and this one). REST_REQUEST alone only refills the
/// player (RE-DEATH-RESET.md §2.1).
const RESPAWN_REQUEST: usize = 0x3AFEBA0;
const GET_EVENT_FLAG_RVA: usize = 0x6C3E60;
const DEATH_COMMITTED: usize = 0x3EA0D74;
const WORLD_CHR_MAN: usize = 0x3D7A1E0;
const ALL_CHR_RESPAWN_RVA: usize = 0xA4C0F0;
/// `0x7A95B0()`: the item restock of a rest (RE-DEATH-RESET §2.1, §1.4).
const ITEM_RESTOCK_RVA: usize = 0x7A95B0;
/// `0xA2BE40(player, idolValue)`: the idol's refill of the player (RE-DEATH-RESET §2.1).
const PLAYER_REST_RVA: usize = 0xA2BE40;
const OBJ_RESPAWN_RVA: usize = 0x899500;
const OBJ_MAN: usize = 0x3D5D600;
const REST_NOW: usize = 0x3AFEB9C;
const REST_ECHO_FOR: Duration = Duration::from_secs(2);
/// After a load the game's init scripts restore flags from the save; those
/// are not news for the friend.
const SETTLE_AFTER_LOAD: Duration = Duration::from_secs(5);
const FLUSH_EVERY: Duration = Duration::from_millis(100);
/// Unacknowledged reliable events allowed before we wait (queue holds 64).
const MAX_IN_FLIGHT: usize = 32;
const STATS_EVERY: Duration = Duration::from_secs(60);

fn base() -> usize {
    game::module_base()
}

/// Player state flags each game toggles for itself (live 7.5: 9100 flips false -> true
/// around a scripted moment; copied to the friend it left them able only to move, no
/// menu, no attacks). Never shared, kept across the join snapshot like 9500..9599.
/// 2026-10-24 (auditor finding): 21114178, 21114179, 21115312, 21115313 and 11125671 bounced
/// between the players on every join, so they are per player too. They sit inside shared
/// records, so the join dump (whole records) is followed by `restore_death_flags`, which
/// puts our own values back, same as 9100/9110.
///
/// 2026-10-24 (dragonrot on NPCs): 627 and 70..=75 are the Dragon's Blood Droplet cure. Common
/// event 626 waits for 627, resets the counter and sets 70-75; as shared flags (group 0, k = 0)
/// a client's cure would cure the host's world, so they are per player too.
pub(crate) const PER_PLAYER_STATE: [u32; 14] =
    [9100, 9110, 21_114_178, 21_114_179, 21_115_312, 21_115_313, 11_125_671, 627, 70, 71, 72, 73, 74, 75];

/// A flag that is per player state (never shared; see `PER_PLAYER_STATE`).
pub(crate) fn per_player_state(id: u32) -> bool {
    PER_PLAYER_STATE.contains(&id)
}

/// Story phase flags (all group 0, k = 8, which `shared_ks(0)` never dumps): 8301 Interior
/// Ministry invasion (set by common event 910), 8302 Ashina burning / 2nd invasion (Demon of
/// Hatred appear flag), 8304 Hirata 2nd memory (Owl Father appear flag), 8306 prologue (on in a
/// new game), 8310 castle enemies of the 8302 phase [INFERRED]. Unshared, each machine ran its
/// own phase. The host decides (`send_allowed`, `apply_flag`), the join sends the mask
/// (`BULK_PHASE`), the client repairs drift. 8303, 8305, 8307, 8330, 8335, 9530 stay local.
pub(crate) const STORY_PHASE: [u32; 5] = [8301, 8302, 8304, 8306, 8310];
/// Endings seen: local, logged and compared by the doctor only.
pub(crate) const ENDINGS: [u32; 4] = [6830, 6831, 6832, 6833];

/// Bit i = `STORY_PHASE[i]` is on.
pub(crate) fn phase_mask(get: impl Fn(u32) -> bool) -> u32 {
    STORY_PHASE.iter().enumerate().fold(0, |m, (i, &id)| if get(id) { m | 1 << i } else { m })
}

/// The five phase flags with the value `mask` gives each.
pub(crate) fn mask_flags(mask: u32) -> [(u32, bool); 5] {
    std::array::from_fn(|i| (STORY_PHASE[i], mask >> i & 1 != 0))
}

/// "8301+8306" (or "none"), for logs and notices.
pub(crate) fn phase_names(mask: u32) -> String {
    let on: Vec<String> = mask_flags(mask).iter().filter(|f| f.1).map(|f| f.0.to_string()).collect();
    if on.is_empty() { "none".to_string() } else { on.join("+") }
}

/// May this machine send its own write of `id`? A client never sends a story phase flag: the
/// host's phase is the session's.
fn send_allowed(id: u32, is_host: bool) -> bool {
    is_host || !STORY_PHASE.contains(&id)
}

fn encode_phase(mask: u32) -> Vec<u8> {
    mask.to_le_bytes().to_vec()
}

fn decode_phase(d: &[u8]) -> Option<u32> {
    <[u8; 4]>::try_from(d).ok().map(u32::from_le_bytes)
}

/// Client: how long a received flag dump waits for the host's `BULK_PHASE`.
const PHASE_WAIT: Duration = Duration::from_secs(3);
/// How often the phase is compared (client repair, host change check).
const PHASE_CHECK: Duration = Duration::from_secs(1);

/// Which flag writes are shared. The game's own net-sync rule (0x6C4240,
/// RE-FINDINGS-2 §1) plus the 7xxxxxxx NPC/dialogue range, which the game
/// never syncs (risky: NPC quest state; reported as such in OHJE).
pub fn should_sync(id: u32) -> bool {
    // Death penalty / Unseen Aid / dragonrot control flags: per player, even though
    // the DS3 rule (k = 9) would share them (live: the friend got "Unseen Aid"
    // whenever the host died).
    // 2026-10-24: ids 20..=30 are the ending / New Game+ requests (InGameStep 0x140A71190:
    // 20-23 start the ending, 30 requests the next lap). Never shared: the companion must
    // not get the ending or NG+ (ngblock.rs).
    if id == 0 || (9500..=9599).contains(&id) || (20..=30).contains(&id) || PER_PLAYER_STATE.contains(&id) || is_item_flag(id) {
        return false;
    }
    // Story phase (group 0, k = 8): shared, host authoritative.
    if STORY_PHASE.contains(&id) {
        return true;
    }
    let top = id / 10_000_000 % 10;
    let k = id / 1000 % 10;
    match top {
        0 => matches!(k, 0 | 1 | 5 | 9),
        1 | 2 => matches!(k, 0 | 1 | 4 | 5 | 6 | 7),
        6 => k == 0,
        7 => true,
        _ => false,
    }
}

/// The `k` digits whose records `should_sync` shares, per group digit. A
/// record is exactly one (group, area, block, k), so a record is either
/// wholly shared or wholly local.
fn shared_ks(group: u32) -> &'static [u32] {
    match group {
        0 => &[0, 1, 5, 9],
        1 | 2 => &[0, 1, 4, 5, 6, 7],
        6 => &[0],
        7 => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        _ => &[],
    }
}

/// One shared flag record of the active bank.
struct FlagRecord {
    /// Lowest flag id stored in it (`id % 1000 == 0`).
    base: u32,
    bits: usize,
    size: usize,
}

fn resolve(fm: usize, id: u32) -> usize {
    let f: extern "C" fn(usize, i32, u8) -> usize = unsafe { std::mem::transmute(base() + RESOLVE_RVA) };
    f(fm, id as i32, 0)
}

/// The record behind `rec` (a record address inside group `g`), checked
/// against the group's block count: the resolver itself does not check it.
fn record_at(fm: usize, g: u32, rec: usize, base_id: u32) -> Option<FlagRecord> {
    let desc = read_ptr(fm + FM_GROUPS)? + GROUP_STRIDE * g as usize;
    let entries = read_ptr(desc)?;
    let count = read::<u32>(desc + 8)? as usize;
    let off = rec.checked_sub(entries)?;
    if off / BLOCK_ENTRY >= count || off % BLOCK_ENTRY >= 10 * RECORD || off % RECORD != 0 {
        return None;
    }
    let bits = read_ptr(rec)?;
    let size = read::<u64>(rec + 8)?;
    if size == 0 || size > MAX_RECORD_BYTES || !in_flag_storage(fm, bits, size as usize) {
        return None;
    }
    Some(FlagRecord { base: base_id, bits, size: size as usize })
}

/// Every record's bit array lives in one of the two bank buffers
/// (`fm+0x00/+0x08`, sizes `fm+0x10/+0x18`, verified in 0x6C3DC0, which
/// memsets both whole). A record outside them is not one we understand:
/// never read or write it.
fn in_flag_storage(fm: usize, bits: usize, size: usize) -> bool {
    (0..2).any(|bank| {
        let (Some(buf), Some(len)) = (read_ptr(fm + 8 * bank), read::<u64>(fm + FM_BUF_SIZES + 8 * bank)) else {
            return false;
        };
        let end = bits.checked_add(size).map(|e| e as u64);
        let lim = (buf as u64).checked_add(len);
        bits >= buf && matches!((end, lim), (Some(e), Some(l)) if e <= l)
    })
}

/// Every shared record of the active bank, each once (several (area,
/// block) pairs can map to the global block). About 4500 resolver calls:
/// one per (group, area, block), the k digits are pointer arithmetic.
fn shared_records(fm: usize) -> Vec<FlagRecord> {
    let mut out = Vec::new();
    for g in [0u32, 1, 2, 6, 7] {
        let ks = shared_ks(g);
        let mut seen = std::collections::HashSet::new();
        for area in 0..AREAS {
            for block in 0..10u32 {
                let id0 = g * 10_000_000 + area * 100_000 + block * 10_000;
                let rec0 = resolve(fm, id0 + ks[0] * 1000);
                if rec0 < 0x10000 {
                    continue;
                }
                let entry = rec0 - RECORD * ks[0] as usize;
                if !seen.insert(entry) {
                    continue;
                }
                for &k in ks {
                    let id = id0 + k * 1000;
                    if let Some(r) = record_at(fm, g, entry + RECORD * k as usize, id)
                        && should_sync(id + 1)
                    {
                        out.push(r);
                    }
                }
            }
        }
    }
    out
}

/// "g1k5"-style class of a record, for the log (which kinds were copied:
/// item pickups would show up as a class here once identified).
fn class_of(base: u32) -> String {
    format!("g{}k{}", base / 10_000_000 % 10, base / 1000 % 10)
}

fn class_summary(bases: impl Iterator<Item = u32>) -> String {
    let mut m: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for b in bases {
        *m.entry(class_of(b)).or_insert(0) += 1;
    }
    m.iter().map(|(k, v)| format!("{k}:{v}")).collect::<Vec<_>>().join(" ")
}

/// The flag manager if loaded and in the real world (bank 0).
fn flag_man() -> Option<usize> {
    read_ptr(base() + EVENT_FLAG_MAN)
        .filter(|&fm| read::<u8>(fm + FM_READY).unwrap_or(0) != 0)
        .filter(|&fm| read::<u8>(fm + FM_BANK).unwrap_or(1) == 0)
}

/// `EV_FLAG` / `EV_TALK_FLAG` payload `b` (PLAN-MULTIPLAYER P4): bit 0 the value, bits 1-3
/// the slot of the player whose world wrote it (7 = unknown), bits 4-30 the sender's
/// version. The host is the sequencer: a client's change reaches the other clients as a
/// host event (`from` = 0), so the origin travels in the payload for the boss gate and the
/// NPC introductions, which decide per player.
const ORIGIN_UNKNOWN: u32 = 7;
const VERSION_MASK: u32 = 0x07FF_FFFF;

fn encode_flag(on: bool, origin: PeerId, version: u32) -> i32 {
    let o = u32::from(origin.slot()).min(ORIGIN_UNKNOWN);
    (u32::from(on) | (o << 1) | ((version & VERSION_MASK) << 4)) as i32
}

/// (value, origin, version).
fn decode_flag(b: i32) -> (bool, Option<PeerId>, u32) {
    let b = b as u32;
    (b & 1 != 0, PeerId::new(((b >> 1) & 7) as u8), (b >> 4) & VERSION_MASK)
}

/// 27-bit wrapping "a is newer than b".
fn newer(a: u32, b: u32) -> bool {
    let d = a.wrapping_sub(b) & VERSION_MASK;
    d != 0 && d <= VERSION_MASK / 2
}

/// Host: a client's flag change waiting to be re-sent to the other clients as our own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Relay {
    on: bool,
    origin: PeerId,
    talk: bool,
    /// We sent a different value of this flag to everyone just before: the origin may have
    /// applied ours after its own write, so it hears the sequenced value too (host order wins).
    echo: bool,
}

/// Host: a different value of ours sent this recently means a write race with the origin.
const ECHO_WINDOW: Duration = Duration::from_secs(5);

/// Host relay targets of a client's change: every other peer, the origin too on a race.
fn relay_targets(peers: impl Iterator<Item = PeerId>, r: &Relay) -> Vec<PeerId> {
    peers.filter(|&p| p != r.origin || r.echo).collect()
}

/// Host: what to re-send of client `from`'s change (`n_peers` connected peers; `ours` =
/// our last value of the flag sent to everyone, and when). Two players: nothing (nobody
/// else to tell, exactly v7.5).
fn relay_for(n_peers: usize, from: PeerId, on: bool, talk: bool, ours: Option<(bool, Instant)>, now: Instant) -> Option<Relay> {
    if n_peers < 2 || from.is_host() {
        return None;
    }
    let echo = ours.is_some_and(|(v, t)| v != on && now.duration_since(t) < ECHO_WINDOW);
    Some(Relay { on, origin: from, talk, echo })
}

/// One peer's own state as its events told us (world.rs keeps one per sender).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerWorld {
    /// Its committed death (`EV_DEATH`), or the boss wipe moment (`note_wipe`).
    pub death_at: Option<Instant>,
    /// Host: "I am back after my death, send what you killed" (`EV_BACK`), not yet taken.
    pub back: bool,
    /// Its boss fight (entity, since) from `EV_BOSS(1)` until it fell, ended, or the peer
    /// died, rested or loaded.
    pub boss: Option<(i32, Instant)>,
    /// The last `EV_BOSS(1)` (entity, when). Unlike `boss` it survives loads and rests; only its
    /// boss falling (`EV_BOSS(2)`) or its death clears it (`peer_recent_boss`).
    pub last_boss: Option<(i32, Instant)>,
    /// It died in a boss fight and waits on its death screen (`EV_DOWN`).
    pub down: bool,
}

#[derive(Default)]
pub struct World {
    /// Session epoch: a client's = the host link's; the host's = the epoch of the link that
    /// started the session (PLAN-MULTIPLAYER P4: a later client's join is not a new session).
    epoch: u64,
    /// The links seen (host: every client; client: the host), per peer epoch.
    links: crate::peers::EpochTracker,
    role_host: Option<bool>,
    ring_seen: Option<u32>,
    /// Last value told to the friend (or received from it) per flag.
    known: HashMap<u32, bool>,
    /// Changed flags waiting to be sent (latest value wins).
    dirty: HashMap<u32, bool>,
    version: u32,
    /// Highest version applied per (sender, flag): every sender counts its own versions.
    remote_version: HashMap<(PeerId, u32), u32>,
    /// Host with 2+ clients: clients' changes to re-send to the others (latest per flag).
    relay_q: BTreeMap<u32, Relay>,
    /// Host: the last value we sent to everyone per flag, and when (write races).
    host_sent: HashMap<u32, (bool, Instant)>,
    /// Host: which client's NPC introduction a talk block belongs to (npcintro.rs).
    talk_order: crate::npcintro::TalkOrder,
    relayed: u32,
    echoed: u32,
    talk_dropped: u32,
    /// Client: flag events from someone other than the host (never sent by our builds).
    not_from_host: u32,
    /// Friend's flags received while the flag manager was not ready.
    queued: Vec<(u32, bool)>,
    last_flush: Option<Instant>,
    last_rest: i32,
    rest_from_friend_at: Option<Instant>,
    /// Death sync (RE-DEATH-RESET.md P3): our `DEATH_COMMITTED` last frame and when we
    /// last died for real (the peers' deaths are in `peer_world`).
    last_committed: u32,
    pub own_death_at: Option<Instant>,
    /// The `own_death_at` we already refilled after (death_refill, once per death reload).
    death_refilled_for: Option<Instant>,
    /// We rested ourselves (not a mirrored rest); taken by the death debuff.
    pub own_rest_now: bool,
    /// What each peer told us about itself (death, back, boss fight, down), keyed by the
    /// sender of the event (PLAN-MULTIPLAYER §5.3). Kept across a link drop, as the old
    /// single-friend fields were, so read it by the current peer's id: with two players
    /// there is one entry per friend id seen in this game (a role switch adds the other).
    peer_world: BTreeMap<PeerId, PeerWorld>,
    /// New `EV_BOSS(1)`s from these peers; bossrules.rs shows the notices and clears them.
    boss_announce: Vec<PeerId>,
    writes_per_id: HashMap<u32, u32>,
    sent: u32,
    received: u32,
    skipped_load: u32,
    lost: u32,
    stats_at: Option<Instant>,
    // Join snapshot.
    is_client: bool,
    epoch_at: Option<Instant>,
    /// Host: joiners whose join snapshot is still to be sent (since when).
    dump_due: BTreeMap<PeerId, Instant>,
    /// Client: the dump for this epoch was applied (or the host has none).
    dump_done: bool,
    /// Client: received, waiting for our flag manager.
    pending_dump: Option<Vec<u8>>,
    pending_dump_at: Option<Instant>,
    /// Client: the host's story phase mask (`BULK_PHASE`, kept current by its flags).
    host_phase: Option<u32>,
    /// Host: the mask last sent to the clients.
    phase_sent: Option<u32>,
    phase_check_at: Option<Instant>,
    phase_held: u32,
    /// Client: the friend's flag changes of this epoch (latest per id).
    /// Held while we wait for the dump, and re-applied after it (they are
    /// newer than the host's snapshot).
    epoch_flags: HashMap<u32, bool>,
    hold_released: bool,
    /// Each player hears an NPC's introduction (npcintro.rs).
    intro: crate::npcintro::Intro,
    /// Dirty flags written in our own conversation: sent as `EV_TALK_FLAG`.
    talk_tagged: std::collections::HashSet<u32>,
    /// Ring-overflow resync: our shared records as last reconciled (this load), the time in
    /// the world last frame (a smaller value = a load), a diff that is due, the last rest seen.
    shadow: Option<HashMap<u32, Vec<u8>>>,
    prev_in_world: Duration,
    diff_due: Option<(Instant, &'static str)>,
    rest_seen: Option<Instant>,
}

impl World {
    /// `in_world_for`: time since the player appeared after the last load. `peers`: this
    /// frame's other players. The host is the flag sequencer (PLAN-MULTIPLAYER §2): a client
    /// sends its flag changes to the host only and takes flags only from the host; the host
    /// applies a client's change and re-sends it to the other clients as its own.
    pub fn on_frame(&mut self, net: &mut Net, peers: &PeerSet, in_world_for: Duration) {
        let now = Instant::now();
        let is_host = net.is_host();
        DOWN_SLOTS.store(down_mask(self.peer_world.iter().map(|(id, p)| (id.slot(), p.down))), Ordering::Release);
        // Sessions per peer link (P4 epochs): the host's `Net::epoch()` changes with every
        // client's join; only a join with nobody from before still linked is a new session.
        let links = if is_host {
            peers.epochs().collect()
        } else {
            vec![(PeerId::HOST, peers.host().map_or(net.epoch(), |h| h.epoch))]
        };
        let joins = self.links.update(links);
        let fresh = joins.fresh || self.role_host != Some(is_host);
        self.role_host = Some(is_host);
        if !fresh && is_host {
            for &id in &joins.joined {
                // A client joined the running session: its own join snapshot, its versions
                // and boss state start over; nothing of the others' changes.
                self.remote_version.retain(|(p, _), _| *p != id);
                self.dump_due.insert(id, now);
                let p = self.peer_mut(id);
                p.boss = None;
                p.down = false;
                log::line(&format!(
                    "world: peer {} joined the running session ({} players): join snapshot for it only",
                    id.slot(),
                    peers.players()
                ));
            }
        }
        if fresh {
            // New session: the friend's state is unknown again.
            self.epoch = self.links.epoch_of(joins.joined.last().copied().unwrap_or(PeerId::HOST)).unwrap_or(net.epoch());
            self.known.clear();
            self.dirty.clear();
            self.remote_version.clear();
            self.relay_q.clear();
            self.host_sent.clear();
            self.talk_order.reset();
            self.is_client = !is_host;
            self.epoch_at = Some(now);
            self.dump_due = if is_host { peers.iter().map(|p| (p.id, now)).collect() } else { BTreeMap::new() };
            self.dump_done = false;
            self.pending_dump = None;
            self.pending_dump_at = None;
            self.host_phase = None;
            self.phase_sent = None;
            self.phase_check_at = None;
            self.epoch_flags.clear();
            self.hold_released = false;
            // The peers' fights and waits end with the session (their deaths and "back"
            // stay for the death sync, as before).
            for p in self.peer_world.values_mut() {
                p.boss = None;
                p.down = false;
            }
            self.boss_announce.clear();
            crate::doors::reset();
            self.intro.reset();
            self.talk_tagged.clear();
        }
        // Our conversation state (npcintro.rs); blocks whose NPC we just heard ourselves.
        let releases = self.intro.frame(now);
        if !self.dump_due.is_empty() {
            self.host_dump(net, now);
        }
        if self.is_client {
            self.client_dump(net, now);
        }
        let settled = in_world_for >= SETTLE_AFTER_LOAD;
        self.phase_check(net, peers, now, is_host, settled);
        let lost_before = self.lost;

        // Our flag writes -> dirty set.
        let count = hook::flag_ring_count();
        let mut n = *self.ring_seen.get_or_insert(count);
        // We are not called on loading screens or while the link is down, and
        // the game keeps writing flags meanwhile: after an hour that was a
        // loop over every write ever made, in one frame. Only the newest
        // FLAG_RING writes still exist; jump to them.
        let behind = count.wrapping_sub(n);
        if behind > hook::FLAG_RING {
            self.lost += behind - hook::FLAG_RING;
            n = count.wrapping_sub(hook::FLAG_RING);
        }
        while n != count {
            match hook::flag_ring_entry(n) {
                FlagEntry::Pending => break,
                FlagEntry::Lost => self.lost += 1,
                FlagEntry::Ready(id, on, bank) => {
                    if bank == 0 && should_sync(id) {
                        *self.writes_per_id.entry(id).or_insert(0) += 1;
                        if !settled {
                            // The save's own value: remember it, so only a
                            // later real change is news.
                            self.skipped_load += 1;
                            self.known.insert(id, on);
                            self.dirty.remove(&id);
                        } else if self.known.get(&id) != Some(&on) {
                            if !send_allowed(id, is_host) {
                                // Story phase on a client: the host decides; the repair puts
                                // the host's value back.
                                self.known.insert(id, on);
                                self.dirty.remove(&id);
                                self.phase_held += 1;
                                if self.phase_held <= 20 {
                                    log::line(&format!("world: phase flag {id} from our script held (host authoritative)"));
                                }
                            // Doors (doors.rs): our own ObjAct toggles the door flag, so
                            // opening a door the friend already opened (flag on, our
                            // object still closed) would switch it off for both.
                            } else if !on && self.known.get(&id) == Some(&true) && crate::doors::keep_open(id) {
                                self.write_quiet(id, true);
                            } else {
                                // NPC talk flags (npcintro.rs): tagged, or kept here while we
                                // hear an NPC the friend met first.
                                match self.intro.own_write(id, on, self.known.get(&id).copied()) {
                                    crate::npcintro::Own::Suppress => {
                                        self.known.insert(id, on);
                                        self.dirty.remove(&id);
                                        self.talk_tagged.remove(&id);
                                    }
                                    crate::npcintro::Own::SendTalk => {
                                        self.talk_tagged.insert(id);
                                        self.dirty.insert(id, on);
                                        self.relay_q.remove(&id);
                                    }
                                    crate::npcintro::Own::Send => {
                                        self.talk_tagged.remove(&id);
                                        self.dirty.insert(id, on);
                                        // Our own newer write wins over a client's still queued.
                                        self.relay_q.remove(&id);
                                    }
                                }
                            }
                        } else {
                            self.dirty.remove(&id);
                        }
                    }
                }
            }
            n = n.wrapping_add(1);
        }
        self.ring_seen = Some(n);
        // Ring overflow / rest: changes the ring dropped are found by a record diff.
        self.diff_resync(now, in_world_for, lost_before);

        // Dirty flags -> friend, paced. Host: to everyone; client: to the host only (the host
        // sequences them). Then, host with 2+ clients: the clients' changes to the others.
        if self.last_flush.is_none_or(|t| now.duration_since(t) >= FLUSH_EVERY) {
            self.last_flush = Some(now);
            let room = MAX_IN_FLIGHT.saturating_sub(net.pending_events());
            let own_dest = if is_host { Dest::All } else { Dest::Host };
            let batch: Vec<(u32, bool)> = self.dirty.iter().take(room).map(|(&k, &v)| (k, v)).collect();
            let used = batch.len();
            for (id, on) in batch {
                self.dirty.remove(&id);
                self.known.insert(id, on);
                self.version = self.version.wrapping_add(1) & VERSION_MASK;
                let kind = if self.talk_tagged.remove(&id) { crate::npcintro::EV_TALK_FLAG } else { EV_FLAG };
                net.send_event_to(own_dest, kind, id as i32, encode_flag(on, peers.me(), self.version));
                if is_host {
                    self.host_sent.insert(id, (on, now));
                }
                crate::audit::flag_write(id, on, true);
                crate::statelog::flag_write(id, on, true);
                self.sent += 1;
                if self.sent <= 30 || self.sent % 100 == 0 {
                    log::line(&format!("world: sent flag {id} = {on} (#{})", self.sent));
                }
            }
            if is_host && !self.relay_q.is_empty() {
                let batch: Vec<(u32, Relay)> = self.relay_q.iter().take(room.saturating_sub(used)).map(|(&k, &r)| (k, r)).collect();
                for (id, r) in batch {
                    self.relay_q.remove(&id);
                    self.version = self.version.wrapping_add(1) & VERSION_MASK;
                    let kind = if r.talk { crate::npcintro::EV_TALK_FLAG } else { EV_FLAG };
                    let b = encode_flag(r.on, r.origin, self.version);
                    for p in relay_targets(peers.iter().map(|p| p.id), &r) {
                        net.send_event_to(Dest::Peer(p), kind, id as i32, b);
                    }
                    self.host_sent.insert(id, (r.on, now));
                    self.relayed += 1;
                    self.echoed += u32::from(r.echo);
                    if self.relayed <= 30 || self.relayed % 100 == 0 {
                        log::line(&format!(
                            "world: re-sent peer {}'s flag {id} = {} to the others (#{}{})",
                            r.origin.slot(),
                            r.on,
                            self.relayed,
                            if r.echo { ", also back to it: write race, our order wins" } else { "" }
                        ));
                    }
                }
            }
        }

        // Our death -> friend (the committed death, not a Resurrection).
        let committed = read::<u32>(base() + DEATH_COMMITTED).unwrap_or(0);
        if committed == 1 && self.last_committed != 1 {
            self.own_death_at = Some(now);
            net.send_event(EV_DEATH, 0, 0);
            log::line("world: we died (death committed), told the friend");
        }
        self.last_committed = committed;

        // Our rest -> friend (not when the rest was the friend's).
        let rest = read::<i32>(base() + REST_NOW).unwrap_or(-1);
        if rest != -1 && rest != self.last_rest {
            let echo = self.rest_from_friend_at.is_some_and(|t| t.elapsed() < REST_ECHO_FOR);
            if !echo {
                net.send_event(EV_REST, rest, game::current_block().unwrap_or(-1));
                log::line(&format!("world: rested at idol {rest}, told the friend"));
                self.own_rest_now = true;
                // Rest = reset for both (restsync.rs): our enemies just respawned.
                crate::restsync::note(crate::restsync::Kind::Own, rest);
                // A rest while we wait / are dead and releasing ends the wait too.
                crate::bossrules::rest_while_dead();
            }
        }
        self.last_rest = rest;

        // Friend -> us.
        for (id, on) in std::mem::take(&mut self.queued) {
            self.apply_flag(id, on);
        }
        // We heard an NPC the friend met first (npcintro.rs): undo our own talk flags of
        // its block, then the friend's decisions there.
        for b in releases {
            let (undo, apply) = self.intro.release(b);
            for (id, v) in undo {
                self.write_quiet(id, v);
            }
            for (id, on) in apply {
                self.apply_flag(id, on);
            }
        }
        for (from, kind, a, b) in net.take_game_events_from() {
            match kind {
                EV_FLAG | crate::npcintro::EV_TALK_FLAG => {
                    let id = a as u32;
                    let (on, tagged, ver) = decode_flag(b);
                    // Clients take flags from the host only (the sequencer).
                    if !is_host && !from.is_host() {
                        self.not_from_host += 1;
                        if self.not_from_host <= 5 {
                            log::line(&format!("world: flag {id} from peer {} ignored: flags come from the host only", from.slot()));
                        }
                        continue;
                    }
                    // The story phase is the host's: a client's phase flag is not taken (our own
                    // builds never send one).
                    if is_host && STORY_PHASE.contains(&id) {
                        self.phase_held += 1;
                        if self.phase_held <= 20 {
                            log::line(&format!("world: phase flag {id} from peer {} ignored: the host decides the story phase", from.slot()));
                        }
                        continue;
                    }
                    if self.remote_version.get(&(from, id)).is_some_and(|&v| !newer(ver, v)) {
                        continue; // an older value arrived late
                    }
                    self.remote_version.insert((from, id), ver);
                    self.received += 1;
                    crate::audit::flag_write(id, on, false);
                    crate::statelog::flag_write(id, on, false);
                    // Whose world wrote it: the host knows its sender; a client reads the
                    // host's tag (a client's change re-sent by the host).
                    let origin = if is_host { from } else { tagged.unwrap_or(from) };
                    let talk = kind == crate::npcintro::EV_TALK_FLAG;
                    // A peer's conversation wrote this talk flag: its NPC is a quest NPC.
                    if talk {
                        crate::talklock::note_peer_talk_flag(from, id, now);
                    }
                    if is_host && talk && should_sync(id) && !self.talk_order.admit(from, id, now) {
                        // Two clients met the same NPC at once: the first in our order decides.
                        self.talk_dropped += 1;
                        if self.talk_dropped <= 20 {
                            log::line(&format!(
                                "world: peer {}'s talk flag {id} = {on} dropped: {}",
                                from.slot(),
                                if crate::talklock::host_only() {
                                    "quest choices are the host's (quest_choices_host_only)"
                                } else {
                                    "another companion's introduction of that NPC came first"
                                }
                            ));
                        }
                        continue;
                    }
                    if is_host
                        && should_sync(id)
                        && let Some(r) = relay_for(peers.len(), from, on, talk, self.host_sent.get(&id).copied(), now)
                    {
                        // Sequencer: the other clients hear it from us (our order, our version).
                        self.relay_q.insert(id, r);
                        self.dirty.remove(&id);
                    }
                    if self.is_client {
                        self.epoch_flags.insert(id, on);
                    }
                    // Newer than the host's dump: wait for it, else the
                    // dump's raw copy would undo this change.
                    // Boss start gate (bossgate.rs): far from the friend's fight, its
                    // map / talk flags wait there and are applied (or dropped) later.
                    // NPC introductions (npcintro.rs): the friend's talk flags of an NPC we
                    // never met wait until we talk to it ourselves.
                    let intro_held = kind == crate::npcintro::EV_TALK_FLAG
                        && !self.waiting_for_dump(now)
                        && should_sync(id)
                        && flag_man().is_some_and(|fm| self.intro.friend_talk(id, on, fm));
                    if !intro_held && !self.waiting_for_dump(now) && !crate::bossgate::hold_friend_flag(origin, id, on) {
                        self.apply_flag(id, on);
                    }
                }
                EV_REST => self.apply_rest(from, a, b),
                EV_DEATH => {
                    log::line(&format!("world: the friend (peer {}) died (death committed)", from.slot()));
                    let p = self.peer_mut(from);
                    p.death_at = Some(Instant::now());
                    // It is out of any boss fight now; if we were waiting for it, it is out of
                    // our wait (a wipe once no engaged peer is left, bossrules.rs).
                    p.boss = None;
                    p.last_boss = None;
                    crate::bossrules::peer_out(from, "wipe");
                }
                EV_BOSS => {
                    log::line(&format!("world: the friend's (peer {}) boss fight: state {a}, boss entity {b}", from.slot()));
                    crate::bossgate::friend_boss_event(from, a, b);
                    match a {
                        1 => {
                            let p = self.peer_mut(from);
                            p.boss = Some((b, Instant::now()));
                            p.last_boss = p.boss;
                            if !self.boss_announce.contains(&from) {
                                self.boss_announce.push(from);
                            }
                        }
                        2 => {
                            // Its fight's boss (the event's entity, else the one it announced).
                            let p = self.peer_mut(from);
                            p.last_boss = None;
                            let known = p.boss.take().map(|(e, _)| e);
                            let entity = if b > 0 { b } else { known.unwrap_or(0) };
                            // Only a kill in the fight we wait for ends our wait (3+ players:
                            // another group's boss in the same block must not).
                            crate::bossrules::boss_defeated_by(from, entity);
                            // Own world (ownworld.rs): flags of this defeat are session progress.
                            crate::ownworld::note_defeat("friend's boss");
                        }
                        3 => {
                            self.peer_mut(from).boss = None;
                            crate::bossrules::peer_out(from, "boss gauge gone");
                        }
                        _ => {}
                    }
                }
                EV_DOWN => {
                    self.peer_mut(from).down = a == 1;
                    log::line(&format!(
                        "world: the friend (peer {}) {}",
                        from.slot(),
                        if a == 1 { "is down and waits on its death screen" } else { "stopped waiting" }
                    ));
                    if a == 1 {
                        crate::bossrules::peer_out(from, "wipe");
                    }
                }
                EV_BACK => {
                    log::line(&format!("world: the friend (peer {}) is back after its death", from.slot()));
                    self.peer_mut(from).back = true;
                }
                // Resync: both players rest (resync.rs).
                crate::resync::EV_RESYNC => crate::resync::on_event(a, b),
                EV_HOST_IDOL if from.is_host() => {
                    log::line(&format!("world: the host's last idol is {a}"));
                    crate::join::set_host_idol(a);
                }
                EV_HOST_IDOL => {}
                // NPC talk lock (talklock.rs): the sender talks to NPC a (b = 1) / stopped (0).
                crate::talklock::EV_TALK_LOCK => crate::talklock::on_event(from, a, b, now),
                // Shared Resurrect unlock (resurrect.rs): the sender's deathblow / kill.
                crate::resurrect::EV_REVIVE_UNLOCK => crate::resurrect::on_event(from, a),
                _ => log::line(&format!("world: unknown event {kind} ({a}, {b})")),
            }
        }

        // NPC talk lock (talklock.rs): follow our conversation, send / refresh / end our lock.
        crate::talklock::tick(net, peers, now);

        // Resurrect lock (resurrect.rs): log ours, share the clears our deathblows / kills make.
        crate::resurrect::tick(net, !peers.is_empty(), now);

        if self.stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
            self.stats_at = Some(now);
            let mut top: Vec<(u32, u32)> = self.writes_per_id.drain().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            top.truncate(5);
            log::line(&format!(
                "world: flags sent {} received {} skipped-at-load {} lost {} dirty {}; snapshot chunks unacked {}; most written {top:?}",
                self.sent,
                self.received,
                self.skipped_load,
                self.lost,
                self.dirty.len(),
                net.pending_bulk_chunks()
            ));
            if self.relayed + self.talk_dropped + self.not_from_host > 0 {
                log::line(&format!(
                    "world: sequencer: re-sent {} clients' flags (write races {}), talk flags dropped {}, flags not from the host ignored {}",
                    self.relayed, self.echoed, self.talk_dropped, self.not_from_host
                ));
            }
        }

        // Doors (doors.rs): replay the peers' door openings, check doors after a rest;
        // each peer's animation marks the flags its ObjActs set (fallback door flags).
        if settled && flag_man().is_some() {
            let anims: Vec<(u8, i32)> = peers.iter().filter_map(|p| Some((p.id.slot(), p.snapshot?.anim))).collect();
            crate::doors::tick(&anims, in_world_for);
        }

        // Resync (both players rest) and the session-start doctor / flag cross-check.
        let session = self.epoch;
        crate::resync::tick(self, net, session, in_world_for);
        crate::doctor::tick(net, peers, session, in_world_for);
        // Idols (idols.rs): light the bonfire entry of every unlocked idol (queue + reconcile).
        crate::idols::tick(in_world_for);
    }

    /// Resync (resync.rs): rest at `idol` through the mailboxes, like a mirrored rest,
    /// without telling the friend again (it rests on its own resync event).
    pub fn resync_rest(&mut self, idol: i32) -> bool {
        self.rest_from_friend_at = Some(Instant::now());
        // Everyone rests: no peer is in a boss fight after it.
        for p in self.peer_world.values_mut() {
            p.boss = None;
        }
        let ok = game::write(base() + REST_REQUEST, idol) && game::write(base() + RESPAWN_REQUEST, idol);
        if ok {
            crate::restsync::note(crate::restsync::Kind::Resync, idol);
        }
        ok
    }

    fn apply_flag(&mut self, id: u32, on: bool) {
        if !should_sync(id) {
            return;
        }
        // Story phase: only the host's value is accepted (the host never applies a client's).
        if let Some(i) = STORY_PHASE.iter().position(|&p| p == id) {
            if !self.is_client {
                return;
            }
            if let Some(m) = self.host_phase.as_mut() {
                *m = if on { *m | 1 << i } else { *m & !(1 << i) };
            }
        }
        let ready = read_ptr(base() + EVENT_FLAG_MAN)
            .filter(|&fm| read::<u8>(fm + FM_READY).unwrap_or(0) != 0)
            // Never into a Remnant memory's bank.
            .filter(|&fm| read::<u8>(fm + FM_BANK).unwrap_or(1) == 0);
        let Some(fm) = ready else {
            if self.queued.len() < 4096 {
                self.queued.push((id, on));
            }
            return;
        };
        // Doors (doors.rs guard 3): a peer's OFF for a door flag that is on here is its
        // ObjAct toggle (vanilla doors never close again): dropped, and everyone hears ON
        // (host: the OFF queued for relay to the other clients is dropped too).
        if crate::doors::ignore_friend_off(id, on) {
            self.known.insert(id, true);
            self.dirty.insert(id, true);
            self.relay_q.remove(&id);
            return;
        }
        // A new idol from the friend: say so (live 2026-10-23: it unlocked silently).
        if on && crate::game::event_flag(id) == Some(false) && idol_flags().is_some_and(|s| s.contains(&id)) {
            log::line(&format!("world: the friend unlocked idol flag {id} for us too"));
            // The game's own "SCULPTOR'S IDOL FOUND" banner, as vanilla lights an idol (dialog.rs).
            crate::dialog::idol_banner("Your companion found a new Sculptor's Idol. It is unlocked for you too.");
        }
        // Our hook sees this write too; `known` makes it no news.
        self.known.insert(id, on);
        self.dirty.remove(&id);
        let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(base() + SET_EVENT_FLAG_RVA) };
        set(fm, id, on, false);
        // Own world (ownworld.rs): a quest unit the friend changed counts as session progress.
        crate::ownworld::note_friend_flag(id);
        // Doors (doors.rs): a door the friend opened opens here too if it is loaded.
        crate::doors::friend_flag(id, on);
        // Idols (idols.rs): the unlock flag alone leaves the idol unlit; light its bonfire entry.
        if on {
            crate::idols::note_flag_on(id);
        }
        if self.received <= 30 || self.received % 100 == 0 {
            log::line(&format!("world: friend set flag {id} = {on} (#{})", self.received));
        }
    }

    /// bossgate.rs: a friend's flag change it held back, applied now (as an EV_FLAG would).
    pub(crate) fn apply_friend_flag(&mut self, id: u32, on: bool) {
        self.apply_flag(id, on);
    }

    /// bossgate.rs: a local write the friend must not hear about (a start flag of the
    /// friend's fight switched back here). `known` takes the value first, so our ring
    /// sees no news. Real world only.
    pub(crate) fn write_quiet(&mut self, id: u32, on: bool) -> bool {
        let Some(fm) = flag_man() else { return false };
        self.known.insert(id, on);
        self.dirty.remove(&id);
        let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(base() + SET_EVENT_FLAG_RVA) };
        set(fm, id, on, false);
        true
    }

    /// Rest at `idol` without telling the friend (the join rest: only we need
    /// our enemies back, the host must not respawn its own).
    pub fn silent_rest(&mut self, idol: i32) {
        self.rest_from_friend_at = Some(Instant::now());
        let ok = game::write(base() + REST_REQUEST, idol) && game::write(base() + RESPAWN_REQUEST, idol);
        log::line(&format!("world: join rest at idol {idol} (ok = {ok}), not told to the friend"));
    }

    fn apply_rest(&mut self, from: PeerId, idol: i32, block: i32) {
        let here = game::current_block().unwrap_or(-2);
        self.rest_from_friend_at = Some(Instant::now());
        // A rest ends the resting peer's boss fight, whatever we do with it.
        self.peer_mut(from).boss = None;
        // Boss rules (SPEC-BOSS-RULES §3.1). Waiting on the death screen: wake up at the
        // idol (the reload also resets the arena, like the rest).
        // Also dead and not waiting (releasing, no death screen): a mailbox rest does
        // nothing for a dead player, so the reload is forced instead (bossrules.rs).
        if crate::bossrules::rest_while_dead() {
            log::line(&format!("world: peer {} rested at idol {idol} while we are dead / waiting on the death screen: waking up (any peer's rest releases)", from.slot()));
            return;
        }
        // Alive in a boss fight: the mailboxes below would reset the arena under us
        // (boss event state half done). Send us to our own idol instead.
        if tuning::boss_expel()
            && game::player().and_then(|p| p.hp()).is_some_and(|hp| hp > 0)
            && game::boss_state().is_some()
        {
            log::line(&format!("world: friend rested at idol {idol} while we are in a boss fight: sending us to our idol"));
            crate::bossrules::request_expel();
            return;
        }
        if here != block {
            // The idol is not loaded here, so its mailbox does nothing: reset our
            // characters and objects directly (no refill; we did not rest).
            let ok = respawn_all_noted(idol);
            // v8.14: a rest by anybody is a rest for everybody: HP, gourd, resurrection and the
            // item restock too, which the idol would only do in its own block.
            let refilled = refill_player_direct();
            log::line(&format!("world: friend rested at idol {idol} in block {block}, we are in {here}: enemies reset directly (ok = {ok}), refilled and restocked directly (ok = {refilled})"));
            return;
        }
        let ok = game::write(base() + REST_REQUEST, idol) && game::write(base() + RESPAWN_REQUEST, idol);
        // `OnEvent_BonfireRespawn` (0x69B030) = both mailboxes + the item restock 0x7A95B0: the
        // mailboxes alone left the friend's Spirit Emblems / pellets unrestocked (live 2026-10-25).
        let restocked = ok && restock_items();
        log::line(&format!("world: friend rested at idol {idol}, resting here too (ok = {ok}), items restocked (ok = {restocked})"));
        // restsync.rs checks that the idol's mailbox really respawned our enemies (else
        // it calls AllChrRespawn directly) and keeps the host's dead from killing ours.
        if ok {
            crate::restsync::note(crate::restsync::Kind::Mirrored, idol);
        }
    }
}

impl World {
    /// A boss wipe seen from the death screen: everyone reloads now, so the death
    /// windows of `death_sync` start from this moment (a long wait must not make the
    /// reload look like a single death). Every peer we know of and every peer in
    /// `peers` counts as dead now.
    pub fn note_wipe(&mut self, peers: &PeerSet) {
        let now = Instant::now();
        self.own_death_at = Some(now);
        for p in peers.iter() {
            self.peer_mut(p.id);
        }
        for p in self.peer_world.values_mut() {
            p.death_at = Some(now);
        }
    }

    /// `death_refill`: once per death reload, when we are back in the world (`since_load` =
    /// time since the load), settled for `delay_s` and alive, give the player a rest's HP,
    /// gourd, resurrection and item restock (`Rest(player,100)` + `0x7A95B0`) WITHOUT the
    /// idol mailboxes, so no enemy respawns and no flag changes. Game thread only.
    pub fn death_refill(&mut self, in_world_since: Instant, delay_s: u64) {
        let Some(t) = self.own_death_at else { return };
        if !crate::tuning::death_refill()
            || self.death_refilled_for == Some(t)
            || in_world_since <= t
            || in_world_since.elapsed().as_secs() < delay_s
        {
            return;
        }
        let Some(p) = game::player() else { return };
        let Some(before) = p.hp() else { return };
        if before <= 0 {
            return; // still dead / not loaded: try again next frame
        }
        self.death_refilled_for = Some(t);
        let ok = refill_player_direct();
        let after = p.hp().unwrap_or(-1);
        log::line(&format!("death: refill after the death reload (hp {before} -> {after}, items restocked ok={ok})"));
    }

    fn peer_mut(&mut self, id: PeerId) -> &mut PeerWorld {
        self.peer_world.entry(id).or_default()
    }

    /// What `id` told us about itself (None: nothing yet).
    pub fn peer(&self, id: PeerId) -> Option<&PeerWorld> {
        self.peer_world.get(&id)
    }

    /// When `id`'s death was committed (`EV_DEATH`, or a boss wipe).
    pub fn peer_death_at(&self, id: PeerId) -> Option<Instant> {
        self.peer(id)?.death_at
    }

    /// `id`'s boss fight (entity, since).
    pub fn peer_boss(&self, id: PeerId) -> Option<(i32, Instant)> {
        self.peer(id)?.boss
    }

    /// `id` started a boss fight within `within` (survives its load; cleared by its death or
    /// its boss falling): (entity, age).
    pub fn peer_recent_boss_info(&self, id: PeerId, within: Duration) -> Option<(i32, Duration)> {
        let (e, t) = self.peer(id)?.last_boss?;
        let age = t.elapsed();
        (age <= within).then_some((e, age))
    }

    /// `id` started a boss fight within `within` (see `peer_recent_boss_info`).
    pub fn peer_recent_boss(&self, id: PeerId, within: Duration) -> bool {
        self.peer_recent_boss_info(id, within).is_some()
    }

    /// `id` waits on its death screen in a boss fight.
    pub fn peer_down(&self, id: PeerId) -> bool {
        self.peer(id).is_some_and(|p| p.down)
    }

    /// bossrules.rs: `id`'s fight is over for us (it loaded since it started).
    pub fn end_peer_boss(&mut self, id: PeerId) {
        if let Some(p) = self.peer_world.get_mut(&id) {
            p.boss = None;
        }
    }

    /// Every peer's running boss fight: (peer, entity, since), in id order.
    pub fn peer_bosses(&self) -> impl Iterator<Item = (PeerId, i32, Instant)> + '_ {
        self.peer_world.iter().filter_map(|(&id, p)| p.boss.map(|(e, t)| (id, e, t)))
    }

    /// Some peer is in a boss fight.
    pub fn any_peer_boss(&self) -> bool {
        self.peer_bosses().next().is_some()
    }

    /// Some peer waits on its death screen in a boss fight.
    pub fn any_peer_down(&self) -> bool {
        self.peer_world.values().any(|p| p.down)
    }

    /// bossrules.rs: the peers whose `EV_BOSS(1)` is still to be announced (taken once).
    pub fn take_boss_announces(&mut self) -> Vec<PeerId> {
        std::mem::take(&mut self.boss_announce)
    }

    /// puppet.rs `death_sync`: `id`'s `EV_BACK`, taken once. By id, not "the lowest entry":
    /// entries outlive their link, so after a role switch in one game (joined a host = peer
    /// 0, then hosted = peer 1) the lowest entry is the old host's (P1d regression fix).
    pub fn take_peer_back(&mut self, id: PeerId) -> bool {
        self.peer_world.get_mut(&id).is_some_and(|p| std::mem::take(&mut p.back))
    }

    /// Client: the friend's flag changes are held back (dump on its way).
    fn waiting_for_dump(&self, now: Instant) -> bool {
        self.is_client
            && tuning::world_on_join()
            && !self.dump_done
            && self.epoch_at.is_some_and(|t| now.duration_since(t) < DUMP_WAIT)
    }

    /// Client: the join warp must wait (the host's world state is not in
    /// yet, at most DUMP_WAIT after the connection).
    pub fn join_hold(&self, epoch: u64) -> bool {
        self.epoch == epoch && self.waiting_for_dump(Instant::now())
    }

    /// Host: dump the shared records once per joiner's connection, to that joiner only (a
    /// client already in the session must never get one: it would zero its records).
    fn host_dump(&mut self, net: &mut Net, now: Instant) {
        if !tuning::world_on_join() {
            for id in std::mem::take(&mut self.dump_due).into_keys() {
                net.send_bulk_to(Dest::Peer(id), BULK_FLAGS, &encode_flag_dump(FLAG_DUMP_DISABLED, &[]));
                log::line(&format!("world: join snapshot off here, told peer {} not to wait", id.slot()));
            }
            return;
        }
        let Some(fm) = flag_man() else {
            // Loading, or in a Remnant memory (bank 1): try again next frame.
            let late: Vec<PeerId> =
                self.dump_due.iter().filter(|(_, t)| now.duration_since(**t) >= DUMP_HOST_WAIT).map(|(id, _)| *id).collect();
            for id in late {
                self.dump_due.remove(&id);
                net.send_bulk_to(Dest::Peer(id), BULK_FLAGS, &encode_flag_dump(FLAG_DUMP_UNAVAILABLE, &[]));
                log::line(&format!("world: our flags are not readable (Remnant or loading), no join snapshot for peer {}", id.slot()));
            }
            return;
        };
        let due: Vec<PeerId> = std::mem::take(&mut self.dump_due).into_keys().collect();
        let t0 = Instant::now();
        let all = shared_records(fm);
        let mut out = Vec::new();
        let mut raw_bytes = 0;
        for r in &all {
            let mut b = vec![0u8; r.size];
            if !game::read_bytes(r.bits, &mut b) {
                continue;
            }
            raw_bytes += b.len();
            if b.iter().any(|&x| x != 0) {
                out.push((r.base, b));
            }
        }
        let data = encode_flag_dump(FLAG_DUMP_OK, &out);
        let mut ok = true;
        // The story phase (group 0, k = 8: not in the records above), right after them.
        let phase = phase_mask(|id| get_flag(fm, id));
        self.phase_sent = Some(phase);
        for &id in &due {
            ok &= net.send_bulk_to(Dest::Peer(id), BULK_FLAGS, &data);
            ok &= net.send_bulk_to(Dest::Peer(id), BULK_PHASE, &encode_phase(phase));
            // The joiner warps to our idol, not to the first idol of our map (live 2026-10-23:
            // host at the temple, joiner warped to the Outskirts idol first, then teleported back).
            net.send_event_to(Dest::Peer(id), EV_HOST_IDOL, game::last_idol(), 0);
        }
        log::line(&format!(
            "world: join snapshot sent to peer(s) {:?} (ok = {ok}), story phase {}: {} shared records ({raw_bytes} B), {} non-zero, {} B on the wire, {:.1} ms; classes {}",
            due.iter().map(|p| p.slot()).collect::<Vec<_>>(),
            phase_names(phase),
            all.len(),
            out.len(),
            data.len(),
            t0.elapsed().as_secs_f32() * 1000.0,
            class_summary(out.iter().map(|(b, _)| *b))
        ));
    }

    /// Client: take the host's dump when it arrives, apply it when our flags
    /// are ready; release the held flag changes after it (or the timeout).
    fn client_dump(&mut self, net: &mut Net, now: Instant) {
        // From the host only (P4: every joiner gets its own; nobody else sends one).
        while let Some((from, d)) = net.take_bulk_from(BULK_FLAGS) {
            if !from.is_host() {
                log::line(&format!("world: a join snapshot from peer {} ignored (the host's only)", from.slot()));
            } else if tuning::world_on_join() {
                self.pending_dump = Some(d);
                self.pending_dump_at = Some(now);
            } else {
                log::line("world: host's join snapshot ignored (option off here)");
            }
        }
        while let Some((from, d)) = net.take_bulk_from(BULK_PHASE) {
            match decode_phase(&d) {
                Some(m) if from.is_host() => {
                    self.host_phase = Some(m);
                    // A change after the join: compared at once (the repair applies it).
                    self.phase_check_at = None;
                }
                Some(_) => log::line(&format!("world: a story phase from peer {} ignored (the host's only)", from.slot())),
                None => log::line(&format!("world: story phase bulk unreadable ({} B), ignored", d.len())),
            }
        }
        // The dump waits briefly for the phase that follows it (applied in the same frame, before
        // the join warp); a dump without records (status != OK) has none.
        let hold_for_phase = self.host_phase.is_none()
            && self.pending_dump_at.is_some_and(|t| now.duration_since(t) < PHASE_WAIT)
            && self.pending_dump.as_deref().and_then(decode_flag_dump).is_some_and(|(status, _)| status == FLAG_DUMP_OK);
        if !hold_for_phase
            && let Some(fm) = flag_man()
            && let Some(d) = self.pending_dump.take()
        {
            let late = self.dump_done || self.epoch_at.is_none_or(|t| now.duration_since(t) >= DUMP_WAIT);
            match decode_flag_dump(&d) {
                Some((FLAG_DUMP_OK, recs)) => self.apply_dump(fm, &recs, late),
                Some((status, _)) => log::line(&format!("world: host sent no join snapshot (status {status})")),
                None => log::line(&format!("world: join snapshot unreadable ({} B), ignored", d.len())),
            }
            self.dump_done = true;
            // The friend's changes since the connection are newer than the
            // snapshot: apply them now (after a late dump: again).
            let held: Vec<(u32, bool)> = self.epoch_flags.iter().map(|(&k, &v)| (k, v)).collect();
            if !held.is_empty() {
                log::line(&format!("world: applying {} flag changes received since the connection", held.len()));
            }
            for (id, on) in held {
                self.apply_flag(id, on);
            }
            self.hold_released = true;
        }
        // Timed out: stop holding the friend's changes.
        if !self.hold_released && self.is_client && !self.waiting_for_dump(now) && self.epoch_at.is_some() {
            self.hold_released = true;
            if tuning::world_on_join() && !self.dump_done {
                log::line("world: no join snapshot from the host in time, joining with our own flags");
            }
            let held: Vec<(u32, bool)> = self.epoch_flags.iter().map(|(&k, &v)| (k, v)).collect();
            for (id, on) in held {
                self.apply_flag(id, on);
            }
        }
    }

    /// Writes all five story phase flags from `mask` in this frame (SetEventFlag, on and off).
    /// `known` takes the values first, so our own ring sees no news. `fm`: bank 0.
    fn apply_phase(&mut self, fm: usize, mask: u32, how: &str) {
        let was = phase_mask(|id| get_flag(fm, id));
        let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(base() + SET_EVENT_FLAG_RVA) };
        for (id, on) in mask_flags(mask) {
            self.known.insert(id, on);
            self.dirty.remove(&id);
            set(fm, id, on, false);
        }
        log::line(&format!("world: story phase {} {how} (was {})", phase_names(mask), phase_names(was)));
    }

    /// Once a second, in the world (not loading): the host re-sends `BULK_PHASE` to all clients
    /// when its own mask changed; a client whose mask differs from the host's re-applies the
    /// host's whole mask (a script of ours moved the phase on its own).
    fn phase_check(&mut self, net: &mut Net, peers: &PeerSet, now: Instant, is_host: bool, settled: bool) {
        if !settled || self.phase_check_at.is_some_and(|t| now.duration_since(t) < PHASE_CHECK) {
            return;
        }
        let Some(fm) = flag_man() else { return };
        self.phase_check_at = Some(now);
        let own = phase_mask(|id| get_flag(fm, id));
        if is_host {
            match self.phase_sent {
                Some(sent) if sent != own => {
                    self.phase_sent = Some(own);
                    if peers.iter().next().is_some() {
                        let ok = net.send_bulk_to(Dest::All, BULK_PHASE, &encode_phase(own));
                        log::line(&format!(
                            "world: our story phase changed {} -> {}, told the clients (ok = {ok})",
                            phase_names(sent),
                            phase_names(own)
                        ));
                    }
                }
                Some(_) => {}
                None => self.phase_sent = Some(own),
            }
        } else if self.is_client
            && self.dump_done
            && let Some(host) = self.host_phase
            && host != own
        {
            self.apply_phase(fm, host, "repaired");
        }
    }

    /// Raw copy: zero every shared record of ours, then the host's bytes.
    fn apply_dump(&mut self, fm: usize, recs: &[(u32, Vec<u8>)], late: bool) {
        let t0 = Instant::now();
        // Own world (ownworld.rs): back up our own shared records before they are zeroed
        // (only the first snapshot of a session; later ones fold the progress so far).
        crate::ownworld::before_join_snapshot(fm);
        let keep = self.own_death_flags(fm);
        let mine = shared_records(fm);
        // Our item / skill flags in every record that is zeroed below, not only in the
        // host's non-zero ones (a record the host never touched is zeroed too).
        let keep_items = self.own_item_flags(fm, recs.iter().map(|(b, _)| *b).chain(mine.iter().map(|r| r.base)));
        let mut zeroed = 0;
        for r in &mine {
            if crate::game::write_bytes(r.bits, &vec![0u8; r.size]) {
                zeroed += r.size;
            }
        }
        let (mut copied, mut bytes, mut missing) = (0, 0, 0);
        for (base_id, b) in recs {
            // Same (group, area, block, k) on our side: block indices come
            // from static map data, so they match between machines [I].
            let g = base_id / 10_000_000 % 10;
            let rec = resolve(fm, *base_id);
            let Some(r) = (rec >= 0x10000).then(|| record_at(fm, g, rec, *base_id)).flatten() else {
                missing += 1;
                continue;
            };
            if !should_sync(base_id + 1) {
                missing += 1;
                continue;
            }
            let n = r.size.min(b.len());
            if crate::game::write_bytes(r.bits, &b[..n]) {
                copied += 1;
                bytes += n;
            }
        }
        self.restore_death_flags(fm, &keep);
        self.restore_death_flags(fm, &keep_items);
        if !keep_items.is_empty() {
            log::line(&format!("world: kept {} of our own item flags across the join snapshot", keep_items.len()));
        }
        // Our hook never saw these writes, and `known` described our old
        // flags: start the change sync from scratch.
        self.known.clear();
        self.dirty.clear();
        // The host's story phase, all five flags in this frame (ahead of the join warp).
        match self.host_phase {
            Some(m) => self.apply_phase(fm, m, "applied"),
            None => log::line("world: no story phase from the host (older build?): ours stays"),
        }
        // Own world: the host's world as it is now = the baseline session progress is measured from.
        crate::ownworld::after_join_snapshot(fm);
        // Raw record copies never pass SetEventFlag: reconcile the idols (idols.rs).
        crate::idols::request_reconcile();
        let menu = read_ptr(base() + MENU_MAN).and_then(|m| read::<u8>(m + MENU_IN_WORLD));
        log::line(&format!(
            "world: join snapshot applied{}: zeroed {} of our shared records ({zeroed} B), copied {copied} of {} host records ({bytes} B), {missing} not resolvable here, {:.1} ms, MenuMan+0x20 = {menu:?}; classes {}",
            if late { " LATE (after the join warp; takes effect on the next load)" } else { "" },
            mine.len(),
            recs.len(),
            t0.elapsed().as_secs_f32() * 1000.0,
            class_summary(recs.iter().map(|(b, _)| *b))
        ));
    }
}

/// Respawn every loaded character and object, as the idol does after a rest
/// (`0xA4C0F0(WorldChrMan)`, `0x899500(*(base+0x3D5D600))`). Not in a Remnant
/// memory (flag bank 1), where the idol skips it too. Game thread only.
pub fn respawn_all() -> bool {
    let in_memory = read_ptr(base() + EVENT_FLAG_MAN).and_then(|fm| read::<u8>(fm + FM_BANK)) != Some(0);
    let (Some(wcm), Some(objs)) = (read_ptr(base() + WORLD_CHR_MAN), read_ptr(base() + OBJ_MAN)) else { return false };
    if in_memory {
        return false;
    }
    let chr: extern "C" fn(usize) = unsafe { std::mem::transmute(base() + ALL_CHR_RESPAWN_RVA) };
    let obj: extern "C" fn(usize) = unsafe { std::mem::transmute(base() + OBJ_RESPAWN_RVA) };
    chr(wcm);
    obj(objs);
    true
}

/// The item restock a rest does (`0x7A95B0()`, called by `OnEvent_BonfireRespawn` after its two
/// mailbox writes, RE-DEATH-RESET §2.1): carried consumables back up from the store. Game thread
/// only; not in a Remnant memory.
pub fn restock_items() -> bool {
    let in_memory = read_ptr(base() + EVENT_FLAG_MAN).and_then(|fm| read::<u8>(fm + FM_BANK)) != Some(0);
    if in_memory {
        return false;
    }
    let restock: extern "C" fn() = unsafe { std::mem::transmute(base() + ITEM_RESTOCK_RVA) };
    restock();
    true
}

/// A rest for a player whose idol is not loaded here: `Rest(player, 100)` (`0xA2BE40`: HP,
/// gourd, resurrection) plus the item restock.
fn refill_player_direct() -> bool {
    let Some(p) = game::player() else { return false };
    let rest: extern "C" fn(usize, i32) = unsafe { std::mem::transmute(base() + PLAYER_REST_RVA) };
    rest(p.0, 100);
    restock_items()
}

/// `respawn_all` for a rest of the friend's (or a resync): recorded in restsync.rs, so
/// both players' kill bookkeeping and the native session follow the reset.
pub fn respawn_all_noted(idol: i32) -> bool {
    let ok = respawn_all();
    if ok {
        crate::restsync::note(crate::restsync::Kind::Direct, idol);
    }
    ok
}

impl World {
    /// Keeps our own death-penalty flags (9500..9599) across the join dump,
    /// which copies the host's whole 9000-9999 record.
    fn own_death_flags(&self, fm: usize) -> Vec<(u32, bool)> {
        let get: extern "C" fn(usize, u32, bool) -> bool = unsafe { std::mem::transmute(base() + GET_EVENT_FLAG_RVA) };
        (9500..=9599).chain(PER_PLAYER_STATE).map(|id| (id, get(fm, id, false))).collect()
    }

    fn restore_death_flags(&self, fm: usize, flags: &[(u32, bool)]) {
        let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(base() + SET_EVENT_FLAG_RVA) };
        for &(id, on) in flags {
            set(fm, id, on, false);
        }
    }
}

/// Item pickup / shop purchase flags: each player picks up and buys their own
/// things (ER Seamless rule), so these never sync. The set is every non-zero
/// `getItemFlagId*` of ItemLotParam and `eventFlag` of ShopLineupParam, read
/// once the params are loaded (RE-FINDINGS-2: classify by membership, not by
/// flag ranges).
/// Slots (bit per slot) whose `EV_DOWN` says they wait dead in a boss fight, as of the last
/// `on_frame`. nsession's ownfill leaves them out of the boss owner rule.
static DOWN_SLOTS: AtomicU32 = AtomicU32::new(0);

pub fn down_slots() -> u32 {
    DOWN_SLOTS.load(Ordering::Acquire)
}

fn down_mask(peers: impl Iterator<Item = (u8, bool)>) -> u32 {
    peers.filter(|&(s, d)| d && s < 32).fold(0, |m, (s, _)| m | 1 << s)
}

static ITEM_FLAGS: std::sync::OnceLock<std::collections::HashSet<u32>> = std::sync::OnceLock::new();

fn item_flags() -> Option<&'static std::collections::HashSet<u32>> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{ITEMLOT_PARAM_ST, ParamDef, SHOP_LINEUP_PARAM};
    use sekiro::sprj::SoloParamRepository;
    if let Some(s) = ITEM_FLAGS.get() {
        return Some(s);
    }
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    // Found by struct name: the vendor crate's fixed table indices are off by two
    // for some tables in 1.06 (SPEC-DEBUFF-SCALING.md), so they are not trusted.
    let file = |name: &str| repo.params().find(|p| p.struct_name() == name).map(|p| &*p.data);
    let lots_file = file(ITEMLOT_PARAM_ST::NAME)?;
    let shops_file = file(SHOP_LINEUP_PARAM::NAME)?;
    let mut set = std::collections::HashSet::new();
    let mut i = 0;
    while let Some(r) = unsafe { lots_file.get_row_by_index::<ITEMLOT_PARAM_ST>(i) } {
        for f in [
            r.get_item_flag_id(),
            r.get_item_flag_id01(),
            r.get_item_flag_id02(),
            r.get_item_flag_id03(),
            r.get_item_flag_id04(),
            r.get_item_flag_id05(),
            r.get_item_flag_id06(),
            r.get_item_flag_id07(),
            r.get_item_flag_id08(),
        ] {
            if f > 0 {
                set.insert(f as u32);
            }
        }
        i += 1;
    }
    let lots = i;
    let mut j = 0;
    while let Some(r) = unsafe { shops_file.get_row_by_index::<SHOP_LINEUP_PARAM>(j) } {
        if r.event_flag() > 0 {
            set.insert(r.event_flag() as u32);
        }
        j += 1;
    }
    // Learned skills are event flags too (SkillParam `unlockEventFlag`): per player,
    // like items (ownworld.rs, 2026-10-23). Optional: a missing table only logs.
    let mut skills = 0;
    if let Some(skill_file) = file(sekiro::param::SKILL_PARAM_ST::NAME) {
        let mut k = 0;
        while let Some(r) = unsafe { skill_file.get_row_by_index::<sekiro::param::SKILL_PARAM_ST>(k) } {
            if r.unlock_event_flag() > 0 && set.insert(r.unlock_event_flag() as u32) {
                skills += 1;
            }
            k += 1;
        }
    }
    // Tutorial pop-ups (MenuTutorialParam begin/end flags): per player, so each player
    // sees a tutorial only where they trigger it themselves (live 2026-10-23: both players
    // got every pop-up).
    let mut tutorials = 0;
    if let Some(tut_file) = file(sekiro::param::MENU_TUTORIAL_PARAM_ST::NAME) {
        let mut k = 0;
        while let Some(r) = unsafe { tut_file.get_row_by_index::<sekiro::param::MENU_TUTORIAL_PARAM_ST>(k) } {
            for f in [r.begin_event_flag_id(), r.end_event_flag_id()] {
                if f > 0 && set.insert(f as u32) {
                    tutorials += 1;
                }
            }
            k += 1;
        }
    }
    if set.is_empty() {
        return None;
    }
    log::line(&format!(
        "world: {} item/shop/skill/tutorial flags are per player ({lots} item lots, {j} shop rows, {skills} skill flags, {tutorials} tutorial flags)",
        set.len()
    ));
    Some(ITEM_FLAGS.get_or_init(|| set))
}

/// Idol unlock flags (BonfireWarpParam `eventflagId`), for the "new idol" notice.
fn idol_flags() -> Option<&'static std::collections::HashSet<u32>> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{BONFIRE_WARP_PARAM_ST, ParamDef};
    use sekiro::sprj::SoloParamRepository;
    static IDOL_FLAGS: std::sync::OnceLock<std::collections::HashSet<u32>> = std::sync::OnceLock::new();
    if let Some(s) = IDOL_FLAGS.get() {
        return Some(s);
    }
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = repo.params().find(|p| p.struct_name() == BONFIRE_WARP_PARAM_ST::NAME).map(|p| &*p.data)?;
    let set: std::collections::HashSet<u32> =
        unsafe { file.rows::<BONFIRE_WARP_PARAM_ST>() }.map(|(_, r)| r.eventflag_id()).filter(|&f| f > 0).map(|f| f as u32).collect();
    (!set.is_empty()).then(|| IDOL_FLAGS.get_or_init(|| set))
}

/// The per-player flag set (items, shops, skills) for ownworld.rs.
pub(crate) fn per_player_flags() -> Option<&'static std::collections::HashSet<u32>> {
    item_flags()
}

fn is_item_flag(id: u32) -> bool {
    item_flags().is_some_and(|s| s.contains(&id))
}

impl World {
    /// Our own item/shop flags inside the shared records the join dump will
    /// overwrite (the host's pickups must not take our items).
    fn own_item_flags(&self, fm: usize, record_bases: impl Iterator<Item = u32>) -> Vec<(u32, bool)> {
        let Some(items) = item_flags() else { return Vec::new() };
        let get: extern "C" fn(usize, u32, bool) -> bool = unsafe { std::mem::transmute(base() + GET_EVENT_FLAG_RVA) };
        // A record covers 1000 flags from its base id.
        let bases: std::collections::HashSet<u32> = record_bases.collect();
        items
            .iter()
            .filter(|&&id| bases.contains(&(id - id % 1000)))
            .map(|&id| (id, get(fm, id, false)))
            .collect()
    }
}

/// A record diff runs this long after the last overflow (the burst may still go on).
const DIFF_AFTER_OVERFLOW: Duration = Duration::from_millis(300);
/// And this long after every rest of ours (the rest's event restart writes a burst).
const DIFF_AFTER_REST: Duration = Duration::from_secs(3);

impl World {
    /// The hook ring keeps the last `hook::FLAG_RING` writes; a burst larger than that
    /// between two frames (live 7.4: every rest, `lost` +~420) dropped real changes
    /// (doors, idols, quest flags) for good. Our shared records are kept as they were at the
    /// last check of this load (`shadow`, taken when the load has settled); after an
    /// overflow, and after every rest, the records are read again and every flag that
    /// changed since then, is shared, and differs from what the friend was told (`known`)
    /// or is about to be told (`dirty`) is sent. Game thread, connected, real world only.
    fn diff_resync(&mut self, now: Instant, in_world_for: Duration, lost_before: u32) {
        if in_world_for < self.prev_in_world {
            // A load: its own init writes are not news; a new baseline once it settles.
            self.shadow = None;
            self.diff_due = None;
        }
        self.prev_in_world = in_world_for;
        if in_world_for < SETTLE_AFTER_LOAD {
            return;
        }
        if self.lost > lost_before {
            self.diff_due = Some((now + DIFF_AFTER_OVERFLOW, "flag ring overflow"));
        }
        let rest = crate::restsync::last_rest();
        if rest != self.rest_seen {
            self.rest_seen = rest;
            if rest.is_some() && self.diff_due.is_none() {
                self.diff_due = Some((now + DIFF_AFTER_REST, "after a rest"));
            }
        }
        let Some(fm) = flag_man() else { return };
        let Some(shadow) = self.shadow.take() else {
            self.shadow = Some(read_shared_records(fm).into_iter().collect());
            return;
        };
        let why = match self.diff_due {
            Some((t, why)) if now >= t => why,
            _ => {
                self.shadow = Some(shadow);
                return;
            }
        };
        self.diff_due = None;
        let t0 = Instant::now();
        let cur = read_shared_records(fm);
        let changed = changed_flags(&shadow, &cur);
        let mut n = 0;
        for &(id, on) in &changed {
            if !should_sync(id) || self.known.get(&id) == Some(&on) || self.dirty.get(&id) == Some(&on) {
                continue;
            }
            // Same door guard as the ring path (doors.rs).
            if !on && self.known.get(&id) == Some(&true) && crate::doors::keep_open(id) {
                self.write_quiet(id, true);
                continue;
            }
            self.talk_tagged.remove(&id);
            self.dirty.insert(id, on);
            n += 1;
        }
        self.shadow = Some(cur.into_iter().collect());
        if why == "flag ring overflow" || n > 0 {
            log::line(&format!(
                "world: {}, resynced {n} flags by diff ({} changed since the last check, {} records, {:.1} ms; ring lost {} in total)",
                why,
                changed.len(),
                shadow.len(),
                t0.elapsed().as_secs_f32() * 1000.0,
                self.lost
            ));
        }
    }
}

/// Flags whose bit differs between two copies of the shared records (a record missing in
/// `old` counts as all off). Bit layout of SetEventFlag: `dword[idx >> 5]` bit
/// `31 - (idx & 31)`, little-endian (ownworld.rs `bit_pos`).
fn changed_flags(old: &HashMap<u32, Vec<u8>>, new: &[(u32, Vec<u8>)]) -> Vec<(u32, bool)> {
    let mut out = Vec::new();
    for (base, cur) in new {
        let prev = old.get(base);
        for (i, &b) in cur.iter().enumerate() {
            let d = b ^ prev.and_then(|v| v.get(i)).copied().unwrap_or(0);
            if d == 0 {
                continue;
            }
            for bit in 0..8u32 {
                if d & (1 << bit) != 0 {
                    let p = 8 * (i as u32 % 4) + bit;
                    let idx = (i as u32 / 4) * 32 + (31 - p);
                    if idx < 1000 {
                        out.push((base + idx, b & (1 << bit) != 0));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod peer_tests {
    use super::*;
    use crate::peers::PeerFrame;

    #[test]
    fn per_peer_state_reads() {
        let mut w = World::default();
        assert_eq!(w.peer_death_at(PeerId(1)), None);
        assert!(!w.any_peer_boss() && !w.any_peer_down() && !w.take_peer_back(PeerId(1)));
        let t = Instant::now();
        w.peer_mut(PeerId(2)).boss = Some((1110800, t));
        w.peer_mut(PeerId(1)).death_at = Some(t);
        w.peer_mut(PeerId(1)).back = true;
        w.peer_mut(PeerId(2)).down = true;
        assert_eq!(w.peer_death_at(PeerId(1)), Some(t));
        assert!(!w.peer_down(PeerId(1)));
        assert!(w.any_peer_down() && w.peer_down(PeerId(2)));
        assert_eq!(w.peer_bosses().collect::<Vec<_>>(), vec![(PeerId(2), 1110800, t)]);
        assert!(!w.take_peer_back(PeerId(2)), "only the sender's");
        assert!(w.take_peer_back(PeerId(1)));
        assert!(!w.take_peer_back(PeerId(1)), "taken once");
        w.end_peer_boss(PeerId(2));
        assert!(!w.any_peer_boss());
        w.end_peer_boss(PeerId(4)); // unknown: no entry made
        assert!(w.peer(PeerId(4)).is_none());
    }

    /// Joined a host (peer 0) and later hosted (peer 1) in one game: the old host's entry
    /// stays, the death sync must read the current friend's (v7.5 had one field for both).
    #[test]
    fn a_stale_lower_entry_does_not_stand_for_the_current_friend() {
        let mut w = World::default();
        let old = Instant::now();
        w.peer_mut(PeerId(0)).death_at = Some(old);
        let now = old + Duration::from_secs(600);
        w.peer_mut(PeerId(1)).death_at = Some(now);
        w.peer_mut(PeerId(1)).back = true;
        assert_eq!(w.peer_death_at(PeerId(1)), Some(now));
        assert!(!w.take_peer_back(PeerId(0)));
        assert!(w.take_peer_back(PeerId(1)));
    }

    #[test]
    fn flag_payload_round_trip_and_version_wrap() {
        for (on, o, v) in [(true, 0u8, 1u32), (false, 3, 12345), (true, 5, VERSION_MASK), (false, 2, 0)] {
            assert_eq!(decode_flag(encode_flag(on, PeerId(o), v)), (on, Some(PeerId(o)), v));
        }
        // Out-of-range origins read as unknown; versions keep 27 bits.
        assert_eq!(decode_flag(encode_flag(true, PeerId(9), 7)).1, None);
        assert_eq!(decode_flag(encode_flag(true, PeerId(1), VERSION_MASK + 3)).2, 2);
        assert!(encode_flag(true, PeerId(5), VERSION_MASK) >= 0, "never negative on the wire");
        assert!(newer(2, 1) && !newer(1, 2) && !newer(5, 5));
        assert!(newer(0, VERSION_MASK), "wraps");
        assert!(newer(3, VERSION_MASK - 2));
    }

    /// The host sequences flags for 3-6 players: a client's change goes to every other
    /// client, back to its origin only after a write race; two players relay nothing.
    #[test]
    fn host_sequencing_targets() {
        let now = Instant::now();
        let a = PeerId(1);
        assert_eq!(relay_for(1, a, true, false, None, now), None, "two players: exactly v7.5");
        assert_eq!(relay_for(3, PeerId::HOST, true, false, None, now), None);
        for n in 2..=5u8 {
            let peers: Vec<PeerId> = (1..=n).map(PeerId).collect();
            let r = relay_for(peers.len(), a, true, false, None, now).unwrap();
            assert_eq!(relay_targets(peers.iter().copied(), &r), peers[1..].to_vec(), "{n} clients");
            // We sent "off" to everyone a moment ago: the origin may have applied it after
            // its "on", so it hears our sequenced "on" too.
            let r = relay_for(peers.len(), a, true, true, Some((false, now)), now).unwrap();
            assert!(r.echo && r.talk);
            assert_eq!(relay_targets(peers.iter().copied(), &r), peers);
        }
        // Same value, or an old send: no race.
        assert!(!relay_for(3, a, true, false, Some((true, now)), now).unwrap().echo);
        let old = now - ECHO_WINDOW - Duration::from_millis(1);
        assert!(!relay_for(3, a, true, false, Some((false, old)), now).unwrap().echo);
    }

    /// Two clients write the same flag at once (A on, B off); the host's arrival order
    /// (A, then B) ends the same in all three worlds.
    #[test]
    fn write_race_ends_in_host_order() {
        let now = Instant::now();
        let (a, b) = (PeerId(1), PeerId(2));
        let peers = [a, b];
        let mut worlds = [false, true, false]; // host, A (wrote on), B (wrote off)
        let mut host_sent: Option<(bool, Instant)> = None;
        let mut inbox: Vec<(PeerId, bool)> = Vec::new();
        for (from, on) in [(a, true), (b, false)] {
            worlds[0] = on; // the host applies in arrival order
            let r = relay_for(2, from, on, false, host_sent, now).unwrap();
            for p in relay_targets(peers.iter().copied(), &r) {
                inbox.push((p, on));
            }
            host_sent = Some((on, now));
        }
        for (p, on) in inbox {
            worlds[p.index()] = on;
        }
        assert_eq!(worlds, [false, false, false]);
    }

    #[test]
    fn wipe_marks_known_and_present_peers_dead() {
        let mut w = World::default();
        w.peer_mut(PeerId(3)).down = true;
        let mut set = PeerSet::new(PeerId::HOST, true);
        set.push(PeerFrame::bare(PeerId(1)));
        let before = Instant::now();
        w.note_wipe(&set);
        assert!(w.own_death_at.is_some_and(|t| t >= before));
        assert!(w.peer_death_at(PeerId(1)).is_some_and(|t| t >= before));
        assert!(w.peer_death_at(PeerId(3)).is_some_and(|t| t >= before));
        assert_eq!(w.peer_death_at(PeerId(2)), None);
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    /// Same layout as SetEventFlag (and ownworld.rs `bit_pos`).
    fn set(bytes: &mut [u8], idx: u32, on: bool) {
        let p = 31 - (idx & 31);
        let (i, m) = ((idx >> 5) as usize * 4 + (p / 8) as usize, 1u8 << (p % 8));
        if on {
            bytes[i] |= m;
        } else {
            bytes[i] &= !m;
        }
    }

    #[test]
    fn diff_finds_exactly_the_changed_flags() {
        let mut a = vec![0u8; 128];
        set(&mut a, 0, true);
        set(&mut a, 107, true);
        set(&mut a, 999, true);
        let old: HashMap<u32, Vec<u8>> = [(11_105_000, a.clone())].into_iter().collect();
        let mut b = a.clone();
        set(&mut b, 107, false);
        set(&mut b, 17, true);
        set(&mut b, 31, true);
        set(&mut b, 32, true);
        let mut got = changed_flags(&old, &[(11_105_000, b)]);
        got.sort_unstable();
        assert_eq!(got, vec![(11_105_017, true), (11_105_031, true), (11_105_032, true), (11_105_107, false)]);
        // A record we had no copy of: every flag on in it counts.
        let mut c = vec![0u8; 128];
        set(&mut c, 5, true);
        assert_eq!(changed_flags(&old, &[(21_105_000, c)]), vec![(21_105_005, true)]);
        assert!(changed_flags(&old, &[(11_105_000, a)]).is_empty());
    }

    #[test]
    fn player_state_flags_stay_local() {
        assert!(!should_sync(9100));
        assert!(!should_sync(9110));
        for id in [21_114_178, 21_114_179, 21_115_312, 21_115_313, 11_125_671] {
            assert!(!should_sync(id), "{id}");
            assert!(PER_PLAYER_STATE.contains(&id), "{id}");
        }
        // Their neighbours in the same records stay shared.
        assert!(should_sync(21_114_177));
        assert!(should_sync(11_125_670));
        assert!(should_sync(9000));
        assert!(should_sync(9200));
    }

    #[test]
    fn story_phase_flags_sync() {
        for id in STORY_PHASE {
            assert!(should_sync(id), "{id}");
        }
        // Neighbours in the same record stay local (8303, 8305, 8307 ...), as do the endings.
        for id in [8303, 8305, 8307, 8330, 8335, 9530, 6830, 6831, 6832, 6833] {
            assert!(!should_sync(id), "{id}");
        }
        for id in ENDINGS {
            assert!(!should_sync(id), "{id}");
        }
    }

    #[test]
    fn client_phase_write_not_sent() {
        for id in STORY_PHASE {
            assert!(!send_allowed(id, false), "{id}: a client never sends it");
            assert!(send_allowed(id, true), "{id}: the host does");
        }
        assert!(send_allowed(1000, false));
        assert!(send_allowed(11_105_000, false));
    }

    #[test]
    fn phase_mask_round_trip() {
        for mask in 0..32u32 {
            let flags = mask_flags(mask);
            let back = phase_mask(|id| flags.iter().any(|&(i, on)| i == id && on));
            assert_eq!(back, mask);
            assert_eq!(decode_phase(&encode_phase(mask)), Some(mask));
        }
        assert_eq!(phase_mask(|id| id == 8302 || id == 8306), 0b01010);
        assert_eq!(phase_names(0b01010), "8302+8306");
        assert_eq!(phase_names(0), "none");
        assert_eq!(decode_phase(&[1, 2, 3]), None);
    }

    #[test]
    fn dragonrot_cure_flags_stay_local() {
        for id in [627, 70, 71, 72, 73, 74, 75] {
            assert!(!should_sync(id), "{id}");
            assert!(per_player_state(id), "{id}");
        }
        for id in [626, 76, 69] {
            assert!(should_sync(id), "{id}");
            assert!(!per_player_state(id), "{id}");
        }
    }

    #[test]
    fn ending_and_new_game_plus_requests_stay_local() {
        for id in 20..=30 {
            assert!(!should_sync(id), "{id}");
        }
        assert!(should_sync(31));
        assert!(should_sync(1000));
    }
}

// ---- Raw record access for ownworld.rs (the joiner's own world, 2026-10-23) ----

/// The flag manager if loaded and in the real world (bank 0), for ownworld.rs.
pub(crate) fn real_world_flag_man() -> Option<usize> {
    flag_man()
}

/// One flag of the active bank (`0x6C3E60`). `fm` from `real_world_flag_man`.
pub(crate) fn get_flag(fm: usize, id: u32) -> bool {
    let get: extern "C" fn(usize, u32, bool) -> bool = unsafe { std::mem::transmute(base() + GET_EVENT_FLAG_RVA) };
    get(fm, id, false)
}

/// Every shared record of ours as (base id, bytes), zero records included.
pub(crate) fn read_shared_records(fm: usize) -> Vec<(u32, Vec<u8>)> {
    shared_records(fm)
        .into_iter()
        .filter_map(|r| {
            let mut b = vec![0u8; r.size];
            game::read_bytes(r.bits, &mut b).then_some((r.base, b))
        })
        .collect()
}

/// Byte size of the record that holds `base` (any record, shared or not), resolved
/// and bound-checked like the join snapshot does.
pub(crate) fn record_size(fm: usize, base_id: u32) -> Option<usize> {
    let rec = resolve(fm, base_id);
    (rec >= 0x10000).then(|| record_at(fm, base_id / 10_000_000 % 10, rec, base_id)).flatten().map(|r| r.size)
}

/// Raw write without SetEventFlag (no event fires, our hook does not see it), like
/// `apply_dump`: zero every shared record first, then copy `recs` in. Returns
/// (records written, records not resolvable here).
pub(crate) fn write_records_raw(fm: usize, recs: &[(u32, Vec<u8>)]) -> (usize, usize) {
    for r in &shared_records(fm) {
        game::write_bytes(r.bits, &vec![0u8; r.size]);
    }
    let (mut written, mut missing) = (0, 0);
    for (base_id, b) in recs {
        let rec = resolve(fm, *base_id);
        let Some(r) = (rec >= 0x10000).then(|| record_at(fm, base_id / 10_000_000 % 10, rec, *base_id)).flatten() else {
            missing += 1;
            continue;
        };
        let n = r.size.min(b.len());
        if game::write_bytes(r.bits, &b[..n]) {
            written += 1;
        }
    }
    (written, missing)
}

/// Where the record of `base_id` resolves now (ownworld.rs restore safety): (group, index
/// of the block entry the resolver picked for the id's area/block in the group's table,
/// k slot inside that entry, byte size). None = not resolvable here.
pub(crate) fn record_tuple(fm: usize, base_id: u32) -> Option<(u8, u32, u8, u32)> {
    let g = base_id / 10_000_000 % 10;
    let rec = resolve(fm, base_id);
    if rec < 0x10000 {
        return None;
    }
    let r = record_at(fm, g, rec, base_id)?;
    let entries = read_ptr(read_ptr(fm + FM_GROUPS)? + GROUP_STRIDE * g as usize)?;
    let off = rec.checked_sub(entries)?;
    Some((g as u8, (off / BLOCK_ENTRY) as u32, (off % BLOCK_ENTRY / RECORD) as u8, r.size as u32))
}

/// Every record of groups 0/1/2/6/7, shared or local, as (base id, bytes), each once
/// (doctor.rs flag cross-check: read only). Same resolver walk as `shared_records`.
pub(crate) fn read_all_records(fm: usize) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    for g in [0u32, 1, 2, 6, 7] {
        let mut seen = std::collections::HashSet::new();
        for area in 0..AREAS {
            for block in 0..10u32 {
                let id0 = g * 10_000_000 + area * 100_000 + block * 10_000;
                let entry = resolve(fm, id0);
                if entry < 0x10000 || !seen.insert(entry) {
                    continue;
                }
                for k in 0..10u32 {
                    if let Some(r) = record_at(fm, g, entry + RECORD * k as usize, id0 + k * 1000) {
                        let mut b = vec![0u8; r.size];
                        if game::read_bytes(r.bits, &mut b) {
                            out.push((r.base, b));
                        }
                    }
                }
            }
        }
    }
    out
}

/// The five story phase flags of the active bank (bit i = `STORY_PHASE[i]`), for ownworld.rs.
pub(crate) fn read_phase(fm: usize) -> u32 {
    phase_mask(|id| get_flag(fm, id))
}

/// Writes the five story phase flags, on and off, with SetEventFlag (ownworld.rs restore).
pub(crate) fn write_phase(fm: usize, mask: u32) {
    let set: extern "C" fn(usize, u32, bool, bool) = unsafe { std::mem::transmute(base() + SET_EVENT_FLAG_RVA) };
    for (id, on) in mask_flags(mask) {
        set(fm, id, on, false);
    }
}
