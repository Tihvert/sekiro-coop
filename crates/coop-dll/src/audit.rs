//! Auditor: finds what one player got and the other did not, and why (IMPLEMENTATION-NOTES
//! §32, test builds, `[DEBUG] audit = 1`, default on). It only observes, logs and reports:
//! nothing here gives an item, sets a flag or changes a character.
//!
//! - **Digest** (bulk `BULK_AUDIT`, message `MSG_DIGEST`): our inventory (every item with its
//!   count), unlocked skills (SkillParam unlock flags on), the per-player item-lot flags we
//!   have taken (ItemLotParam get flags on) with the lot's source as far as we know it, a
//!   checksum per shared flag record (only the bits `world::should_sync` shares) and the
//!   reward pipeline counters. Sent about 45 s after a connection (the hello: a v7.6 friend
//!   without the auditor only logs one "bulk transfer kind 60 received" line and never
//!   answers, so nothing more is sent to it), 25 s after a rest of ours, 30 s after a boss /
//!   miniboss died here, every `audit_every_s` (180) seconds, and 60 s after a comparison
//!   that left differences pending. The receiver compares the friend's digest with its own
//!   state of that moment and answers with its digest, so both sides log the comparison.
//! - **Classification.** A lot flag one side has and the other lacks: EXPECTED when its source
//!   is a map pickup / treasure (per player by design) or unknown; SUSPECT when it is an enemy
//!   drop (NpcParam item lot), a boss / miniboss drop (deathblows >= 2), an NPC talk / event
//!   gift (AwardItemLot outside a boss window, seen at run time on either side) or a boss
//!   reward (AwardItemLot in the 15 s after HandleBossDefeat). Key items (prosthetic tools,
//!   non-consumable goods with max 1) one side owns without a lot difference that explains it:
//!   SUSPECT (sev 2). Skills learned on one side only: SUSPECT (sev 1, each player buys their
//!   own; worth a look when one could not). Shared flag records whose checksums differ are
//!   asked for in detail (`MSG_DETAIL_REQ` / `MSG_DETAIL`) and the exact flag ids reported.
//!   A difference is reported only after it persisted for 45 s (a grant or flag still on its
//!   way is not a bug), each finding once per game run.
//! - **Boss / miniboss HP** (`MSG_HP`, 2 Hz, only while a boss is within 60 m of us, the friend
//!   is in our block and has the auditor, and the bulk queue is idle): HP / posture / lives of
//!   each such boss. SUSPECT when the HP ratios differ by more than 10 % for 3 s, the lives
//!   for 3 s, the posture ratios by more than 30 % for 5 s, or when a boss shows up (6 s after a
//!   load, or when its gauge appears) below its max HP with no damage seen this rest cycle
//!   (here or in the friend's samples).
//! - **Reward pipeline.** keyitems.rs (script grants, boss drops) and bossrules.rs (boss
//!   rewards) report sent / received / resolved. The receiver checks after resolving that the
//!   lot flags are on or the items owned (SUSPECT if not) and acks (`MSG_REWARD`); the sender
//!   reports a grant not resolved 30 s after the friend received it (90 s after sending when
//!   no receipt came).
//! - **Flag ping-pong** (world.rs): the same shared flag written alternately by us and by the
//!   friend in 3 runs within 60 s = SUSPECT "per-player flag?" (this caught nothing in 7.5,
//!   it did not exist; flag 9100 did exactly this).
//! - `%LOCALAPPDATA%\sekiro-coop\audit.txt`: a running summary, newest first. Every SUSPECT
//!   is also one `audit: SUSPECT (sev N) ...` line in log.txt.
//! - `%LOCALAPPDATA%\sekiro-coop\audit-lots.txt`: lot sources learned at run time (script /
//!   boss reward), kept across game starts.
//!
//! **3-6 players** (PLAN-MULTIPLAYER §5.19): star like the rest of multi. Every pair above is
//! one `Link`: the host keeps one per client, a client one (the host); "the friend" in a text
//! is that partner. Digests, detail requests, HP samples and reward acks go to the partner
//! only (`Dest::Peer`); reward counters are per peer. Per-player data keeps the 2-player rules,
//! shared records must match host <-> each client (so across all). The host's rounds are
//! staggered (`STAGGER_MS` per partner, `ROUND_GAP_MS` between any two of its own rounds).
//! With 3+ players, findings and log lines about one peer start with `[peer S 'name']`
//! (`audit: SUSPECT [peer 2 'Friend2'] (sev 3) ...`); two players: v7.7's lines unchanged.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, Chr, read, read_bytes, read_ptr};
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::{items, livesync, log, nsession, persist, world};

/// `[DEBUG] audit` (default on in test builds).
pub static ENABLED: AtomicBool = AtomicBool::new(true);
/// `[DEBUG] audit_every_s`: seconds between periodic digests.
pub static EVERY_S: AtomicU32 = AtomicU32::new(180);

/// Bulk kind of every auditor message (unused before: 1, 5-7, 20-23, 30, 41-43, 50, 0x4E).
pub const BULK_AUDIT: u8 = 60;
const WIRE: u8 = 1;
const MSG_DIGEST: u8 = 1;
const MSG_DETAIL_REQ: u8 = 2;
const MSG_DETAIL: u8 = 3;
const MSG_HP: u8 = 4;
const MSG_REWARD: u8 = 5;

const REASON_HELLO: u8 = 1;
const REASON_PERIODIC: u8 = 2;
const REASON_REST: u8 = 3;
const REASON_BOSS: u8 = 4;
const REASON_FOLLOWUP: u8 = 5;
const REASON_REPLY: u8 = 6;

// Times in `game::now_ms` milliseconds.
const GRACE_MS: u64 = 45_000;
const FOLLOWUP_MS: u64 = 60_000;
/// The client follows up later: the host's round normally covers it.
const FOLLOWUP_CLIENT_MS: u64 = 75_000;
const HELLO_AFTER_MS: u64 = 45_000;
const NO_AUDIT_AFTER_MS: u64 = 90_000;
const REST_AFTER_MS: u64 = 25_000;
const BOSS_AFTER_MS: u64 = 30_000;
const MIN_GAP_MS: u64 = 15_000;
/// 3+ players: a round with every partner (hello, rest, boss death) starts this much later per
/// partner after the first.
const STAGGER_MS: u64 = 10_000;
/// The host: at least this long between two digest rounds it starts (any clients).
const ROUND_GAP_MS: u64 = 5_000;
/// A round the friend started covers our own one due this soon.
const COVERED_MS: u64 = 30_000;
const IN_WORLD_FOR_DIGEST: Duration = Duration::from_secs(15);
const SCAN_EVERY_MS: u64 = 500;
const HP_EVERY_MS: u64 = 500;
const HP_FRESH_MS: u64 = 1_500;
const HP_NEAR_M: f32 = 60.0;
const HP_LIMIT_MS: u64 = 3_000;
const POSTURE_LIMIT_MS: u64 = 5_000;
const HP_RATIO: f32 = 0.10;
const POSTURE_RATIO: f32 = 0.30;
/// HP drops count as damage only after this (persist.rs writes boss records at 3 s).
const DAMAGE_AFTER_LOAD: Duration = Duration::from_secs(6);
const PINGPONG_WINDOW_MS: u64 = 60_000;
const PINGPONG_RUNS: usize = 3;
const REWARD_GIVE_MS: u64 = 30_000;
const REWARD_NO_RECEIPT_MS: u64 = 90_000;
const RESOLVE_CHECK_MS: u64 = 3_000;
/// A boss drop may lie at the corpse: the receiver has this long to pick it up.
const RESOLVE_CHECK_DROP_MS: u64 = 90_000;
const FILE_EVERY_MS: u64 = 2_000;

const MAX_ITEMS: usize = 4096;
const MAX_LIST: usize = 8192;
const MAX_DETAIL_RECORDS: usize = 16;
const MANY_RECORDS: usize = 64;
const MAX_FLAG_FINDINGS: usize = 40;
const MAX_KEPT_FINDINGS: usize = 400;
const MAX_INBOX: usize = 64;
const MAX_HP_ENTRIES: usize = 8;

const REWARD_COUNTERS: usize = 12;

// --- Pure part (unit tested) -----------------------------------------------------------

/// Where a lot's items come from, least to most specific.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Source {
    Unknown = 0,
    Enemy = 1,
    BossDrop = 2,
    Script = 3,
    BossReward = 4,
}

impl Source {
    pub fn from_u8(v: u8) -> Source {
        match v {
            1 => Source::Enemy,
            2 => Source::BossDrop,
            3 => Source::Script,
            4 => Source::BossReward,
            _ => Source::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Unknown => "map pickup / treasure / unknown",
            Source::Enemy => "enemy drop",
            Source::BossDrop => "boss / miniboss drop",
            Source::Script => "NPC talk / event gift",
            Source::BossReward => "boss reward",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Per player by design (map pickups).
    Expected,
    Info,
    /// Probably a bug: severity 1 (low) .. 3 (high).
    Suspect(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// Dedupe key: each key is reported once per game run.
    pub key: String,
    pub class: Class,
    pub text: String,
}

impl Finding {
    fn new(key: String, class: Class, text: String) -> Self {
        Self { key, class, text }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// Weapons (prosthetic tools), protectors, accessories, non-consumable goods with max 1.
    Key,
    /// Non-consumable stackable goods (Prayer Beads, Gourd Seeds, materials).
    Progress,
    /// Consumables, co-op items, unknown rows: never compared.
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LotInfo {
    pub row: i32,
    pub source: Source,
    /// Full item ids (category << 28 | id) of the lot row.
    pub items: Vec<u32>,
}

/// Game data the comparison needs (tests use a fake).
pub trait Lookup {
    fn kind(&self, full: u32) -> ItemKind;
    fn name(&self, full: u32) -> String;
    /// The lot row behind an item-lot get flag.
    fn lot(&self, flag: u32) -> Option<LotInfo>;
    fn skill_row(&self, flag: u32) -> Option<u32>;
}

/// One side's state at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Digest {
    pub seq: u32,
    pub reason: u8,
    /// The sender wants our digest back.
    pub reply: bool,
    pub is_host: bool,
    /// (full item id, count), sorted.
    pub items: Vec<(u32, u32)>,
    /// Skill unlock flags that are on.
    pub skills: Vec<u32>,
    /// Item-lot get flags that are on, with the source the sender knows.
    pub lots: Vec<(u32, u8)>,
    /// (shared record base id, checksum of its shared bits that are on), non-zero only.
    pub records: Vec<(u32, u32)>,
    /// Per pipe (`Pipe`): sent, received, resolved, not given.
    pub rewards: [u32; REWARD_COUNTERS],
}

struct W(Vec<u8>);

impl W {
    fn new(msg: u8) -> Self {
        W(vec![WIRE, msg])
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend(v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend(v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend(v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend(v.to_le_bytes());
    }
}

struct R<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or([0; 8])))
    }
    fn done(&self) -> bool {
        self.p == self.d.len()
    }
}

pub fn encode_digest(d: &Digest) -> Vec<u8> {
    let mut w = W::new(MSG_DIGEST);
    w.u32(d.seq);
    w.u8(d.reason);
    w.u8(d.reply as u8 | (d.is_host as u8) << 1);
    let items = &d.items[..d.items.len().min(MAX_ITEMS)];
    w.u16(items.len() as u16);
    for &(id, n) in items {
        w.u32(id);
        w.u32(n);
    }
    let skills = &d.skills[..d.skills.len().min(MAX_ITEMS)];
    w.u16(skills.len() as u16);
    for &f in skills {
        w.u32(f);
    }
    let lots = &d.lots[..d.lots.len().min(MAX_LIST)];
    w.u16(lots.len() as u16);
    for &(f, s) in lots {
        w.u32(f);
        w.u8(s);
    }
    let recs = &d.records[..d.records.len().min(MAX_LIST)];
    w.u16(recs.len() as u16);
    for &(b, h) in recs {
        w.u32(b);
        w.u32(h);
    }
    w.u8(REWARD_COUNTERS as u8);
    for &c in &d.rewards {
        w.u32(c);
    }
    w.0
}

/// The body after `WIRE, MSG_DIGEST`.
fn decode_digest(r: &mut R) -> Option<Digest> {
    let mut d = Digest { seq: r.u32()?, reason: r.u8()?, ..Default::default() };
    let bits = r.u8()?;
    d.reply = bits & 1 != 0;
    d.is_host = bits & 2 != 0;
    for _ in 0..r.u16()? {
        d.items.push((r.u32()?, r.u32()?));
    }
    for _ in 0..r.u16()? {
        d.skills.push(r.u32()?);
    }
    for _ in 0..r.u16()? {
        d.lots.push((r.u32()?, r.u8()?));
    }
    for _ in 0..r.u16()? {
        d.records.push((r.u32()?, r.u32()?));
    }
    let n = r.u8()? as usize;
    for i in 0..n {
        let c = r.u32()?;
        if i < REWARD_COUNTERS {
            d.rewards[i] = c;
        }
    }
    Some(d)
}

/// The auditor's messages on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Digest(Digest),
    DetailReq(Vec<u32>),
    /// (record base, shared flag ids on in it).
    Detail(Vec<(u32, Vec<u32>)>),
    Hp(Vec<(i32, HpSample)>),
    /// (pipe, key, lot, state).
    Reward(Vec<(u8, u64, i32, u8)>),
}

pub fn encode_detail_req(bases: &[u32]) -> Vec<u8> {
    let mut w = W::new(MSG_DETAIL_REQ);
    let bases = &bases[..bases.len().min(MAX_DETAIL_RECORDS)];
    w.u8(bases.len() as u8);
    for &b in bases {
        w.u32(b);
    }
    w.0
}

pub fn encode_detail(recs: &[(u32, Vec<u32>)]) -> Vec<u8> {
    let mut w = W::new(MSG_DETAIL);
    let recs = &recs[..recs.len().min(MAX_DETAIL_RECORDS)];
    w.u8(recs.len() as u8);
    for (b, ids) in recs {
        let ids = &ids[..ids.len().min(1000)];
        w.u32(*b);
        w.u16(ids.len() as u16);
        for &i in ids {
            w.u32(i);
        }
    }
    w.0
}

pub fn encode_hp(list: &[(i32, HpSample)]) -> Vec<u8> {
    let mut w = W::new(MSG_HP);
    let list = &list[..list.len().min(MAX_HP_ENTRIES)];
    w.u8(list.len() as u8);
    for (id, s) in list {
        w.i32(*id);
        w.i32(s.hp);
        w.i32(s.max_hp);
        w.i32(s.posture);
        w.i32(s.max_posture);
        w.u8(s.lives.clamp(-128, 127) as i8 as u8);
    }
    w.0
}

pub fn encode_reward(list: &[(u8, u64, i32, u8)]) -> Vec<u8> {
    let mut w = W::new(MSG_REWARD);
    let list = &list[..list.len().min(64)];
    w.u8(list.len() as u8);
    for &(p, k, l, s) in list {
        w.u8(p);
        w.u64(k);
        w.i32(l);
        w.u8(s);
    }
    w.0
}

pub fn decode(d: &[u8]) -> Option<Msg> {
    let mut r = R { d, p: 0 };
    if r.u8()? != WIRE {
        return None;
    }
    let msg = match r.u8()? {
        MSG_DIGEST => Msg::Digest(decode_digest(&mut r)?),
        MSG_DETAIL_REQ => {
            let n = r.u8()?;
            Msg::DetailReq((0..n).map(|_| r.u32()).collect::<Option<_>>()?)
        }
        MSG_DETAIL => {
            let mut v = Vec::new();
            for _ in 0..r.u8()? {
                let b = r.u32()?;
                let m = r.u16()?;
                v.push((b, (0..m).map(|_| r.u32()).collect::<Option<_>>()?));
            }
            Msg::Detail(v)
        }
        MSG_HP => {
            let mut v = Vec::new();
            for _ in 0..r.u8()? {
                let id = r.i32()?;
                let s = HpSample { hp: r.i32()?, max_hp: r.i32()?, posture: r.i32()?, max_posture: r.i32()?, lives: r.u8()? as i8 as i32 };
                v.push((id, s));
            }
            Msg::Hp(v)
        }
        MSG_REWARD => {
            let mut v = Vec::new();
            for _ in 0..r.u8()? {
                v.push((r.u8()?, r.u64()?, r.i32()?, r.u8()?));
            }
            Msg::Reward(v)
        }
        _ => return None,
    };
    r.done().then_some(msg)
}

/// What a lot difference means, by the lot's source.
pub fn lot_class(source: Source, key_item: bool) -> (Class, &'static str) {
    match source {
        Source::Unknown if key_item => (Class::Info, "a key item from a map pickup or from a script the auditor never saw run"),
        Source::Unknown => (Class::Expected, "map pickups are per player"),
        Source::Enemy => (Class::Suspect(2), "enemy drops should reach both players (or it still lies at the corpse)"),
        Source::BossDrop => (Class::Suspect(3), "boss / miniboss drops should reach both players"),
        Source::Script => (Class::Suspect(3), "NPC / event gifts should reach both players (keyitems.rs)"),
        Source::BossReward => (Class::Suspect(3), "boss rewards should reach both players (bossrules.rs)"),
    }
}

/// Labels for one direction of a difference.
fn sides(friend_has: bool) -> (&'static str, &'static str, &'static str) {
    if friend_has { ("the friend has", "we do not", "friend") } else { ("we have", "the friend does not", "us") }
}

/// The friend's digest against ours: every difference, classified (not yet filtered by
/// time or dedupe; shared records are compared by `record_mismatch`).
pub fn compare(ours: &Digest, theirs: &Digest, lk: &dyn Lookup) -> Vec<Finding> {
    let mut out = Vec::new();
    let our_lots: HashMap<u32, u8> = ours.lots.iter().copied().collect();
    let their_lots: HashMap<u32, u8> = theirs.lots.iter().copied().collect();
    let mut explained: HashSet<u32> = HashSet::new();
    for friend_has in [true, false] {
        let (has, lacks) = if friend_has { (&their_lots, &our_lots) } else { (&our_lots, &their_lots) };
        let mut diff: Vec<(u32, u8)> = has.iter().filter(|(f, _)| !lacks.contains_key(f)).map(|(&f, &s)| (f, s)).collect();
        diff.sort_unstable();
        for (flag, src) in diff {
            let info = lk.lot(flag);
            let source = Source::from_u8(src).max(info.as_ref().map_or(Source::Unknown, |i| i.source));
            let items: Vec<u32> = info.as_ref().map(|i| i.items.clone()).unwrap_or_default();
            let key_item = items.iter().any(|&i| lk.kind(i) == ItemKind::Key);
            explained.extend(items.iter().copied());
            let (class, why) = lot_class(source, key_item);
            let (a, b, who) = sides(friend_has);
            let names: Vec<String> = items.iter().map(|&i| lk.name(i)).collect();
            let row = info.as_ref().map_or_else(|| "?".to_owned(), |i| i.row.to_string());
            out.push(Finding::new(
                format!("lot:{flag}:{who}"),
                class,
                format!("{a} lot flag {flag} (lot {row}: {}) [{}], {b}: {why}", names.join(", "), source.label()),
            ));
        }
    }
    let our_items: HashMap<u32, u32> = ours.items.iter().copied().collect();
    let their_items: HashMap<u32, u32> = theirs.items.iter().copied().collect();
    for friend_has in [true, false] {
        let (has, lacks) = if friend_has { (&their_items, &our_items) } else { (&our_items, &their_items) };
        let mut diff: Vec<(u32, u32)> =
            has.iter().filter(|(id, n)| **n > 0 && lacks.get(id).is_none_or(|&m| m == 0)).map(|(&i, &n)| (i, n)).collect();
        diff.sort_unstable();
        for (id, n) in diff {
            if explained.contains(&id) {
                continue;
            }
            let (a, b, who) = sides(friend_has);
            let class = match lk.kind(id) {
                ItemKind::Key => Class::Suspect(2),
                ItemKind::Progress => Class::Info,
                ItemKind::Other => continue,
            };
            out.push(Finding::new(
                format!("item:{id}:{who}"),
                class,
                format!(
                    "{a} {} x{n}, {b}, and no item-lot difference explains it (a shop, a quest hand-in, a used key item, the key-item repair?)",
                    lk.name(id)
                ),
            ));
        }
    }
    let our_skills: HashSet<u32> = ours.skills.iter().copied().collect();
    let their_skills: HashSet<u32> = theirs.skills.iter().copied().collect();
    for friend_has in [true, false] {
        let (has, lacks) = if friend_has { (&their_skills, &our_skills) } else { (&our_skills, &their_skills) };
        let mut diff: Vec<u32> = has.difference(lacks).copied().collect();
        diff.sort_unstable();
        for flag in diff {
            let (a, b, who) = sides(friend_has);
            let row = lk.skill_row(flag).map_or_else(|| "?".to_owned(), |r| r.to_string());
            out.push(Finding::new(
                format!("skill:{flag}:{who}"),
                Class::Suspect(1),
                format!(
                    "{a} learned skill row {row} (unlock flag {flag}), {b}: each player buys their own skills; a bug only if the other could not learn it (tree locked, text missing)"
                ),
            ));
        }
    }
    for p in Pipe::ALL {
        let i = p as usize * 4;
        let (sent, got) = (ours.rewards[i], theirs.rewards[i + 1]);
        if got < sent {
            out.push(Finding::new(
                format!("rcount:sent:{}", p as u8),
                Class::Suspect(3),
                format!("we sent {sent} {}s on this connection, the friend has received {got}", p.label()),
            ));
        }
        let (sent, got) = (theirs.rewards[i], ours.rewards[i + 1]);
        if got < sent {
            out.push(Finding::new(
                format!("rcount:recv:{}", p as u8),
                Class::Suspect(3),
                format!("the friend sent {sent} {}s on this connection, we have received {got}", p.label()),
            ));
        }
    }
    out
}

/// Shared records whose checksums differ (or exist on one side only).
pub fn record_mismatch(ours: &[(u32, u32)], theirs: &[(u32, u32)]) -> Vec<u32> {
    let a: HashMap<u32, u32> = ours.iter().copied().collect();
    let b: HashMap<u32, u32> = theirs.iter().copied().collect();
    let mut out: Vec<u32> = a.keys().chain(b.keys()).copied().filter(|k| a.get(k) != b.get(k)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The flag ids that are on in one record (`base` + bytes, SetEventFlag's bit layout) and
/// pass `keep`.
pub fn record_ids(base: u32, bytes: &[u8], keep: impl Fn(u32) -> bool) -> Vec<u32> {
    let mut out = Vec::new();
    for (i, &b) in bytes.iter().enumerate() {
        if b == 0 {
            continue;
        }
        for bit in 0..8u32 {
            if b & (1 << bit) != 0 {
                let p = 8 * (i as u32 % 4) + bit;
                let idx = (i as u32 / 4) * 32 + (31 - p);
                if idx < 1000 && keep(base + idx) {
                    out.push(base + idx);
                }
            }
        }
    }
    out.sort_unstable();
    out
}

/// FNV-1a over the ids.
pub fn checksum(ids: &[u32]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for id in ids {
        for b in id.to_le_bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

/// Flags of the detail records that differ: ours (base -> ids on here) against the friend's.
pub fn flag_diff(ours: &HashMap<u32, Vec<u32>>, theirs: &[(u32, Vec<u32>)]) -> Vec<Finding> {
    let mut out = Vec::new();
    for (base, ids) in theirs {
        let t: HashSet<u32> = ids.iter().copied().collect();
        let o: HashSet<u32> = ours.get(base).map(|v| v.iter().copied().collect()).unwrap_or_default();
        let mut diff: Vec<(u32, bool)> = t.difference(&o).map(|&f| (f, true)).chain(o.difference(&t).map(|&f| (f, false))).collect();
        diff.sort_unstable();
        for (flag, friend_on) in diff {
            if out.len() >= MAX_FLAG_FINDINGS {
                return out;
            }
            let npc = flag / 10_000_000 % 10 == 7;
            let (where_on, where_off) = if friend_on { ("for the friend", "here") } else { ("here", "for the friend") };
            out.push(Finding::new(
                format!("flag:{flag}"),
                if npc { Class::Suspect(1) } else { Class::Suspect(2) },
                format!(
                    "shared flag {flag} is on {where_on} and off {where_off} (record {base}){}",
                    if npc { ": an NPC talk flag, npcintro.rs holds those until you meet the NPC yourself" } else { ": the flag sync missed it" }
                ),
            ));
        }
    }
    out
}

/// Differences have to persist before they are reported.
#[derive(Default)]
pub struct Tracker {
    first: HashMap<String, u64>,
}

impl Tracker {
    /// One full observation: the findings seen continuously for `grace` ms are due; keys
    /// not in `found` are forgotten. Returns (due, still pending).
    pub fn observe(&mut self, found: Vec<Finding>, now: u64, grace: u64) -> (Vec<Finding>, usize) {
        let keys: HashSet<&str> = found.iter().map(|f| f.key.as_str()).collect();
        self.first.retain(|k, _| keys.contains(k.as_str()));
        let mut pending = 0;
        let mut due = Vec::new();
        for f in found {
            let t = *self.first.entry(f.key.clone()).or_insert(now);
            if now.saturating_sub(t) >= grace {
                due.push(f);
            } else {
                pending += 1;
            }
        }
        (due, pending)
    }
}

#[derive(Clone, Copy, Debug)]
struct Run {
    ours: bool,
    last: u64,
    on: bool,
}

/// The same shared flag written alternately by us and by the friend.
#[derive(Default)]
pub struct PingPong {
    runs: HashMap<u32, Vec<Run>>,
    done: HashSet<u32>,
}

impl PingPong {
    /// One write of `id` (ours = we sent it, else the friend's write we applied).
    pub fn note(&mut self, id: u32, ours: bool, on: bool, now: u64) -> Option<Finding> {
        if self.done.contains(&id) {
            return None;
        }
        if self.runs.len() > 4096 {
            self.runs.retain(|_, v| v.last().is_some_and(|r| now.saturating_sub(r.last) <= PINGPONG_WINDOW_MS));
            if self.runs.len() > 4096 {
                self.runs.clear();
            }
        }
        let v = self.runs.entry(id).or_default();
        v.retain(|r| now.saturating_sub(r.last) <= PINGPONG_WINDOW_MS);
        match v.last_mut() {
            Some(r) if r.ours == ours => {
                r.last = now;
                r.on = on;
            }
            _ => v.push(Run { ours, last: now, on }),
        }
        if v.len() < PINGPONG_RUNS {
            return None;
        }
        let seq: Vec<String> = v.iter().map(|r| format!("{}={}", if r.ours { "us" } else { "friend" }, r.on)).collect();
        self.runs.remove(&id);
        self.done.insert(id);
        // 7xxxxxxx = NPC talk flags: npcintro.rs keeps some per player on purpose.
        let npc = id / 10_000_000 % 10 == 7;
        Some(Finding::new(
            format!("pingpong:{id}"),
            Class::Suspect(if npc { 2 } else { 3 }),
            format!(
                "flag {id} ping-pongs between the players ({} within 60 s): {}",
                seq.join(" -> "),
                if npc {
                    "an NPC talk flag: npcintro.rs per-player intro, or both games fighting over it?"
                } else {
                    "a per-player flag? Each game sets it for itself (like 9100 in 7.5: input locked)"
                }
            ),
        ))
    }
}

/// One boss / miniboss copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HpSample {
    pub hp: i32,
    pub max_hp: i32,
    pub posture: i32,
    pub max_posture: i32,
    pub lives: i32,
}

impl HpSample {
    fn hp_ratio(&self) -> f32 {
        ratio(self.hp, self.max_hp)
    }
    fn posture_ratio(&self) -> f32 {
        ratio(self.posture, self.max_posture)
    }
}

fn ratio(v: i32, max: i32) -> f32 {
    if max > 0 { (v.max(0) as f32 / max as f32).clamp(0.0, 1.0) } else { 0.0 }
}

/// A boss that shows up below its max HP (1 % rounding allowed).
pub fn starts_damaged(s: HpSample) -> bool {
    s.hp > 0 && s.max_hp > 0 && (s.hp as i64) * 100 < (s.max_hp as i64) * 99
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mismatch {
    Hp,
    Lives,
    Posture,
}

/// Mismatch timers of one boss between our copy and the friend's.
#[derive(Clone, Copy, Debug, Default)]
pub struct HpTimer {
    hp: Option<u64>,
    lives: Option<u64>,
    posture: Option<u64>,
    fired: [bool; 3],
}

impl HpTimer {
    /// One comparison; the mismatches that just lasted past their limit (each once until the
    /// copies agree again).
    pub fn step(&mut self, local: HpSample, remote: HpSample, now: u64) -> Vec<Mismatch> {
        let mut out = Vec::new();
        let checks = [
            (Mismatch::Hp, (local.hp_ratio() - remote.hp_ratio()).abs() > HP_RATIO, HP_LIMIT_MS),
            (Mismatch::Lives, local.lives != remote.lives, HP_LIMIT_MS),
            (Mismatch::Posture, (local.posture_ratio() - remote.posture_ratio()).abs() > POSTURE_RATIO, POSTURE_LIMIT_MS),
        ];
        for (i, (kind, bad, limit)) in checks.into_iter().enumerate() {
            let since = match kind {
                Mismatch::Hp => &mut self.hp,
                Mismatch::Lives => &mut self.lives,
                Mismatch::Posture => &mut self.posture,
            };
            if !bad {
                *since = None;
                self.fired[i] = false;
                continue;
            }
            let t = *since.get_or_insert(now);
            if now.saturating_sub(t) > limit && !self.fired[i] {
                self.fired[i] = true;
                out.push(kind);
            }
        }
        out
    }

    fn since(&self, m: Mismatch) -> Option<u64> {
        match m {
            Mismatch::Hp => self.hp,
            Mismatch::Lives => self.lives,
            Mismatch::Posture => self.posture,
        }
    }
}

/// The three reward pipelines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Pipe {
    /// keyitems.rs `BULK_ITEM_GRANT` (NPC talk / event lots).
    Script = 0,
    /// keyitems.rs `BULK_BOSS_DROP`.
    BossDrop = 1,
    /// bossrules.rs `BULK_BOSS_REWARD`.
    BossReward = 2,
}

impl Pipe {
    const ALL: [Pipe; 3] = [Pipe::Script, Pipe::BossDrop, Pipe::BossReward];

    fn from_u8(v: u8) -> Option<Pipe> {
        Pipe::ALL.into_iter().find(|p| *p as u8 == v)
    }

    pub fn label(self) -> &'static str {
        match self {
            Pipe::Script => "script lot",
            Pipe::BossDrop => "boss drop",
            Pipe::BossReward => "boss reward lot",
        }
    }
}

pub const ST_RECEIVED: u8 = 1;
pub const ST_OK: u8 = 2;
pub const ST_NOT_OK: u8 = 3;

/// Two ids in one key (boss entity + lot).
pub fn key2(a: i32, b: i32) -> u64 {
    (a as u32 as u64) << 32 | b as u32 as u64
}

/// One peer's side of one of our grants.
#[derive(Clone, Copy)]
struct Want {
    peer: PeerId,
    received: Option<u64>,
    resolved: Option<bool>,
    reported: bool,
}

struct Outgoing {
    pipe: Pipe,
    key: u64,
    lot: i32,
    sent: u64,
    /// The partners that ran the auditor when we sent it (only theirs ack: the host's grant
    /// is checked with every client, a client's with the host).
    to: Vec<Want>,
}

/// Our sent grants and each partner's acks; the reward counters per peer.
#[derive(Default)]
pub struct Rewards {
    out: Vec<Outgoing>,
    /// Per peer, per pipe (`Pipe`): sent to it, received from it, resolved, not given.
    counters: BTreeMap<PeerId, [u32; REWARD_COUNTERS]>,
}

impl Rewards {
    /// One grant of ours went out to the partners `to`: (peer, it runs the auditor).
    pub fn sent(&mut self, pipe: Pipe, key: u64, lot: i32, to: &[(PeerId, bool)], now: u64) {
        for &(p, _) in to {
            self.count(p, pipe, 0);
        }
        self.out.retain(|o| o.to.iter().any(|w| w.resolved.is_none()) || now.saturating_sub(o.sent) < 600_000);
        if self.out.len() >= 256 {
            self.out.remove(0);
        }
        let to = to.iter().filter(|t| t.1).map(|&(peer, _)| Want { peer, received: None, resolved: None, reported: false }).collect();
        self.out.push(Outgoing { pipe, key, lot, sent: now, to });
    }

    /// Counter `what` (0 sent, 1 received, 2 resolved, 3 not given) of `pipe` for `peer`.
    pub fn count(&mut self, peer: PeerId, pipe: Pipe, what: usize) {
        self.counters.entry(peer).or_default()[pipe as usize * 4 + what] += 1;
    }

    /// The counters towards one peer (what our digest for it carries).
    pub fn counters(&self, peer: PeerId) -> [u32; REWARD_COUNTERS] {
        self.counters.get(&peer).copied().unwrap_or_default()
    }

    /// A partner left or its link started over: its counters and pending acks go.
    pub fn forget(&mut self, peer: PeerId) {
        self.counters.remove(&peer);
        for o in &mut self.out {
            o.to.retain(|w| w.peer != peer);
        }
    }

    /// A partner's ack of one of ours.
    pub fn ack(&mut self, pipe: Pipe, key: u64, lot: i32, state: u8, from: PeerId, now: u64) -> Option<Finding> {
        let o = self.out.iter_mut().find(|o| o.pipe == pipe && o.key == key)?;
        let sent = o.sent;
        let w = o.to.iter_mut().find(|w| w.peer == from)?;
        match state {
            ST_RECEIVED => {
                w.received.get_or_insert(now);
                None
            }
            ST_OK | ST_NOT_OK => {
                let ok = state == ST_OK;
                let first = w.resolved.is_none();
                w.resolved = Some(ok);
                let secs = now.saturating_sub(sent) / 1000;
                if first && !ok {
                    Some(Finding::new(
                        format!("rnotok:{}:{key}", pipe as u8),
                        Class::Suspect(3),
                        format!("our {} {lot} (key {key:#x}) was handled by the friend {secs} s after we sent it, but its lot flags are off there and the items are not owned: not given", pipe.label()),
                    ))
                } else if first && w.reported {
                    Some(Finding::new(
                        format!("rlate:{}:{key}", pipe as u8),
                        Class::Info,
                        format!("our {} {lot} (key {key:#x}) was resolved by the friend after all, {secs} s after we sent it (ok = {ok})", pipe.label()),
                    ))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Grants a partner has not resolved in time: (that partner, finding).
    pub fn overdue(&mut self, now: u64) -> Vec<(PeerId, Finding)> {
        let mut out = Vec::new();
        for o in self.out.iter_mut() {
            let (pipe, key, lot, sent) = (o.pipe, o.key, o.lot, o.sent);
            for w in o.to.iter_mut().filter(|w| w.resolved.is_none() && !w.reported) {
                let late = match w.received {
                    Some(r) => now.saturating_sub(r) > REWARD_GIVE_MS,
                    None => now.saturating_sub(sent) > REWARD_NO_RECEIPT_MS,
                };
                if !late {
                    continue;
                }
                w.reported = true;
                let secs = now.saturating_sub(sent) / 1000;
                let text = match w.received {
                    Some(r) => format!(
                        "our {} {lot} (key {key:#x}) reached the friend {} s ago and is still not given there ({secs} s since we sent it; a menu or a load delays it, a lost one never comes)",
                        pipe.label(),
                        now.saturating_sub(r) / 1000
                    ),
                    None => format!("our {} {lot} (key {key:#x}) sent {secs} s ago never reached the friend's reward code", pipe.label()),
                };
                out.push((w.peer, Finding::new(format!("rlost:{}:{key}", pipe as u8), Class::Suspect(3), text)));
            }
        }
        out
    }
}

/// `[peer S 'name'] `: what findings and log lines about one peer start with (3+ players).
pub fn tag(id: PeerId, name: &str) -> String {
    format!("[peer {} '{name}'] ", id.slot())
}

/// One audit partner: the host keeps one per client, a client one (the host). With two
/// players it is exactly the one friend of v7.7, with the same timings.
pub struct Link {
    pub id: PeerId,
    pub name: String,
    pub epoch: u64,
    /// The peer runs the auditor: one of its digests arrived on this link.
    pub audit: bool,
    no_audit_logged: bool,
    hello_sent: bool,
    last_sent: u64,
    next_periodic: u64,
    due: Option<(u64, u8)>,
    tracker: Tracker,
    flag_tracker: Tracker,
    /// The peer's boss HP samples (entity -> sample, when) and our mismatch timers with it.
    remote_hp: HashMap<i32, (HpSample, u64)>,
    hp_timers: HashMap<i32, HpTimer>,
    hp_sent_at: u64,
    acks: Vec<(u8, u64, i32, u8)>,
    outbox: Vec<Vec<u8>>,
}

/// One digest of a partner against ours.
pub struct Observed {
    /// Differences that persisted for the grace time (not yet deduped).
    pub due: Vec<Finding>,
    pub pending: usize,
    /// Shared records that differ.
    pub mism: Vec<u32>,
}

impl Link {
    /// A new link (or the same peer on a new link): the hello digest `offset` ms later than
    /// with one peer (several clients' rounds are staggered).
    pub fn new(id: PeerId, name: String, epoch: u64, now: u64, every_ms: u64, offset: u64) -> Link {
        let first = now + HELLO_AFTER_MS + offset;
        Link {
            id,
            name,
            epoch,
            audit: false,
            no_audit_logged: false,
            hello_sent: false,
            last_sent: 0,
            next_periodic: first + every_ms,
            due: Some((first, REASON_HELLO)),
            tracker: Tracker::default(),
            flag_tracker: Tracker::default(),
            remote_hp: HashMap::new(),
            hp_timers: HashMap::new(),
            hp_sent_at: 0,
            acks: Vec::new(),
            outbox: Vec::new(),
        }
    }

    fn schedule(&mut self, at: u64, reason: u8) {
        if self.due.is_none_or(|(t, _)| at < t) {
            self.due = Some((at, reason));
        }
    }

    /// The partner's digest against ours (ours built for this partner: its reward counters).
    /// Per-player data keeps the 2-player rules (`compare`); shared records must match.
    pub fn observe(&mut self, ours: &Digest, theirs: &Digest, lk: &dyn Lookup, now: u64) -> Observed {
        self.audit = true;
        let found = compare(ours, theirs, lk);
        let mism = record_mismatch(&ours.records, &theirs.records);
        let (due, pending) = self.tracker.observe(found, now, GRACE_MS);
        Observed { due, pending, mism }
    }
}

/// The partners this frame, (id, link epoch, name): host every client, a client the host
/// (`PeerSet::link_epochs`). A new id or a new epoch starts that link over (as every new
/// connection did with two players); the others keep their state. Returns (started,
/// dropped) ids; a restarted link is in both.
pub fn sync_links(links: &mut Vec<Link>, want: &[(PeerId, u64, String)], now: u64, every_ms: u64) -> (Vec<PeerId>, Vec<PeerId>) {
    let mut dropped = Vec::new();
    links.retain(|l| {
        let keep = want.iter().any(|w| w.0 == l.id && w.1 == l.epoch);
        if !keep {
            dropped.push(l.id);
        }
        keep
    });
    let mut started = Vec::new();
    for (rank, (id, epoch, name)) in want.iter().enumerate() {
        match links.iter_mut().find(|l| l.id == *id) {
            Some(l) => l.name.clone_from(name),
            None => {
                links.push(Link::new(*id, name.clone(), *epoch, now, every_ms, rank as u64 * STAGGER_MS));
                started.push(*id);
            }
        }
    }
    links.sort_by_key(|l| l.id);
    (started, dropped)
}

/// When the next round of ours is due, after the friend's round covered one.
pub fn covered(due: Option<u64>, now: u64) -> Option<u64> {
    due.filter(|&t| t > now + COVERED_MS)
}

// --- Game data ---------------------------------------------------------------------------

const GAME_DATA_MAN: usize = 0x3D5AAC0;
const GDM_PLAYER_GAME_DATA: usize = 0x08;
/// PlayerGameData -> EquipGameData (keyitems.rs uses it for FindItem; the vendor layout agrees,
/// see the unit test).
const PGD_EQUIP_GAME_DATA: usize = 0x278;
const INV_ITEMS: usize = std::mem::offset_of!(sekiro::sprj::EquipGameData, equip_inventory_data)
    + std::mem::offset_of!(sekiro::sprj::EquipInventoryData, items_data);
const INV_NORMAL_CAP: usize = std::mem::offset_of!(sekiro::sprj::InventoryItemsData, normal_items_capacity);
const INV_NORMAL_HEAD: usize = std::mem::offset_of!(sekiro::sprj::InventoryItemsData, normal_items_head);
const INV_KEY_CAP: usize = std::mem::offset_of!(sekiro::sprj::InventoryItemsData, key_items_capacity);
const INV_KEY_HEAD: usize = std::mem::offset_of!(sekiro::sprj::InventoryItemsData, key_items_head);
const INV_ENTRY: usize = 16;
const MAX_SLOTS: usize = 8192;
/// NpcParam raw offsets (keyitems.rs / scaling.rs): item lot 1, deathblow count.
const NPC_ITEM_LOT_1: usize = 44;
const NPC_DEATHBLOWS: usize = 0x126;
const DATA_POSTURE: usize = 0x148;
const GOODS: u32 = 0x4000_0000;

fn valid_item(full: u32) -> bool {
    matches!(full >> 28, 0 | 1 | 2 | 4)
}

/// Our inventory: (full item id, count), key items and normal items.
pub(crate) fn read_inventory() -> Option<Vec<(u32, u32)>> {
    let pgd = read_ptr(game::module_base() + GAME_DATA_MAN).and_then(|g| read_ptr(g + GDM_PLAYER_GAME_DATA))?;
    let inv = pgd + PGD_EQUIP_GAME_DATA + INV_ITEMS;
    let mut m: BTreeMap<u32, u32> = BTreeMap::new();
    for (head, cap) in [(INV_KEY_HEAD, INV_KEY_CAP), (INV_NORMAL_HEAD, INV_NORMAL_CAP)] {
        let n = read::<u32>(inv + cap)? as usize;
        if n == 0 {
            continue;
        }
        if n > MAX_SLOTS {
            return None;
        }
        let p = read_ptr(inv + head)?;
        let mut b = vec![0u8; n * INV_ENTRY];
        if !read_bytes(p, &mut b) {
            return None;
        }
        for e in b.chunks_exact(INV_ENTRY) {
            let id = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
            let q = u32::from_le_bytes([e[8], e[9], e[10], e[11]]);
            if valid_item(id) && q > 0 && q < 1_000_000 {
                *m.entry(id).or_insert(0) += q;
            }
        }
    }
    Some(m.into_iter().collect())
}

struct LotRow {
    items: Vec<u32>,
    flags: Vec<u32>,
    /// One slot holds all of the weight (a sure drop).
    sure: bool,
}

/// Built once from the params.
struct Index {
    /// Item-lot get flag -> lot row id.
    flags: HashMap<u32, i32>,
    rows: HashMap<i32, LotRow>,
    /// Lot rows given by NpcParam item lots (rows L..L+7 while they exist).
    npc_source: HashMap<i32, Source>,
    /// Skill unlock flag -> SkillParam row.
    skills: HashMap<u32, u32>,
    /// Goods id -> (consumed on use, max count).
    goods: HashMap<u32, (bool, i16)>,
}

/// The full item id of a lot slot (keyitems.rs `sure_goods_of_lot` rule).
fn lot_full(cat: i32, id: i32) -> u32 {
    let (cat, id) = (cat as u32, id as u32);
    if cat >> 28 != 0 {
        cat & 0xF000_0000 | id
    } else if matches!(cat, 3 | 4) {
        GOODS | id
    } else {
        id
    }
}

fn build_index() -> Option<Index> {
    use fromsoftware_shared::FromStatic;
    use sekiro::param::{EQUIP_PARAM_GOODS_ST, ITEMLOT_PARAM_ST, NPC_PARAM_ST, ParamDef, SKILL_PARAM_ST};
    use sekiro::sprj::SoloParamRepository;
    let repo = unsafe { SoloParamRepository::instance() }.ok()?;
    let file = |name: &str| repo.params().find(|p| p.struct_name() == name).map(|p| &*p.data);
    let lots = file(ITEMLOT_PARAM_ST::NAME)?;
    let mut flags = HashMap::new();
    let mut rows = HashMap::new();
    for (id, r) in unsafe { lots.rows::<ITEMLOT_PARAM_ST>() } {
        let slots = [
            (r.lot_item_id01(), r.lot_item_category01(), r.lot_item_base_point01(), r.get_item_flag_id01()),
            (r.lot_item_id02(), r.lot_item_category02(), r.lot_item_base_point02(), r.get_item_flag_id02()),
            (r.lot_item_id03(), r.lot_item_category03(), r.lot_item_base_point03(), r.get_item_flag_id03()),
            (r.lot_item_id04(), r.lot_item_category04(), r.lot_item_base_point04(), r.get_item_flag_id04()),
            (r.lot_item_id05(), r.lot_item_category05(), r.lot_item_base_point05(), r.get_item_flag_id05()),
            (r.lot_item_id06(), r.lot_item_category06(), r.lot_item_base_point06(), r.get_item_flag_id06()),
            (r.lot_item_id07(), r.lot_item_category07(), r.lot_item_base_point07(), r.get_item_flag_id07()),
            (r.lot_item_id08(), r.lot_item_category08(), r.lot_item_base_point08(), r.get_item_flag_id08()),
        ];
        let total: u32 = slots.iter().map(|s| u32::from(s.2)).sum();
        let real: Vec<_> = slots.iter().filter(|s| s.0 > 0 && s.2 > 0).collect();
        let sure = real.len() == 1 && real.iter().all(|s| u32::from(s.2) == total);
        let items: Vec<u32> = slots.iter().filter(|s| s.0 > 0).map(|s| lot_full(s.1, s.0)).collect();
        let mut fl: Vec<u32> = Vec::new();
        for f in slots.iter().map(|s| s.3).chain([r.get_item_flag_id()]) {
            if f > 0 && !fl.contains(&(f as u32)) {
                fl.push(f as u32);
            }
        }
        for &f in &fl {
            flags.entry(f).or_insert(id as i32);
        }
        rows.insert(id as i32, LotRow { items, flags: fl, sure });
    }
    let mut npc_source = HashMap::new();
    if let Some(npcs) = file(NPC_PARAM_ST::NAME) {
        for (_, r) in unsafe { npcs.rows::<NPC_PARAM_ST>() } {
            let p = r as *const NPC_PARAM_ST as usize;
            let (Some(lot), Some(db)) = (read::<i32>(p + NPC_ITEM_LOT_1), read::<i8>(p + NPC_DEATHBLOWS)) else { continue };
            if lot <= 0 {
                continue;
            }
            let src = if db >= 2 { Source::BossDrop } else { Source::Enemy };
            for row in lot..lot.saturating_add(8) {
                if !rows.contains_key(&row) {
                    break;
                }
                let e = npc_source.entry(row).or_insert(src);
                *e = (*e).max(src);
            }
        }
    }
    let mut skills = HashMap::new();
    if let Some(sk) = file(SKILL_PARAM_ST::NAME) {
        for (id, r) in unsafe { sk.rows::<SKILL_PARAM_ST>() } {
            if r.unlock_event_flag() > 0 {
                skills.entry(r.unlock_event_flag() as u32).or_insert(id);
            }
        }
    }
    let mut goods = HashMap::new();
    if let Some(g) = file(EQUIP_PARAM_GOODS_ST::NAME) {
        for (id, r) in unsafe { g.rows::<EQUIP_PARAM_GOODS_ST>() } {
            goods.insert(id, (r.is_consume(), r.max_num()));
        }
    }
    let bosses = npc_source.values().filter(|s| **s == Source::BossDrop).count();
    log::line(&format!(
        "audit: index built: {} item lot rows, {} lot flags, {} enemy lot rows ({bosses} boss / miniboss), {} skills, {} goods rows",
        rows.len(),
        flags.len(),
        npc_source.len(),
        skills.len(),
        goods.len()
    ));
    Some(Index { flags, rows, npc_source, skills, goods })
}

struct GameLookup<'a> {
    idx: &'a Index,
    runtime: &'a HashMap<i32, Source>,
    names: std::cell::RefCell<HashMap<u32, String>>,
}

impl GameLookup<'_> {
    fn source_of_row(&self, row: i32) -> Source {
        let mut s = self.idx.npc_source.get(&row).copied().unwrap_or(Source::Unknown);
        // AwardItemLot(L) runs the rows L, L+1.. as one lot: a runtime source of an earlier
        // row of the same run counts.
        for k in 0..8 {
            let r = row - k;
            if k > 0 && !self.idx.rows.contains_key(&r) {
                break;
            }
            if let Some(&rt) = self.runtime.get(&r) {
                s = s.max(rt);
            }
        }
        if s == Source::Unknown && static_script_lot(row) {
            s = Source::Script;
        }
        s
    }
}

/// keyitems.rs reconcile rule (§33, from the randomizer data [I]): lot ids below 1 000 000
/// are script / talk / boss lots (EMEVD 2003[4], ESD 104, boss awards), except three map lots.
pub fn static_script_lot(row: i32) -> bool {
    (1..1_000_000).contains(&row) && !matches!(row, 61400 | 63610 | 63660)
}

impl Lookup for GameLookup<'_> {
    fn kind(&self, full: u32) -> ItemKind {
        match full >> 28 {
            0..=2 => ItemKind::Key,
            4 => {
                let id = full & 0x0FFF_FFFF;
                if items::is_coop_goods(id) {
                    return ItemKind::Other;
                }
                match self.idx.goods.get(&id) {
                    Some(&(false, max)) if max <= 1 => ItemKind::Key,
                    Some(&(false, _)) => ItemKind::Progress,
                    _ => ItemKind::Other,
                }
            }
            _ => ItemKind::Other,
        }
    }

    fn name(&self, full: u32) -> String {
        if let Some(n) = self.names.borrow().get(&full) {
            return n.clone();
        }
        let id = full & 0x0FFF_FFFF;
        let n = match full >> 28 {
            4 => format!("goods {id} '{}'", items::goods_name(id).unwrap_or_else(|| "?".into())),
            0 => format!("weapon / tool {id}"),
            1 => format!("protector {id}"),
            2 => format!("accessory {id}"),
            c => format!("category {c} id {id}"),
        };
        self.names.borrow_mut().insert(full, n.clone());
        n
    }

    fn lot(&self, flag: u32) -> Option<LotInfo> {
        let row = *self.idx.flags.get(&flag)?;
        let items = self.idx.rows.get(&row).map(|r| r.items.clone()).unwrap_or_default();
        Some(LotInfo { row, source: self.source_of_row(row), items })
    }

    fn skill_row(&self, flag: u32) -> Option<u32> {
        self.idx.skills.get(&flag).copied()
    }
}

fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// --- State ---------------------------------------------------------------------------------

/// One boss / miniboss we watch in this load (the mismatch timers are per peer, `Link`).
#[derive(Clone, Copy)]
struct Boss {
    chr: usize,
    alive: bool,
    last_hp: i32,
    checked: bool,
    sample: HpSample,
}

/// A peer's grant we handled: its lot flags are checked a moment later.
struct Check {
    pipe: Pipe,
    key: u64,
    lot: i32,
    flags: Vec<u32>,
    due: u64,
    /// The grant's sender (acks go back to it when it is our partner).
    from: PeerId,
}

#[derive(Default)]
struct State {
    is_host: bool,
    /// 3+ players: findings and log lines about one peer start with `[peer S 'name']`.
    multi: bool,
    /// The audit partners (host: each client; client: the host), id order.
    links: Vec<Link>,
    /// Every peer's name this frame (tags of findings about any peer).
    names: HashMap<PeerId, String>,
    /// The last single partner's name (audit.txt header with two players).
    peer: String,
    seq: u32,
    /// When our last own digest round went out (host: rounds 5 s apart, PLAN-MULTIPLAYER §5.19).
    round_at: u64,
    index: Option<Option<Index>>,
    index_at: u64,
    runtime: HashMap<i32, Source>,
    runtime_loaded: bool,
    awards: Vec<(i32, Source)>,
    reported: HashSet<String>,
    /// (time, class, peer tag, text)
    findings: Vec<(u64, Class, String, String)>,
    totals: [u32; 3],
    /// The latest comparison with each partner (tagged with 3+ players).
    last_compare: BTreeMap<PeerId, String>,
    compares: u32,
    file_dirty: bool,
    file_at: u64,
    pingpong: PingPong,
    rewards: Rewards,
    checks: Vec<Check>,
    inbox: Vec<(PeerId, Vec<u8>)>,
    // Bosses.
    boss_kind: HashMap<usize, bool>,
    bosses: HashMap<i32, Boss>,
    damaged: HashSet<i32>,
    rest_seen: Option<Instant>,
    rest_gen: u32,
    gauge_prev: Option<i32>,
    scan_at: u64,
    last_in_world: Duration,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// The hooks run on the game thread, never inside our own tick; `try_lock` anyway, so a
/// hook can never block the game.
fn with_hook(f: impl FnOnce(&mut State)) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut g) = STATE.try_lock() {
        f(g.get_or_insert_with(State::default));
    }
}

impl State {
    /// `[peer S 'name'] ` with 3+ players, else nothing (two players: v7.7's lines).
    fn tag(&self, id: PeerId) -> String {
        if !self.multi {
            return String::new();
        }
        tag(id, self.names.get(&id).map_or("", String::as_str))
    }

    fn link_idx(&self, id: PeerId) -> Option<usize> {
        self.links.iter().position(|l| l.id == id)
    }

    fn link(&mut self, id: PeerId) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| l.id == id)
    }

    /// A finding of our own (no peer).
    fn report(&mut self, f: Finding) {
        self.report_tagged(String::new(), f);
    }

    /// A finding about one peer: deduped per peer, tagged with 3+ players.
    fn report_from(&mut self, id: PeerId, mut f: Finding) {
        f.key = peer_key(id, &f.key);
        let tag = self.tag(id);
        self.report_tagged(tag, f);
    }

    fn report_tagged(&mut self, tag: String, f: Finding) {
        if !self.reported.insert(f.key.clone()) {
            return;
        }
        match f.class {
            Class::Suspect(s) => {
                self.totals[0] += 1;
                log::line(&format!("audit: SUSPECT {tag}(sev {s}) {}", f.text));
            }
            Class::Expected => self.totals[1] += 1,
            Class::Info => {
                self.totals[2] += 1;
                log::line(&format!("audit: info: {tag}{}", f.text));
            }
        }
        if self.findings.len() >= MAX_KEPT_FINDINGS {
            // Drop the oldest expected one first, else the oldest.
            let i = self.findings.iter().position(|x| x.1 == Class::Expected).unwrap_or(0);
            self.findings.remove(i);
        }
        self.findings.push((unix_now(), f.class, tag, f.text));
        self.file_dirty = true;
    }

    /// A round with every partner, staggered (the first one as with two players).
    fn schedule_all(&mut self, at: u64, reason: u8) {
        for (rank, l) in self.links.iter_mut().enumerate() {
            l.schedule(at + rank as u64 * STAGGER_MS, reason);
        }
    }

    fn lookup(&self) -> Option<GameLookup<'_>> {
        let idx = self.index.as_ref()?.as_ref()?;
        Some(GameLookup { idx, runtime: &self.runtime, names: Default::default() })
    }

    fn load_runtime(&mut self) {
        if self.runtime_loaded {
            return;
        }
        self.runtime_loaded = true;
        if let Ok(t) = std::fs::read_to_string(dir().join("audit-lots.txt")) {
            for line in t.lines() {
                let mut it = line.split_whitespace();
                if let (Some(Ok(lot)), Some(Ok(src))) = (it.next().map(str::parse::<i32>), it.next().map(str::parse::<u8>)) {
                    let s = Source::from_u8(src);
                    let e = self.runtime.entry(lot).or_insert(s);
                    *e = (*e).max(s);
                }
            }
        }
    }

    /// Lot sources seen at run time (our AwardItemLot calls) -> memory and file.
    fn take_awards(&mut self) {
        if self.awards.is_empty() {
            return;
        }
        self.load_runtime();
        let mut new = String::new();
        for (lot, s) in std::mem::take(&mut self.awards) {
            let e = self.runtime.entry(lot).or_insert(Source::Unknown);
            if s > *e {
                *e = s;
                let _ = writeln!(new, "{lot} {}", s as u8);
            }
        }
        if !new.is_empty() {
            use std::io::Write;
            let _ = std::fs::create_dir_all(dir());
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join("audit-lots.txt")) {
                let _ = f.write_all(new.as_bytes());
            }
        }
    }

    /// Our state now, with the reward counters towards `peer`.
    fn build_digest(&self, reason: u8, reply: bool, peer: PeerId) -> Option<Digest> {
        if game::remnant() {
            return None;
        }
        let fm = world::real_world_flag_man()?;
        let idx = self.index.as_ref()?.as_ref()?;
        let lk = GameLookup { idx, runtime: &self.runtime, names: Default::default() };
        let items = read_inventory()?;
        let mut skills: Vec<u32> = idx.skills.keys().copied().filter(|&f| world::get_flag(fm, f)).collect();
        skills.sort_unstable();
        let mut lots: Vec<(u32, u8)> =
            idx.flags.iter().filter(|(f, _)| world::get_flag(fm, **f)).map(|(&f, &row)| (f, lk.source_of_row(row) as u8)).collect();
        lots.sort_unstable();
        let mut records: Vec<(u32, u32)> = world::read_shared_records(fm)
            .into_iter()
            .map(|(b, bytes)| (b, record_ids(b, &bytes, world::should_sync)))
            .filter(|(_, ids)| !ids.is_empty())
            .map(|(b, ids)| (b, checksum(&ids)))
            .collect();
        records.sort_unstable();
        Some(Digest { seq: self.seq, reason, reply, is_host: self.is_host, items, skills, lots, records, rewards: self.rewards.counters(peer) })
    }

    fn send(&mut self, n: &mut Net, to: PeerId, reason: u8, reply: bool, now: u64) -> bool {
        match self.build_digest(reason, reply, to) {
            Some(d) => self.transmit(n, to, d, now),
            None => false,
        }
    }

    fn transmit(&mut self, n: &mut Net, to: PeerId, mut d: Digest, now: u64) -> bool {
        self.seq = self.seq.wrapping_add(1);
        d.seq = self.seq;
        let (reason, reply) = (d.reason, d.reply);
        let body = encode_digest(&d);
        let ok = n.send_bulk_to(Dest::Peer(to), BULK_AUDIT, &body);
        let tag = self.tag(to);
        if let Some(l) = self.link(to) {
            l.last_sent = now;
            if reply && !l.audit {
                l.hello_sent = true;
            }
        }
        log::line(&format!(
            "audit: {tag}sent digest #{} ({}, {} bytes, ok = {ok}): {} items, {} lot flags, {} skills, {} shared records",
            d.seq,
            reason_label(reason),
            body.len(),
            d.items.len(),
            d.lots.len(),
            d.skills.len(),
            d.records.len()
        ));
        ok
    }

    fn on_digest(&mut self, n: &mut Net, from: PeerId, theirs: Digest, now: u64) {
        let Some(i) = self.link_idx(from) else {
            log::line(&format!("audit: a digest from peer {} (not our partner: host <-> each client), ignored", from.slot()));
            return;
        };
        let tag = self.tag(from);
        if !self.links[i].audit {
            log::line(&format!("audit: {tag}the friend runs the auditor too: comparisons on for this connection"));
        }
        self.links[i].audit = true;
        let Some(ours) = self.build_digest(REASON_REPLY, false, from) else {
            log::line(&format!(
                "audit: {tag}friend's digest arrived, ours is not readable now (remnant / params / inventory): not compared"
            ));
            return;
        };
        if theirs.reply {
            // Our answer, built now: the friend compares it with its state of the moment.
            self.transmit(n, from, ours.clone(), now);
        }
        let Some(idx) = self.index.as_ref().and_then(Option::as_ref) else { return };
        let lk = GameLookup { idx, runtime: &self.runtime, names: Default::default() };
        let ob = self.links[i].observe(&ours, &theirs, &lk, now);
        drop(lk);
        let mut new = 0;
        for f in ob.due {
            if !self.reported.contains(&peer_key(from, &f.key)) {
                new += 1;
            }
            self.report_from(from, f);
        }
        let mism = ob.mism.len();
        if mism > MANY_RECORDS {
            self.report_from(
                from,
                Finding::new(
                    "records-many".into(),
                    Class::Suspect(2),
                    format!("{mism} shared flag records differ: the worlds are far apart (a missed join snapshot or own-world restore?)"),
                ),
            );
        } else if mism > 0 {
            self.links[i].outbox.push(encode_detail_req(&ob.mism));
        }
        let pending = ob.pending;
        self.compares += 1;
        let c = &ours.rewards;
        let t = &theirs.rewards;
        let text = format!(
            "comparison #{} with the friend's digest #{} ({}): items {} here / {} there, lot flags {} / {}, skills {} / {}, shared records {} / {} ({mism} differ); rewards sent/received/resolved/not given here {:?} there {:?}; {pending} difference(s) pending, {new} new report(s)",
            self.compares,
            theirs.seq,
            reason_label(theirs.reason),
            ours.items.len(),
            theirs.items.len(),
            ours.lots.len(),
            theirs.lots.len(),
            ours.skills.len(),
            theirs.skills.len(),
            ours.records.len(),
            theirs.records.len(),
            c,
            t
        );
        log::line(&format!("audit: {tag}{text}"));
        self.last_compare.insert(from, format!("{tag}{text}"));
        self.file_dirty = true;
        let every = self.every_ms();
        let after = if self.is_host { FOLLOWUP_MS } else { FOLLOWUP_CLIENT_MS };
        let l = &mut self.links[i];
        // This round covers one of ours due soon; differences left: look again later.
        l.due = covered(l.due.map(|d| d.0), now).zip(l.due.map(|d| d.1));
        l.next_periodic = now + every;
        // (Record differences schedule their own follow-up from the detail answer.)
        if pending > 0 {
            l.schedule(now + after, REASON_FOLLOWUP);
        }
    }

    fn every_ms(&self) -> u64 {
        let s = EVERY_S.load(Ordering::Relaxed).max(30) as u64 * 1000;
        if self.is_host { s } else { s + 15_000 }
    }

    fn on_detail_req(&mut self, from: PeerId, bases: Vec<u32>) {
        let Some(fm) = world::real_world_flag_man() else { return };
        let want: HashSet<u32> = bases.into_iter().collect();
        let mut recs: Vec<(u32, Vec<u32>)> = world::read_shared_records(fm)
            .into_iter()
            .filter(|(b, _)| want.contains(b))
            .map(|(b, bytes)| (b, record_ids(b, &bytes, world::should_sync)))
            .collect();
        // Requested records that do not resolve here count as all off.
        for b in &want {
            if !recs.iter().any(|r| r.0 == *b) {
                recs.push((*b, Vec::new()));
            }
        }
        recs.sort_by_key(|r| r.0);
        if let Some(l) = self.link(from) {
            l.outbox.push(encode_detail(&recs));
        }
    }

    fn on_detail(&mut self, from: PeerId, theirs: Vec<(u32, Vec<u32>)>, now: u64) {
        let Some(i) = self.link_idx(from) else { return };
        let Some(fm) = world::real_world_flag_man() else { return };
        let want: HashSet<u32> = theirs.iter().map(|r| r.0).collect();
        let ours: HashMap<u32, Vec<u32>> = world::read_shared_records(fm)
            .into_iter()
            .filter(|(b, _)| want.contains(b))
            .map(|(b, bytes)| (b, record_ids(b, &bytes, world::should_sync)))
            .collect();
        let found = flag_diff(&ours, &theirs);
        let n = found.len();
        let (due, pending) = self.links[i].flag_tracker.observe(found, now, GRACE_MS);
        for f in due {
            self.report_from(from, f);
        }
        let tag = self.tag(from);
        log::line(&format!("audit: {tag}shared flag detail of {} record(s): {n} flag(s) differ, {pending} pending", theirs.len()));
        if pending > 0 {
            let after = if self.is_host { FOLLOWUP_MS } else { FOLLOWUP_CLIENT_MS };
            self.links[i].schedule(now + after, REASON_FOLLOWUP);
        }
    }

    /// A peer's grant handled here: are its lot flags on (or its items owned)?
    fn run_checks(&mut self, now: u64) {
        if !self.checks.iter().any(|c| now >= c.due) {
            return;
        }
        let (due, later): (Vec<Check>, Vec<Check>) = std::mem::take(&mut self.checks).into_iter().partition(|c| now >= c.due);
        self.checks = later;
        let Some(fm) = world::real_world_flag_man() else { return };
        let owned: HashSet<u32> = read_inventory().unwrap_or_default().into_iter().map(|(i, _)| i).collect();
        for c in due {
            let mut flags = c.flags.clone();
            if flags.is_empty()
                && let Some(Some(idx)) = &self.index
            {
                // A boss drop: the sure rows of the lot run (rows lot, lot+1.. while they exist).
                for row in c.lot.max(1)..c.lot.max(1).saturating_add(8) {
                    let Some(r) = idx.rows.get(&row) else { break };
                    if r.sure {
                        flags.extend(r.flags.iter().copied());
                    }
                }
            }
            let off: Vec<u32> = flags.iter().copied().filter(|&f| !world::get_flag(fm, f)).collect();
            // Consumables count too (Herb Catalogue Scrap is one); only our co-op items do not.
            let coop = |i: u32| i >> 28 == 4 && items::is_coop_goods(i & 0x0FFF_FFFF);
            let missing: Vec<u32> = match self.lookup() {
                Some(lk) => off.iter().filter_map(|f| lk.lot(*f)).flat_map(|l| l.items).filter(|&i| !coop(i) && !owned.contains(&i)).collect(),
                None => Vec::new(),
            };
            let ok = off.is_empty() || missing.is_empty();
            if !ok {
                self.rewards.count(c.from, c.pipe, 3);
                let names: Vec<String> = self.lookup().map(|lk| missing.iter().map(|m| lk.name(*m)).collect()).unwrap_or_default();
                self.report_from(
                    c.from,
                    Finding::new(
                        format!("rgive:{}:{}", c.pipe as u8, c.key),
                        Class::Suspect(3),
                        format!(
                            "the friend's {} {} (key {:#x}) was handled here, but its lot flags {off:?} are off and we do not own {names:?}: not given",
                            c.pipe.label(),
                            c.lot,
                            c.key
                        ),
                    ),
                );
            }
            if let Some(l) = self.link(c.from) {
                l.acks.push((c.pipe as u8, c.key, c.lot, if ok { ST_OK } else { ST_NOT_OK }));
            }
        }
    }

    fn handle(&mut self, n: &mut Net, from: PeerId, d: Vec<u8>, now: u64) {
        match decode(&d) {
            Some(Msg::Digest(dg)) => self.on_digest(n, from, dg, now),
            Some(Msg::DetailReq(b)) => self.on_detail_req(from, b),
            Some(Msg::Detail(v)) => self.on_detail(from, v, now),
            Some(Msg::Hp(v)) => {
                for (id, s) in v {
                    if s.hp > 0 && s.max_hp > 0 && s.hp < s.max_hp {
                        self.damaged.insert(id);
                    }
                    if let Some(l) = self.link(from)
                        && (l.remote_hp.len() < 256 || l.remote_hp.contains_key(&id))
                    {
                        l.remote_hp.insert(id, (s, now));
                    }
                }
            }
            Some(Msg::Reward(v)) => {
                for (p, key, lot, st) in v {
                    if let Some(pipe) = Pipe::from_u8(p)
                        && let Some(f) = self.rewards.ack(pipe, key, lot, st, from, now)
                    {
                        self.report_from(from, f);
                    }
                }
            }
            None => log::line(&format!("audit: unreadable message ({} bytes), ignored", d.len())),
        }
    }

    fn boss_scan(&mut self, c: &mut Ctx, now: u64) {
        let in_world = c.in_world_for;
        if in_world < Duration::from_secs(3) {
            return;
        }
        let gauge = game::boss_state().map(|b| b.entity);
        let me = c.player.position();
        let mut near: Vec<(i32, HpSample)> = Vec::new();
        let cycle = self.rest_gen;
        let mut seen: HashSet<i32> = HashSet::new();
        let mut found: Vec<(PeerId, Finding)> = Vec::new();
        for chr in nsession::enemy_chrs() {
            let Some(id) = chr.entity_id().filter(|&e| e > 0) else { continue };
            if self.boss_kind.len() > 2048 {
                self.boss_kind.clear();
            }
            let multi = *self.boss_kind.entry(chr.0).or_insert_with(|| livesync::multi_life(livesync::lives0_of(chr)));
            let tracked = self.bosses.get(&id).is_some_and(|b| b.chr == chr.0);
            if !multi && gauge != Some(id) && !tracked {
                continue;
            }
            let Some((data, hp, max_hp, max_posture, lives)) = livesync::read_data(chr) else { continue };
            let posture = read::<i32>(data + DATA_POSTURE).unwrap_or(0);
            let s = HpSample { hp, max_hp, posture, max_posture, lives };
            seen.insert(id);
            let prev = self.bosses.get(&id).copied().filter(|b| b.chr == chr.0);
            if prev.is_none() {
                // A new character: fresh mismatch timers with every peer.
                for l in &mut self.links {
                    l.hp_timers.remove(&id);
                }
            }
            let mut b = prev.unwrap_or(Boss { chr: chr.0, alive: hp > 0, last_hp: hp, checked: false, sample: s });
            let alive = hp > 0;
            if let Some(p) = prev {
                if alive && hp < p.last_hp && in_world >= DAMAGE_AFTER_LOAD {
                    self.damaged.insert(id);
                }
                if p.alive && !alive {
                    log::line(&format!("audit: boss / miniboss {id} died here, digest in {} s", BOSS_AFTER_MS / 1000));
                    self.schedule_all(now + BOSS_AFTER_MS, REASON_BOSS);
                }
            }
            b.alive = alive;
            b.last_hp = hp;
            b.sample = s;
            if alive && !b.checked && in_world >= DAMAGE_AFTER_LOAD {
                b.checked = true;
                self.first_sight(id, s, "loaded", cycle);
            }
            // Our copy against each partner's (the host: every client in its samples).
            for l in self.links.iter_mut() {
                let Some(&(r, at)) = l.remote_hp.get(&id) else { continue };
                if now.saturating_sub(at) > HP_FRESH_MS || !(alive || r.hp > 0) {
                    continue;
                }
                let timer = l.hp_timers.entry(id).or_default();
                for m in timer.step(s, r, now) {
                    let secs = timer.since(m).map_or(0, |t| now.saturating_sub(t)) as f32 / 1000.0;
                    let (sev, what) = match m {
                        Mismatch::Hp => (3, "HP differs"),
                        Mismatch::Lives => (3, "lives differ"),
                        Mismatch::Posture => (1, "posture differs"),
                    };
                    found.push((
                        l.id,
                        Finding::new(
                            format!("hp:{id}:{cycle}:{m:?}"),
                            Class::Suspect(sev),
                            format!(
                                "boss / miniboss {id}: {what} between the machines for {secs:.1} s: here HP {}/{} ({:.0} %) posture {}/{} lives {}, the friend HP {}/{} ({:.0} %) posture {}/{} lives {}",
                                s.hp,
                                s.max_hp,
                                s.hp_ratio() * 100.0,
                                s.posture,
                                s.max_posture,
                                s.lives,
                                r.hp,
                                r.max_hp,
                                r.hp_ratio() * 100.0,
                                r.posture,
                                r.max_posture,
                                r.lives
                            ),
                        ),
                    ));
                }
            }
            for (peer, f) in found.drain(..) {
                self.report_from(peer, f);
            }
            let close = me.zip(chr.position()).is_some_and(|(a, p)| game::distance(a, p) <= HP_NEAR_M);
            if close && (alive || prev.is_some_and(|p| p.alive)) {
                near.push((id, s));
            }
            self.bosses.insert(id, b);
        }
        self.bosses.retain(|id, _| seen.contains(id));
        for l in &mut self.links {
            l.hp_timers.retain(|id, _| seen.contains(id));
        }
        // The gauge appeared: the start of an engagement.
        if gauge != self.gauge_prev
            && let Some(e) = gauge
            && let Some(b) = self.bosses.get(&e).copied()
            && b.alive
        {
            self.first_sight(e, b.sample, "at the start of its boss fight (gauge)", cycle);
        }
        self.gauge_prev = gauge;
        // Our samples for each partner in our block that runs the auditor (its own rate and
        // bulk queue: one slow link does not hold back the others).
        if near.is_empty() {
            return;
        }
        near.truncate(MAX_HP_ENTRIES);
        let mut body: Option<Vec<u8>> = None;
        for l in self.links.iter_mut() {
            let here = c.peers.by_id(l.id).is_some_and(|p| p.snapshot.is_some_and(|s| !s.hidden) && p.in_block(c.here));
            if here && l.audit && now.saturating_sub(l.hp_sent_at) >= HP_EVERY_MS && c.net.pending_bulk_chunks_to(l.id) == 0 {
                l.hp_sent_at = now;
                let b = body.get_or_insert_with(|| encode_hp(&near));
                c.net.send_bulk_to(Dest::Peer(l.id), BULK_AUDIT, b);
            }
        }
    }

    fn first_sight(&mut self, id: i32, s: HpSample, when: &str, cycle: u32) {
        if !starts_damaged(s) || self.damaged.contains(&id) {
            return;
        }
        let record = persist::boss_record(id);
        let (sev, why) = match record {
            None => (3, "and there is no boss record of it: nothing explains the damage".to_owned()),
            Some(r) => (
                2,
                format!(
                    "a boss record (persist.rs: hp {}/{} lives {}/{}) says it was damaged this rest cycle, but the auditor saw no damage here or in the friend's HP samples (a record from a fight while apart, or a wrong record?)",
                    r.hp, r.max_hp, r.lives, r.lives0
                ),
            ),
        };
        self.report(Finding::new(
            format!("fullhp:{id}:{cycle}"),
            Class::Suspect(sev),
            format!(
                "boss / miniboss {id} {when} with HP {}/{} ({:.0} %) lives {}, no damage seen this rest cycle, {why}",
                s.hp,
                s.max_hp,
                s.hp_ratio() * 100.0,
                s.lives
            ),
        ));
    }

    fn write_file(&mut self, now: u64) {
        if !self.file_dirty || now.saturating_sub(self.file_at) < FILE_EVERY_MS {
            return;
        }
        self.file_dirty = false;
        self.file_at = now;
        let mut t = String::new();
        let _ = writeln!(t, "Sekiro co-op auditor (audit.rs, IMPLEMENTATION-NOTES 32). Newest first.");
        let partners = if self.multi || self.links.len() > 1 {
            let v: Vec<String> = self.links.iter().map(|l| format!("{} '{}' (runs the auditor: {})", l.id.slot(), l.name, l.audit)).collect();
            format!("peers {}", if v.is_empty() { "-".to_owned() } else { v.join(", ") })
        } else {
            format!(
                "friend {} | friend runs the auditor: {}",
                if self.peer.is_empty() { "-" } else { self.peer.as_str() },
                self.links.first().is_some_and(|l| l.audit)
            )
        };
        let _ = writeln!(t, "Updated [{}] | we are the {} | {partners}", unix_now(), if self.is_host { "host" } else { "client" });
        let _ = writeln!(t, "This game run: {} SUSPECT, {} expected, {} info.", self.totals[0], self.totals[1], self.totals[2]);
        let _ = writeln!(
            t,
            "SUSPECT = probably a bug (sev 3 high, 2 medium, 1 low). expected = per player by design (map pickups). info = context."
        );
        let _ = writeln!(t, "The same SUSPECT lines are in log.txt as 'audit: SUSPECT'. The auditor only observes: it never gives or sets anything.\n");
        let latest: Vec<&str> = self.last_compare.values().map(String::as_str).collect();
        let _ = writeln!(t, "== Latest comparison ==\n{}\n", if latest.is_empty() { "(none yet)".to_owned() } else { latest.join("\n") });
        let _ = writeln!(t, "== SUSPECT (newest first) ==");
        for (at, c, tag, text) in self.findings.iter().rev() {
            if let Class::Suspect(s) = c {
                let _ = writeln!(t, "[{at}] SUSPECT {tag}(sev {s}) {text}");
            }
        }
        let _ = writeln!(t, "\n== info / expected (newest first) ==");
        for (at, c, tag, text) in self.findings.iter().rev() {
            match c {
                Class::Info => {
                    let _ = writeln!(t, "[{at}] info: {tag}{text}");
                }
                Class::Expected => {
                    let _ = writeln!(t, "[{at}] expected: {tag}{text}");
                }
                Class::Suspect(_) => {}
            }
        }
        let _ = std::fs::create_dir_all(dir());
        let _ = std::fs::write(dir().join("audit.txt"), t);
    }
}

/// A finding's dedupe key about one peer (each peer's findings are reported once each).
fn peer_key(id: PeerId, key: &str) -> String {
    format!("p{}:{key}", id.slot())
}

fn reason_label(r: u8) -> &'static str {
    match r {
        REASON_HELLO => "hello",
        REASON_PERIODIC => "periodic",
        REASON_REST => "after a rest",
        REASON_BOSS => "after a boss death",
        REASON_FOLLOWUP => "follow-up",
        REASON_REPLY => "reply",
        _ => "?",
    }
}

// --- Hooks (other modules) -----------------------------------------------------------------

/// world.rs: a shared flag we sent (`ours`) or a peer's write we applied.
pub fn flag_write(id: u32, on: bool, ours: bool) {
    with_hook(|s| {
        if let Some(f) = s.pingpong.note(id, ours, on, game::now_ms()) {
            s.report(f);
        }
    });
}

/// bossrules.rs `award_detour`: AwardItemLot(lot) ran here (`boss` = in the boss window or
/// our replay of a peer's boss reward).
pub fn note_award(lot: i32, boss: bool) {
    if lot <= 0 {
        return;
    }
    with_hook(|s| {
        if s.awards.len() < 256 {
            s.awards.push((lot, if boss { Source::BossReward } else { Source::Script }));
        }
    });
}

/// keyitems.rs / bossrules.rs: we sent a grant of `pipe` to every peer.
pub fn reward_sent(pipe: Pipe, key: u64, lot: i32) {
    with_hook(|s| {
        let to: Vec<(PeerId, bool)> = s.links.iter().map(|l| (l.id, l.audit)).collect();
        s.rewards.sent(pipe, key, lot, &to, game::now_ms());
    });
}

/// keyitems.rs / bossrules.rs: a grant of peer `from` arrived here.
pub fn reward_received(pipe: Pipe, key: u64, lot: i32, from: PeerId) {
    with_hook(|s| {
        s.rewards.count(from, pipe, 1);
        if let Some(l) = s.link(from) {
            l.acks.push((pipe as u8, key, lot, ST_RECEIVED));
        }
    });
}

/// keyitems.rs / bossrules.rs: a grant of peer `from` was handled here (given or skipped).
/// `flags`: its lot flags if known (empty for a boss drop: the lot's sure rows are used).
pub fn reward_resolved(pipe: Pipe, key: u64, lot: i32, flags: &[u32], from: PeerId) {
    with_hook(|s| {
        s.rewards.count(from, pipe, 2);
        let wait = if pipe == Pipe::BossDrop { RESOLVE_CHECK_DROP_MS } else { RESOLVE_CHECK_MS };
        if s.checks.len() < 128 && !s.checks.iter().any(|c| c.pipe == pipe && c.key == key && c.from == from) {
            s.checks.push(Check { pipe, key, lot, flags: flags.to_vec(), due: game::now_ms() + wait, from });
        }
    });
}

/// puppet.rs, every frame right after the net frame (also on loading screens): our bulk
/// kind leaves the net inbox at once (it holds 24 transfers, the oldest are dropped).
pub fn pump(n: &mut Net) {
    let mut got = Vec::new();
    while let Some(d) = n.take_bulk_from(BULK_AUDIT) {
        got.push(d);
    }
    if got.is_empty() || !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(mut g) = STATE.try_lock() {
        let s = g.get_or_insert_with(State::default);
        for d in got {
            if s.inbox.len() >= MAX_INBOX {
                s.inbox.remove(0);
            }
            s.inbox.push(d);
        }
    }
}

/// puppet.rs, every frame in the world while connected (after persist).
pub struct Ctx<'a> {
    pub net: &'a mut Net,
    pub peers: &'a PeerSet,
    pub player: Chr,
    pub here: Option<i32>,
    pub in_world_for: Duration,
}

pub fn tick(c: Ctx) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let Ok(mut g) = STATE.try_lock() else { return };
    let s = g.get_or_insert_with(State::default);
    let now = game::now_ms();
    let n: &mut Net = c.net;
    s.is_host = n.is_host();
    s.names = c.peers.iter().map(|p| (p.id, p.name.clone())).collect();
    // Star like the rest of multi (PLAN-MULTIPLAYER §5.19): the host compares itself with each
    // client, a client with the host; shared state equal host <-> each = equal for all.
    let multi = c.peers.len() > 1;
    if multi != s.multi {
        s.multi = multi;
        log::line(&if multi {
            format!(
                "audit: {} players in the session: {}; findings about one peer start with [peer N 'name']",
                c.peers.players(),
                if s.is_host { "comparing with each client on its own schedule" } else { "comparing with the host (the host compares every client)" }
            )
        } else {
            "audit: two players again: findings untagged".to_owned()
        });
    }
    let want: Vec<(PeerId, u64, String)> =
        c.peers.link_epochs().into_iter().map(|(id, e)| (id, e, s.names.get(&id).cloned().unwrap_or_default())).collect();
    let every = s.every_ms();
    let (started, dropped) = sync_links(&mut s.links, &want, now, every);
    // A partner gone or on a new link: its acks can no longer come; a new link starts over
    // (as a new connection did with two players).
    for id in &dropped {
        s.rewards.forget(*id);
    }
    for id in &started {
        s.checks.retain(|c| c.from != *id);
        let tag = s.tag(*id);
        let first = s.links.iter().find(|l| l.id == *id).and_then(|l| l.due).map_or(HELLO_AFTER_MS, |d| d.0.saturating_sub(now));
        log::line(&format!(
            "audit: {tag}on for this connection (we are the {}), first digest in {} s; findings go to audit.txt and 'audit: SUSPECT' lines",
            if s.is_host { "host" } else { "client" },
            first / 1000
        ));
    }
    if !started.is_empty() {
        let ids: Vec<PeerId> = s.links.iter().map(|l| l.id).collect();
        s.last_compare.retain(|k, _| ids.contains(k));
    }
    if let [l] = s.links.as_slice() {
        s.peer.clone_from(&l.name);
    }
    // A load: the bosses are new characters.
    if c.in_world_for < s.last_in_world {
        s.bosses.clear();
        s.boss_kind.clear();
        s.gauge_prev = None;
    }
    s.last_in_world = c.in_world_for;
    // A rest of ours (own or mirrored): a new rest cycle.
    let rest = crate::restsync::last_rest();
    if rest != s.rest_seen {
        if s.rest_seen.is_some() || rest.is_some() {
            s.damaged.clear();
            s.rest_gen += 1;
            if rest.is_some() {
                s.schedule_all(now + REST_AFTER_MS, REASON_REST);
            }
        }
        s.rest_seen = rest;
    }
    if s.index.is_none() && now.saturating_sub(s.index_at) >= 10_000 {
        s.index_at = now;
        if let Some(i) = build_index() {
            s.index = Some(Some(i));
        }
    }
    s.load_runtime();
    s.take_awards();
    for (from, d) in std::mem::take(&mut s.inbox) {
        s.handle(n, from, d, now);
    }
    if s.scan_at == 0 || now.saturating_sub(s.scan_at) >= SCAN_EVERY_MS {
        s.scan_at = now;
        let mut ctx = Ctx { net: &mut *n, peers: c.peers, player: c.player, here: c.here, in_world_for: c.in_world_for };
        s.boss_scan(&mut ctx, now);
        s.run_checks(now);
        for (peer, f) in s.rewards.overdue(now) {
            s.report_from(peer, f);
        }
    }
    let ids: Vec<PeerId> = s.links.iter().map(|l| l.id).collect();
    for id in ids {
        let Some(i) = s.link_idx(id) else { continue };
        let tag = s.tag(id);
        let l = &mut s.links[i];
        // An older friend never answers the hello: say so once, then stay quiet towards it.
        if !l.audit && l.hello_sent && !l.no_audit_logged && now.saturating_sub(l.last_sent) > NO_AUDIT_AFTER_MS {
            l.no_audit_logged = true;
            log::line(&format!(
                "audit: {tag}no digest from the friend within 90 s of ours: its build has no auditor, comparisons off for this connection (local checks stay on)"
            ));
        }
        if l.next_periodic != 0 && now >= l.next_periodic {
            l.next_periodic = now + every;
            l.schedule(now, REASON_PERIODIC);
        }
        let (due, first, audit, last_sent) = (l.due, !l.hello_sent && !l.audit, l.audit, l.last_sent);
        if let Some((at, reason)) = due
            && now >= at
            && c.in_world_for >= IN_WORLD_FOR_DIGEST
        {
            let allowed = audit || first;
            // The host's rounds with several clients go out ROUND_GAP_MS apart (two players:
            // always true, MIN_GAP_MS is longer).
            let gap = !s.is_host || now.saturating_sub(s.round_at) >= ROUND_GAP_MS;
            if !allowed {
                s.links[i].due = None;
            } else if now.saturating_sub(last_sent) >= MIN_GAP_MS && gap {
                // Not readable now (remnant, params, inventory): try again in 5 s.
                let ok = s.send(n, id, if first { REASON_HELLO } else { reason }, true, now);
                if ok {
                    s.round_at = now;
                }
                s.links[i].due = if ok { None } else { Some((now + 5_000, reason)) };
            }
        }
        let l = &mut s.links[i];
        if l.audit {
            if !l.acks.is_empty() {
                let acks = std::mem::take(&mut l.acks);
                n.send_bulk_to(Dest::Peer(id), BULK_AUDIT, &encode_reward(&acks));
            }
            for m in std::mem::take(&mut l.outbox) {
                n.send_bulk_to(Dest::Peer(id), BULK_AUDIT, &m);
            }
        } else {
            l.acks.clear();
            l.outbox.clear();
        }
    }
    s.write_file(now);
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Lookup for Fake {
        fn kind(&self, full: u32) -> ItemKind {
            match full {
                0x4000_0B6C => ItemKind::Key,      // 2924: an esoteric text
                0x4000_0FA0 => ItemKind::Progress, // 4000: Prayer Bead
                0x0000_1234 => ItemKind::Key,      // a prosthetic tool
                _ => ItemKind::Other,
            }
        }
        fn name(&self, full: u32) -> String {
            format!("item {full:#x}")
        }
        fn lot(&self, flag: u32) -> Option<LotInfo> {
            let (row, source, items) = match flag {
                6705 => (61000, Source::Unknown, vec![0x4000_0B6C]),
                6761 => (10200000, Source::BossDrop, vec![0x4000_0FA0]),
                50000100 => (1000100, Source::Unknown, vec![0x4000_03E8]),
                50006242 => (62402, Source::Enemy, vec![0x4000_23F6]),
                _ => return None,
            };
            Some(LotInfo { row, source, items })
        }
        fn skill_row(&self, flag: u32) -> Option<u32> {
            (flag == 6900).then_some(100)
        }
    }

    fn by_key<'a>(v: &'a [Finding], key: &str) -> Option<&'a Finding> {
        v.iter().find(|f| f.key == key)
    }

    #[test]
    fn digest_and_messages_round_trip_and_reject_garbage() {
        let d = Digest {
            seq: 7,
            reason: REASON_REST,
            reply: true,
            is_host: true,
            items: vec![(0x4000_0FA0, 3), (0x1234, 1)],
            skills: vec![6900],
            lots: vec![(6705, 3), (6761, 0)],
            records: vec![(11_000_000, 0xDEAD_BEEF)],
            rewards: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let b = encode_digest(&d);
        assert_eq!(decode(&b), Some(Msg::Digest(d)));
        assert_eq!(decode(&b[..b.len() - 1]), None);
        let mut long = b.clone();
        long.push(0);
        assert_eq!(decode(&long), None);
        assert_eq!(decode(&[2, MSG_DIGEST]), None);
        assert_eq!(decode(&[WIRE, 99]), None);
        let det = vec![(11_000_000, vec![11_000_001, 11_000_005]), (12_000_000, vec![])];
        assert_eq!(decode(&encode_detail(&det)), Some(Msg::Detail(det)));
        assert_eq!(decode(&encode_detail_req(&[1, 2])), Some(Msg::DetailReq(vec![1, 2])));
        let hp = vec![(1100300, HpSample { hp: 100, max_hp: 2000, posture: 5, max_posture: 900, lives: 2 })];
        assert_eq!(decode(&encode_hp(&hp)), Some(Msg::Hp(hp)));
        let rw = vec![(1, key2(1100300, 10200000), 10200000, ST_OK)];
        assert_eq!(decode(&encode_reward(&rw)), Some(Msg::Reward(rw)));
    }

    #[test]
    fn lot_differences_are_classified_by_source() {
        // The friend got the boss drop (Prayer Bead) and a talk gift (the text, known to it as
        // a script lot); we have a map pickup the friend lacks and an enemy drop.
        let ours = Digest { lots: vec![(50000100, 0), (50006242, 0)], ..Default::default() };
        let theirs = Digest { lots: vec![(6705, Source::Script as u8), (6761, 0)], ..Default::default() };
        let f = compare(&ours, &theirs, &Fake);
        assert_eq!(by_key(&f, "lot:6705:friend").map(|x| x.class), Some(Class::Suspect(3)));
        assert!(by_key(&f, "lot:6705:friend").unwrap().text.contains("NPC talk / event gift"));
        assert_eq!(by_key(&f, "lot:6761:friend").map(|x| x.class), Some(Class::Suspect(3)));
        assert_eq!(by_key(&f, "lot:50000100:us").map(|x| x.class), Some(Class::Expected));
        assert_eq!(by_key(&f, "lot:50006242:us").map(|x| x.class), Some(Class::Suspect(2)));
        // A key item from an unknown lot is worth a note, not a bug report.
        let theirs = Digest { lots: vec![(6705, 0)], ..Default::default() };
        let f = compare(&Digest::default(), &theirs, &Fake);
        assert_eq!(by_key(&f, "lot:6705:friend").map(|x| x.class), Some(Class::Info));
        // Same flags on both sides: nothing.
        assert!(compare(&theirs, &theirs, &Fake).is_empty());
    }

    #[test]
    fn items_explained_by_a_lot_are_not_reported_twice() {
        let ours = Digest::default();
        let theirs = Digest { items: vec![(0x4000_0B6C, 1), (0x4000_0FA0, 2), (0x1234, 1), (0x4000_0001, 9)], lots: vec![(6705, 3)], ..Default::default() };
        let f = compare(&ours, &theirs, &Fake);
        // The text is explained by lot 61000; the bead is a progress item (info); the tool is
        // a key item nothing explains; the consumable is never compared.
        assert!(by_key(&f, "item:1073744748:friend").is_none());
        assert_eq!(by_key(&f, &format!("item:{}:friend", 0x4000_0FA0u32)).map(|x| x.class), Some(Class::Info));
        assert_eq!(by_key(&f, &format!("item:{}:friend", 0x1234u32)).map(|x| x.class), Some(Class::Suspect(2)));
        assert!(by_key(&f, &format!("item:{}:friend", 0x4000_0001u32)).is_none());
        // Owned on both sides with other counts: not a difference.
        let ours = Digest { items: vec![(0x1234, 3)], ..Default::default() };
        let theirs = Digest { items: vec![(0x1234, 1)], ..Default::default() };
        assert!(compare(&ours, &theirs, &Fake).is_empty());
    }

    #[test]
    fn skills_and_reward_counters() {
        let mut ours = Digest { skills: vec![6900], ..Default::default() };
        ours.rewards[0] = 3; // we sent 3 script lots
        let mut theirs = Digest::default();
        theirs.rewards[1] = 2; // the friend received 2
        theirs.rewards[4] = 1; // the friend sent one boss drop, we received none
        let f = compare(&ours, &theirs, &Fake);
        assert_eq!(by_key(&f, "skill:6900:us").map(|x| x.class), Some(Class::Suspect(1)));
        assert!(by_key(&f, "skill:6900:us").unwrap().text.contains("row 100"));
        assert_eq!(by_key(&f, "rcount:sent:0").map(|x| x.class), Some(Class::Suspect(3)));
        assert_eq!(by_key(&f, "rcount:recv:1").map(|x| x.class), Some(Class::Suspect(3)));
        assert!(by_key(&f, "rcount:sent:1").is_none());
    }

    #[test]
    fn differences_must_persist_before_they_are_reported() {
        let mut t = Tracker::default();
        let f = |k: &str| Finding::new(k.into(), Class::Suspect(3), String::new());
        let (due, pending) = t.observe(vec![f("a"), f("b")], 1_000, GRACE_MS);
        assert!(due.is_empty());
        assert_eq!(pending, 2);
        // "b" went away (the grant arrived), "a" stays.
        let (due, pending) = t.observe(vec![f("a")], 30_000, GRACE_MS);
        assert!(due.is_empty() && pending == 1);
        let (due, _) = t.observe(vec![f("a"), f("b")], 47_000, GRACE_MS);
        assert_eq!(due.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), vec!["a"]);
        // "b" came back at 47 s: its clock started again.
        let (due, pending) = t.observe(vec![f("b")], 60_000, GRACE_MS);
        assert!(due.is_empty() && pending == 1);
    }

    #[test]
    fn records_and_flags() {
        // SetEventFlag layout: flag index i lives at byte (i/32)*4 + (31 - i%32)/8, bit (31 - i%32)%8.
        let set = |bytes: &mut [u8], idx: u32| {
            let p = 31 - (idx & 31);
            bytes[(idx >> 5) as usize * 4 + (p / 8) as usize] |= 1 << (p % 8);
        };
        let mut b = vec![0u8; 128];
        for i in [0, 5, 31, 32, 999] {
            set(&mut b, i);
        }
        let all = record_ids(11_000_000, &b, |_| true);
        assert_eq!(all, vec![11_000_000, 11_000_005, 11_000_031, 11_000_032, 11_000_999]);
        let kept = record_ids(11_000_000, &b, |id| id != 11_000_005);
        assert_eq!(kept.len(), 4);
        assert_ne!(checksum(&all), checksum(&kept));
        assert_eq!(record_mismatch(&[(1, 5), (2, 6)], &[(1, 5), (2, 7), (3, 1)]), vec![2, 3]);
        let ours: HashMap<u32, Vec<u32>> = [(11_000_000, vec![11_000_001]), (71_000_000, vec![])].into_iter().collect();
        let theirs = vec![(11_000_000, vec![11_000_002]), (71_000_000, vec![71_000_010])];
        let f = flag_diff(&ours, &theirs);
        assert_eq!(by_key(&f, "flag:11000002").map(|x| x.class), Some(Class::Suspect(2)));
        assert!(by_key(&f, "flag:11000001").unwrap().text.contains("on here and off for the friend"));
        assert_eq!(by_key(&f, "flag:71000010").map(|x| x.class), Some(Class::Suspect(1)));
    }

    #[test]
    fn ping_pong_needs_three_alternating_runs_within_a_minute() {
        let mut p = PingPong::default();
        // 9100 in 7.5: we set it, the friend's game flips it back, ours again.
        assert!(p.note(9100, true, false, 1_000).is_none());
        assert!(p.note(9100, true, true, 1_500).is_none()); // same side: one run
        assert!(p.note(9100, false, false, 9_000).is_none());
        let f = p.note(9100, true, true, 20_000).expect("third run");
        assert_eq!(f.class, Class::Suspect(3));
        assert!(f.text.contains("us=true -> friend=false -> us=true"));
        // Reported once.
        assert!(p.note(9100, false, true, 21_000).is_none());
        // Writes far apart are not a ping-pong.
        assert!(p.note(11, true, true, 0).is_none());
        assert!(p.note(11, false, false, 70_000).is_none());
        assert!(p.note(11, true, true, 140_000).is_none());
        // One side only, however often: never.
        for t in 0..10 {
            assert!(p.note(12, true, t % 2 == 0, t * 1000).is_none());
        }
    }

    #[test]
    fn hp_mismatch_needs_three_seconds() {
        let s = |hp: i32, lives: i32| HpSample { hp, max_hp: 1000, posture: 0, max_posture: 500, lives };
        let mut t = HpTimer::default();
        assert!(t.step(s(500, 2), s(560, 2), 0).is_empty()); // 6 %: fine
        assert!(t.step(s(500, 2), s(700, 2), 1_000).is_empty()); // 20 %: timer starts
        assert!(t.step(s(500, 2), s(700, 2), 3_500).is_empty());
        assert_eq!(t.step(s(500, 2), s(700, 2), 4_100), vec![Mismatch::Hp]);
        assert!(t.step(s(500, 2), s(700, 2), 9_000).is_empty()); // once
        // Back in sync, then apart again: fires again after 3 s.
        assert!(t.step(s(500, 2), s(500, 2), 10_000).is_empty());
        assert!(t.step(s(500, 2), s(900, 2), 11_000).is_empty());
        assert_eq!(t.step(s(500, 2), s(900, 2), 14_500), vec![Mismatch::Hp]);
        // Lives.
        let mut t = HpTimer::default();
        assert!(t.step(s(1000, 2), s(1000, 1), 0).is_empty());
        assert_eq!(t.step(s(1000, 2), s(1000, 1), 3_200), vec![Mismatch::Lives]);
        // A short blip (a hit in flight) is not reported.
        let mut t = HpTimer::default();
        assert!(t.step(s(1000, 1), s(600, 1), 0).is_empty());
        assert!(t.step(s(600, 1), s(600, 1), 400).is_empty());
        assert!(t.step(s(600, 1), s(600, 1), 5_000).is_empty());
        // First sight below max.
        assert!(starts_damaged(s(400, 2)));
        assert!(!starts_damaged(s(995, 2)));
        assert!(!starts_damaged(s(0, 0)));
    }

    #[test]
    fn rewards_not_given_in_time_are_reported_once() {
        // Two players: the one friend (peer 1) is the only partner.
        let p = PeerId(1);
        let mut r = Rewards::default();
        r.sent(Pipe::Script, 1, 61000, &[(p, true)], 0);
        r.sent(Pipe::BossDrop, key2(1100300, 10200000), 10200000, &[(p, true)], 0);
        r.sent(Pipe::BossReward, 3, 2000, &[(p, false)], 0); // friend without auditor: never checked
        assert_eq!(r.counters(p)[0], 1);
        assert!(r.ack(Pipe::Script, 1, 61000, ST_RECEIVED, p, 5_000).is_none());
        assert!(r.overdue(30_000).is_empty());
        let f = r.overdue(36_000);
        assert_eq!(f.len(), 1);
        assert!(f[0].1.text.contains("still not given"));
        assert!(r.overdue(40_000).is_empty());
        // The boss drop never got a receipt: 90 s.
        assert!(r.overdue(80_000).is_empty());
        let f = r.overdue(91_000);
        assert_eq!(f.len(), 1);
        assert!(f[0].1.text.contains("never reached"));
        // A late resolution says so; a "not given" ack is a bug of its own.
        assert_eq!(r.ack(Pipe::Script, 1, 61000, ST_OK, p, 100_000).map(|f| f.class), Some(Class::Info));
        r.sent(Pipe::Script, 9, 62402, &[(p, true)], 200_000);
        assert_eq!(r.ack(Pipe::Script, 9, 62402, ST_NOT_OK, p, 206_000).map(|f| f.class), Some(Class::Suspect(3)));
        assert!(r.overdue(400_000).is_empty());
        assert!(r.ack(Pipe::Script, 77, 1, ST_OK, p, 0).is_none());
    }

    #[test]
    fn rewards_are_acked_and_counted_per_peer() {
        // The host's grant to three clients: 1 and 2 run the auditor, 3 does not.
        let (a, b, c) = (PeerId(1), PeerId(2), PeerId(3));
        let mut r = Rewards::default();
        r.sent(Pipe::Script, 5, 61000, &[(a, true), (b, true), (c, false)], 0);
        assert_eq!([r.counters(a)[0], r.counters(b)[0], r.counters(c)[0]], [1, 1, 1]);
        // Client 1 gives it; client 2 never answers; client 3 is never asked.
        assert!(r.ack(Pipe::Script, 5, 61000, ST_RECEIVED, a, 1_000).is_none());
        assert!(r.ack(Pipe::Script, 5, 61000, ST_OK, a, 4_000).is_none());
        assert!(r.ack(Pipe::Script, 5, 61000, ST_OK, c, 4_000).is_none());
        let f = r.overdue(91_000);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].0, b);
        assert!(f[0].1.text.contains("never reached"));
        assert!(r.overdue(200_000).is_empty());
        // A "not given" from one peer is that peer's finding only.
        r.sent(Pipe::BossReward, 6, 2000, &[(a, true), (b, true)], 300_000);
        assert!(r.ack(Pipe::BossReward, 6, 2000, ST_OK, b, 303_000).is_none());
        assert_eq!(r.ack(Pipe::BossReward, 6, 2000, ST_NOT_OK, a, 303_000).map(|f| f.class), Some(Class::Suspect(3)));
        // Received / resolved / not given counted per sender; a peer that left is forgotten.
        r.count(b, Pipe::Script, 1);
        r.count(b, Pipe::Script, 3);
        assert_eq!(r.counters(b)[..4], [1, 1, 0, 1]);
        assert_eq!(r.counters(a)[1], 0);
        r.sent(Pipe::Script, 7, 61000, &[(a, true), (b, true)], 400_000);
        r.forget(b);
        assert_eq!(r.counters(b), [0; REWARD_COUNTERS]);
        assert_eq!(r.overdue(500_000).iter().map(|x| x.0).collect::<Vec<_>>(), vec![a]);
    }

    fn link(slot: u8) -> Link {
        Link::new(PeerId(slot), format!("p{slot}"), 1, 0, 180_000, 0)
    }

    #[test]
    fn each_peer_is_compared_with_the_host_on_its_own() {
        // The host and client 1 agree; client 2 lacks the boss drop and one shared record differs.
        let host = Digest { lots: vec![(6761, 0)], records: vec![(11_000_000, 5), (12_000_000, 9)], ..Default::default() };
        let c1 = host.clone();
        let c2 = Digest { lots: vec![], records: vec![(11_000_000, 6), (12_000_000, 9)], ..Default::default() };
        let (mut l1, mut l2) = (link(1), link(2));
        let o = l1.observe(&host, &c1, &Fake, 1_000);
        assert!(o.due.is_empty() && o.pending == 0 && o.mism.is_empty());
        let o = l2.observe(&host, &c2, &Fake, 1_000);
        assert_eq!((o.pending, o.mism), (1, vec![11_000_000]));
        assert!(l1.audit && l2.audit);
        // Client 1 now lacks it too (later): each link keeps its own grace clock.
        let o = l1.observe(&host, &c2, &Fake, 30_000);
        assert!(o.due.is_empty() && o.pending == 1);
        let o = l2.observe(&host, &c2, &Fake, 47_000);
        assert_eq!(o.due.iter().map(|f| (f.key.as_str(), f.class)).collect::<Vec<_>>(), vec![("lot:6761:us", Class::Suspect(3))]);
        let o = l1.observe(&host, &c2, &Fake, 47_000);
        assert!(o.due.is_empty() && o.pending == 1);
        let o = l1.observe(&host, &c2, &Fake, 76_000);
        assert_eq!(o.due.len(), 1);
        // Per-player data keeps the 2-player rules: a map pickup only one side has is expected.
        let ours = Digest { lots: vec![(50000100, 0)], ..Default::default() };
        let mut l3 = link(3);
        assert_eq!(l3.observe(&ours, &Digest::default(), &Fake, 0).pending, 1);
        let o = l3.observe(&ours, &Digest::default(), &Fake, GRACE_MS);
        assert_eq!(o.due.iter().map(|f| f.class).collect::<Vec<_>>(), vec![Class::Expected]);
        // Dedupe keys and tags are per peer.
        assert_eq!(peer_key(PeerId(2), "lot:6761:us"), "p2:lot:6761:us");
        assert_ne!(peer_key(PeerId(1), "x"), peer_key(PeerId(2), "x"));
        assert_eq!(tag(PeerId(2), "Friend2"), "[peer 2 'Friend2'] ");
    }

    #[test]
    fn links_follow_the_peers_and_restart_one_at_a_time() {
        let w = |v: &[(u8, u64)]| v.iter().map(|&(s, e)| (PeerId(s), e, format!("n{s}"))).collect::<Vec<_>>();
        let mut links = Vec::new();
        // Two players: one link, hello at 45 s and the periodic round after it, as v7.7.
        let (st, dr) = sync_links(&mut links, &w(&[(1, 1)]), 0, 180_000);
        assert_eq!((st, dr), (vec![PeerId(1)], vec![]));
        assert_eq!(links[0].due, Some((HELLO_AFTER_MS, REASON_HELLO)));
        assert_eq!(links[0].next_periodic, HELLO_AFTER_MS + 180_000);
        // Two more clients join at once: their hellos are staggered.
        let (st, _) = sync_links(&mut links, &w(&[(1, 1), (2, 1), (3, 1)]), 1_000, 180_000);
        assert_eq!(st, vec![PeerId(2), PeerId(3)]);
        assert_eq!(links[1].due, Some((1_000 + HELLO_AFTER_MS + STAGGER_MS, REASON_HELLO)));
        assert_eq!(links[2].due, Some((1_000 + HELLO_AFTER_MS + 2 * STAGGER_MS, REASON_HELLO)));
        links[0].audit = true;
        links[2].audit = true;
        // Peer 2 reconnects (new epoch): only its link starts over; names follow the roster.
        let mut want = w(&[(1, 1), (2, 2), (3, 1)]);
        want[2].2 = "Friend2".into();
        let (st, dr) = sync_links(&mut links, &want, 100_000, 180_000);
        assert_eq!((st, dr), (vec![PeerId(2)], vec![PeerId(2)]));
        assert!(links[0].audit && !links[1].audit && links[2].audit);
        assert_eq!((links[1].epoch, links[2].name.as_str()), (2, "Friend2"));
        // Peer 1 leaves.
        let (st, dr) = sync_links(&mut links, &w(&[(2, 2), (3, 1)]), 200_000, 180_000);
        assert_eq!((st, dr), (vec![], vec![PeerId(1)]));
        assert_eq!(links.iter().map(|l| l.id).collect::<Vec<_>>(), vec![PeerId(2), PeerId(3)]);
    }

    #[test]
    fn a_round_of_the_friend_covers_ours_due_soon() {
        assert_eq!(covered(Some(10_000), 0), None);
        assert_eq!(covered(Some(40_000), 0), Some(40_000));
        assert_eq!(covered(None, 0), None);
        // §33 lot-id rule: talk / event lots below 1 000 000, map lots above.
        assert!(static_script_lot(61000) && static_script_lot(2000));
        assert!(!static_script_lot(61400) && !static_script_lot(13604000) && !static_script_lot(0));
    }

    #[test]
    fn inventory_layout_matches_the_find_item_constant() {
        assert_eq!(std::mem::offset_of!(sekiro::sprj::PlayerGameData, equip_game_data), PGD_EQUIP_GAME_DATA);
        assert_eq!(size_of::<sekiro::sprj::EquipInventoryDataListEntry>(), INV_ENTRY);
        assert!(valid_item(GOODS | 2920) && valid_item(0x1234) && !valid_item(u32::MAX));
        assert_eq!(lot_full(4, 2920), GOODS | 2920);
        assert_eq!(lot_full(0x4000_0000u32 as i32, 2920), GOODS | 2920);
        assert_eq!(lot_full(0, 70000), 70000);
    }
}
