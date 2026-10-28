//! Session-start doctor and the flag whitelist cross-check (2026-10-23, NOT verified live;
//! IMPLEMENTATION-NOTES §22). Log and overlay notices only: nothing here changes behaviour.
//!
//! Doctor: 3 s into every session both sides send `BULK_DOCTOR` (kind 30, additive: an
//! older build leaves it unread in its bulk inbox) with `key=value` lines: NG cycle
//! (GameDataMan `+0x70`, the clear count `0x10B8D10` reads, clamped to 0..7 there, +100
//! row offset when `0xA8E120` says so [V IDA 2026-10-23]), flags 6911 and 9830 (the other
//! per-world difficulty state SPEC-DEBUFF-SCALING §3.4 names: 6911 picks the +100
//! ClearCountCorrectParam rows; 9830 is the Demon Bell), save slot + `total_sen_earned`
//! (the own-world fingerprint), protocol version and the settings that must match
//! (`native_session` + stage, `sync_posture`, `enemy_mode`) or are the host's anyway
//! (scaling percentages, logged only). Each side compares and warns: "different NG cycle:
//! enemy HP will differ", ...
//!
//! Cross-check: the game's own "is this flag net-synced" `IsFlagNetSynced(EventFlagMan*,
//! int id) -> bool` at `0x6C4240` [V IDA 2026-10-23: a pure function of the id, called
//! from SetEventFlag `0x6C4980`; top digit 0: k in {0,1,5,9}; 1/2: k in {0,1,4,5,6,7};
//! 6: k == 0; others never] against world.rs `should_sync`, over every flag that is on in
//! the loaded records of groups 0/1/2/6/7 plus every id our SetEventFlag hook ring saw
//! (its last 128 writes before the session, then every write during it). Logged once 10 s
//! into a session, and once more after 10 minutes for the ids first seen meanwhile.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use crate::game::{self, read, read_ptr};
use crate::hook::{self, FlagEntry};
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::{bossrules, log, tuning, world};

/// Bulk transfer kind (world.rs uses 1, 5, 6; puppet.rs 7; testsync 20-23; nsession 0x4E).
pub const BULK_DOCTOR: u8 = 30;
/// `[GAMEPLAY] allow_cross_cycle` (default off): co-op across different NG cycles.
pub static ALLOW_CROSS_CYCLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const SEND_AFTER: Duration = Duration::from_secs(3);
const CHECK_AFTER: Duration = Duration::from_secs(10);
const SECOND_CHECK_AFTER: Duration = Duration::from_secs(600);
const EXAMPLES: usize = 50;
const MAX_SEEN: usize = 200_000;

const GAME_DATA_MAN: usize = 0x3D5AAC0;
/// Clear count (NG cycle), read by `0x10B8D10` as `min(max(v, 0), 7)`.
const GDM_CLEAR_COUNT: usize = 0x70;
const IS_FLAG_NET_SYNCED_RVA: usize = 0x6C4240;
/// Kuro's Charm per SekiroTool (RE-FINDINGS-2 §1, local flag); picks the +100 NG rows.
const FLAG_CHARM: u32 = 6911;
/// Demon Bell (shared flag, the host's after the join snapshot).
const FLAG_BELL: u32 = 9830;

// ---------------------------------------------------------------- doctor (pure)

pub type Info = BTreeMap<String, String>;

pub fn encode(info: &Info) -> Vec<u8> {
    info.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<String>().into_bytes()
}

pub fn decode(data: &[u8]) -> Info {
    String::from_utf8_lossy(data)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// One difference: the log text, and the overlay notice for the ones that matter in play.
#[derive(Debug, PartialEq)]
pub struct Finding {
    pub log: String,
    pub notice: Option<String>,
}

fn ng_name(v: &str) -> String {
    match v.parse::<i64>() {
        Ok(0) => "NG".into(),
        Ok(n) => format!("NG+{n}"),
        Err(_) => v.into(),
    }
}

/// Our info vs the friend's.
pub fn compare(ours: &Info, theirs: &Info) -> Vec<Finding> {
    let mut out = Vec::new();
    let differs = |k: &str| matches!((ours.get(k), theirs.get(k)), (Some(a), Some(b)) if a != b);
    let pair = |k: &str| format!("{} here, {} on the friend's side", ours.get(k).map_or("?", |s| s), theirs.get(k).map_or("?", |s| s));
    if differs("ng") {
        let (a, b) = (ng_name(&ours["ng"]), ng_name(&theirs["ng"]));
        out.push(Finding {
            log: format!("different NG cycle: {a} here, {b} on the friend's side: enemy HP will differ"),
            notice: Some(format!("Different NG cycle (you {a}, companion {b}): enemy HP will differ.")),
        });
    }
    if differs("f6911") {
        out.push(Finding {
            log: format!("flag 6911 (Kuro's Charm, NG row +100) differs: {}: enemy HP / damage will differ", pair("f6911")),
            notice: Some("Kuro's Charm state differs: enemy HP and damage will differ.".into()),
        });
    }
    if differs("f9830") {
        // 2026-10-24: now also a notice. The flag is shared (k = 9), so a difference that
        // is still there when the doctor compares (3 s+ after the join snapshot) is real.
        // The game picks the ClearCountCorrectParam row itself (0x10B8D10, hooking it was
        // rejected, RE-FINDINGS-2 §5b), so the host's value cannot be forced for scaling:
        // warn only [ARV: which of the two flags is the Bell Demon item and which the Charm
        // comes from SekiroTool's table, not checked in game].
        out.push(Finding {
            log: format!("Demon Bell (flag 9830) differs: {} (shared flag: the host's should apply after the join snapshot)", pair("f9830")),
            notice: Some("Demon Bell state differs: enemy damage will differ.".into()),
        });
    }
    let readable = |k: &str| ours.get(k).is_some_and(|v| v != "?") && theirs.get(k).is_some_and(|v| v != "?");
    if differs("phase") && readable("phase") {
        // The phase is the host's (world.rs): the client repairs it, so a difference that is
        // still there when the doctor compares (3 s+ after the join snapshot) is real.
        let names = |i: &Info| world::phase_names(i.get("phase").and_then(|v| u32::from_str_radix(v, 16).ok()).unwrap_or(0));
        let host_is_us = ours.get("role").map(String::as_str) == Some("host");
        let (host, other, other_name) = if host_is_us { (names(ours), names(theirs), "companion") } else { (names(theirs), names(ours), "you") };
        out.push(Finding {
            log: format!("story phase differs: {} (host {host}, {other_name} {other}); the host's applies", pair("phase")),
            notice: Some(format!("Story phase differs (host {host}, {other_name} {other}): host's world applies.")),
        });
    }
    if differs("endings") && readable("endings") {
        out.push(Finding { log: format!("endings seen differ: {} (each side keeps its own)", pair("endings")), notice: None });
    }
    if differs("rot") && readable("rot") {
        // Rot Essences (goods 9500..=9526) are never shared (keyitems.rs), so the NPCs' sickness
        // is whatever the player holds.
        out.push(Finding {
            log: format!("Dragonrot differs (Rot Essences held): {}", pair("rot")),
            notice: Some("Dragonrot differs: NPCs show your own sickness.".into()),
        });
    }
    if differs("proto") {
        out.push(Finding { log: format!("protocol version differs: {}", pair("proto")), notice: Some("Different co-op mod versions: update both.".into()) });
    }
    let native = |i: &Info| i.get("native_session").cloned();
    if differs("native_session") || (native(ours).as_deref() == Some("1") && differs("native_stage")) {
        out.push(Finding {
            log: format!("native_session / stage differ: {} / {}", pair("native_session"), pair("native_stage")),
            notice: Some("native_session differs: use the same value on both PCs.".into()),
        });
    }
    if differs("sync_posture") {
        out.push(Finding {
            log: format!("sync_posture differs: {}", pair("sync_posture")),
            notice: Some("sync_posture differs: use the same value on both PCs.".into()),
        });
    }
    if differs("enemy_mode") {
        out.push(Finding { log: format!("enemy_mode differs: {}", pair("enemy_mode")), notice: Some("enemy_mode differs: use the same value on both PCs.".into()) });
    }
    for k in ["sync_art", "death_debuffs"] {
        if differs(k) {
            out.push(Finding { log: format!("{k} differs: {} (each side uses its own)", pair(k)), notice: None });
        }
    }
    if differs("scale") {
        out.push(Finding { log: format!("scaling percentages differ: {} (the host's are used on both)", pair("scale")), notice: None });
    }
    out
}

/// NG cycle out of an [`Info`] (`ng`, clamped 0..7; `?` or missing = unknown).
fn cycle_of(i: &Info) -> Option<u32> {
    i.get("ng").and_then(|v| v.parse().ok())
}

/// Refuse the session: both cycles known, different, and `allow_cross_cycle` off.
pub fn refuse(ours: Option<u32>, theirs: Option<u32>, allow: bool) -> bool {
    !allow && matches!((ours, theirs), (Some(a), Some(b)) if a != b)
}

fn gather(is_host: bool) -> Info {
    let mut m = Info::new();
    let mut put = |k: &str, v: String| {
        m.insert(k.to_string(), v);
    };
    put("role", if is_host { "host" } else { "client" }.into());
    put("proto", coop_net::protocol::VERSION.to_string());
    let raw = read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read::<i32>(g + GDM_CLEAR_COUNT));
    put("ng_raw", raw.map_or("?".into(), |v| v.to_string()));
    put("ng", raw.map_or("?".into(), |v| v.clamp(0, 7).to_string()));
    let flag = |id| game::event_flag(id).map_or("?".to_string(), |b| (b as u8).to_string());
    put("f6911", flag(FLAG_CHARM));
    put("f9830", flag(FLAG_BELL));
    // Story phase and endings seen (bit i = world.rs `STORY_PHASE[i]` / `ENDINGS[i]`), hex; "?" unreadable.
    let on = |id| game::event_flag(id);
    let mask = |ids: &[u32]| {
        ids.iter().enumerate().try_fold(0u32, |m, (i, &id)| on(id).map(|b| if b { m | 1 << i } else { m })).map_or("?".to_string(), |m| format!("{m:x}"))
    };
    put("phase", mask(&world::STORY_PHASE));
    put("endings", mask(&world::ENDINGS));
    put(
        "rot",
        crate::keyitems::owned_rot_essences().map_or("?".to_string(), |v| {
            if v.is_empty() { "none".to_string() } else { v.iter().map(u32::to_string).collect::<Vec<_>>().join(",") }
        }),
    );
    let (slot, sen) = crate::ownworld::fingerprint();
    put("slot", slot.map_or("?".into(), |s| s.to_string()));
    put("sen", sen.map_or("?".into(), |s| s.to_string()));
    put("native_session", (tuning::native_session() as u8).to_string());
    put("native_stage", tuning::native_session_stage().to_string());
    put("sync_posture", (tuning::sync_posture() as u8).to_string());
    put("sync_art", (tuning::sync_art_effects() as u8).to_string());
    put("enemy_mode", tuning::enemy_mode().to_string());
    put("merge_quests", tuning::merge_quests().to_string());
    put("own_world_mode", crate::ownworld::mode_name().into());
    put("death_debuffs", (tuning::DEATH_DEBUFFS.load(std::sync::atomic::Ordering::Relaxed) as u8).to_string());
    let p = |a: &std::sync::atomic::AtomicU32| a.load(std::sync::atomic::Ordering::Relaxed);
    put(
        "scale",
        format!(
            "{}/{}/{}/{}",
            p(&tuning::ENEMY_HEALTH_SCALING),
            p(&tuning::BOSS_HEALTH_SCALING),
            p(&tuning::ENEMY_POSTURE_SCALING),
            p(&tuning::BOSS_POSTURE_SCALING)
        ),
    );
    m
}

fn one_line(i: &Info) -> String {
    i.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- flag cross-check (pure)

/// Why our rule and the game's disagree on `id` (for the class summary).
fn reason(id: u32, per_player: &dyn Fn(u32) -> bool) -> String {
    if id == 0 {
        "id 0".into()
    } else if (9500..=9599).contains(&id) {
        "death 9500-9599".into()
    } else if per_player(id) {
        "item/shop/skill".into()
    } else if id / 10_000_000 % 10 == 7 {
        "group 7 (NPC)".into()
    } else {
        format!("g{}k{}", id / 10_000_000 % 10, id / 1000 % 10)
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub compared: usize,
    pub both_shared: usize,
    pub both_local: usize,
    pub ours_only: usize,
    pub game_only: usize,
    pub ours_only_by: BTreeMap<String, usize>,
    pub game_only_by: BTreeMap<String, usize>,
    pub ours_only_examples: Vec<u32>,
    pub game_only_examples: Vec<u32>,
}

/// `ours` = world.rs `should_sync`, `theirs` = the game's `IsFlagNetSynced`.
pub fn cross_check(
    ids: impl IntoIterator<Item = u32>,
    ours: impl Fn(u32) -> bool,
    theirs: impl Fn(u32) -> bool,
    per_player: &dyn Fn(u32) -> bool,
) -> Report {
    let mut ids: Vec<u32> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    let mut r = Report { compared: ids.len(), ..Default::default() };
    for id in ids {
        match (ours(id), theirs(id)) {
            (true, true) => r.both_shared += 1,
            (false, false) => r.both_local += 1,
            (true, false) => {
                r.ours_only += 1;
                *r.ours_only_by.entry(reason(id, per_player)).or_insert(0) += 1;
                if r.ours_only_examples.len() < EXAMPLES {
                    r.ours_only_examples.push(id);
                }
            }
            (false, true) => {
                r.game_only += 1;
                *r.game_only_by.entry(reason(id, per_player)).or_insert(0) += 1;
                if r.game_only_examples.len() < EXAMPLES {
                    r.game_only_examples.push(id);
                }
            }
        }
    }
    r
}

/// The flags that are on in `recs` (base id + the bit layout of SetEventFlag).
fn on_flags(recs: &[(u32, Vec<u8>)]) -> Vec<u32> {
    let mut out = Vec::new();
    for (base, bytes) in recs {
        for (w, dw) in bytes.chunks_exact(4).enumerate() {
            let v = u32::from_le_bytes([dw[0], dw[1], dw[2], dw[3]]);
            if v == 0 {
                continue;
            }
            for bit in 0..32u32 {
                // dword[idx >> 5] |= 1 << (31 - (idx & 31)) (ownworld.rs `bit_pos`).
                if v & (1 << (31 - bit)) != 0 {
                    let idx = w as u32 * 32 + bit;
                    if idx < 1000 {
                        out.push(base + idx);
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- runtime

#[derive(Default)]
struct State {
    /// The session (world.rs `session_epoch`): the cross-check runs once per session.
    epoch: u64,
    since: Option<Instant>,
    /// Doctor exchange per peer link (host <-> each client; a client with the host): the
    /// link epoch, since when, and whether ours went out.
    links: BTreeMap<PeerId, (u64, Instant, bool)>,
    ring: Option<u32>,
    seen: HashSet<u32>,
    compared: HashSet<u32>,
    first_check: bool,
    second_check: bool,
}

static STATE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);

fn game_rule(fm: usize) -> impl Fn(u32) -> bool {
    let f: extern "C" fn(usize, i32) -> bool = unsafe { std::mem::transmute(game::module_base() + IS_FLAG_NET_SYNCED_RVA) };
    move |id| f(fm, id as i32)
}

fn per_player(id: u32) -> bool {
    world::per_player_flags().is_some_and(|s| s.contains(&id))
}

fn log_report(what: &str, r: &Report, t0: Instant) {
    log::line(&format!(
        "flagcheck: {what}: {} flags compared, same {} (both shared {}, both local {}), ours only {} {:?}, game only {} {:?}, {:.1} ms",
        r.compared,
        r.both_shared + r.both_local,
        r.both_shared,
        r.both_local,
        r.ours_only,
        r.ours_only_by,
        r.game_only,
        r.game_only_by,
        t0.elapsed().as_secs_f32() * 1000.0
    ));
    if !r.ours_only_examples.is_empty() {
        log::line(&format!("flagcheck: we sync, the game would not (first {}): {:?}", r.ours_only_examples.len(), r.ours_only_examples));
    }
    if !r.game_only_examples.is_empty() {
        log::line(&format!("flagcheck: the game would sync, we do not (first {}): {:?}", r.game_only_examples.len(), r.game_only_examples));
    }
}

/// Every connected in-world frame (world.rs `on_frame`, game thread). `session`: world.rs'
/// session epoch (a second client's join is not a new session on the host).
pub fn tick(net: &mut Net, peers: &PeerSet, session: u64, in_world_for: Duration) {
    let Ok(mut g) = STATE.lock() else { return };
    let st = g.get_or_insert_with(State::default);
    let now = Instant::now();
    if st.epoch != session {
        let ring = hook::flag_ring_count().wrapping_sub(hook::FLAG_RING);
        *st = State { epoch: session, since: Some(now), ring: Some(ring), ..Default::default() };
    }
    let since = st.since.map_or(Duration::ZERO, |t| now.duration_since(t));
    // Host <-> each client, a client with the host (PLAN-MULTIPLAYER P4).
    let links = peers.link_epochs();
    st.links.retain(|id, _| links.iter().any(|(l, _)| l == id));
    for &(id, e) in &links {
        let l = st.links.entry(id).or_insert((e, now, false));
        if l.0 != e {
            *l = (e, now, false);
        }
    }

    // Every flag id our hook sees (real world only).
    let count = hook::flag_ring_count();
    let mut n = st.ring.unwrap_or(count);
    if count.wrapping_sub(n) > hook::FLAG_RING {
        n = count.wrapping_sub(hook::FLAG_RING);
    }
    while n != count {
        match hook::flag_ring_entry(n) {
            FlagEntry::Pending => break,
            FlagEntry::Ready(id, _, 0) if st.seen.len() < MAX_SEEN => {
                st.seen.insert(id);
            }
            _ => {}
        }
        n = n.wrapping_add(1);
    }
    st.ring = Some(n);

    // Doctor.
    if in_world_for >= SEND_AFTER && world::real_world_flag_man().is_some() {
        let mut ours: Option<Info> = None;
        for (&id, l) in st.links.iter_mut() {
            if l.2 || now.duration_since(l.1) < SEND_AFTER {
                continue;
            }
            let info = ours.get_or_insert_with(|| gather(net.is_host()));
            if net.send_bulk_to(Dest::Peer(id), BULK_DOCTOR, &encode(info)) {
                l.2 = true;
                log::line(&format!("doctor: sent ours to peer {}: {}", id.slot(), one_line(info)));
            }
        }
    }
    while let Some((from, data)) = net.take_bulk_from(BULK_DOCTOR) {
        if !st.links.contains_key(&from) {
            log::line(&format!("doctor: peer {}'s report ignored (we compare with the host only)", from.slot()));
            continue;
        }
        let theirs = decode(&data);
        let ours = gather(net.is_host());
        let found = compare(&ours, &theirs);
        log::line(&format!("doctor: peer {}'s: {}", from.slot(), one_line(&theirs)));
        log::line(&format!("doctor: ours now: {}", one_line(&ours)));
        if found.is_empty() {
            log::line("doctor: no mismatch");
        }
        let refused = refuse(cycle_of(&ours), cycle_of(&theirs), ALLOW_CROSS_CYCLE.load(std::sync::atomic::Ordering::Relaxed));
        // A refused cycle difference gets its own REFUSED line and notice instead of the warning.
        let is_cycle = |f: &Finding| f.log.starts_with("different NG cycle");
        for f in found.iter().filter(|f| !(refused && is_cycle(f))) {
            log::line(&format!("doctor: WARNING {}", f.log));
        }
        if refused {
            let (a, b) = (ng_name(&ours["ng"]), ng_name(&theirs["ng"]));
            log::line(&format!("doctor: REFUSED different NG cycle (ours {a}, peer {} {b})", from.slot()));
            bossrules::notice(
                format!("Different NG cycle (you {a}, companion {b}): co-op refused. Set allow_cross_cycle = 1 on both PCs to allow."),
                12,
            );
            if !net.is_host() {
                crate::net::set_intent(crate::net::Intent::Solo);
            }
        }
        let notices: Vec<&str> = found.iter().filter(|f| !(refused && is_cycle(f))).filter_map(|f| f.notice.as_deref()).take(3).collect();
        if !notices.is_empty() {
            bossrules::notice(notices.join("\n"), 12);
        }
    }

    // Flag whitelist cross-check (log only).
    let first = !st.first_check && since >= CHECK_AFTER;
    let second = st.first_check && !st.second_check && since >= SECOND_CHECK_AFTER;
    if !(first || second) {
        return;
    }
    let Some(fm) = world::real_world_flag_man() else { return };
    let t0 = Instant::now();
    if first {
        st.first_check = true;
        let from_records = on_flags(&world::read_all_records(fm));
        let n_rec = from_records.len();
        let ring: Vec<u32> = st.seen.iter().copied().collect();
        let ids: Vec<u32> = from_records.into_iter().chain(ring.iter().copied()).collect();
        st.compared.extend(ids.iter().copied());
        let r = cross_check(ids, world::should_sync, game_rule(fm), &per_player);
        log_report(&format!("session start ({n_rec} on in the loaded records, {} from the hook ring)", ring.len()), &r, t0);
    } else {
        st.second_check = true;
        let fresh: Vec<u32> = st.seen.iter().copied().filter(|id| !st.compared.contains(id)).collect();
        let r = cross_check(fresh, world::should_sync, game_rule(fm), &per_player);
        log_report("flags first written in the last 10 minutes", &r, t0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(pairs: &[(&str, &str)]) -> Info {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn cross_cycle_refusal() {
        assert!(!refuse(Some(1), Some(1), false), "equal");
        assert!(refuse(Some(0), Some(2), false), "different");
        assert!(!refuse(Some(0), Some(2), true), "allowed");
        assert!(!refuse(None, Some(2), false) && !refuse(Some(0), None, false) && !refuse(None, None, false), "unknown");
        assert_eq!(cycle_of(&info(&[("ng", "3")])), Some(3));
        assert_eq!(cycle_of(&info(&[("ng", "?")])), None);
        assert_eq!(cycle_of(&info(&[])), None);
    }

    #[test]
    fn info_round_trip() {
        let i = info(&[("ng", "1"), ("role", "host"), ("scale", "35/100/50/50")]);
        assert_eq!(decode(&encode(&i)), i);
        assert!(decode(b"garbage\n\n=x\n").get("garbage").is_none());
    }

    #[test]
    fn mismatches_that_matter_get_a_notice() {
        let a = info(&[("ng", "0"), ("f6911", "0"), ("native_session", "0"), ("native_stage", "2"), ("sync_posture", "1"), ("scale", "35/100/50/50")]);
        assert!(compare(&a, &a).is_empty());
        let b = info(&[("ng", "2"), ("f6911", "1"), ("native_session", "0"), ("native_stage", "3"), ("sync_posture", "0"), ("scale", "0/0/0/0")]);
        let f = compare(&a, &b);
        assert_eq!(f.len(), 4, "{f:?}");
        assert!(f[0].notice.as_deref().unwrap().contains("you NG, companion NG+2"));
        assert!(f[0].log.contains("enemy HP will differ"));
        assert!(f[1].notice.is_some(), "6911");
        assert!(f[2].log.contains("sync_posture") && f[2].notice.is_some());
        assert!(f[3].log.contains("host's are used") && f[3].notice.is_none(), "scaling is the host's anyway");
        // The stage only matters while the native session is on.
        let on_a = info(&[("native_session", "1"), ("native_stage", "2")]);
        let on_b = info(&[("native_session", "1"), ("native_stage", "3")]);
        assert_eq!(compare(&on_a, &on_b).len(), 1);
        // A key the other side does not send (older build) is not a mismatch.
        assert!(compare(&a, &info(&[("ng", "0")])).is_empty());
    }

    #[test]
    fn bell_demon_and_kuros_charm_mismatches_warn() {
        let host = info(&[("role", "host"), ("f6911", "0"), ("f9830", "1")]);
        let client = info(&[("role", "client"), ("f6911", "1"), ("f9830", "0")]);
        for (a, b) in [(&host, &client), (&client, &host)] {
            let f = compare(a, b);
            assert_eq!(f.len(), 2, "{f:?}");
            assert!(f[0].log.contains("6911") && f[0].notice.as_deref().unwrap().contains("Kuro's Charm"));
            assert!(f[1].log.contains("9830") && f[1].notice.as_deref().unwrap().contains("Demon Bell"));
        }
        assert!(compare(&host, &host).is_empty());
        // An older build that sends neither flag is no mismatch.
        assert!(compare(&host, &info(&[("role", "client")])).is_empty());
    }

    #[test]
    fn phase_mismatch_warns() {
        let host = info(&[("role", "host"), ("phase", "a"), ("endings", "0"), ("rot", "none")]);
        let client = info(&[("role", "client"), ("phase", "9"), ("endings", "1"), ("rot", "9500,9501")]);
        let f = compare(&client, &host);
        assert_eq!(f.len(), 3, "{f:?}");
        assert_eq!(f[0].notice.as_deref(), Some("Story phase differs (host 8302+8306, you 8301+8306): host's world applies."));
        assert!(f[1].log.contains("endings") && f[1].notice.is_none(), "endings: log only");
        assert_eq!(f[2].notice.as_deref(), Some("Dragonrot differs: NPCs show your own sickness."));
        // The host sees the same difference from its side.
        let g = compare(&host, &client);
        assert!(g[0].notice.as_deref().unwrap().contains("host 8302+8306, companion 8301+8306"));
        // Same phase, unreadable ("?") or missing (older build) values: no warning.
        assert!(compare(&host, &host).is_empty());
        let unknown = info(&[("role", "client"), ("phase", "?"), ("endings", "?"), ("rot", "?")]);
        assert!(compare(&host, &unknown).is_empty());
        assert!(compare(&host, &info(&[("role", "client")])).is_empty());
    }

    #[test]
    fn cross_check_counts_and_classifies() {
        let game = |id: u32| {
            let (top, k) = (id / 10_000_000 % 10, id / 1000 % 10);
            match top {
                0 => matches!(k, 0 | 1 | 5 | 9),
                1 | 2 => matches!(k, 0 | 1 | 4 | 5 | 6 | 7),
                6 => k == 0,
                _ => false,
            }
        };
        let pp = |id: u32| id == 11_100_700;
        let ours = |id: u32| id != 0 && !(9500..=9599).contains(&id) && !pp(id) && (game(id) || id / 10_000_000 % 10 == 7);
        let ids = [0, 9_501, 9_801, 11_100_700, 11_100_701, 71_100_500, 71_100_501, 11_102_000, 11_102_000];
        let r = cross_check(ids, ours, game, &pp);
        assert_eq!(r.compared, 8, "duplicates counted once");
        assert_eq!((r.both_shared, r.both_local, r.ours_only, r.game_only), (2, 1, 2, 3));
        assert_eq!(r.ours_only_by.get("group 7 (NPC)"), Some(&2));
        assert_eq!(r.game_only_by.get("item/shop/skill"), Some(&1));
        assert_eq!(r.game_only_by.get("death 9500-9599"), Some(&1));
        assert_eq!(r.game_only_by.get("id 0"), Some(&1));
        assert_eq!(r.game_only_examples, vec![0, 9_501, 11_100_700]);
    }

    #[test]
    fn on_flags_reads_the_set_event_flag_layout() {
        let mut b = vec![0u8; 128];
        // idx 0 -> top bit of dword 0 (byte 3, 0x80); idx 33 -> dword 1, bit 30 (byte 7, 0x40).
        b[3] = 0x80;
        b[7] = 0x40;
        assert_eq!(on_flags(&[(11_100_000, b)]), vec![11_100_000, 11_100_033]);
    }
}
