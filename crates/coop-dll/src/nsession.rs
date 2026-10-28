//! The native session (settings `native_session` = 1 and `native_session_stage` = 3 by
//! default; docs/SPEC-NATIVE-SESSION.md, IMPLEMENTATION-NOTES § 18 / 18b). NOT verified live.
//!
//! The game's own online code (DS3-style NetAI enemy ownership, enemy HP packets,
//! player hit packets) runs on top of our coop-net link instead of a FromSoftware
//! session. The real `SprjSessionManager` is never started and its member vector
//! stays empty:
//! - §1 the session checks are faked at their call sites (InSession, IsHost,
//!   IsClient, member count), never in the shared getters (Steam, summons and the
//!   save code call those too);
//! - §2.1 five entry hooks on FrpgSessionManager's send / receive functions carry
//!   the whitelisted packet types over our link: unreliable ones (4, 5) as PlayerRaw
//!   packets of type `PTYPE_BASE | type`, reliable ones and the handshake through our
//!   own lane (module `lane`, PlayerRaw `PTYPE_BASE | 0xFE / 0xFF`);
//! - §2.2 the three receive loops that iterate the member vector (types 70, 4, 71)
//!   get a fake one-member vector, whose member only our receive hook knows;
//! - §2.3 the "any member" receive functions of the small player packets (21, 28,
//!   52, 18, 42, 45) are served from our queues;
//! - §4 the host feeds the friend's candidate list (type 22) straight into the
//!   game's ownership table with the game's own functions.
//!
//! Stages (`native_session_stage`): 1 = hooks installed, packets only counted (no
//! patch, nothing routed); 2 = + enemies (types 4/5/22/23/70/71 and the hit packet
//! 21, §1, §2.2, §4); 3 = + 28/52/18/42; 4 = + ObjAct events 45 (experimental).
//! Spec stage 5 (SpEffects) has no packet to route and §6 (the game's own scaling)
//! is not better than scaling.rs: see IMPLEMENTATION-NOTES § 18b.
//!
//! Safety rules:
//! - `native_session = 0`: nothing is installed, patched or sent; `tick` only
//!   keeps `OWNS_ENEMIES` false. Packets of ours from a friend are consumed and
//!   dropped.
//! - With 1, the hooks go in at the title screen (or on the first engagement when
//!   switched on later) after every prologue was checked (all or nothing) and stay as
//!   pass-through afterwards. The call-site patches are written only while engaged,
//!   with every other thread suspended (module `freeze`), after every site was
//!   checked, and restored byte for byte on every disengagement (setting off, link
//!   down, load, friend in another block, fallback rule, peer left).
//! - A failure switches to our own sync from the next load (never mid-fight); a
//!   failed byte check keeps the native session off for the rest of the run.
//! - Both players must use the same `native_session_stage`: a handshake (Hello,
//!   netId list, Ready) has to agree before anything is patched.
//!
//! What our own systems hand over while the native session owns the enemies is
//! in one place: `step_aside` below.
//!
//! N players (PROTO 5, PLAN-MULTIPLAYER §5.14, NOT verified live): a star through the
//! host. The host runs one handshake / engagement per client (`Link`), a client one
//! with the host. The game sees ONE fake member whose inbox merges every peer's packets
//! (IDA §5.1e); player netIds travel as `0xFFF0 | slot` (§5.1d). Every game packet and
//! the Owned / Lives messages go hop by hop: a client sends to the host only, tagged
//! with its origin slot and a send mode (all / host only / one peer); the host applies
//! it and re-queues an "all" one to every other engaged client under its own
//! generation for that link. The host's type 23 reaches the right client through the
//! recipient wrap at `0xA1DFC1` (rcx = the recipient chr) or `0xA1DFDD` (rcx = its
//! record). `MSG_ENGAGED` tells the clients who is engaged (ownfill, Owned lists).
//! Safety rules for N: a peer's own trouble disengages only that peer; our block
//! change, a rest, a load disengage all; a same-block peer that cannot join the native
//! session (unavailable, latched failure, fallback world, no hello) keeps everyone in
//! that block on our own sync (no half-engaged block); more than
//! `native_session_max_players` players, or 3+ without a recipient wrap, = our own sync.
//! With one peer everything behaves as the 2-player PROTO 4 build.
//!
//! Every RVA and byte below was re-read in IDA on a private copy
//! (F:\Temp\ida_ns\db.i64, jobs o1-o4, 2026-10-23).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use coop_net::{MAX_PLAYER_RAW, Session};

use crate::game::{Chr, module_base, read, read_bytes, read_ptr, write};
use crate::hook::{freeze, write_frozen, write_frozen_budget};
use crate::peers::{MAX_PLAYERS, PeerFrame, PeerId, PeerSet};
use crate::{hook, log, netai, tuning};

// ---------------------------------------------------------------------------
// Step aside: the one place that says what our own sync gives up.
// ---------------------------------------------------------------------------

/// True only while the native session is ENGAGED at stage >= 2 (review #5): before
/// that (settle, remote spawn, handshake, cooldown, friend elsewhere) and after a
/// disengagement our own enemy sync runs, so one of the two always does. Set by
/// `tick` and cleared by every disengagement.
static OWNS_ENEMIES: AtomicBool = AtomicBool::new(false);

pub mod step_aside {
    //! Native session on (stage >= 2) -> these of ours are off:
    //! - enemies.rs: the whole mode-2/3/4 sync (NetAI vtable / slot 37, record
    //!   writes, host record sending, HP / death authority, cumulative damage
    //!   reports, lethal hold, throw window, UpdateCoords pinning, CompletelyNoMove).
    //!   Kept: the entity-id scan and the loot step (items stay ours), and the scan
    //!   list for scaling and the death rule (P3 dead list stays ours too: the
    //!   game's type 5 only carries host -> client HP, it cannot tell the host
    //!   which enemies died on the client before the host's death reload).
    //! - enemies.rs's DamageReport and posture.rs's posture / deathblow sync (posture.rs
    //!   asks `enemy_sync_off` itself): from stage 2 the game's own hit packet 21 carries
    //!   every player hit on an enemy owned elsewhere, with HP (`+4`) and posture
    //!   (`+6`, `+8`) damage (`0xB6A040`, IDA o2), so a second report would double it.
    //!   Evidence for 21 / 28 is logged (`nsession: evidence ...`).
    //! - activation.rs: the host's "every-frame AI near the friend" (the friend now
    //!   owns the enemies near them; spec §4: off at first).
    //!
    //! Not changed: artfx.rs. Bullets (type 63) stay artfx's (`NATIVE_BULLETS` below).
    //! The damage table is vanilla since the artfx review fix, nothing to hand back.
    //!
    //! Never off: native.rs (the remote player itself), world.rs (flags, join
    //! snapshot; it wins over type 45, see `STAGE4_TYPES`), rest / Unseen Aid / boss
    //! rules, items, scaling, UI.
    //!
    //! When a stage is proven live, the custom path it replaces can be deleted: these
    //! functions are the only places that switch it off.

    use std::sync::atomic::Ordering;

    /// Bullets through the game's own type 63 instead of artfx.rs: no. Both machines'
    /// local players carry the same playerNo in their own game, and our native remote
    /// must have a different one (native.rs), so the owner field of every received
    /// record has to be rewritten to the remote's net form (`0xE8F040`) anyway, at the
    /// same three call sites in `0xB1D600` that artfx.rs already patches. A native
    /// copy would be artfx.rs again; artfx.rs is the single active bullet path, and
    /// nsession never whitelists 63 (unit test `bullets_stay_with_artfx`).
    pub const NATIVE_BULLETS: bool = false;

    /// enemies.rs (and posture.rs through it): skip the mode-2/3/4 enemy sync,
    /// DamageReport and posture reports included.
    pub fn enemy_sync_off() -> bool {
        super::OWNS_ENEMIES.load(Ordering::Relaxed)
    }

    /// puppet.rs / activation.rs: no forced update interval near the friend.
    pub fn activation_off() -> bool {
        super::OWNS_ENEMIES.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Wire format (pure, unit-tested)
// ---------------------------------------------------------------------------

/// Bulk kind of the first build's handshake; still consumed (and ignored) so an old
/// friend's messages never reach anything else.
pub const BULK_KIND: u8 = 0x4E;
/// PlayerRaw types `PTYPE_BASE | x`: x < 128 = an unreliable game packet of type x,
/// `LANE_DATA` / `LANE_ACK` = our reliable lane (module `lane`).
pub const PTYPE_BASE: u32 = 0x4E53_0000;
const PTYPE_MASK: u32 = 0xFFFF_0000;
const LANE_DATA: u32 = 0xFE;
const LANE_ACK: u32 = 0xFF;
/// Bumped when the nsession wire format changes (2: own reliable lane, List with HP;
/// 3: Owned, type 18 / 28 player netIds rewritten; 4: Lives, livesync.rs; 5: N players:
/// player netIds `0xFFF0 | slot`, game frames with origin + send mode, Owned / Lives with
/// origin, Engaged).
const PROTO: u8 = 5;

const MSG_HELLO: u8 = 1;
const MSG_LIST: u8 = 2;
const MSG_READY: u8 = 3;
const MSG_LEAVE: u8 = 4;
const MSG_OWNED: u8 = 5;
const MSG_LIVES: u8 = 6;
/// Host -> clients: the slots engaged with the host now.
const MSG_ENGAGED: u8 = 7;
/// Max HP of the enemies in our List (same generation), sent right after it on that link
/// (v7.7 §34; v7.7 used 7, taken here by Engaged; PROTO 5 peers only, so no clash).
const MSG_MAX_HP: u8 = 8;

/// Session slots (host 0, clients 1..=5); per-link state is indexed by slot.
const SLOTS: usize = MAX_PLAYERS;

/// `[ADVANCED] native_session_max_players` (2..6, settings.rs; the host's value decides):
/// with more players in the session the host keeps our own sync (no handshakes).
pub static MAX_NATIVE_PLAYERS: AtomicU8 = AtomicU8::new(MAX_PLAYERS as u8);
/// At most this many netIds in one Owned message.
const MAX_OWNED: usize = 2048;
/// At most this many enemies in one Lives message.
pub const MAX_LIVES: usize = 1024;

/// One enemy's lives / death state in a Lives message (livesync.rs): netId (`chr+0x78`),
/// lives left (`data+0x25C`), flags (`LIFE_DEAD`, `LIFE_FRESH`).
pub type LifeEntry = (u16, i8, u8);
/// The enemy is dead on the sender's machine (HP 0).
pub const LIFE_DEAD: u8 = 1;
/// The sender saw the death happen during the current engagement (alive -> dead), so the
/// receiver gives its own loot for it; an old death (before a load) is restored without.
pub const LIFE_FRESH: u8 = 2;

/// `0x8E6FA0` refuses anything with `size + 8 > 20480`.
const MAX_GAME_PACKET: usize = 20480 - 8;
const MAX_PAIRS: usize = 4096;

/// Why a side left (Leave reason byte, for the log).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Reason {
    Other = 0,
    Load = 1,
    SettingOff = 2,
    FriendElsewhere = 3,
    NoRemote = 4,
    No23 = 5,
    NetIdMismatch = 6,
    CheckFailed = 7,
    PeerRestarted = 8,
    /// A rest reset the enemies (restsync.rs): out, and a new handshake after `REST_HOLD`.
    Rest = 9,
    /// Host: our own sync for this block (too many players, no recipient wrap, or a
    /// peer here that cannot join the native session). Not latched.
    OwnSync = 10,
}

impl Reason {
    fn from_u8(v: u8) -> Self {
        match v {
            10 => Self::OwnSync,
            9 => Self::Rest,
            1 => Self::Load,
            2 => Self::SettingOff,
            3 => Self::FriendElsewhere,
            4 => Self::NoRemote,
            5 => Self::No23,
            6 => Self::NetIdMismatch,
            7 => Self::CheckFailed,
            8 => Self::PeerRestarted,
            _ => Self::Other,
        }
    }
}

/// One loaded enemy in the netId check list: entity id, netId, HP.
type Pair = (i32, u16, i32);

/// Handshake messages (lane key 0).
#[derive(Clone, Debug, PartialEq)]
enum Msg {
    /// `generation` counts the sender's handshakes; List / Ready / Leave / packets name it.
    Hello { proto: u8, stage: u8, host: bool, block: i32, generation: u32 },
    /// `{entity id, netId, HP}` of the enemies the sender has loaded (§3 netId check;
    /// the HP keeps a host death reload from healing the client's enemies).
    List { generation: u32, block: i32, pairs: Vec<Pair> },
    /// The sender checked our list of `peer_generation` and is ready to engage with it.
    Ready { generation: u32, peer_generation: u32 },
    Leave { generation: u32, latch: bool, reason: u8 },
    /// Every 2 s while engaged: the netIds of the enemies owned on `origin`'s machine
    /// (`0xA4A650`), so each side can log the enemies nobody owns (no controller).
    /// `generation` is the sending hop's (the host re-tags what it relays).
    Owned { generation: u32, origin: u8, ids: Vec<u16> },
    /// livesync.rs: lives / death of enemies that lost a life or died on `origin`'s
    /// machine (changes, and every 2 s all of them).
    Lives { generation: u32, origin: u8, entries: Vec<LifeEntry> },
    /// Host -> each engaged client: the client slots engaged with the host now.
    Engaged { generation: u32, slots: Vec<u8> },
    /// `{entity id, max HP}` of the enemies in the sender's List of `generation`, per link
    /// (not relayed): the engage HP check compares HP only where both machines have the
    /// same max (scaling settled, §34).
    MaxHp { generation: u32, max: Vec<(i32, i32)> },
}

fn encode(m: &Msg) -> Vec<u8> {
    let mut v = Vec::with_capacity(32);
    match m {
        Msg::Hello { proto, stage, host, block, generation } => {
            v.extend_from_slice(&[MSG_HELLO, *proto, *stage, *host as u8]);
            v.extend_from_slice(&block.to_le_bytes());
            v.extend_from_slice(&generation.to_le_bytes());
        }
        Msg::List { generation, block, pairs } => {
            v.push(MSG_LIST);
            v.extend_from_slice(&generation.to_le_bytes());
            v.extend_from_slice(&block.to_le_bytes());
            let pairs = &pairs[..pairs.len().min(MAX_PAIRS)];
            v.extend_from_slice(&(pairs.len() as u16).to_le_bytes());
            for (e, n, hp) in pairs {
                v.extend_from_slice(&e.to_le_bytes());
                v.extend_from_slice(&n.to_le_bytes());
                v.extend_from_slice(&hp.to_le_bytes());
            }
        }
        Msg::Ready { generation, peer_generation } => {
            v.push(MSG_READY);
            v.extend_from_slice(&generation.to_le_bytes());
            v.extend_from_slice(&peer_generation.to_le_bytes());
        }
        Msg::Leave { generation, latch, reason } => {
            v.push(MSG_LEAVE);
            v.extend_from_slice(&generation.to_le_bytes());
            v.push(*latch as u8);
            v.push(*reason);
        }
        Msg::Owned { generation, origin, ids } => {
            v.push(MSG_OWNED);
            v.extend_from_slice(&generation.to_le_bytes());
            v.push(*origin);
            let ids = &ids[..ids.len().min(MAX_OWNED)];
            v.extend_from_slice(&(ids.len() as u16).to_le_bytes());
            for n in ids {
                v.extend_from_slice(&n.to_le_bytes());
            }
        }
        Msg::Lives { generation, origin, entries } => {
            v.push(MSG_LIVES);
            v.extend_from_slice(&generation.to_le_bytes());
            v.push(*origin);
            let entries = &entries[..entries.len().min(MAX_LIVES)];
            v.extend_from_slice(&(entries.len() as u16).to_le_bytes());
            for (n, lives, flags) in entries {
                v.extend_from_slice(&n.to_le_bytes());
                v.push(*lives as u8);
                v.push(*flags);
            }
        }
        Msg::Engaged { generation, slots } => {
            v.push(MSG_ENGAGED);
            v.extend_from_slice(&generation.to_le_bytes());
            let slots = &slots[..slots.len().min(SLOTS)];
            v.push(slots.len() as u8);
            v.extend_from_slice(slots);
        }
        Msg::MaxHp { generation, max } => {
            v.push(MSG_MAX_HP);
            v.extend_from_slice(&generation.to_le_bytes());
            let max = &max[..max.len().min(MAX_PAIRS)];
            v.extend_from_slice(&(max.len() as u16).to_le_bytes());
            for (e, m) in max {
                v.extend_from_slice(&e.to_le_bytes());
                v.extend_from_slice(&m.to_le_bytes());
            }
        }
    }
    v
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

fn decode(b: &[u8]) -> Option<Msg> {
    let mut r = Reader { b, at: 0 };
    let m = match r.u8()? {
        MSG_HELLO => {
            let proto = r.u8()?;
            let stage = r.u8()?;
            let host = match r.u8()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            Msg::Hello { proto, stage, host, block: r.i32()?, generation: r.u32()? }
        }
        MSG_LIST => {
            let generation = r.u32()?;
            let block = r.i32()?;
            let n = r.u16()? as usize;
            if n > MAX_PAIRS {
                return None;
            }
            let mut pairs = Vec::with_capacity(n);
            for _ in 0..n {
                pairs.push((r.i32()?, r.u16()?, r.i32()?));
            }
            Msg::List { generation, block, pairs }
        }
        MSG_READY => Msg::Ready { generation: r.u32()?, peer_generation: r.u32()? },
        MSG_LEAVE => Msg::Leave { generation: r.u32()?, latch: r.u8()? != 0, reason: r.u8()? },
        MSG_OWNED => {
            let generation = r.u32()?;
            let origin = r.u8()?;
            let n = r.u16()? as usize;
            if n > MAX_OWNED || origin as usize >= SLOTS {
                return None;
            }
            let mut ids = Vec::with_capacity(n);
            for _ in 0..n {
                ids.push(r.u16()?);
            }
            Msg::Owned { generation, origin, ids }
        }
        MSG_LIVES => {
            let generation = r.u32()?;
            let origin = r.u8()?;
            let n = r.u16()? as usize;
            if n > MAX_LIVES || origin as usize >= SLOTS {
                return None;
            }
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push((r.u16()?, r.u8()? as i8, r.u8()?));
            }
            Msg::Lives { generation, origin, entries }
        }
        MSG_ENGAGED => {
            let generation = r.u32()?;
            let n = r.u8()? as usize;
            if n > SLOTS {
                return None;
            }
            let slots = r.take(n)?.to_vec();
            if slots.iter().any(|&s| s as usize >= SLOTS) {
                return None;
            }
            Msg::Engaged { generation, slots }
        }
        MSG_MAX_HP => {
            let generation = r.u32()?;
            let n = r.u16()? as usize;
            if n > MAX_PAIRS {
                return None;
            }
            let mut max = Vec::with_capacity(n);
            for _ in 0..n {
                max.push((r.i32()?, r.i32()?));
            }
            Msg::MaxHp { generation, max }
        }
        _ => return None,
    };
    (r.at == b.len()).then_some(m)
}

/// Send modes of a game packet on the wire (PLAN-MULTIPLAYER §5.2.3).
/// The game broadcast it: the host applies it and relays it to every other engaged client.
const MODE_ALL: u8 = 0;
/// To the host only (22, a hit on an enemy the sender owns): never relayed.
const MODE_HOST: u8 = 1;
/// From the host to this one client only (23).
const MODE_PEER: u8 = 2;
/// `u32 generation, u8 origin, u8 mode` in front of the game's bytes.
const RAW_HEADER: usize = 6;

/// Game packet body (unreliable PlayerRaw or a lane message): the sending hop's
/// generation for this link, the slot whose game made the packet, the send mode, then
/// the game's bytes (player netIds in their `0xFFF0 | slot` form).
fn encode_raw(generation: u32, origin: u8, mode: u8, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(RAW_HEADER + data.len());
    v.extend_from_slice(&generation.to_le_bytes());
    v.push(origin);
    v.push(mode);
    v.extend_from_slice(data);
    v
}

fn decode_raw(b: &[u8]) -> Option<(u32, u8, u8, &[u8])> {
    let generation = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
    let (origin, mode) = (*b.get(4)?, *b.get(5)?);
    let d = &b[RAW_HEADER..];
    ((origin as usize) < SLOTS && mode <= MODE_PEER && !d.is_empty() && d.len() <= MAX_GAME_PACKET)
        .then_some((generation, origin, mode, d))
}

/// Where a packet goes, as decided where it was made (send hooks) or relayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dst {
    /// Every engaged peer but the origin (a client: through the host).
    All,
    /// The host only.
    Host,
    /// One peer (host -> client).
    Peer(u8),
}

/// The wire mode of a packet we send to `dst` (a client reaches everyone through the host).
fn wire_mode(host: bool, dst: Dst) -> u8 {
    match (host, dst) {
        (_, Dst::All) => MODE_ALL,
        (true, Dst::Peer(_)) => MODE_PEER,
        _ => MODE_HOST,
    }
}

/// The links a packet goes out on: `engaged` = bit per link slot we are engaged with.
/// Host: every engaged client but the origin, or the one peer. Client: the host link.
fn targets(host: bool, dst: Dst, origin: u8, engaged: u8) -> Vec<u8> {
    let on = |s: u8| (s as usize) < SLOTS && engaged >> s & 1 != 0;
    if !host {
        return if on(0) { vec![0] } else { Vec::new() };
    }
    match dst {
        Dst::All => (1..SLOTS as u8).filter(|&s| s != origin && on(s)).collect(),
        Dst::Peer(s) if s != 0 && on(s) => vec![s],
        _ => Vec::new(),
    }
}

/// A received frame's origin is believable: a client can only send its own packets to
/// the host; a client hears only the host (with the host's or another client's origin).
fn origin_ok(host: bool, from: u8, origin: u8, me: u8) -> bool {
    if host { from != 0 && origin == from } else { from == 0 && origin != me }
}

/// The host relays what a client broadcast (mode "all"); nothing else is ever relayed.
fn relays(host: bool, mode: u8) -> bool {
    host && mode == MODE_ALL
}

/// Our own reliable stream (review 2026-10-23, finding a): selective-repeat ARQ over
/// PlayerRaw, independent of coop-net's bulk pipe (world flags, boss rewards), with
/// priority classes and its own window, so a 20 KB behavior packet never holds back
/// the ownership list or a flag event. Pure, unit-tested.
///
/// A message (handshake, one game packet) is cut into fragments of `FRAG` bytes; every
/// fragment has its own sequence number and is re-sent every `RTO` until acked. The
/// receiver acks each fragment, reassembles, and hands complete messages out in order
/// per key (key 0 = handshake, else the game type), so packets of one type stay in
/// order while other types overtake them. A gap older than `GAP_SKIP` is skipped.
mod lane {
    use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
    use std::time::{Duration, Instant};

    /// Fragment payload (PlayerRaw allows 1100 B; header 21 B).
    pub const FRAG: usize = 1000;
    pub const HEADER: usize = 4 + 4 + 4 + 1 + 4 + 2 + 2;
    pub const RTO: Duration = Duration::from_millis(200);
    /// Fragments in flight: all classes / the bulk class (behavior) alone.
    pub const WINDOW: usize = 128;
    pub const WINDOW_BULK: usize = 96;
    pub const MAX_TRIES: u32 = 40;
    pub const GAP_SKIP: Duration = Duration::from_secs(3);
    pub const MAX_ACKS: usize = 250;
    /// Classes: 0 handshake + ownership, 1 small game packets, 2 behavior (big).
    pub const CLASSES: usize = 3;
    const MAX_MSG: usize = 64 * 1024;

    struct Frag {
        bytes: Vec<u8>,
        class: u8,
        sent_at: Instant,
        tries: u32,
    }

    struct Partial {
        since: Instant,
        key: u8,
        kseq: u32,
        parts: Vec<Option<Vec<u8>>>,
        got: usize,
    }

    #[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Stats {
        pub sent: u32,
        pub resent: u32,
        pub given_up: u32,
        pub acked: u32,
        pub received: u32,
        pub duplicates: u32,
        pub delivered: u32,
        pub gaps_skipped: u32,
        pub bad: u32,
    }

    pub struct Lane {
        id: u32,
        next_seq: u32,
        next_msg: u32,
        kseq: HashMap<u8, u32>,
        queues: [VecDeque<Vec<u8>>; CLASSES],
        inflight: BTreeMap<u32, Frag>,
        // Receive side.
        peer: Option<u32>,
        seen: HashSet<u32>,
        seen_max: u32,
        partial: HashMap<u32, Partial>,
        next_k: HashMap<u8, u32>,
        held: HashMap<u8, BTreeMap<u32, (Instant, Vec<u8>)>>,
        acks: Vec<u32>,
        pub stats: Stats,
    }

    impl Lane {
        pub fn new(id: u32) -> Self {
            Self {
                id,
                next_seq: 0,
                next_msg: 0,
                kseq: HashMap::new(),
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                inflight: BTreeMap::new(),
                peer: None,
                seen: HashSet::new(),
                seen_max: 0,
                partial: HashMap::new(),
                next_k: HashMap::new(),
                held: HashMap::new(),
                acks: Vec::new(),
                stats: Stats::default(),
            }
        }

        /// Fragments waiting or in flight.
        pub fn backlog(&self) -> usize {
            self.queues.iter().map(VecDeque::len).sum::<usize>() + self.inflight.len()
        }

        /// Queues one message for `key` (0 handshake, else the game type) in `class`.
        pub fn queue(&mut self, key: u8, class: u8, msg: &[u8]) {
            if msg.len() > MAX_MSG {
                self.stats.bad += 1;
                return;
            }
            let class = (class as usize).min(CLASSES - 1);
            let m = self.next_msg;
            self.next_msg = self.next_msg.wrapping_add(1);
            let k = self.kseq.entry(key).or_insert(0);
            let kseq = *k;
            *k = k.wrapping_add(1);
            let chunks: Vec<&[u8]> = if msg.is_empty() { vec![&[][..]] } else { msg.chunks(FRAG).collect() };
            let count = chunks.len() as u16;
            for (i, c) in chunks.into_iter().enumerate() {
                let mut f = Vec::with_capacity(HEADER + c.len());
                f.extend_from_slice(&self.id.to_le_bytes());
                f.extend_from_slice(&[0; 4]); // seq, filled in when sent
                f.extend_from_slice(&m.to_le_bytes());
                f.push(key);
                f.extend_from_slice(&kseq.to_le_bytes());
                f.extend_from_slice(&(i as u16).to_le_bytes());
                f.extend_from_slice(&count.to_le_bytes());
                f.extend_from_slice(c);
                self.queues[class].push_back(f);
            }
        }

        /// Datagrams to send now: `(true = data / false = ack, bytes)`.
        pub fn poll(&mut self, now: Instant) -> Vec<(bool, Vec<u8>)> {
            let mut out = Vec::new();
            let mut drop = Vec::new();
            for (&seq, f) in self.inflight.iter_mut() {
                if now.saturating_duration_since(f.sent_at) >= RTO {
                    if f.tries >= MAX_TRIES {
                        drop.push(seq);
                        continue;
                    }
                    f.tries += 1;
                    f.sent_at = now;
                    self.stats.resent += 1;
                    out.push((true, f.bytes.clone()));
                }
            }
            for s in drop {
                self.inflight.remove(&s);
                self.stats.given_up += 1;
            }
            for class in 0..CLASSES {
                loop {
                    let bulk = self.inflight.values().filter(|f| f.class as usize == CLASSES - 1).count();
                    let full = self.inflight.len() >= WINDOW || (class == CLASSES - 1 && bulk >= WINDOW_BULK);
                    if full {
                        break;
                    }
                    let Some(mut bytes) = self.queues[class].pop_front() else { break };
                    let seq = self.next_seq;
                    self.next_seq = self.next_seq.wrapping_add(1);
                    bytes[4..8].copy_from_slice(&seq.to_le_bytes());
                    self.stats.sent += 1;
                    out.push((true, bytes.clone()));
                    self.inflight.insert(seq, Frag { bytes, class: class as u8, sent_at: now, tries: 1 });
                }
            }
            for chunk in self.acks.chunks(MAX_ACKS) {
                let mut a = Vec::with_capacity(6 + 4 * chunk.len());
                a.extend_from_slice(&self.peer.unwrap_or(0).to_le_bytes());
                a.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
                for s in chunk {
                    a.extend_from_slice(&s.to_le_bytes());
                }
                out.push((false, a));
            }
            self.acks.clear();
            out
        }

        /// An ack datagram from the peer (for our lane id only).
        pub fn on_ack(&mut self, b: &[u8]) {
            let (Some(id), Some(n)) = (b.get(..4), b.get(4..6)) else {
                self.stats.bad += 1;
                return;
            };
            if u32::from_le_bytes(id.try_into().unwrap_or_default()) != self.id {
                return;
            }
            let n = u16::from_le_bytes(n.try_into().unwrap_or_default()) as usize;
            if b.len() != 6 + 4 * n {
                self.stats.bad += 1;
                return;
            }
            for c in b[6..].chunks_exact(4) {
                let s = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                if self.inflight.remove(&s).is_some() {
                    self.stats.acked += 1;
                }
            }
        }

        /// A data datagram: the messages it completes, in per-key order.
        pub fn on_data(&mut self, b: &[u8], now: Instant) -> Vec<(u8, Vec<u8>)> {
            let mut out = Vec::new();
            if b.len() < HEADER {
                self.stats.bad += 1;
                return out;
            }
            let u32_at = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
            let (id, seq, m, key, kseq) = (u32_at(0), u32_at(4), u32_at(8), b[12], u32_at(13));
            let idx = u16::from_le_bytes([b[17], b[18]]) as usize;
            let count = u16::from_le_bytes([b[19], b[20]]) as usize;
            if count == 0 || idx >= count || count * FRAG > MAX_MSG + FRAG {
                self.stats.bad += 1;
                return out;
            }
            if self.peer != Some(id) {
                // The peer's lane restarted (new link): forget its old stream.
                self.peer = Some(id);
                self.seen.clear();
                self.seen_max = 0;
                self.partial.clear();
                self.next_k.clear();
                self.held.clear();
                self.acks.clear();
            }
            self.acks.push(seq);
            if !self.seen.insert(seq) {
                self.stats.duplicates += 1;
                return out;
            }
            self.stats.received += 1;
            self.seen_max = self.seen_max.max(seq);
            if self.seen.len() > 20_000 {
                let floor = self.seen_max.saturating_sub(10_000);
                self.seen.retain(|&s| s >= floor);
            }
            // Review #6: a message whose last fragment never comes is dropped after a while.
            let stale = GAP_SKIP * 10;
            self.partial.retain(|_, p| now.saturating_duration_since(p.since) < stale);
            let p = self.partial.entry(m).or_insert_with(|| Partial { since: now, key, kseq, parts: vec![None; count], got: 0 });
            if p.parts.len() != count || p.key != key || p.kseq != kseq {
                self.stats.bad += 1;
                return out;
            }
            if p.parts[idx].is_none() {
                p.parts[idx] = Some(b[HEADER..].to_vec());
                p.got += 1;
            }
            if p.got == count {
                let Some(p) = self.partial.remove(&m) else { return out };
                let msg: Vec<u8> = p.parts.into_iter().flatten().flatten().collect();
                self.held.entry(key).or_default().insert(kseq, (now, msg));
            }
            self.release(key, now, &mut out);
            // Gaps of other keys age out too.
            let keys: Vec<u8> = self.held.keys().copied().filter(|&k| k != key).collect();
            for k in keys {
                self.release(k, now, &mut out);
            }
            out
        }

        fn release(&mut self, key: u8, now: Instant, out: &mut Vec<(u8, Vec<u8>)>) {
            let Some(h) = self.held.get_mut(&key) else { return };
            let next = self.next_k.entry(key).or_insert(0);
            // Late copies of what was already handed out (or skipped).
            h.retain(|&k, _| k.wrapping_sub(*next) < u32::MAX / 2);
            loop {
                if let Some((_, msg)) = h.remove(next) {
                    *next = next.wrapping_add(1);
                    self.stats.delivered += 1;
                    out.push((key, msg));
                    continue;
                }
                let oldest = h.iter().map(|(_, (t, _))| *t).min();
                if oldest.is_some_and(|t| now.saturating_duration_since(t) >= GAP_SKIP)
                    && let Some((&k, _)) = h.iter().min_by_key(|(k, _)| k.wrapping_sub(*next))
                {
                    *next = k;
                    self.stats.gaps_skipped += 1;
                    continue;
                }
                break;
            }
        }
    }
}

// --- Packet types (spec §2.4) ---------------------------------------------------

const TYPES: usize = 128;
const T_MOVE: u8 = 4;
const T_HP: u8 = 5;
const T_DAMAGE: u8 = 21;
const T_CAND: u8 = 22;
const T_OWNERS: u8 = 23;
const T_HIT2: u8 = 28;
/// Throw / grab (two chrs moved to two positions, `0x7FAB00` <- `0x8D7D70`).
const T_THROW: u8 = 18;
const T_BEHAVIOR: u8 = 70;
const T_ACTION: u8 = 71;
const T_OBJ_EVENT: u8 = 45;

/// Stage 2: enemy ownership, movement, behavior, action, HP and the hit packet 21.
/// 21 is the damage router's own report (`0xB6A040`, IDA o2): a local player's hit on
/// an enemy owned elsewhere (table action 3) is applied locally AND broadcast as 21,
/// one on an enemy owned here by a client (action 2) goes to the host; the receiver
/// applies it (`0xA4F940` -> `0x8D77B0` -> `0xB680C0`/`0xB68FF0`). Without it the
/// client's hits on host-owned enemies would do nothing (review finding b), so it is
/// part of the enemy stage. Our own DamageReport is off then (no double damage).
const STAGE2_TYPES: [u8; 7] = [T_MOVE, T_HP, T_CAND, T_OWNERS, T_BEHAVIOR, T_ACTION, T_DAMAGE];
/// Stage 3: the other small player packets ([I]: 28 second hit record `0xB6C460`,
/// 52 chr state by entity id, 18 throw / grab (IDA ns2 o7, player netIds rewritten), 42 map object).
const STAGE3_TYPES: [u8; 4] = [T_HIT2, 52, T_THROW, 42];
/// Stage 4 (experimental, not the default): ObjAct events (doors, levers, chests),
/// `0x6BC1B0` -> `0x8DB7E0` `{u16 actor chr+0x78, u8 slot | flags, u8, u16 object}`,
/// received by `0x6BEBB0` -> `0x8D8750`. world.rs's flag sync stays the authority for
/// progress: an ObjAct only replays the friend's interaction (animation, the object's
/// state now); the event flag it sets on the receiver is the value world.rs brings
/// anyway, a flag write to its current value changes nothing and world.rs only sends
/// changes, so whichever arrives first applies and the other is a no-op. Not routed:
/// type 47 (`0x8DA640`, host-only, behind the unpatched global IsHost `0x8E4790`).
const STAGE4_TYPES: [u8; 1] = [T_OBJ_EVENT];
pub const MAX_STAGE: u8 = 4;

/// Types the hooks route at `stage` (bit per type).
fn whitelist(stage: u8) -> u128 {
    let mut m = 0u128;
    let mut add = |types: &[u8]| {
        for &t in types {
            m |= 1 << t;
        }
    };
    if stage >= 2 {
        add(&STAGE2_TYPES);
    }
    if stage >= 3 {
        add(&STAGE3_TYPES);
    }
    if stage >= 4 {
        add(&STAGE4_TYPES);
    }
    m
}

/// Movement and HP go unreliable (newer ones replace lost ones); everything else
/// reliable (behavior, action, ownership, hits must not be lost).
fn reliable(t: u8) -> bool {
    !matches!(t, T_MOVE | T_HP)
}

/// Lane class: ownership first, behavior (the big one) last.
fn lane_class(t: u8) -> u8 {
    match t {
        T_CAND | T_OWNERS => 0,
        T_BEHAVIOR => 2,
        _ => 1,
    }
}

/// Receive queue depth per type (spec §2.4).
fn queue_cap(t: u8) -> usize {
    match t {
        T_MOVE => 32,
        T_HP | T_BEHAVIOR | T_ACTION => 16,
        T_CAND | T_OWNERS => 4,
        _ => 32,
    }
}

// --- Player netIds in types 21 / 28 (§3) ------------------------------------------

/// A player netId on the wire: `0xFFF0 | slot` (host 0, clients 1..=5) in place of the
/// machine-local one (playerNo order differs per machine, IDA §5.1d). Two players:
/// the host's own player is 0xFFF0 and the client's 0xFFF1 on both machines (PROTO 4 had
/// "sender" / "receiver"; same meaning, other bytes).
const WIRE_SLOT: u16 = 0xFFF0;
const NO_NET_ID: u16 = 0xFFFF;

/// Player netIds by slot (ours at our slot, each native remote at its peer's slot).
type PnetMap = [u16; SLOTS];

fn pnet_fields(t: u8) -> &'static [usize] {
    match t {
        T_DAMAGE => &[0x00, 0x0C],
        // Second hit record: `+0x04` the chr it is applied to (`0xA4F940` resolves
        // HIDWORD(rec[0]) and calls `0xB63FE0` on it), `+0x08` the other chr; `+0` is a
        // small kind value (0/1/4/8 in the live 7.1 evidence), not a netId (IDA ns2 o8).
        T_HIT2 => &[0x04, 0x08],
        // Throw / grab / deathblow (`0x7FAB00`, IDA ns2 o7): `+0x20` defender, `+0x22`
        // attacker. Both chrs are placed at the packet's two positions and the throw
        // starts. Without the rewrite the sender's own player (0xA000 on both
        // machines) was the RECEIVER's own player: every throw or deathblow of one
        // player pulled the other one onto them (live 7.1 "constant teleports").
        T_THROW => &[0x20, 0x22],
        // ObjAct: `+0` the acting chr's `chr+0x78` (`0xBF16A0`, IDA o3).
        T_OBJ_EVENT => &[0x00],
        _ => &[],
    }
}

fn get_u16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn put_u16(d: &mut [u8], at: usize, v: u16) {
    if let Some(b) = d.get_mut(at..at + 2) {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

/// Sender: a local player netId (ours or a native remote's) -> `0xFFF0 | its slot`.
fn pnet_to_wire(d: &mut [u8], t: u8, map: &PnetMap) {
    for &at in pnet_fields(t) {
        let Some(v) = get_u16(d, at) else { continue };
        if v == NO_NET_ID {
            continue;
        }
        if let Some(s) = map.iter().position(|&m| m == v) {
            put_u16(d, at, WIRE_SLOT | s as u16);
        }
    }
}

/// Does a netId field of `d` name a slot whose player we have no netId for (`pnet_from_wire` would
/// write 0xFFFF)?
fn pnet_unmapped(d: &[u8], t: u8, map: &PnetMap) -> bool {
    pnet_fields(t).iter().any(|&at| {
        get_u16(d, at).is_some_and(|v| v & 0xFFF0 == WIRE_SLOT && ((v & 0xF) as usize) < SLOTS && map[(v & 0xF) as usize] == NO_NET_ID)
    })
}

/// Receiver: `0xFFF0 | slot` -> our player (our slot) or our native remote of that
/// slot's player (0xFFFF when we have none: the game finds no chr). The same mapping for
/// every origin, so the host relays the wire bytes unchanged.
fn pnet_from_wire(d: &mut [u8], t: u8, map: &PnetMap) {
    for &at in pnet_fields(t) {
        if let Some(v) = get_u16(d, at)
            && v & 0xFFF0 == WIRE_SLOT
            && ((v & 0xF) as usize) < SLOTS
        {
            put_u16(d, at, map[(v & 0xF) as usize]);
        }
    }
}

// --- netId check (§3, §7) ---------------------------------------------------------

/// (common entity ids, mismatches `(entity, ours, theirs)`). Entity ids that occur
/// more than once on either side are left out (they name no single enemy).
fn netid_check(ours: &[Pair], theirs: &[Pair]) -> (usize, Vec<(i32, u16, u16)>) {
    fn unique(list: &[Pair]) -> HashMap<i32, u16> {
        let mut seen: HashMap<i32, Option<u16>> = HashMap::new();
        for &(e, n, _) in list {
            seen.entry(e).and_modify(|v| *v = None).or_insert(Some(n));
        }
        seen.into_iter().filter_map(|(e, n)| Some((e, n?))).collect()
    }
    let a = unique(ours);
    let b = unique(theirs);
    let mut common = 0;
    let mut bad = Vec::new();
    let mut keys: Vec<_> = a.keys().copied().collect();
    keys.sort_unstable();
    for e in keys {
        if let (Some(&x), Some(&y)) = (a.get(&e), b.get(&e)) {
            common += 1;
            if x != y {
                bad.push((e, x, y));
            }
        }
    }
    (common, bad)
}

/// Host death rule under the native session (owner decision 2026-10-23: a host death
/// does not reset the world): enemies alive on both sides take the LOWER HP of the
/// two on engage, so the host's death reload (full HP) cannot heal the client's
/// damaged enemies through type 5. Dead ones are left to the P3 dead list (it marks
/// them "no loot", so no kill is rewarded twice). Returns `(entity, new HP)` to write
/// here. Duplicate entity ids are left out.
///
/// Raw HP only means the same on both machines when both have the same max (scaling.rs
/// gives both the host's values, but one side can lag after a load or a rest: live
/// 2026-10-24, 1120353 at 558 on the host vs max 349 on the friend, §34). `our_max` /
/// `their_max` (entity -> max HP; the friend's from its MaxHp message, None from a 7.6
/// friend): an enemy is compared only when our max is known, our HP is not above it, the
/// friend's HP is not above it either, and (when the friend sent its max) both maxes are
/// the same within 1 %.
fn hp_adoptions(ours: &[Pair], our_max: &HashMap<i32, i32>, theirs: &[Pair], their_max: Option<&HashMap<i32, i32>>) -> Vec<(i32, i32)> {
    let out = hp_adoptions_raw(ours, theirs);
    out.into_iter()
        .filter(|&(e, other)| {
            let Some(&m) = our_max.get(&e).filter(|&&m| m > 0) else { return false };
            let hp = ours.iter().find(|p| p.0 == e).map_or(0, |p| p.2);
            if hp > m || other > m {
                return false;
            }
            their_max.is_none_or(|t| t.get(&e).is_some_and(|&tm| tm > 0 && (tm - m).abs() * 100 <= m))
        })
        .collect()
}

fn hp_adoptions_raw(ours: &[Pair], theirs: &[Pair]) -> Vec<(i32, i32)> {
    let counts = |l: &[Pair]| {
        let mut c: HashMap<i32, u32> = HashMap::new();
        for &(e, ..) in l {
            *c.entry(e).or_insert(0) += 1;
        }
        c
    };
    let (mine, other) = (counts(ours), counts(theirs));
    let peer: HashMap<i32, i32> = theirs.iter().map(|&(e, _, hp)| (e, hp)).collect();
    let mut out: Vec<(i32, i32)> = ours
        .iter()
        .filter(|(e, ..)| mine.get(e) == Some(&1) && other.get(e) == Some(&1))
        .filter_map(|&(e, _, hp)| {
            let other = *peer.get(&e)?;
            (hp > 0 && other > 0 && other < hp).then_some((e, other))
        })
        .collect();
    out.sort_unstable();
    out
}

/// Type 22 payload `{i32 n; u32 entries[n]}`: n if well formed (§4).
fn parse_22(d: &[u8]) -> Option<usize> {
    let n = i32::from_le_bytes(d.get(..4)?.try_into().ok()?);
    (0..=1023).contains(&n).then_some(n as usize).filter(|&n| d.len() == 4 * (n + 1))
}

// ---------------------------------------------------------------------------
// Game addresses (RVAs, all checked in IDA o1)
// ---------------------------------------------------------------------------

const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const GAME_MAN_RVA: usize = 0x3D5C0D0;
/// GameMan +0xBA6 = `0x832050()`; nonzero -> the client needs the `0xA4BA93` patch.
const GAME_MAN_BA6: usize = 0xBA6;
const WCM_SYNC_TABLE: usize = 0x630;
/// u16 netId of a chr (players too; `0xBF16A0`, the damage router reads it).
const CHR_NET_ID: usize = 0x78;
/// WorldChrMan chr sets (`0xA4DB20`): count +0x1C8, set pointers +0x1D0, 24 B apart;
/// a set is `{i32 n; ...; +8 entries 56 B apart, chr at +0}`.
const WCM_SET_COUNT: usize = 0x1C8;
const WCM_SETS: usize = 0x1D0;
const WCM_SET_STRIDE: usize = 24;
const SET_ENTRIES: usize = 8;
const SET_ENTRY: usize = 56;
const ENEMY_VTABLE_RVA: usize = 0x2A27F28;
/// Sync table `T`: timers (spec §4, logged once).
const T_TIMERS: usize = 0x398;
/// Sync table countdowns (IDA ns2 o1/o2): `0xA1D2D0` subtracts the frame time from both
/// every update; `0xA1DD00` (host) distributes 23 when `+0x39C <= 0` and reloads it from
/// `+0x398` (3 s), `0xA1CED0` (client) sends 22 when `+0x3A4 <= 0` and reloads it from
/// `+0x3A0` (5 s). Writing 0 makes the next update do it.
const T_DUE_23: usize = 0x39C;
const T_DUE_22: usize = 0x3A4;
/// WorldChrMan `+0x678`: u32 that `0xA509F0` (WorldChrManImp's update task, the only
/// caller of `0xA4E520` (22) and `0xA1D2D0` (23, 4, 5, 70, 71)) increments every update
/// (IDA ns2 o3). It stands still while the game's world is paused (live 7.1: the host
/// after its death reload, 45 s, sync-table timers frozen at [3.0, 2.98, 5.0, 4.98]).
const WCM_UPDATES: usize = 0x678;
/// The world update counts as running when `WCM_UPDATES` moved this recently.
const WORLD_STILL_AFTER: Duration = Duration::from_millis(500);
/// `0xA4DB20` chr loop: `forceLocal = members <= 1` for slot 37 of every chr. The client
/// patches it only after its first 23: until then nothing is owned there, and every
/// enemy would sit in NetAI without data (frozen / walking in place).
const CHR_LOOP_SITE: usize = 0xA4DC38;
/// `(T, u32 chr handle) -> record` (creates one if missing).
const FIND_RECORD_RVA: usize = 0xA1D7E0;
/// `(record, i32 n, u32* list)`: replaces the record's candidate list.
const SET_CANDIDATES_RVA: usize = 0xA1C6C0;
/// `(WCM, u16 netId) -> u8`: the enemy is owned here.
const OWNED_HERE_RVA: usize = 0xA4A650;

// --- §1 call-site patches -----------------------------------------------------

const TRUE5: &[u8] = &[0xB0, 0x01, 0x90, 0x90, 0x90];
const FALSE5: &[u8] = &[0x30, 0xC0, 0x90, 0x90, 0x90];
const CNT2: &[u8] = &[0xB8, 0x02, 0x00, 0x00, 0x00];
/// `mov ecx, 56 * 2` (+2 nop) and the 7-byte `sub` it replaces.
const ECX_2M: &[u8] = &[0xB9, 0x70, 0x00, 0x00, 0x00, 0x90, 0x90];
const NOP7: &[u8] = &[0x90; 7];

struct Site {
    rva: usize,
    orig: &'static [u8],
    host: Option<&'static [u8]>,
    client: Option<&'static [u8]>,
    name: &'static str,
}

const SITES: &[Site] = &[
    // §1a InSession (call 0x1072FE0) in the NetAI send / receive functions.
    Site { rva: 0xA1AF9C, orig: &[0xE8, 0x3F, 0x80, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession recv 70" },
    Site { rva: 0xA1B36D, orig: &[0xE8, 0x6E, 0x7C, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession send 70" },
    Site { rva: 0xA1BE41, orig: &[0xE8, 0x9A, 0x71, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession recv 4" },
    Site { rva: 0xA1C18B, orig: &[0xE8, 0x50, 0x6E, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession send 4" },
    Site { rva: 0xA1C8A3, orig: &[0xE8, 0x38, 0x67, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession recv 5" },
    Site { rva: 0xA1CA03, orig: &[0xE8, 0xD8, 0x65, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession send 5 changed" },
    Site { rva: 0xA1CCCA, orig: &[0xE8, 0x11, 0x63, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession send 5 full" },
    Site { rva: 0xA1D918, orig: &[0xE8, 0xC3, 0x56, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession recv 71" },
    Site { rva: 0xA1DB08, orig: &[0xE8, 0xD3, 0x54, 0x65, 0x00], host: Some(TRUE5), client: Some(TRUE5), name: "ns InSession send 71" },
    // §1b IsHost (call 0x1072F80) / IsClient (call 0x1072F00).
    Site { rva: 0xA1CF85, orig: &[0xE8, 0xF6, 0x5F, 0x65, 0x00], host: Some(TRUE5), client: Some(FALSE5), name: "ns IsHost 22" },
    Site { rva: 0xA1DDC6, orig: &[0xE8, 0xB5, 0x51, 0x65, 0x00], host: Some(TRUE5), client: Some(FALSE5), name: "ns IsHost 23" },
    Site { rva: 0xA1D42A, orig: &[0xE8, 0x51, 0x5B, 0x65, 0x00], host: Some(TRUE5), client: Some(FALSE5), name: "ns IsHost 5 full" },
    Site { rva: 0xA1D4B0, orig: &[0xE8, 0xCB, 0x5A, 0x65, 0x00], host: Some(TRUE5), client: Some(FALSE5), name: "ns IsHost 5 changed" },
    Site { rva: 0xA1D506, orig: &[0xE8, 0xF5, 0x59, 0x65, 0x00], host: Some(FALSE5), client: Some(TRUE5), name: "ns IsClient read 5" },
    Site { rva: 0xA4B8A4, orig: &[0xE8, 0xD7, 0x76, 0x62, 0x00], host: Some(TRUE5), client: Some(FALSE5), name: "ns IsHost HP table write" },
    Site { rva: 0xA4BA81, orig: &[0xE8, 0x7A, 0x74, 0x62, 0x00], host: Some(FALSE5), client: Some(TRUE5), name: "ns IsClient HP table read" },
    // §1c member count (call 0x8E3E60).
    Site { rva: 0xA4DC38, orig: &[0xE8, 0x23, 0x62, 0xE9, 0xFF], host: Some(CNT2), client: Some(CNT2), name: "ns members chr loop" },
    Site { rva: 0xA4A6CB, orig: &[0xE8, 0x90, 0x97, 0xE9, 0xFF], host: Some(CNT2), client: Some(CNT2), name: "ns members IsOwnedHere" },
    // §1c inline counts `(end - begin) / 56`: rcx := 56 * 2.
    Site { rva: 0xA1CF13, orig: &[0x49, 0x8B, 0x88, 0x68, 0x01, 0x00, 0x00], host: Some(ECX_2M), client: Some(ECX_2M), name: "ns count 22 a" },
    Site { rva: 0xA1CF24, orig: &[0x49, 0x2B, 0x88, 0x60, 0x01, 0x00, 0x00], host: Some(NOP7), client: Some(NOP7), name: "ns count 22 b" },
    Site { rva: 0xA1DD5E, orig: &[0x49, 0x8B, 0x88, 0x68, 0x01, 0x00, 0x00], host: Some(ECX_2M), client: Some(ECX_2M), name: "ns count 23 a" },
    Site { rva: 0xA1DD65, orig: &[0x49, 0x2B, 0x88, 0x60, 0x01, 0x00, 0x00], host: Some(NOP7), client: Some(NOP7), name: "ns count 23 b" },
    Site { rva: 0xA4E5BE, orig: &[0x48, 0x8B, 0x88, 0x68, 0x01, 0x00, 0x00], host: Some(ECX_2M), client: Some(ECX_2M), name: "ns count candidates a" },
    Site { rva: 0xA4E5C5, orig: &[0x48, 0x2B, 0x88, 0x60, 0x01, 0x00, 0x00], host: Some(NOP7), client: Some(NOP7), name: "ns count candidates b" },
];

/// §1b client: `test al, al` -> `xor al, al` at `0xA4BA93` when GameMan+0xBA6 != 0.
const HP_OFFLINE_SITE: usize = 0xA4BA93;
const HP_OFFLINE_ORIG: &[u8] = &[0x84, 0xC0];
const HP_OFFLINE_NEW: &[u8] = &[0x30, 0xC0];

/// Stage 4: `0x6BE270()` (member count > 0) guarding the type-45 ObjAct send in `0x6BC1B0`.
const OBJ_EVENT_SITE: usize = 0x6BC504;
const OBJ_EVENT_ORIG: &[u8] = &[0xE8, 0x67, 0x1D, 0x00, 0x00];

/// §2.2 `mov reg, [S]` (RIP-relative to `base+0x3D948E8`) -> `lea reg, [FAKE_S]`.
struct LeaSite {
    rva: usize,
    orig: [u8; 7],
    name: &'static str,
}

const LEA_SITES: [LeaSite; 4] = [
    LeaSite { rva: 0xA1AFB1, orig: [0x48, 0x8B, 0x35, 0x30, 0x99, 0x37, 0x03], name: "ns fake S recv 70" },
    LeaSite { rva: 0xA1AFF4, orig: [0x48, 0x8B, 0x35, 0xED, 0x98, 0x37, 0x03], name: "ns fake S recv 70 (assert path)" },
    LeaSite { rva: 0xA1BE56, orig: [0x48, 0x8B, 0x1D, 0x8B, 0x8A, 0x37, 0x03], name: "ns fake S recv 4" },
    LeaSite { rva: 0xA1BE91, orig: [0x48, 0x8B, 0x1D, 0x50, 0x8A, 0x37, 0x03], name: "ns fake S recv 4 (assert path)" },
];

/// §2.2 `call 0x8E4010` (member vector `S+0x158`) in the type-71 receive -> our stub.
const VEC_CALL_SITE: usize = 0xA1D936;
const VEC_CALL_ORIG: [u8; 5] = [0xE8, 0xD5, 0x66, 0xEC, 0xFF];

/// Host, N players: who gets a type 23 (IDA P0 §5.1a, `0xA1DD00`, re-read). Per record
/// the recipient chr is in rcx at `call 0xA29DD0` @`0xA1DFC1` (chr -> member), the record
/// in rcx at `call 0xA1D240` @`0xA1DFDD` (its only caller); `0x8E6FA0(.., 23, ..)`
/// @`0xA1E005` follows with nothing in between. Each call goes through a stub that keeps
/// rcx (`LAST23_CHR` / `LAST23_REC`); the to-member hook reads and clears them.
struct Wrap {
    rva: usize,
    orig: [u8; 5],
    target: usize,
    stub_off: usize,
    var_off: usize,
    name: &'static str,
}

const WRAPS_23: [Wrap; 2] = [
    Wrap {
        rva: 0xA1DFC1,
        orig: [0xE8, 0x0A, 0xBE, 0x00, 0x00],
        target: 0xA29DD0,
        stub_off: STUB23_CHR_OFF,
        var_off: LAST23_CHR_OFF,
        name: "ns type-23 recipient chr 0xA1DFC1",
    },
    Wrap {
        rva: 0xA1DFDD,
        orig: [0xE8, 0x5E, 0xF2, 0xFF, 0xFF],
        target: 0xA1D240,
        stub_off: STUB23_REC_OFF,
        var_off: LAST23_REC_OFF,
        name: "ns type-23 recipient record 0xA1DFDD",
    },
];

/// `mov [rip+var], rcx; jmp target` at `stub` (12 bytes).
fn wrap_stub(stub: usize, var: usize, target: usize) -> Option<Vec<u8>> {
    let mut v = vec![0x48, 0x89, 0x0D];
    v.extend_from_slice(&rel32(stub + 7, var)?);
    v.push(0xE9);
    v.extend_from_slice(&rel32(stub + 12, target)?);
    Some(v)
}

// Fake page layout (one 0x2000 allocation near the module).
const FAKE_S_OFF: usize = 0x000; // 0x170 bytes of a fake session object
const FAKE_MEMBERS_OFF: usize = 0x200; // 4 x 56 B empty members
const FAKE_MEMBERS_N: usize = 4;
const STUB_OFF: usize = 0x1000; // lea rax, [FAKE_S+0x158]; ret
/// Type-23 recipient wraps (IDA §5.1a): `mov [rip+LAST23_x], rcx; jmp <original target>`.
const STUB23_CHR_OFF: usize = 0x1040;
const STUB23_REC_OFF: usize = 0x1060;
/// The u64 slots the wraps write (the recipient chr / its ownership record).
const LAST23_CHR_OFF: usize = 0x1100;
const LAST23_REC_OFF: usize = 0x1108;
const S_MEMBERS_BEGIN: usize = 0x160;
const S_MEMBERS_END: usize = 0x168;
const S_MEMBER_VEC: usize = 0x158;

// --- §2.1 / §2.3 entry hooks -------------------------------------------------------

const H_BCAST: usize = 0;
const H_TO_HOST: usize = 1;
const H_TO_MEMBER: usize = 2;
const H_RECV_MEMBER: usize = 3;
const H_RECV_HOST: usize = 4;
/// "Any member" receivers 21, 28, 52, 18, 42.
const H_ANY: usize = 5;
const HOOKS: usize = 11;

const ANY_PROLOGUE: &[u8] = &[0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x89, 0x74, 0x24, 0x10, 0x57];

struct HookDef {
    rva: usize,
    prologue: &'static [u8],
    name: &'static str,
}

const HOOK_DEFS: [HookDef; HOOKS] = [
    HookDef { rva: 0x8E7210, prologue: &[0x48, 0x89, 0x5C, 0x24, 0x10], name: "ns broadcast 0x8E7210" },
    HookDef { rva: 0x8E7310, prologue: &[0x48, 0x89, 0x6C, 0x24, 0x10], name: "ns send to host 0x8E7310" },
    HookDef { rva: 0x8E6FA0, prologue: &[0x48, 0x8B, 0xC4, 0x4C, 0x89, 0x48, 0x20], name: "ns send to member 0x8E6FA0" },
    HookDef { rva: 0x8E6B50, prologue: &[0x48, 0x89, 0x5C, 0x24, 0x08], name: "ns recv from member 0x8E6B50" },
    HookDef { rva: 0x8E6D20, prologue: &[0x48, 0x89, 0x6C, 0x24, 0x10], name: "ns recv from host 0x8E6D20" },
    HookDef { rva: 0x8D77B0, prologue: ANY_PROLOGUE, name: "ns any member 21 0x8D77B0" },
    HookDef { rva: 0x8D7320, prologue: ANY_PROLOGUE, name: "ns any member 28 0x8D7320" },
    HookDef { rva: 0x8D7290, prologue: ANY_PROLOGUE, name: "ns any member 52 0x8D7290" },
    HookDef { rva: 0x8D7D70, prologue: ANY_PROLOGUE, name: "ns any member 18 0x8D7D70" },
    HookDef { rva: 0x8D8260, prologue: &[0x48, 0x89, 0x5C, 0x24, 0x08, 0x48, 0x89, 0x6C, 0x24, 0x10], name: "ns any member 42 0x8D8260" },
    HookDef { rva: 0x8D8750, prologue: ANY_PROLOGUE, name: "ns any member 45 0x8D8750" },
];

/// (type, receive cap) of the "any member" receivers, in HOOK_DEFS order from H_ANY.
const ANY_TYPES: [(u8, u32); 6] = [(T_DAMAGE, 0xD0), (T_HIT2, 0x80), (52, 0x68), (T_THROW, 0x2C), (42, 0), (T_OBJ_EVENT, 6)];
// ---------------------------------------------------------------------------
// State shared with the hooks (any thread: atomics and try_lock only)
// ---------------------------------------------------------------------------

static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
/// A check failed (bytes, memory): the mode stays off for this run.
static BROKEN: AtomicBool = AtomicBool::new(false);
static TRAMP: [AtomicUsize; HOOKS] = [const { AtomicUsize::new(0) }; HOOKS];
static FAKE_PAGE: AtomicUsize = AtomicUsize::new(0);
/// Bit per usable type-23 recipient wrap (`WRAPS_23`: stub written, call bytes as
/// expected); 0xFF = not checked yet. 0 = the host cannot tell its clients' 23s apart:
/// the native session is kept to 2 players.
static WRAP23: AtomicU8 = AtomicU8::new(0xFF);

/// Engaged at stage >= 2: whitelisted types go over our link.
static ROUTE: AtomicBool = AtomicBool::new(false);
/// Stage >= 2: the "any member" receivers serve our queues (whitelist per type).
static ANY_MEMBER: AtomicBool = AtomicBool::new(false);
static ROLE_HOST: AtomicBool = AtomicBool::new(false);
static WL_LO: AtomicU64 = AtomicU64::new(0);
static WL_HI: AtomicU64 = AtomicU64::new(0);
/// Our session slot (`PeerSet::me`: host 0), refreshed every tick.
static ME: AtomicU8 = AtomicU8::new(0);
/// Per link (index = the peer's slot; a client's one link is 0 = the host): our
/// handshake generation for it, tagged on everything we send on that link while engaged.
static GEN_OUT: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// Per link: `1 << 32 | that peer's generation` while engaged with it (only that
/// generation is accepted from it), else 0.
static ACCEPT: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// Bit = slot of a link we are engaged with.
static ENGAGED_LINKS: AtomicU8 = AtomicU8::new(0);
/// Client: the client slots engaged with our host (its latest Engaged message), bit per slot.
static PEER_ENGAGED: AtomicU8 = AtomicU8::new(0);
/// Player netIds by slot (0xFFFF unknown): ours at our slot, each native remote at its
/// peer's slot. `chr+0x78` (what the damage router writes) and the `0xA4CE60` form when
/// it differs (rewritten too, on send).
static PNET: [AtomicU16; SLOTS] = [const { AtomicU16::new(NO_NET_ID) }; SLOTS];
static PNET_ALT: [AtomicU16; SLOTS] = [const { AtomicU16::new(NO_NET_ID) }; SLOTS];
/// Each peer's native remote handle `u32 chr+8` (= the key of its ownership record).
static PEER_HANDLE: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
/// Hex dumps of the first packets of the less known types (evidence for 21/28/52/18/42/45).
static EVIDENCE: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
const EVIDENCE_PER_TYPE: u32 = 8;
/// Type-23 packets handed to the game (client: the 5 s rule).
static SERVED_23: AtomicU32 = AtomicU32::new(0);
/// Enemy sync packets (4 / 5 / 70 / 71) of an engaged link received: the sender's sync
/// table runs (client: the 5 s rule starts with the first one).
static SYNC_IN: AtomicU32 = AtomicU32::new(0);
/// Host: type-23 sends whose recipient was not known, given to the one engaged peer
/// (the 2-player rule).
static SINGLE_23: AtomicU32 = AtomicU32::new(0);

// Per type: tx = to the link, rx = from the link, srv = handed to the game,
// drop = queue overflow / busy lock / stale, big = over the size limit,
// relay = host: a client's broadcast re-queued for the others, unmapped = a to-member
// send whose recipient is not known (dropped, never broadcast).
static TX: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static RX: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static SRV: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static DROP: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static BIG: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static RELAY: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
static UNMAPPED: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
/// Served packets dropped because a player netId in them names a slot without a player here.
static PNET_DROP: [AtomicU32; TYPES] = [const { AtomicU32::new(0) }; TYPES];
/// Types the game asked each hook for (send hooks: sent, recv hooks: asked), pass-through included.
static SEEN: [[AtomicU32; TYPES]; HOOKS] = [const { [const { AtomicU32::new(0) }; TYPES] }; HOOKS];
static CALLS: [AtomicU32; HOOKS] = [const { AtomicU32::new(0) }; HOOKS];
static ROUTED: [AtomicU32; HOOKS] = [const { AtomicU32::new(0) }; HOOKS];

const MAX_UNREL: usize = 256;
const MAX_REL_BYTES: usize = 1 << 20;

/// One game packet to send: made by our game (`origin` = us) or relayed (host).
struct Out {
    t: u8,
    origin: u8,
    dst: Dst,
    /// The game's bytes, player netIds in wire form.
    data: Vec<u8>,
}

struct Outbox {
    unrel: VecDeque<Out>,
    rel: VecDeque<Out>,
    rel_bytes: usize,
    /// Handshake lane messages (key 0) per link slot.
    ctrl: VecDeque<(u8, Vec<u8>)>,
}

static OUTBOX: Mutex<Outbox> =
    Mutex::new(Outbox { unrel: VecDeque::new(), rel: VecDeque::new(), rel_bytes: 0, ctrl: VecDeque::new() });
/// Received game packets per type (index = type), filled lazily: (origin slot, wire
/// bytes). One merged inbox for the one fake member (IDA §5.1e).
static INBOX: Mutex<Vec<VecDeque<(u8, Vec<u8>)>>> = Mutex::new(Vec::new());

fn idx(t: u32) -> usize {
    (t as usize).min(TYPES - 1)
}

fn whitelisted(t: u32) -> bool {
    match t {
        0..=63 => WL_LO.load(Ordering::Relaxed) >> t & 1 != 0,
        64..=127 => WL_HI.load(Ordering::Relaxed) >> (t - 64) & 1 != 0,
        _ => false,
    }
}

fn set_whitelist(m: u128) {
    WL_LO.store(m as u64, Ordering::Relaxed);
    WL_HI.store((m >> 64) as u64, Ordering::Relaxed);
}

fn bump(a: &[AtomicU32; TYPES], t: u32) {
    a[idx(t)].fetch_add(1, Ordering::Relaxed);
}

/// We are engaged with `link` and `generation` is its current one.
fn accepts(link: u8, generation: u32) -> bool {
    (link as usize) < SLOTS && ACCEPT[link as usize].load(Ordering::Acquire) == 1 << 32 | generation as u64
}

fn engaged_links() -> u8 {
    ENGAGED_LINKS.load(Ordering::Acquire)
}

fn mask_slots(mask: u8) -> Vec<u8> {
    (0..SLOTS as u8).filter(|&s| mask >> s & 1 != 0).collect()
}

fn pnet_map(alt: bool) -> PnetMap {
    let a = if alt { &PNET_ALT } else { &PNET };
    std::array::from_fn(|s| a[s].load(Ordering::Relaxed))
}

/// The peer whose native remote has ownership-record handle `h`.
fn slot_of_handle(h: u32) -> Option<u8> {
    if h == 0 || h == u32::MAX {
        return None;
    }
    PEER_HANDLE.iter().position(|a| a.load(Ordering::Relaxed) == h).map(|s| s as u8)
}

/// How many machines feed the merged inbox (receive queue depth scales with it).
fn origins() -> usize {
    let mut n = engaged_links().count_ones() as usize;
    if !ROLE_HOST.load(Ordering::Relaxed) {
        // The host, plus the other clients it relays (its Engaged set names us too).
        n += (PEER_ENGAGED.load(Ordering::Relaxed).count_ones() as usize).saturating_sub(1);
    }
    n.max(1)
}

fn clear_queues() {
    if let Ok(mut o) = OUTBOX.lock() {
        o.unrel.clear();
        o.rel.clear();
        o.rel_bytes = 0;
    }
    if let Ok(mut g) = INBOX.lock() {
        for q in g.iter_mut() {
            q.clear();
        }
    }
}

/// A peer that left alone: what it sent and the game has not taken yet goes.
fn drop_inbound_from(origin: u8) {
    if let Ok(mut g) = INBOX.lock() {
        for q in g.iter_mut() {
            q.retain(|(o, _)| *o != origin);
        }
    }
}

/// A handshake lane message for one link.
fn queue_ctrl(to: u8, m: &Msg) {
    if let Ok(mut o) = OUTBOX.lock() {
        o.ctrl.push_back((to, encode(m)));
    }
}

/// One message per engaged link, each with our generation for that link.
fn queue_ctrl_engaged(make: impl Fn(u32) -> Msg) {
    for s in mask_slots(engaged_links()) {
        queue_ctrl(s, &make(GEN_OUT[s as usize].load(Ordering::Relaxed)));
    }
}

/// Into the outbox (reliable when the type is, or when it does not fit a PlayerRaw).
fn put(o: &mut Outbox, out: Out, counter: &[AtomicU32; TYPES]) {
    let t = out.t;
    if reliable(t) || out.data.len() + RAW_HEADER > MAX_PLAYER_RAW {
        while o.rel_bytes + out.data.len() > MAX_REL_BYTES {
            let Some(old) = o.rel.pop_front() else { break };
            o.rel_bytes -= old.data.len();
            bump(&DROP, old.t as u32);
        }
        o.rel_bytes += out.data.len();
        o.rel.push_back(out);
    } else {
        if o.unrel.len() >= MAX_UNREL
            && let Some(old) = o.unrel.pop_front()
        {
            bump(&DROP, old.t as u32);
        }
        o.unrel.push_back(out);
    }
    bump(counter, t as u32);
}

/// A game packet of ours (send hooks, game threads: never blocks).
fn queue_out(t: u8, dst: Dst, data: Vec<u8>) {
    let Ok(mut o) = OUTBOX.try_lock() else {
        bump(&DROP, t as u32);
        return;
    };
    put(&mut o, Out { t, origin: ME.load(Ordering::Relaxed), dst, data }, &TX);
}

/// Host: a client's broadcast goes on to the other engaged clients, origin and wire
/// bytes unchanged (our frame).
fn relay(t: u8, origin: u8, d: &[u8]) {
    let Ok(mut o) = OUTBOX.lock() else { return };
    put(&mut o, Out { t, origin, dst: Dst::All, data: d.to_vec() }, &RELAY);
}

/// Pure: does a packet of type `t` from `origin` count as "the host's sync runs" (`SYNC_IN`,
/// the client's host liveness and first-23 rule)? A client counts the host's packets only:
/// another client's relayed 4 / 5 / 70 / 71 say nothing about the host's world update.
fn counts_as_sync_in(host: bool, t: u8, origin: u8) -> bool {
    matches!(t, T_MOVE | T_HP | T_BEHAVIOR | T_ACTION) && (host || origin == 0)
}

/// A game packet from `origin`'s machine (net.rs, our frame): wire bytes, rewritten
/// when the game takes it.
fn push_in(origin: u8, t: u8, data: Vec<u8>) {
    bump(&RX, t as u32);
    if !whitelisted(t as u32) {
        bump(&DROP, t as u32);
        return;
    }
    if counts_as_sync_in(ROLE_HOST.load(Ordering::Relaxed), t, origin) {
        SYNC_IN.fetch_add(1, Ordering::Relaxed);
    }
    let cap = queue_cap(t) * origins();
    let Ok(mut g) = INBOX.lock() else { return };
    if g.len() < TYPES {
        g.resize_with(TYPES, VecDeque::new);
    }
    let q = &mut g[t as usize];
    if q.len() >= cap {
        q.pop_front();
        bump(&DROP, t as u32);
    }
    q.push_back((origin, data));
}

/// One queued packet of type `t` into the game's buffer: its length, 0 if none
/// (the callers loop `while (recv())`). Player netIds are rewritten here, with the
/// mapping of this moment (the wire form names slots, so the origin does not matter).
fn serve(t: u32, buf: *mut u8, cap: u32) -> i32 {
    if (buf as usize) < 0x10000 || t as usize >= TYPES {
        return 0;
    }
    let Ok(mut g) = INBOX.try_lock() else { return 0 };
    let Some(q) = g.get_mut(t as usize) else { return 0 };
    while let Some((origin, mut p)) = q.pop_front() {
        if p.is_empty() || p.len() > cap as usize {
            bump(&BIG, t);
            continue;
        }
        if !pnet_fields(t as u8).is_empty() {
            let map = pnet_map(false);
            // 3+ players: a packet naming a player we have no remote for (a relayed peer's
            // player whose remote is not here) would hand the game 0xFFFF; dropped and counted.
            if origins() > 1 && pnet_unmapped(&p, t as u8, &map) {
                bump(&PNET_DROP, t);
                continue;
            }
            pnet_from_wire(&mut p, t as u8, &map);
        }
        evidence("rx", t as u8, &p);
        if t == T_THROW as u32 {
            // Both players' deathblows on one enemy within a second (live 2026-10-24,
            // 1120300): each received throw re-placed the defender into the remote's throw
            // (whose hit does nothing here) and cut our own player's running one off, so no
            // copy lost a life. Our player's throw on it keeps running; the friend's own
            // runs on its machine; livesync.rs merges the lives (§34). Since §39 the lock
            // decides (first come, every machine agrees; `on_game` already dropped the
            // losers); the old one-sided drop is the fallback only with the lock off.
            let collide_drop = !DEATHBLOW_LOCK && THROW_COLLIDE_DROP.load(Ordering::Relaxed);
            let ours = THROWS.try_lock().ok().filter(|_| collide_drop).and_then(|g| {
                throw_collides(&g[..], &p, &slot_pnets(origin), crate::game::now_ms())
            });
            if let Some(age) = ours {
                THROW_DROPS.fetch_add(1, Ordering::Relaxed);
                if THROW_DROP_LOGS.fetch_add(1, Ordering::Relaxed) < 30 {
                    log::line(&format!(
                        "nsession: the friend's throw {} on enemy netId {:#x} (peer {origin}) not applied here: our own player's throw on it started {age} ms ago (two deathblows at once cancel each other)",
                        get_u16(&p, THROW_ID).unwrap_or(0),
                        get_u16(&p, THROW_DEFENDER).unwrap_or(NO_NET_ID)
                    ));
                }
                continue;
            }
            note_throw(&p, false, origin);
        }
        // SAFETY: the game passes a buffer of `cap` bytes (checked above).
        unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), buf, p.len()) };
        bump(&SRV, t);
        if t == T_OWNERS as u32 {
            SERVED_23.fetch_add(1, Ordering::Relaxed);
        }
        return p.len() as i32;
    }
    0
}

// --- Stealth ownership claims (IMPLEMENTATION-NOTES §27) ---------------------------
//
// Type 22 is the client's ownership candidate list `{i32 n; u32 (netId | prio << 16)[n]}`
// (`0xA1CED0` builds and sends it from `T+0x380..0x388`). The host keeps one list per
// player (`0xA1D7E0` / `0xA1C6C0`) and settles each netId between every pair of players
// in `0xA1C590` [VAR, N-generic: IDA §5.1c]: a player keeps a netId when the other
// player's prio for it is 0, or lower, or equal with fewer candidates in total; the
// loser's entry is removed (`0xA1B700`). Owner lists (23) are built from what is left
// (`0xA1DD00`). Higher prio wins. So the game's own way to ask for an enemy is a high
// prio in 22: a client gives the enemies it sneaks up on prio 0xFFFF; the host drops its
// clients' entries for enemies the host sneaks up on (unless they claim them too).
// enemies.rs decides the claims.

/// netIds we claim for a stealth / plunging deathblow (enemies.rs `stealth_claims`).
static CLAIMS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
/// Claims a client gave up because the host owns the enemy too (`double_claim_drop`).
static DROPPED_CLAIMS: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static CLAIM_PACKETS: AtomicU32 = AtomicU32::new(0);
static CLAIM_ENTRIES: AtomicU32 = AtomicU32::new(0);
static STRIPPED: AtomicU32 = AtomicU32::new(0);
pub const CLAIM_PRIO: u32 = 0xFFFF;

pub fn set_stealth_claims(ids: Vec<u16>) {
    if let Ok(mut g) = CLAIMS.lock() {
        if ids.iter().any(|n| !g.contains(n)) {
            CLAIMS_GREW.store(true, Ordering::Relaxed);
        }
        *g = ids;
    }
}

/// A claim was added (stealth approach or our player's throw): the client sends its
/// candidates on the next update, the host settles them again at once (instead of the
/// game's 5 s / 3 s timers; live 2026-10-24 a host claim took up to 3 s, §34).
static CLAIMS_GREW: AtomicBool = AtomicBool::new(false);

// v7.7 behaviours behind ini keys (live 2026-10-24 bisect: fps drops / posture not
// reset after a deathblow since v7.7). Default off = the v7.6 behaviour.
/// `[NETWORK] ns_force_distribution`: a claim / contest change forces the game's 22 / 23
/// on the next update and re-feeds the friend's last 22 (v7.7, §34). Off: the game's own
/// 3 s / 5 s timers (v7.6). On: at most once per `FORCE_MIN_GAP`.
pub static FORCE_DISTRIBUTION: AtomicBool = AtomicBool::new(false);
/// `[NETWORK] ns_throw_claims`: our player's throw claims its defender for
/// `THROW_CLAIM_MS` (ownership moves to us mid-deathblow, v7.7). Off: v7.6.
pub static THROW_CLAIMS_ON: AtomicBool = AtomicBool::new(false);
/// `[NETWORK] ns_throw_collide_drop`: the friend's throw on an enemy our player is
/// deathblowing is not applied here (v7.7). Off: every friend's throw is applied (v7.6).
pub static THROW_COLLIDE_DROP: AtomicBool = AtomicBool::new(false);
const FORCE_MIN_GAP_MS: u64 = 1000;
static FORCED_AT_MS: AtomicU64 = AtomicU64::new(0);
static FORCES_SKIPPED: AtomicU32 = AtomicU32::new(0);

/// `make_due` for the v7.7 forced distributions: only with `ns_force_distribution = 1`,
/// at most once per second.
fn force_due(off: usize) -> bool {
    if !FORCE_DISTRIBUTION.load(Ordering::Relaxed) {
        return false;
    }
    let now = crate::game::now_ms();
    let last = FORCED_AT_MS.load(Ordering::Relaxed);
    if last != 0 && now.saturating_sub(last) < FORCE_MIN_GAP_MS {
        FORCES_SKIPPED.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    FORCED_AT_MS.store(now, Ordering::Relaxed);
    make_due(off)
}
static CONTESTS_WON: AtomicU32 = AtomicU32::new(0);
static CONTESTS_LOST: AtomicU32 = AtomicU32::new(0);
static THROW_CLAIMS: AtomicU32 = AtomicU32::new(0);

/// Everything we claim now: the stealth claims (enemies.rs) and the enemies our own
/// player started a throw on in the last `THROW_CLAIM_MS` (the deathblow runs on our AI).
fn claims_now() -> Vec<u16> {
    let dropped = DROPPED_CLAIMS.try_lock().map(|d| d.clone()).unwrap_or_default();
    let mut v = claims_raw();
    v.retain(|n| !dropped.contains(n));
    v
}

/// Every claim we would make, the ones dropped for a double claim included.
fn claims_raw() -> Vec<u16> {
    let mut v = CLAIMS.try_lock().map(|c| c.clone()).unwrap_or_default();
    if !THROW_CLAIMS_ON.load(Ordering::Relaxed) {
        return v;
    }
    if let Ok(g) = THROWS.try_lock() {
        for n in our_throw_targets(&g[..], crate::game::now_ms(), THROW_CLAIM_MS) {
            if enemy_net_id(n) && !v.contains(&n) {
                v.push(n);
            }
        }
    }
    v
}

/// The netIds the friend claims (prio `CLAIM_PRIO`) in its candidate list.
fn claimed_in_22(d: &[u8]) -> Vec<u16> {
    let mut v: Vec<u16> =
        list_22(d).unwrap_or_default().into_iter().filter(|x| x >> 16 == CLAIM_PRIO).map(|x| (x & 0xFFFF) as u16).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Host: both machines claim one enemy. Who runs it: a player throw (deathblow) decides
/// first (the earlier one, the host on the same moment); else the nearer player
/// (horizontal metres to the enemy) with `CONTEST_HYST_M` in favour of the current
/// holder (`held` = the host won it last time). Live 2026-10-24: the friend's prio-0xFFFF
/// claim beat every host claim, so the friend owned the miniboss 1120300 for the whole
/// fight and the host deathblowed a NetAI copy.
fn host_wins_contest(ours_age: Option<u64>, theirs_age: Option<u64>, d_host: Option<f32>, d_friend: Option<f32>, held: bool) -> bool {
    match (ours_age, theirs_age) {
        (Some(a), Some(b)) => return a >= b,
        (Some(_), None) => return true,
        (None, Some(_)) => return false,
        (None, None) => {}
    }
    match (d_host, d_friend) {
        (Some(h), Some(f)) => {
            if held {
                h <= f + CONTEST_HYST_M
            } else {
                h + CONTEST_HYST_M < f
            }
        }
        (Some(_), None) => true,
        _ => held,
    }
}

const CONTEST_HYST_M: f32 = 1.5;

/// The native session owns the enemies (engaged, stage >= 2): claims make sense.
pub fn claims_wanted() -> bool {
    OWNS_ENEMIES.load(Ordering::Relaxed)
}

fn list_22(d: &[u8]) -> Option<Vec<u32>> {
    parse_22(d)?;
    Some(d[4..].chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect())
}

fn encode_22(list: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 * list.len());
    out.extend_from_slice(&(list.len() as i32).to_le_bytes());
    for e in list {
        out.extend_from_slice(&e.to_le_bytes());
    }
    out
}

/// Client: our claims get prio 0xFFFF in our candidate list (appended when missing).
/// Returns the new payload and how many entries carry a claim; None = malformed.
fn claim_22(d: &[u8], claims: &[u16]) -> Option<(Vec<u8>, usize)> {
    let mut list = list_22(d)?;
    let mut n = 0;
    for &id in claims {
        let e = id as u32 | CLAIM_PRIO << 16;
        match list.iter().position(|x| (*x & 0xFFFF) as u16 == id) {
            Some(i) => {
                list[i] = e;
                n += 1;
            }
            None if list.len() < 1023 => {
                list.push(e);
                n += 1;
            }
            None => {}
        }
    }
    Some((encode_22(&list), n))
}

/// Host: a client's entries for enemies in `claims` (ours, or contested ones another
/// machine won) are removed, so the netId stays with its winner, unless that client
/// claims it as well (prio 0xFFFF) and `won` (the contested ones it lost) does not name it
/// (`host_candidates` passes the same set twice: the client keeps nothing of it).
/// Returns the new payload and how many entries were removed; None = malformed.
fn strip_22(d: &[u8], claims: &[u16], won: &[u16]) -> Option<(Vec<u8>, usize)> {
    let list = list_22(d)?;
    let kept: Vec<u32> = list
        .iter()
        .copied()
        .filter(|&x| {
            let n = (x & 0xFFFF) as u16;
            !claims.contains(&n) || (x >> 16 == CLAIM_PRIO && !won.contains(&n))
        })
        .collect();
    let n = list.len() - kept.len();
    Some((encode_22(&kept), n))
}

/// Copies an outgoing game packet into our outbox. True = handled (the game's
/// own send is not run: with an empty member vector it would send nothing).
fn route_send(t: u32, data: *const u8, size: u32, dst: Dst) -> bool {
    let size = size as usize;
    if size == 0 || size > MAX_GAME_PACKET {
        bump(&BIG, t);
        return true;
    }
    let mut v = vec![0u8; size];
    if !read_bytes(data as usize, &mut v) {
        bump(&DROP, t);
        return true;
    }
    // Client: stealth / throw claims ride in our ownership candidates (the game's own
    // request path).
    let c = if t == T_CAND as u32 && !ROLE_HOST.load(Ordering::Relaxed) { claims_now() } else { Vec::new() };
    if !c.is_empty()
        && let Some((claimed, n)) = claim_22(&v, &c)
    {
        v = claimed;
        CLAIM_PACKETS.fetch_add(1, Ordering::Relaxed);
        CLAIM_ENTRIES.store(n as u32, Ordering::Relaxed);
    }
    if !pnet_fields(t as u8).is_empty() {
        // Both forms of a player netId: `chr+0x78` (what the damage router writes) and `0xA4CE60`.
        pnet_to_wire(&mut v, t as u8, &pnet_map(false));
        pnet_to_wire(&mut v, t as u8, &pnet_map(true));
    }
    evidence("tx", t as u8, &v);
    if t == T_THROW as u32 {
        note_throw(&v, true, 0);
        dblock::on_sent(&v);
    }
    queue_out(t as u8, dst, v);
    true
}

/// The first few packets of the [I] types, as hex (what is really in 21 / 28 / 52 / 18 /
/// 42 / 45 decides whether they can replace our own reports for good).
fn evidence(dir: &str, t: u8, d: &[u8]) {
    if !matches!(t, T_DAMAGE | T_HIT2 | 52 | 18 | 42 | T_OBJ_EVENT) {
        return;
    }
    if EVIDENCE[t as usize].fetch_add(1, Ordering::Relaxed) >= EVIDENCE_PER_TYPE * 2 {
        return;
    }
    let hex: Vec<String> = d.iter().take(64).map(|b| format!("{b:02x}")).collect();
    let map: Vec<String> = pnet_map(false).iter().map(|n| format!("{n:#x}")).collect();
    log::line(&format!(
        "nsession: evidence {dir} type {t} ({} B, us slot {}, player netIds by slot [{}]): {}",
        d.len(),
        ME.load(Ordering::Relaxed),
        map.join(" "),
        hex.join(" ")
    ));
}

fn fake_member_index(m: usize) -> Option<usize> {
    let page = FAKE_PAGE.load(Ordering::Acquire);
    if page == 0 {
        return None;
    }
    let base = page + FAKE_MEMBERS_OFF;
    let off = m.checked_sub(base)?;
    (off % SET_ENTRY == 0 && off / SET_ENTRY < FAKE_MEMBERS_N).then_some(off / SET_ENTRY)
}

/// Which link a to-member send with no real member goes to (pure). `by_chr` / `by_rec`:
/// the recipient of a type 23 from the wraps. A known recipient must be engaged; an
/// unknown one is the only engaged peer (the 2-player rule), else nobody (never a
/// broadcast: a 23 for the wrong client would hand it someone else's enemies). Wraps that disagree: nobody.
fn resolve_member(is23: bool, by_chr: Option<u8>, by_rec: Option<u8>, engaged: u8) -> Option<u8> {
    // The two wraps name different players: one of them is stale, so neither is trusted.
    if is23 && let (Some(a), Some(b)) = (by_chr, by_rec) && a != b {
        return None;
    }
    if is23 && let Some(p) = by_chr.or(by_rec) {
        return ((p as usize) < SLOTS && engaged >> p & 1 != 0).then_some(p);
    }
    (engaged.count_ones() == 1).then(|| engaged.trailing_zeros() as u8)
}

/// The u64 a recipient wrap writes, on the fake page.
fn last23_var(off: usize) -> Option<&'static AtomicUsize> {
    let page = FAKE_PAGE.load(Ordering::Acquire);
    // SAFETY: the page is ours (leaked, RWX, page aligned), the offset 8-aligned.
    (page != 0).then(|| unsafe { &*((page + off) as *const AtomicUsize) })
}

/// The recipient of the type 23 being sent now (by chr, by record), cleared after reading.
fn take_last23() -> (Option<u8>, Option<u8>) {
    let chr = last23_var(LAST23_CHR_OFF).map_or(0, |v| v.swap(0, Ordering::AcqRel));
    let rec = last23_var(LAST23_REC_OFF).map_or(0, |v| v.swap(0, Ordering::AcqRel));
    let by_chr = crate::native::peer_of_chr(chr).map(PeerId::slot);
    let by_rec = if rec >= 0x10000 { read::<u32>(rec).and_then(slot_of_handle) } else { None };
    (by_chr, by_rec)
}

/// Destination of a to-member send with our fake member (it stands for every peer) or
/// an empty member (`0xA29DD0`'s result: host 23 -> the wraps' recipient).
fn member_dst(t: u32, fake: bool) -> Option<Dst> {
    if fake {
        return Some(Dst::All);
    }
    if !ROLE_HOST.load(Ordering::Relaxed) {
        return Some(Dst::Host);
    }
    let is23 = t == T_OWNERS as u32;
    let (by_chr, by_rec) = if is23 { take_last23() } else { (None, None) };
    let p = resolve_member(is23, by_chr, by_rec, engaged_links())?;
    if is23 && by_chr.or(by_rec).is_none() {
        SINGLE_23.fetch_add(1, Ordering::Relaxed);
    }
    Some(Dst::Peer(p))
}

// --- Detours (game threads; never panic, pass-through unless routing) ---------------

type SendFn = extern "C" fn(usize, u32, *const u8, u32) -> u8;
type SendMemberFn = extern "C" fn(usize, usize, u32, *const u8, u32) -> u8;
type RecvMemberFn = extern "C" fn(usize, usize, u32, *mut u8, u32) -> i32;
type RecvHostFn = extern "C" fn(usize, u32, *mut u8, u32) -> i32;
type AnyFn = extern "C" fn(usize, *mut u8) -> u8;
type AnyCapFn = extern "C" fn(usize, *mut u8, u32) -> u8;

/// The original of hook `h`. `install_detour` writes the jump before it returns the
/// trampoline, so a game thread can arrive here a moment before we stored it: wait.
fn tramp(h: usize) -> usize {
    loop {
        let t = TRAMP[h].load(Ordering::Acquire);
        if t != 0 {
            return t;
        }
        std::hint::spin_loop();
    }
}

fn seen(h: usize, t: u32) {
    CALLS[h].fetch_add(1, Ordering::Relaxed);
    SEEN[h][idx(t)].fetch_add(1, Ordering::Relaxed);
}

fn routed(h: usize) {
    ROUTED[h].fetch_add(1, Ordering::Relaxed);
}

/// `0x8E7210(mgr, type, data, size) -> u8`: broadcast to every member.
extern "C" fn bcast_detour(mgr: usize, t: u32, data: *const u8, size: u32) -> u8 {
    seen(H_BCAST, t);
    if ROUTE.load(Ordering::Acquire) && whitelisted(t) {
        routed(H_BCAST);
        let _ = std::panic::catch_unwind(|| route_send(t, data, size, Dst::All));
        return 1;
    }
    let f: SendFn = unsafe { std::mem::transmute(tramp(H_BCAST)) };
    f(mgr, t, data, size)
}

/// `0x8E7310(mgr, type, data, size) -> u8`: send to the host (client side).
extern "C" fn to_host_detour(mgr: usize, t: u32, data: *const u8, size: u32) -> u8 {
    seen(H_TO_HOST, t);
    if ROUTE.load(Ordering::Acquire) && !ROLE_HOST.load(Ordering::Relaxed) && whitelisted(t) {
        routed(H_TO_HOST);
        let _ = std::panic::catch_unwind(|| route_send(t, data, size, Dst::Host));
        return 1;
    }
    let f: SendFn = unsafe { std::mem::transmute(tramp(H_TO_HOST)) };
    f(mgr, t, data, size)
}

/// `0x8E6FA0(mgr, member, type, data, size) -> u8`: one of our fake members (= every
/// peer), or an empty member (`0xA29DD0`'s result for type 23: the recipient the wraps
/// saw, `member_dst`).
extern "C" fn to_member_detour(mgr: usize, m: usize, t: u32, data: *const u8, size: u32) -> u8 {
    seen(H_TO_MEMBER, t);
    if ROUTE.load(Ordering::Acquire) && whitelisted(t) {
        let fake = fake_member_index(m).is_some();
        if fake || (m >= 0x10000 && read::<u64>(m) == Some(0)) {
            routed(H_TO_MEMBER);
            let _ = std::panic::catch_unwind(|| match member_dst(t, fake) {
                Some(d) => {
                    route_send(t, data, size, d);
                }
                None => bump(&UNMAPPED, t),
            });
            return 1;
        }
    }
    let f: SendMemberFn = unsafe { std::mem::transmute(tramp(H_TO_MEMBER)) };
    f(mgr, m, t, data, size)
}

/// `0x8E6B50(mgr, member, type, buf, cap) -> i32`: our fake member = every peer (merged inbox).
extern "C" fn recv_member_detour(mgr: usize, m: usize, t: u32, buf: *mut u8, cap: u32) -> i32 {
    seen(H_RECV_MEMBER, t);
    if ROUTE.load(Ordering::Acquire) && whitelisted(t) && fake_member_index(m).is_some() {
        routed(H_RECV_MEMBER);
        return std::panic::catch_unwind(|| serve(t, buf, cap)).unwrap_or(0);
    }
    let f: RecvMemberFn = unsafe { std::mem::transmute(tramp(H_RECV_MEMBER)) };
    f(mgr, m, t, buf, cap)
}

/// `0x8E6D20(mgr, type, buf, cap) -> i32`: from the host (client side).
extern "C" fn recv_host_detour(mgr: usize, t: u32, buf: *mut u8, cap: u32) -> i32 {
    seen(H_RECV_HOST, t);
    if ROUTE.load(Ordering::Acquire) && !ROLE_HOST.load(Ordering::Relaxed) && whitelisted(t) {
        routed(H_RECV_HOST);
        return std::panic::catch_unwind(|| serve(t, buf, cap)).unwrap_or(0);
    }
    let f: RecvHostFn = unsafe { std::mem::transmute(tramp(H_RECV_HOST)) };
    f(mgr, t, buf, cap)
}

/// The "any member" receivers `u8 (a1, buf[, cap])`: 1 if a packet was copied.
fn any_entry(k: usize, a1: usize, buf: *mut u8, cap_arg: u32) -> u8 {
    let (t, cap) = ANY_TYPES[k];
    let h = H_ANY + k;
    seen(h, t as u32);
    if ROUTE.load(Ordering::Acquire) && ANY_MEMBER.load(Ordering::Relaxed) && whitelisted(t as u32) {
        routed(h);
        let cap = if cap == 0 { cap_arg } else { cap };
        return std::panic::catch_unwind(|| (serve(t as u32, buf, cap) > 0) as u8).unwrap_or(0);
    }
    let tr = tramp(h);
    if ANY_TYPES[k].1 == 0 {
        let f: AnyCapFn = unsafe { std::mem::transmute(tr) };
        f(a1, buf, cap_arg)
    } else {
        let f: AnyFn = unsafe { std::mem::transmute(tr) };
        f(a1, buf)
    }
}

extern "C" fn any21_detour(a1: usize, buf: *mut u8) -> u8 {
    any_entry(0, a1, buf, 0)
}
extern "C" fn any28_detour(a1: usize, buf: *mut u8) -> u8 {
    any_entry(1, a1, buf, 0)
}
extern "C" fn any52_detour(a1: usize, buf: *mut u8) -> u8 {
    any_entry(2, a1, buf, 0)
}
extern "C" fn any18_detour(a1: usize, buf: *mut u8) -> u8 {
    any_entry(3, a1, buf, 0)
}
extern "C" fn any42_detour(a1: usize, buf: *mut u8, cap: u32) -> u8 {
    any_entry(4, a1, buf, cap)
}
extern "C" fn any45_detour(a1: usize, buf: *mut u8) -> u8 {
    any_entry(5, a1, buf, 0)
}

fn detour_fns() -> [usize; HOOKS] {
    [
        bcast_detour as *const () as usize,
        to_host_detour as *const () as usize,
        to_member_detour as *const () as usize,
        recv_member_detour as *const () as usize,
        recv_host_detour as *const () as usize,
        any21_detour as *const () as usize,
        any28_detour as *const () as usize,
        any52_detour as *const () as usize,
        any18_detour as *const () as usize,
        any42_detour as *const () as usize,
        any45_detour as *const () as usize,
    ]
}

// ---------------------------------------------------------------------------
// Install / patch (game thread, FrameBegin)
// ---------------------------------------------------------------------------

fn bytes_at(rva: usize, n: usize) -> Option<Vec<u8>> {
    let mut v = vec![0u8; n];
    read_bytes(module_base() + rva, &mut v).then_some(v)
}

fn rel32(from_next: usize, to: usize) -> Option<[u8; 4]> {
    i32::try_from(to as i64 - from_next as i64).ok().map(i32::to_le_bytes)
}

/// The fake session page: zeroed fake `S` whose member vector `+0x160 / +0x168`
/// holds one empty member, and the stub `lea rax, [FAKE_S+0x158]; ret`.
fn fake_page() -> Option<usize> {
    let have = FAKE_PAGE.load(Ordering::Acquire);
    if have != 0 {
        return Some(have);
    }
    let page = hook::alloc_near_pub(module_base() + 0xA1B000)?;
    let s = page + FAKE_S_OFF;
    let members = page + FAKE_MEMBERS_OFF;
    let stub = page + STUB_OFF;
    let mut code = vec![0x48, 0x8D, 0x05];
    code.extend_from_slice(&rel32(stub + 7, s + S_MEMBER_VEC)?);
    code.push(0xC3);
    // SAFETY: fresh RWX allocation of 0x2000 bytes, nothing points into it yet.
    unsafe {
        std::ptr::write_bytes(page as *mut u8, 0, 0x1200);
        ((s + S_MEMBERS_BEGIN) as *mut usize).write(members);
        // One member for every peer: the merged inbox (IDA §5.1e; not one per peer).
        ((s + S_MEMBERS_END) as *mut usize).write(members + SET_ENTRY);
        std::ptr::copy_nonoverlapping(code.as_ptr(), stub as *mut u8, code.len());
        // The type-23 recipient wraps (a stub that cannot be encoded stays zero = unused).
        for w in &WRAPS_23 {
            if let Some(c) = wrap_stub(page + w.stub_off, page + w.var_off, module_base() + w.target) {
                std::ptr::copy_nonoverlapping(c.as_ptr(), (page + w.stub_off) as *mut u8, c.len());
            }
        }
    }
    // Leaked on purpose (never freed: the game may hold the pointers).
    FAKE_PAGE.store(page, Ordering::Release);
    log::line(&format!(
        "nsession: fake S at {s:#x}, members {members:#x}, member-vector stub {stub:#x}, type-23 recipient stubs {:#x} / {:#x}",
        page + STUB23_CHR_OFF,
        page + STUB23_REC_OFF
    ));
    Some(page)
}

/// Bit per usable type-23 recipient wrap (stub written, the call's bytes as expected),
/// read once while nothing of ours is patched (at start, or before the first patch plan).
fn wrap_mask() -> u8 {
    let have = WRAP23.load(Ordering::Acquire);
    if have != 0xFF {
        return have;
    }
    let Some(page) = fake_page() else { return 0 };
    let mut m = 0u8;
    let mut parts = Vec::new();
    for (i, w) in WRAPS_23.iter().enumerate() {
        let stub = read::<u8>(page + w.stub_off) == Some(0x48);
        let found = bytes_at(w.rva, w.orig.len());
        let ok = stub && found.as_deref() == Some(&w.orig[..]);
        if ok {
            m |= 1 << i;
        }
        parts.push(format!("{} {}", w.name, if ok { "ok".to_string() } else { format!("NOT usable (stub {stub}, bytes {found:02x?})") }));
    }
    WRAP23.store(m, Ordering::Release);
    log::line(&format!(
        "nsession: type-23 recipient wraps: {}{}",
        parts.join(", "),
        if m == 0 { "; the native session runs with 2 players only" } else { "" }
    ));
    m
}

/// All ten entry hooks, or none: every prologue is checked before the first write.
fn install_hooks() -> Result<(), String> {
    if HOOKS_INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    for d in &HOOK_DEFS {
        let found = bytes_at(d.rva, d.prologue.len()).ok_or_else(|| format!("{} unreadable", d.name))?;
        if found != d.prologue {
            return Err(format!(
                "{} has {found:02x?}, expected {:02x?} (observe hooks on? set observe = 0)",
                d.name, d.prologue
            ));
        }
    }
    let fns = detour_fns();
    let mut ok = 0;
    for (i, d) in HOOK_DEFS.iter().enumerate() {
        match hook::install_detour(d.rva, d.prologue, fns[i], d.name) {
            Some(t) => {
                TRAMP[i].store(t, Ordering::Release);
                ok += 1;
            }
            None => {
                // The earlier ones stay in, as pass-through (ROUTE is false).
                return Err(format!("{} could not be hooked ({ok}/{HOOKS} in)", d.name));
            }
        }
    }
    HOOKS_INSTALLED.store(true, Ordering::Release);
    // Not fatal: without it ownerless enemies keep no owner (the 7.4 behaviour).
    if !ownfill::install() {
        log::line("nsession: IsOwnedHere hook not installed: enemies nobody owns stay without an owner");
    }
    // Not fatal either: without it a second deathblow can still start, the lock then
    // only settles the collision (drop / give way, §39).
    if DEATHBLOW_LOCK && !dblock::install() {
        log::line("nsession: throw gate hook not installed: the deathblow lock only settles collisions");
    }
    log::line(&format!("nsession: hooks {ok}/{HOOKS} installed (5 send/recv + 5 any-member), pass-through until engaged"));
    Ok(())
}

/// lib.rs init, only when `native_session = 1` at game start: the entry hooks go in
/// at the title screen (no game thread is inside them yet), as pass-through. With the
/// setting switched on later they go in on the first engagement instead.
pub fn install_at_start() {
    if let Err(e) = install_hooks() {
        BROKEN.store(true, Ordering::Release);
        log::line(&format!("nsession: hook check failed at start: {e}; native session OFF for this run"));
        unavailable(&format!("hook check failed: {e}"));
        return;
    }
    // The fake session page too, from the near arena while it is fresh.
    if fake_page().is_none() {
        BROKEN.store(true, Ordering::Release);
        unavailable("no memory near the module for the fake session");
        return;
    }
    // Checked once now, while nothing of ours is patched.
    let _ = wrap_mask();
}

/// Once per run on the overlay (and always in the log): the native session cannot run.
fn unavailable(reason: &str) {
    static SHOWN: AtomicBool = AtomicBool::new(false);
    log::line(&format!("nsession: native session unavailable: {reason}"));
    if !SHOWN.swap(true, Ordering::Relaxed) {
        crate::bossrules::notice(format!("Native session unavailable: {reason}"), 10);
    }
}

/// One applied patch, restored byte for byte on disengage.
#[derive(Clone)]
struct Applied {
    rva: usize,
    orig: Vec<u8>,
    new: Vec<u8>,
    name: &'static str,
}

/// Every §1 / §2.2 patch for `host`, checked first and applied all or nothing. `wraps`
/// (host, a world for 3+ players): + the usable type-23 recipient wraps (`wrap_mask`).
fn plan_patches(host: bool, stage: u8, wraps: bool) -> Result<Vec<Applied>, String> {
    let base = module_base();
    let page = fake_page().ok_or("no memory near the module for the fake session")?;
    let mut plan = Vec::new();
    if host && wraps {
        let mask = wrap_mask();
        for w in WRAPS_23.iter().enumerate().filter(|(i, _)| mask >> i & 1 != 0).map(|(_, w)| w) {
            let mut new = w.orig.to_vec();
            new[1..5].copy_from_slice(&rel32(base + w.rva + 5, page + w.stub_off).ok_or("type-23 stub out of rel32 range")?);
            plan.push(Applied { rva: w.rva, orig: w.orig.to_vec(), new, name: w.name });
        }
    }
    for s in SITES {
        let new = if host { s.host } else { s.client };
        if let Some(new) = new {
            plan.push(Applied { rva: s.rva, orig: s.orig.to_vec(), new: new.to_vec(), name: s.name });
        }
    }
    if !host && read_ptr(base + GAME_MAN_RVA).and_then(|g| read::<u8>(g + GAME_MAN_BA6)).is_some_and(|v| v != 0) {
        plan.push(Applied {
            rva: HP_OFFLINE_SITE,
            orig: HP_OFFLINE_ORIG.to_vec(),
            new: HP_OFFLINE_NEW.to_vec(),
            name: "ns client HP offline check",
        });
    }
    for l in &LEA_SITES {
        let mut new = l.orig.to_vec();
        new[1] = 0x8D; // mov -> lea, same register
        new[3..7].copy_from_slice(&rel32(base + l.rva + 7, page + FAKE_S_OFF).ok_or("fake S out of rel32 range")?);
        plan.push(Applied { rva: l.rva, orig: l.orig.to_vec(), new, name: l.name });
    }
    let mut call = VEC_CALL_ORIG.to_vec();
    call[1..5].copy_from_slice(&rel32(base + VEC_CALL_SITE + 5, page + STUB_OFF).ok_or("stub out of rel32 range")?);
    plan.push(Applied { rva: VEC_CALL_SITE, orig: VEC_CALL_ORIG.to_vec(), new: call, name: "ns fake member vector recv 71" });
    if stage >= 4 {
        plan.push(Applied { rva: OBJ_EVENT_SITE, orig: OBJ_EVENT_ORIG.to_vec(), new: TRUE5.to_vec(), name: "ns members ObjAct send 45" });
    }
    for p in &plan {
        let found = bytes_at(p.rva, p.orig.len()).ok_or_else(|| format!("{} unreadable", p.name))?;
        if found != p.orig {
            return Err(format!("{} at {:#x} has {found:02x?}, expected {:02x?}", p.name, p.rva, p.orig));
        }
    }
    Ok(plan)
}

/// The start of `apply_patches`' message for a suspend phase that ran out of its budget.
const TIMED_OUT_MSG: &str = "suspending the game's threads took longer than";
/// Timed-out suspend phases in a row before the engage gives up (as 600 busy frames did).
const BUSY_TIMEOUTS_MAX: u32 = 12;

/// v8.61: the wait before the next patch try after `timeouts` (>= 1) timed-out suspend phases
/// in a row: 1, 2, 4, 8, 8 ... s. Every try froze the game for 250-800 ms (once 5.5 s), and
/// the old every-frame retry made 12 of them in 7 s (host 10:55:55-10:56:02, 2026-10-28).
pub(crate) fn busy_retry_gap(timeouts: u32) -> Duration {
    Duration::from_millis(1000u64 << timeouts.clamp(1, 4).saturating_sub(1))
}

/// Err(true, why) = a thread kept sitting in a target: try again next frame.
fn apply_patches(plan: Vec<Applied>) -> Result<Vec<Applied>, (bool, String)> {
    let base = module_base();
    let items: Vec<(usize, &[u8], &[u8])> = plan.iter().map(|p| (base + p.rva, &p.orig[..], &p.new[..])).collect();
    match write_frozen_budget(&items, freeze::ENGAGE_BUDGET_MS) {
        Ok(()) => {
            for p in &plan {
                log::line(&format!("hook: {} patched at {:#x} (threads suspended)", p.name, base + p.rva));
            }
            Ok(plan)
        }
        Err(freeze::Error::Bytes(i)) => Err((false, format!("{} changed under us (nothing written)", plan[i].name))),
        // Busy(0) = the suspend phase ran out of its budget (hook.rs `write_all_timed`).
        Err(freeze::Error::Busy(0)) => Err((true, format!("{TIMED_OUT_MSG} {} ms (nothing written)", freeze::ENGAGE_BUDGET_MS))),
        Err(freeze::Error::Busy(rip)) => Err((true, format!("a game thread is at {rip:#x} inside a patch site (nothing written)"))),
        Err(e) => Err((false, format!("frozen write failed: {e:?} (nothing written)"))),
    }
}

/// Restores every patch (threads suspended; falls back to plain writes if a thread
/// keeps sitting in a target). A site that no longer holds our bytes is logged and left.
fn restore_patches(done: &mut Vec<Applied>) {
    if done.is_empty() {
        return;
    }
    let base = module_base();
    let n = done.len();
    let items: Vec<(usize, &[u8], &[u8])> = done.iter().map(|p| (base + p.rva, &p.new[..], &p.orig[..])).collect();
    let frozen = write_frozen(&items);
    let mut ok = n;
    if frozen.is_err() {
        log::line(&format!("nsession: frozen restore failed ({frozen:?}), restoring one by one"));
        ok = 0;
        for p in done.iter().rev() {
            if hook::patch_code(p.rva, &p.new, &p.orig, p.name) {
                ok += 1;
            } else {
                // Someone else changed the bytes: never touch the mode again this run.
                BROKEN.store(true, Ordering::Release);
                log::line(&format!("nsession: WARNING {} at rva {:#x} could not be restored", p.name, p.rva));
            }
        }
    }
    done.clear();
    log::line(&format!("nsession: patches restored {ok}/{n}"));
}

// ---------------------------------------------------------------------------
// Game-side reads (game thread)
// ---------------------------------------------------------------------------

fn wcm() -> Option<usize> {
    read_ptr(module_base() + WORLD_CHR_MAN_RVA)
}

/// Every chr in WorldChrMan's chr sets (the loop of `0xA4DB20`).
fn all_chrs() -> Vec<Chr> {
    let mut out = Vec::new();
    let Some(w) = wcm() else { return out };
    let n = read::<i32>(w + WCM_SET_COUNT).unwrap_or(0).clamp(0, 64) as usize;
    for i in 0..n {
        let Some(set) = read_ptr(w + WCM_SETS + WCM_SET_STRIDE * i).filter(|&p| p >= 0x10000) else { continue };
        let cnt = read::<i32>(set).unwrap_or(0).clamp(0, 4096) as usize;
        let Some(entries) = read_ptr(set + SET_ENTRIES).filter(|&p| p >= 0x10000) else { continue };
        for j in 0..cnt {
            if let Some(c) = read_ptr(entries + SET_ENTRY * j).filter(|&p| p >= 0x10000) {
                out.push(Chr(c));
            }
        }
    }
    out
}

fn enemies() -> Vec<Chr> {
    let vt = module_base() + ENEMY_VTABLE_RVA;
    all_chrs()
        .into_iter()
        .filter(|c| read_ptr(c.0).is_some_and(|v| v == vt || crate::rtti::class_name(c.0).is_some_and(|n| n.contains("EnemyIns"))))
        .collect()
}

/// `{entity id, netId}` of every loaded enemy with both (the §3 check).
fn our_pairs() -> Vec<Pair> {
    let mut v: Vec<Pair> = enemies()
        .into_iter()
        .filter_map(|c| Some((c.entity_id().filter(|&e| e > 0)?, netai::net_id_of(c)?, c.hp().unwrap_or(0))))
        .collect();
    v.sort_unstable();
    v.dedup();
    v.truncate(MAX_PAIRS);
    v
}

/// `{entity id, max HP}` of the enemies in `our_pairs` (MaxHp message, the engage HP check).
fn our_max_hp() -> Vec<(i32, i32)> {
    let mut v: Vec<(i32, i32)> = enemies()
        .into_iter()
        .filter_map(|c| Some((c.entity_id().filter(|&e| e > 0)?, crate::livesync::read_data(c)?.2)))
        .collect();
    v.sort_unstable();
    v.dedup_by_key(|p| p.0);
    v.truncate(MAX_PAIRS);
    v
}

/// (Com AI active, NetAI active, other, owned here) over the loaded enemies.
fn owners() -> (u32, u32, u32, u32) {
    let (mut com, mut net, mut other, mut here) = (0, 0, 0, 0);
    let Some(w) = wcm() else { return (0, 0, 0, 0) };
    let owned: extern "C" fn(usize, u16) -> u8 = unsafe { std::mem::transmute(module_base() + OWNED_HERE_RVA) };
    for c in enemies() {
        match netai::active_kind(c) {
            0 => net += 1,
            1 => com += 1,
            _ => other += 1,
        }
        if let Some(n) = netai::net_id(c)
            && owned(w, n) != 0
        {
            here += 1;
        }
    }
    (com, net, other, here)
}

fn sync_table() -> Option<usize> {
    read_ptr(wcm()? + WCM_SYNC_TABLE).filter(|&t| t >= 0x10000)
}

/// WorldChrMan's update counter (`WCM_UPDATES`).
fn world_updates() -> Option<u32> {
    read::<u32>(wcm()? + WCM_UPDATES)
}

/// The sync table's four timers (`T+0x398`: 23 period, 23 due, 22 period, 22 due).
fn timers() -> Option<[f32; 4]> {
    read::<[f32; 4]>(sync_table()? + T_TIMERS)
}

/// Makes the next sync-table update send 22 (client) / distribute 23 (host) at once
/// instead of up to 5 / 3 s later. Game thread (our FrameBegin), plain float write.
fn make_due(off: usize) -> bool {
    sync_table().is_some_and(|t| write(t + off, 0.0f32))
}

/// netIds of the loaded enemies owned on this machine (`0xA4A650`).
fn owned_ids() -> Vec<u16> {
    let Some(w) = wcm() else { return Vec::new() };
    let owned: extern "C" fn(usize, u16) -> u8 = unsafe { std::mem::transmute(module_base() + OWNED_HERE_RVA) };
    let mut v: Vec<u16> = enemies().into_iter().filter_map(netai::net_id).filter(|&n| owned(w, n) != 0).collect();
    v.sort_unstable();
    v.dedup();
    v.truncate(MAX_OWNED);
    v
}

/// netIds of the loaded boss members (table bosses and enemies with two or more lives, as
/// ownfill's boss rule counts them).
fn boss_net_ids() -> Vec<u16> {
    enemies()
        .into_iter()
        .filter(|&c| {
            c.entity_id().is_some_and(|e| crate::bosses::encounter_of(e).is_some())
                || crate::livesync::multi_life(crate::livesync::lives0_of(c))
        })
        .filter_map(netai::net_id)
        .collect()
}

/// Marker in an Owned list: "our world update is paused (menu), the enemies near you that
/// we owned are left out of this list". Sorts after every real netId (`NO_NET_ID` is 0xFFFF);
/// an older peer sees an id no enemy has.
const OWNED_PAUSED_MARK: u16 = 0xFFFE;
/// Our world update stood still this long while engaged: hand our enemies on.
const WORLD_PAUSED_AFTER: Duration = Duration::from_millis(1500);
/// Both machines own (and we claim) an enemy this long (> `OWNED_EVERY`, the lists lag):
/// the lower slot wins.
const DOUBLE_CLAIM_AFTER: Duration = Duration::from_millis(2500);

fn owned_is_paused(ids: &[u16]) -> bool {
    ids.binary_search(&OWNED_PAUSED_MARK).is_ok()
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum PauseStep {
    Idle,
    Begin,
    End,
}

/// Pause detection with hysteresis: begins when our world update has stood still for
/// `WORLD_PAUSED_AFTER`, ends the moment it runs (stalled < `WORLD_STILL_AFTER`).
#[derive(Default)]
struct PauseTracker {
    active: bool,
}

impl PauseTracker {
    fn step(&mut self, stalled: Duration) -> PauseStep {
        if !self.active && stalled >= WORLD_PAUSED_AFTER {
            self.active = true;
            PauseStep::Begin
        } else if self.active && stalled < WORLD_STILL_AFTER {
            self.active = false;
            PauseStep::End
        } else {
            PauseStep::Idle
        }
    }
}

/// Of our owned enemies (netId, position) the ones a peer would take over while we are
/// paused, and which one (the peers' own ownfill rule: nearest within `ownfill::NEAR_M`,
/// the host 5 m ahead). `peers`: the other engaged players that are not paused.
fn handover_targets(owned: &[(u16, [f32; 3])], peers: &[(u8, Option<[f32; 3]>)]) -> Vec<(u16, u8)> {
    let mut out = Vec::new();
    for &(n, p) in owned {
        let d: Vec<(u8, Option<f32>)> = peers.iter().map(|&(s, o)| (s, o.map(|o| crate::game::distance(o, p)))).collect();
        for (i, &(s, dist)) in d.iter().enumerate() {
            let Some(ds) = dist else { continue };
            let others: Vec<(u8, Option<f32>)> = d.iter().enumerate().filter(|&(j, _)| j != i).map(|(_, &x)| x).collect();
            if ownfill::wanted(s, ds, &others) {
                out.push((n, s));
                break;
            }
        }
    }
    out
}

/// Client (or any non-lowest slot): drop our claim on an enemy that a lower slot owns
/// too, once both have owned it for `DOUBLE_CLAIM_AFTER`. The host (slot 0) never drops.
fn double_claim_drop(me: u8, we_claim: bool, we_own: bool, owners: &[u8], both_for: Duration) -> bool {
    we_claim && we_own && owners.iter().any(|&s| s < me) && both_for >= DOUBLE_CLAIM_AFTER
}

/// The ownerless report's counters: every netId counts once (an enemy can sit in several
/// chr sets: live 396 / 397 "alive" for 232 enemies).
#[derive(Default)]
struct OwnerTally {
    seen: std::collections::HashSet<u16>,
    nobody: u32,
    both: u32,
}

impl OwnerTally {
    /// True when this netId is counted as owned by nobody (first sighting only).
    fn add(&mut self, n: u16, here: bool, there: bool) -> bool {
        if !self.seen.insert(n) {
            return false;
        }
        if here && there {
            self.both += 1;
        }
        if here || there {
            return false;
        }
        self.nobody += 1;
        true
    }
}

/// Enemies within this distance of a player that nobody owns are listed one by one.
const OWNERLESS_NEAR_M: f32 = 40.0;

/// Every 10 s while engaged: enemies owned on no machine (`theirs` = the other engaged
/// players' latest Owned lists, `others` their positions). Such an enemy has no
/// controller anywhere: NetAI without records (frozen or walking in place) on every screen.
fn ownerless_report(player: Chr, others: &[(u8, Option<[f32; 3]>)], theirs: Option<&[(u8, Vec<u16>)]>) {
    let Some(theirs) = theirs else {
        log::line("nsession: ownerless check: no owned list from every engaged peer yet");
        return;
    };
    let Some(w) = wcm() else { return };
    let owned: extern "C" fn(usize, u16) -> u8 = unsafe { std::mem::transmute(module_base() + OWNED_HERE_RVA) };
    let me = player.position();
    let mut near = Vec::new();
    let mut both_ids: Vec<String> = Vec::new();
    let mut tally = OwnerTally::default();
    for c in enemies() {
        let Some(n) = netai::net_id(c) else { continue };
        if c.hp().is_some_and(|hp| hp <= 0) {
            continue;
        }
        let here = owned(w, n) != 0;
        let there = theirs.iter().any(|(_, ids)| ids.binary_search(&n).is_ok());
        // Before the early `continue`: `tally.add` is false for every owned enemy, which
        // is exactly the both-claimed case.
        if here && there && both_ids.len() < 4 {
            let how = if ownfill::forced(n) { "forced here" } else { "game here" };
            both_ids.push(format!("{n:#x} ent {} ({how})", c.entity_id().unwrap_or(-1)));
        }
        if !tally.add(n, here, there) {
            continue;
        }
        let Some(p) = c.position() else { continue };
        let d = |o: Option<[f32; 3]>| o.map_or(f32::MAX, |o| crate::game::distance(p, o));
        let dm = d(me);
        let df = others.iter().map(|(_, o)| d(*o)).fold(f32::MAX, f32::min);
        if dm.min(df) <= OWNERLESS_NEAR_M {
            let kind = match netai::active_kind(c) {
                0 => "NetAI",
                1 => "own AI",
                2 => "other manipulator",
                _ => "unreadable",
            };
            near.push((
                dm.min(df),
                format!(
                    "{} netId {n:#x} at {:.1},{:.1},{:.1} {kind} interval {:?}, {:.0} m from us, {:.0} m from the nearest other player",
                    c.entity_id().unwrap_or(-1),
                    p[0],
                    p[1],
                    p[2],
                    read::<i32>(c.0 + 0x110),
                    dm.min(9999.0),
                    df.min(9999.0)
                ),
            ));
        }
    }
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    let list: Vec<String> = near.iter().take(8).map(|(_, s)| s.clone()).collect();
    log::line(&format!(
        "nsession: ownerless (10 s): {} alive enemies owned by nobody, {} of them within {OWNERLESS_NEAR_M} m of a player{}{}",
        tally.nobody,
        near.len(),
        if list.is_empty() { String::new() } else { format!(": {}", list.join("; ")) },
        if tally.both > 0 {
            let ids = if both_ids.is_empty() { String::new() } else { format!(" [{}]", both_ids.join(", ")) };
            format!("; {} claimed by both machines{ids}", tally.both)
        } else {
            String::new()
        }
    ));
}

/// Host: the friend's candidate list into the record of the friend's remote (§4).
fn feed_22(payload: &[u8], remote: Chr) -> Result<usize, String> {
    let n = parse_22(payload).ok_or_else(|| format!("bad type-22 payload ({} bytes)", payload.len()))?;
    let h = read::<u32>(remote.0 + 8).filter(|&h| h != u32::MAX && h != 0).ok_or("remote has no handle")?;
    let t = sync_table().ok_or("no sync table")?;
    let base = module_base();
    let find: extern "C" fn(usize, u32) -> usize = unsafe { std::mem::transmute(base + FIND_RECORD_RVA) };
    let set: extern "C" fn(usize, i32, *const u32) = unsafe { std::mem::transmute(base + SET_CANDIDATES_RVA) };
    let rec = find(t, h);
    if rec < 0x10000 {
        return Err("no record for the remote".into());
    }
    let list: Vec<u32> = payload[4..].chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    set(rec, n as i32, list.as_ptr());
    Ok(n)
}

/// Every queued packet of type `t`: (origin slot, bytes).
fn take_inbound(t: u8) -> Vec<(u8, Vec<u8>)> {
    INBOX.lock().ok().and_then(|mut g| g.get_mut(t as usize).map(|q| q.drain(..).collect())).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Link side (net.rs, our frame)
// ---------------------------------------------------------------------------

/// What one peer told us on its current link (host: each client; client: the host).
#[derive(Default)]
struct Peer {
    epoch: u64,
    hello: Option<(u8, u8, bool, i32, u32)>,
    list: Option<(u32, i32, Vec<Pair>)>,
    /// (peer's generation, our generation it is ready for).
    ready: Option<(u32, u32)>,
    leave: Option<(u32, bool, u8)>,
    /// Its MaxHp for its List of that generation (§34; none from an older build).
    max_hp: Option<(u32, Vec<(i32, i32)>)>,
    stale_packets: u32,
    /// The newest handshake generation seen from this peer on this link (None = none yet).
    /// Reset with the Peer record on every new connection (new epoch), so a peer that
    /// restarted its game (generations start at 1 again) is never held to the old run.
    newest_gen: Option<u32>,
    /// Handshake messages of an older generation than `newest_gen`, ignored (for the log).
    stale_ignored: u32,
}

/// `got` is an older handshake generation than `newest` (wrap-safe: generations are a
/// wrapping u32 counter, so "older" = behind by less than half the range). 0 = unknown,
/// never older. Where the old code went wrong: hello / list / ready / leave were stored
/// whatever their generation, so an old hello arriving late (the reliable lane delivers
/// a backlog in one burst after a stall) replaced the peer's newest one and `ready`
/// from a generation long gone was accepted for a handshake of ours that had moved on.
fn gen_older(got: u32, newest: u32) -> bool {
    got != 0 && newest != 0 && got != newest && newest.wrapping_sub(got) < 0x8000_0000
}

/// The message is stale: lower than the newest generation already seen on this link.
fn hello_is_stale(newest: Option<u32>, got: u32) -> bool {
    newest.is_some_and(|n| gen_older(got, n))
}

/// The newest generation after seeing `got` (a higher one raises it; 0 never counts).
fn raise_newest(newest: Option<u32>, got: u32) -> Option<u32> {
    match newest {
        _ if got == 0 => newest,
        Some(n) if !gen_older(n, got) => Some(n),
        _ => Some(got),
    }
}

/// Log the first few ignored stale messages of a link, then every 50th.
fn stale_log_due(count: u32) -> bool {
    count <= 5 || count % 50 == 0
}

/// Why a client / host must stay out of ENGAGED for this handshake (None = engage). The
/// peer's `ready` must name its hello generation `pgen` and OUR current generation (an
/// older one is stale), and nothing newer than `pgen` may have been seen from it since.
fn engage_block_reason(our_gen: u32, pgen: u32, ready: Option<(u32, u32)>, newest: Option<u32>) -> Option<String> {
    match ready {
        Some((rg, for_ours)) if rg == pgen && for_ours == our_gen => {}
        Some((rg, for_ours)) => {
            return Some(format!("its ready is for generation {rg} / our {for_ours}, not {pgen} / our current {our_gen}"));
        }
        None => return Some("no ready from the peer yet".into()),
    }
    match newest {
        Some(n) if n != pgen => Some(format!("the peer is already at generation {n}, past the {pgen} it was ready for")),
        _ => None,
    }
}

/// What one origin's machine told us, directly or relayed by the host, each with the
/// link it came on and that hop's generation (valid while `accepts(link, generation)`).
#[derive(Default)]
struct Origin {
    /// The latest Owned list (link, generation, when, sorted netIds).
    owned: Option<(u8, u32, Instant, Vec<u16>)>,
    /// Lives messages not taken yet (link, generation, entries); livesync.rs.
    lives: Vec<(u8, u32, Vec<LifeEntry>)>,
}

struct Book {
    peers: [Option<Peer>; SLOTS],
    origins: [Origin; SLOTS],
    /// Client: the host's latest Engaged message (its generation for us, client slots).
    engaged: Option<(u32, Vec<u8>)>,
}

static BOOK: Mutex<Book> = Mutex::new(Book {
    peers: [const { None }; SLOTS],
    origins: [const { Origin { owned: None, lives: Vec::new() } }; SLOTS],
    engaged: None,
});
/// Our reliable lane per link (module `lane`); a new one (new id) after every link drop.
static LANES: Mutex<[Option<lane::Lane>; SLOTS]> = Mutex::new([const { None }; SLOTS]);
static LANE_IDS: AtomicU32 = AtomicU32::new(0);

fn new_lane() -> lane::Lane {
    // Unique per lane of this process and unlikely to repeat across runs.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    lane::Lane::new(t ^ LANE_IDS.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9) | 1)
}

fn book() -> std::sync::MutexGuard<'static, Book> {
    BOOK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// net.rs: a bulk transfer of `BULK_KIND` from `from` (the first build's handshake): ignored.
pub fn on_bulk(_from: PeerId, data: &[u8], _epoch: u64) {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        log::line(&format!("nsession: an older build's handshake ({} B) ignored: both players need the same build", data.len()));
    }
}

/// A handshake message on link `from`: stored whatever the setting (cheap). Host: an
/// engaged client's Owned / Lives go on to the other engaged clients, re-tagged with our
/// generation for each.
fn on_control(from: u8, data: &[u8], epoch: u64) {
    let Some(m) = decode(data) else {
        log::line(&format!("nsession: undecodable handshake message ({} bytes) from peer {from} dropped", data.len()));
        return;
    };
    let s = from as usize;
    if s >= SLOTS {
        return;
    }
    let host = ROLE_HOST.load(Ordering::Relaxed);
    let me = ME.load(Ordering::Relaxed);
    // The others an engaged client's message goes on to (none on a client, none with one peer).
    let onward = |origin: u8, generation: u32| -> Vec<u8> {
        if relays(host, MODE_ALL) && accepts(from, generation) {
            targets(true, Dst::All, origin, engaged_links())
        } else {
            Vec::new()
        }
    };
    let mut out: Vec<(u8, Msg)> = Vec::new();
    {
        let mut g = book();
        if g.peers[s].as_ref().is_none_or(|p| p.epoch != epoch) {
            g.peers[s] = Some(Peer { epoch, ..Default::default() });
        }
        // Handshake messages of a generation older than the newest seen on this link are
        // ignored (module comment at `gen_older`); a newer one raises the mark.
        let hs = match &m {
            Msg::Hello { generation, .. } => Some(("hello", *generation)),
            Msg::List { generation, .. } => Some(("list", *generation)),
            Msg::Ready { generation, .. } => Some(("ready", *generation)),
            Msg::Leave { generation, .. } => Some(("leave", *generation)),
            Msg::MaxHp { generation, .. } => Some(("maxhp", *generation)),
            _ => None,
        };
        if let (Some((kind, got)), Some(p)) = (hs, g.peers[s].as_mut()) {
            if hello_is_stale(p.newest_gen, got) {
                p.stale_ignored += 1;
                if stale_log_due(p.stale_ignored) {
                    log::line(&format!(
                        "nsession: ignored stale {kind} generation {got} (newest {}) from peer {from} ({} ignored on this link)",
                        p.newest_gen.unwrap_or(0),
                        p.stale_ignored
                    ));
                }
                return;
            }
            p.newest_gen = raise_newest(p.newest_gen, got);
        }
        match m {
            Msg::Hello { proto, stage, host: h, block, generation } => {
                log::line(&format!(
                    "nsession: peer {from} hello: proto {proto} stage {stage} {} block {block} generation {generation}",
                    if h { "host" } else { "client" }
                ));
                if let Some(p) = g.peers[s].as_mut() {
                    p.hello = Some((proto, stage, h, block, generation));
                }
            }
            Msg::List { generation, block, pairs } => {
                log::line(&format!("nsession: peer {from} netId list generation {generation} block {block}: {} enemies", pairs.len()));
                if let Some(p) = g.peers[s].as_mut() {
                    p.list = Some((generation, block, pairs));
                }
            }
            Msg::Ready { generation, peer_generation } => {
                log::line(&format!("nsession: peer {from} ready generation {generation} for our {peer_generation}"));
                if let Some(p) = g.peers[s].as_mut() {
                    p.ready = Some((generation, peer_generation));
                }
            }
            Msg::Leave { generation, latch, reason } => {
                log::line(&format!(
                    "nsession: peer {from} left generation {generation} ({:?}, latch {latch})",
                    Reason::from_u8(reason)
                ));
                if let Some(p) = g.peers[s].as_mut() {
                    p.leave = Some((generation, latch, reason));
                }
            }
            Msg::Owned { generation, origin, mut ids } => {
                if !origin_ok(host, from, origin, me) {
                    return;
                }
                ids.sort_unstable();
                for t in onward(origin, generation) {
                    let generation = GEN_OUT[t as usize].load(Ordering::Relaxed);
                    out.push((t, Msg::Owned { generation, origin, ids: ids.clone() }));
                }
                g.origins[origin as usize].owned = Some((from, generation, Instant::now(), ids));
            }
            Msg::Lives { generation, origin, entries } => {
                if !origin_ok(host, from, origin, me) {
                    return;
                }
                for t in onward(origin, generation) {
                    let generation = GEN_OUT[t as usize].load(Ordering::Relaxed);
                    out.push((t, Msg::Lives { generation, origin, entries: entries.clone() }));
                }
                // Bounded: livesync takes them every frame while engaged, and drops them when not.
                let l = &mut g.origins[origin as usize].lives;
                if l.len() >= 64 {
                    l.remove(0);
                }
                l.push((from, generation, entries));
            }
            Msg::Engaged { generation, slots } => {
                if host || from != 0 {
                    return;
                }
                if g.engaged.as_ref().is_none_or(|(_, old)| *old != slots) {
                    log::line(&format!("nsession: the host's engaged clients: {slots:?} (generation {generation})"));
                }
                g.engaged = Some((generation, slots));
            }
            Msg::MaxHp { generation, max } => {
                if let Some(p) = g.peers[s].as_mut() {
                    p.max_hp = Some((generation, max));
                }
            }
        }
    }
    for (t, m) in out {
        queue_ctrl(t, &m);
    }
}

// --- Lives / death sync support (livesync.rs, IMPLEMENTATION-NOTES §29) ----------------

/// One value for a set of engagements: `(slot, ACCEPT)` pairs; one = its ACCEPT (the
/// 2-player key, `1 << 32 | peer generation`), several = a hash of them, none = 0.
fn engaged_key_of(keys: &[(u8, u64)]) -> u64 {
    match keys {
        [] => 0,
        [(_, k)] => *k,
        _ => {
            let mut h: u64 = 0xCBF2_9CE4_8422_2325;
            for &(s, k) in keys {
                for b in std::iter::once(s).chain(k.to_le_bytes()) {
                    h = (h ^ b as u64).wrapping_mul(0x0100_0000_01B3);
                }
            }
            1 << 32 | (h as u32 ^ (h >> 32) as u32) as u64
        }
    }
}

/// Engaged (the peers' packets are accepted): nonzero, and it changes whenever an
/// engagement starts or ends (host: any of its clients'); else 0. livesync.rs starts
/// over whenever this changes. With one peer: `1 << 32 | its generation`.
pub fn engaged_key() -> u64 {
    if !OWNS_ENEMIES.load(Ordering::Relaxed) {
        return 0;
    }
    let keys: Vec<(u8, u64)> = mask_slots(engaged_links())
        .into_iter()
        .map(|s| (s, ACCEPT[s as usize].load(Ordering::Acquire)))
        .filter(|&(_, k)| k != 0)
        .collect();
    engaged_key_of(&keys)
}

/// Queues our Lives message for every engaged peer (handshake lane, reliable, in order;
/// a client's goes to the host, which passes it on).
pub fn send_lives(entries: Vec<LifeEntry>) {
    if entries.is_empty() || engaged_key() == 0 {
        return;
    }
    let origin = ME.load(Ordering::Relaxed);
    queue_ctrl_engaged(|generation| Msg::Lives { generation, origin, entries: entries.clone() });
}

/// Every other player's Lives entries that came on a link we are engaged with (its
/// current generation), oldest first per player; the rest is dropped. livesync merges
/// them by minimum, so the order between players does not matter.
pub fn take_lives() -> Vec<LifeEntry> {
    let key = engaged_key();
    let mut g = book();
    let mut out = Vec::new();
    for o in g.origins.iter_mut() {
        let msgs = std::mem::take(&mut o.lives);
        if key == 0 {
            continue;
        }
        for (link, generation, entries) in msgs {
            if accepts(link, generation) {
                out.extend(entries);
            }
        }
    }
    out
}

/// The fresh Owned lists of these slots (link accepted, younger than `OWNED_FRESH`);
/// None while any of them has none (we cannot tell what it owns yet).
fn owned_lists(slots: &[u8]) -> Option<Vec<(u8, Vec<u16>)>> {
    let g = book();
    slots
        .iter()
        .map(|&s| {
            g.origins[s as usize]
                .owned
                .as_ref()
                .filter(|(link, generation, at, _)| accepts(*link, *generation) && at.elapsed() < OWNED_FRESH)
                .map(|(.., ids)| (s, ids.clone()))
        })
        .collect()
}

/// The loaded enemies (WorldChrMan's chr sets, enemy class only).
pub fn enemy_chrs() -> Vec<Chr> {
    enemies()
}

/// The enemy with this netId is owned on this machine (`0xA4A650`). Game thread.
pub fn owned_here(net_id: u16) -> bool {
    let Some(w) = wcm() else { return false };
    let owned: extern "C" fn(usize, u16) -> u8 = unsafe { std::mem::transmute(module_base() + OWNED_HERE_RVA) };
    owned(w, net_id) != 0
}

/// v8.30b (F11 line tag): our forced bit is set for this netId.
pub(crate) fn forced_here(n: u16) -> bool {
    ownfill::forced(n)
}

/// See `forced_here`: the game's native ownership answer is nonzero.
pub(crate) fn game_owned_here(n: u16) -> bool {
    wcm().is_some_and(|w| ownfill::game_owned(w, n) != 0)
}

/// Ownerless enemies near the players get an owner (IMPLEMENTATION-NOTES §31, NOT verified
/// live). Live 7.4: `ownerless (10 s): 200 alive enemies owned by nobody, 3 within 40 m`
/// on the friend (enemies 7-20 m away, NetAI, update interval -1: running in place, a
/// backstab still possible). The game hands out ownership only from the players' candidate
/// lists (22, `0xA4E520`), and an enemy that is NetAI here is not updated here (interval
/// -1), so one that nobody listed when the engagement started never becomes a candidate:
/// it stays NetAI without records on both machines. Fix: `0xA4A650` (IsOwnedHere(WCM,
/// netId), used by EnemyIns slot 37 `0xA14B50` to pick ComManipulator / NetAI, by the
/// sender `0xA1C100` for 4 / 70 / 71 and by the damage router `0xB6A040` / `0xB6F4C0`;
/// IDA v75 o10) is detoured: it also answers "owned here" for the netIds in `FORCED`.
/// Such an enemy runs our own AI, its state goes to the friend like any enemy we own, and
/// it becomes our candidate for the game's next distribution. Which machine takes one:
/// the nearest engaged player (the host's distance counts 5 m less; a tie goes to the
/// lowest slot), only within 40 m, never one the game or another player owns (their
/// Owned lists, every 2 s); a conflict (two own it) is dropped at once by the higher slot
/// and after 3 s by the lowest (then it was the game's). Two players: the 7.4 rule.
pub(crate) mod ownfill {
    use super::*;

    const OWNED_HERE_PROLOGUE: [u8; 11] = [0x48, 0x83, 0xEC, 0x28, 0x48, 0x8B, 0x89, 0x30, 0x06, 0x00, 0x00];
    static TRAMP: AtomicUsize = AtomicUsize::new(0);
    static ANY: AtomicBool = AtomicBool::new(false);
    static BITS: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];

    pub const NEAR_M: f32 = 40.0;
    /// Hysteresis (live 2026-10-24: "taken 127, given back 112" for enemies 40-48 m away):
    /// we take at <= `NEAR_M`, and let one go only beyond `NEAR_M + GIVE_BACK_EXTRA_M` ...
    pub const GIVE_BACK_EXTRA_M: f32 = 8.0;
    const FAR_M: f32 = NEAR_M + GIVE_BACK_EXTRA_M;
    /// ... that stayed beyond (no longer `keep`) for this long without a break.
    pub const GIVE_BACK_AFTER: Duration = Duration::from_secs(5);
    const TIE_M: f32 = 5.0;
    const HOST_CONFLICT_DROP: Duration = Duration::from_secs(3);

    fn bit(n: u16) -> (usize, u64) {
        ((n >> 6) as usize, 1u64 << (n & 63))
    }

    pub fn forced(n: u16) -> bool {
        let (i, m) = bit(n);
        ANY.load(Ordering::Relaxed) && BITS[i].load(Ordering::Relaxed) & m != 0
    }

    pub fn set(n: u16, on: bool) {
        let (i, m) = bit(n);
        if on {
            BITS[i].fetch_or(m, Ordering::Relaxed);
            ANY.store(true, Ordering::Release);
        } else {
            BITS[i].fetch_and(!m, Ordering::Relaxed);
        }
    }

    pub fn clear_all() {
        ANY.store(false, Ordering::Release);
        for b in &BITS {
            b.store(0, Ordering::Relaxed);
        }
    }

    extern "C" fn detour(w: usize, n: u16) -> u8 {
        if forced(n) && crate::mod_alive() {
            return 1;
        }
        game_owned(w, n)
    }

    /// The game's own answer (the trampoline once hooked).
    pub fn game_owned(w: usize, n: u16) -> u8 {
        let t = TRAMP.load(Ordering::Acquire);
        let f: extern "C" fn(usize, u16) -> u8 =
            unsafe { std::mem::transmute(if t != 0 { t } else { module_base() + OWNED_HERE_RVA }) };
        f(w, n)
    }

    pub fn install() -> bool {
        if TRAMP.load(Ordering::Acquire) != 0 {
            return true;
        }
        let d: extern "C" fn(usize, u16) -> u8 = detour;
        match hook::install_detour(OWNED_HERE_RVA, &OWNED_HERE_PROLOGUE, d as usize, "IsOwnedHere (0xA4A650)") {
            Some(t) => {
                TRAMP.store(t, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// The distance a player competes with: the host's counts `TIE_M` less (the 7.4 "host
    /// on a tie within 5 m"). Minimum (key, slot) wins: exactly one taker.
    fn key(slot: u8, d: f32) -> f32 {
        if slot == 0 { d } else { d + TIE_M }
    }

    /// Do we (`me`, `d_me` from the enemy, our key lowered by `bonus`) beat every other
    /// engaged player (`others`: slot, distance if known)? A tie goes to the lower slot. An
    /// unknown distance does not compete, except the host's: a client never takes without it.
    fn beats(me: u8, mine: f32, others: &[(u8, Option<f32>)]) -> bool {
        others.iter().all(|&(s, d)| match d {
            None => s != 0,
            Some(d) if s < me => mine < key(s, d),
            Some(d) => mine <= key(s, d),
        })
    }

    /// The pure rule: should this machine take an enemy nobody owns? Two players (host 0,
    /// client 1): the host when `d_host <= d_client + 5`, the client when `d_client + 5 <
    /// d_host`; no position of the other: only the host.
    pub fn wanted(me: u8, d_me: f32, others: &[(u8, Option<f32>)]) -> bool {
        d_me <= NEAR_M && beats(me, key(me, d_me), others)
    }

    /// A boss is never decided by "who is nearer" (v8.12: Genichiro / Tomoe flipped between
    /// the machines every few seconds, a second AI ran the copy and the boss jumped behind the
    /// player, live 2026-10-25): the host owns it while it is within `BOSS_HOST_M`; a client
    /// only when the host is clearly farther than that.
    pub const BOSS_HOST_M: f32 = 150.0;
    /// The client lets a boss go back once the host is within this (hysteresis below `BOSS_HOST_M`).
    const BOSS_HOST_BACK_M: f32 = 130.0;

    fn host_far(others: &[(u8, Option<f32>)], beyond: f32) -> bool {
        others.iter().all(|&(s, d)| s != 0 || d.is_some_and(|d| d > beyond))
    }

    /// v8.57: with the host out of the rule (down, `boss_rivals`) a client takes the boss at the
    /// host's own range: the Divine Dragon is fought from 60-85 m, beyond `NEAR_M`.
    fn host_listed(others: &[(u8, Option<f32>)]) -> bool {
        others.iter().any(|&(s, _)| s == 0)
    }

    pub fn wanted_boss(me: u8, d_me: f32, others: &[(u8, Option<f32>)]) -> bool {
        if me == 0 || !host_listed(others) { d_me <= BOSS_HOST_M } else { d_me <= NEAR_M && host_far(others, BOSS_HOST_M) }
    }

    pub fn keep_boss(me: u8, d_me: f32, others: &[(u8, Option<f32>)]) -> bool {
        if me == 0 || !host_listed(others) {
            d_me <= BOSS_HOST_M + 20.0
        } else {
            d_me <= FAR_M && host_far(others, BOSS_HOST_BACK_M)
        }
    }

    /// The players a boss's owner rule weighs: one waiting dead in the fight (`EV_DOWN`,
    /// bit per slot in `down`) does not count. Live 2026-10-27 (Divine Dragon): the dead
    /// spectating host kept the dragon forced, its HP sat at 1 for 165 s and the living
    /// friend never got the final deathblow (the game itself had answered "not ours").
    pub fn boss_rivals(others: &[(u8, Option<f32>)], down: u32) -> Vec<(u8, Option<f32>)> {
        others.iter().copied().filter(|&(s, _)| s >= 32 || down & (1 << s) == 0).collect()
    }

    /// Keep one we took? (hysteresis: let go only clearly past the rule: our key counts
    /// another `TIE_M` less; two players: the host up to 10 m farther, the client while
    /// nearer, 50 m at most)
    pub fn keep(me: u8, d_me: f32, others: &[(u8, Option<f32>)]) -> bool {
        d_me <= FAR_M && beats(me, key(me, d_me) - TIE_M, others)
    }

    /// Dwell for giving back: `beyond` = how long `keep` has been false without a break
    /// (None = it holds). Pure: true once it has stayed beyond for `GIVE_BACK_AFTER`.
    pub fn give_back_due(beyond: Option<Duration>) -> bool {
        beyond.is_some_and(|d| d >= GIVE_BACK_AFTER)
    }

    /// Drop a conflicting one now? `owners` = the other slots that list it as owned. A
    /// lower slot among them: at once; else (we are the lowest) after `HOST_CONFLICT_DROP`.
    /// v8.30b: the host drops a non-boss at once (the friend's game-native ownership cannot be
    /// dropped, so a double claim only desyncs the AI); bosses keep the old sticky rules.
    pub fn conflict_drop(me: u8, owners: &[u8], conflict_for: Duration, is_boss: bool) -> bool {
        if me == 0 && !is_boss && !owners.is_empty() {
            return true;
        }
        owners.iter().any(|&s| s < me) || conflict_for >= HOST_CONFLICT_DROP
    }

    /// v8.30b: after a conflict drop the host does not retake that enemy for this long.
    pub const RETAKE_COOLDOWN: Duration = Duration::from_secs(10);
    /// Minimum gap between two "given back (another player owns it)" lines per netId.
    const GIVE_LOG_GAP: Duration = Duration::from_secs(5);

    /// Drop a held enemy because the game's own IsOwnedHere answer turned nonzero? Never for a
    /// host boss (v8.25a: our forced bit and a nonzero native answer both return 1 through the
    /// detour, so dropping only made the boss flip-flop and warp on the friend).
    pub fn drop_on_native(me: u8, is_boss: bool) -> bool {
        !(me == 0 && is_boss)
    }

    /// Minimum gap between two native-change log lines per boss.
    const BOSS_DIAG_GAP: Duration = Duration::from_secs(5);

    #[derive(Default)]
    pub struct Fill {
        /// netId -> (entity, since, conflict since, beyond the keep rule since)
        pub ours: HashMap<u16, (i32, Instant, Option<Instant>, Option<Instant>)>,
        /// netId -> the game's last native owner answer seen for a held boss.
        pub native: HashMap<u16, u8>,
        /// netId -> last time a rate-limited boss diagnostic line was logged.
        pub diag_at: HashMap<u16, Instant>,
        /// netId -> time of a conflict drop ("another player owns it"); no retake for `RETAKE_COOLDOWN`.
        pub cooldown: HashMap<u16, Instant>,
        /// netId -> last time a conflict "given back" line was logged.
        pub give_log_at: HashMap<u16, Instant>,
        pub at: Option<Instant>,
        pub taken: u32,
        pub given: u32,
        pub logs: u32,
    }

    impl Fill {
        pub fn reset(&mut self) {
            clear_all();
            set_boss_forced(Vec::new());
            self.ours.clear();
            self.native.clear();
            self.diag_at.clear();
            self.cooldown.clear();
            self.give_log_at.clear();
            self.logs = 0;
            self.taken = 0;
            self.given = 0;
        }

        fn log(&mut self, s: String) {
            self.logs += 1;
            // A boss line is never rate limited (v8.12: the 30-line cap hid the boss owner flips
            // that made it jump between the players).
            if self.logs <= 30 || self.logs % 50 == 0 || s.contains("(boss)") {
                log::line(&s);
            }
        }

        /// Every 250 ms while engaged (stage >= 2, the client after its first 23). `others`:
        /// the other engaged players (slot, native remote position); `theirs`: their Owned lists.
        pub fn step(&mut self, me: u8, player: Chr, others: &[(u8, Option<[f32; 3]>)], theirs: Option<&[(u8, Vec<u16>)]>) {
            if TRAMP.load(Ordering::Acquire) == 0 {
                return;
            }
            let now = Instant::now();
            if self.at.is_some_and(|t| now.duration_since(t) < Duration::from_millis(250)) {
                return;
            }
            self.at = Some(now);
            // No Owned list from every engaged player yet: we cannot tell what they own.
            let Some(theirs) = theirs else { return };
            let Some(w) = wcm() else { return };
            let my_pos = player.position();
            let owners_of = |n: u16| -> Vec<u8> {
                theirs.iter().filter(|(_, ids)| ids.binary_search(&n).is_ok()).map(|(s, _)| *s).collect()
            };
            let dists = |p: [f32; 3]| -> (f32, Vec<(u8, Option<f32>)>) {
                let d_me = my_pos.map_or(f32::MAX, |m| crate::game::distance(m, p));
                (d_me, others.iter().map(|&(s, o)| (s, o.map(|o| crate::game::distance(o, p)))).collect())
            };
            // v8.51: minibosses too (two or more lives): Headless 1500680 and Shichimen 1700520
            // were "claimed by both machines" through the game's own distribution (live
            // 2026-10-27, v8.50), as only table bosses were forced to the host.
            let is_boss = |c: Chr| {
                c.entity_id().is_some_and(|e| crate::bosses::encounter_of(e).is_some())
                    || crate::livesync::multi_life(crate::livesync::lives0_of(c))
            };
            // We wait dead in a boss fight: a living player owns the boss (it gets the deathblow).
            let me_down = crate::bossrules::spectating();
            let down = crate::world::down_slots();
            let mut alive_ids = HashMap::new();
            for c in enemies() {
                let Some(n) = netai::net_id(c) else { continue };
                if c.hp().is_none_or(|hp| hp <= 0) {
                    continue;
                }
                let Some(p) = c.position() else { continue };
                alive_ids.insert(n, (c, p));
            }
            // Ours first: still needed?
            let mut drop: Vec<(u16, &'static str)> = Vec::new();
            let mut diag: Vec<String> = Vec::new();
            for (&n, (_, _, conflict, beyond)) in self.ours.iter_mut() {
                let Some(&(c, p)) = alive_ids.get(&n) else {
                    drop.push((n, "dead or unloaded"));
                    continue;
                };
                let boss = is_boss(c);
                let go = game_owned(w, n);
                if boss {
                    let old = self.native.insert(n, go);
                    if old.is_some_and(|o| o != go)
                        && self.diag_at.get(&n).is_none_or(|t| now.duration_since(*t) >= BOSS_DIAG_GAP)
                    {
                        self.diag_at.insert(n, now);
                        diag.push(format!(
                            "ownfill: BOSS (boss) netId {n:#x} native owner answer {}->{go} while forced (sticky: kept)",
                            old.unwrap_or(0)
                        ));
                    }
                }
                if boss && me_down {
                    drop.push((n, "we are dead (spectating): a living player takes it"));
                    continue;
                }
                if go != 0 && drop_on_native(me, boss) {
                    drop.push((n, "the game gives it to us now"));
                    continue;
                }
                let owners = owners_of(n);
                if !owners.is_empty() {
                    let since = *conflict.get_or_insert(now);
                    // The host keeps a boss in a conflict: the client (higher slot) gives it up at once.
                    if !(boss && me == 0) && conflict_drop(me, &owners, now.duration_since(since), boss) {
                        drop.push((n, "another player owns it"));
                    }
                    continue;
                }
                *conflict = None;
                let (d_me, ds) = dists(p);
                let kept = if boss { keep_boss(me, d_me, &boss_rivals(&ds, down)) } else { keep(me, d_me, &ds) };
                if kept {
                    *beyond = None;
                } else if give_back_due(Some(now.duration_since(*beyond.get_or_insert(now)))) {
                    drop.push((n, "another player is nearer / we are far"));
                }
            }
            for l in diag {
                self.log(l);
            }
            for (n, why) in drop {
                set(n, false);
                self.native.remove(&n);
                self.diag_at.remove(&n);
                if let Some((e, since, ..)) = self.ours.remove(&n) {
                    self.given += 1;
                    let tag = if crate::bosses::encounter_of(e).is_some() { " (boss)" } else { "" };
                    if !tag.is_empty() {
                        let (d, pos, hp) = alive_ids.get(&n).map_or((f32::NAN, None, None), |&(c, p)| {
                            (my_pos.map_or(f32::NAN, |m| crate::game::distance(m, p)), Some(p), c.hp())
                        });
                        let o = owners_of(n);
                        self.log(format!(
                            "ownfill: BOSS (boss) netId {n:#x} ent {e} drop why={why} game_owned={} forced={} d_me={d:.0} others={:?} pos={pos:?} hp={hp:?} owners={o:?}",
                            game_owned(w, n),
                            forced(n),
                            others.iter().map(|&(s, o)| (s, o.map(|o| my_pos.map_or(f32::NAN, |m| crate::game::distance(m, o))))).collect::<Vec<_>>()
                        ));
                    }
                    let line = format!(
                        "ownfill: enemy {e}{tag} netId {n:#x} given back after {} ms ({why})",
                        now.duration_since(since).as_millis()
                    );
                    if why == "another player owns it" {
                        self.cooldown.insert(n, now);
                        // Bypasses the 30-line cap; one per netId per 5 s.
                        if self.give_log_at.get(&n).is_none_or(|t| now.duration_since(*t) >= GIVE_LOG_GAP) {
                            self.give_log_at.insert(n, now);
                            log::line(&line);
                        }
                    } else {
                        self.log(line);
                    }
                }
            }
            self.cooldown.retain(|_, t| now.duration_since(*t) < RETAKE_COOLDOWN);
            self.give_log_at.retain(|_, t| now.duration_since(*t) < Duration::from_secs(60));
            // New ones: alive, near, nobody owns them.
            for (&n, &(c, p)) in &alive_ids {
                if self.cooldown.contains_key(&n) {
                    continue;
                }
                let boss = is_boss(c);
                if boss && me_down {
                    continue;
                }
                let host_boss = me == 0 && boss;
                if self.ours.contains_key(&n) || (game_owned(w, n) != 0 && !host_boss) || !owners_of(n).is_empty() {
                    continue;
                }
                let (d_me, ds) = dists(p);
                let take = if boss { wanted_boss(me, d_me, &boss_rivals(&ds, down)) } else { wanted(me, d_me, &ds) };
                if !take {
                    continue;
                }
                set(n, true);
                let e = c.entity_id().unwrap_or(-1);
                if boss {
                    self.native.insert(n, game_owned(w, n));
                    self.log(format!(
                        "ownfill: BOSS (boss) netId {n:#x} ent {e} take why=wanted_boss game_owned={} forced={} d_me={d_me:.0} others={:?} pos={p:?} hp={:?} owners=[]",
                        game_owned(w, n),
                        forced(n),
                        ds,
                        c.hp()
                    ));
                }
                self.ours.insert(n, (e, now, None, None));
                self.taken += 1;
                let near = ds.iter().filter_map(|&(_, d)| d).fold(f32::MAX, f32::min);
                let tag = if is_boss(c) { " (boss)" } else { "" };
                self.log(format!(
                    "ownfill: enemy {e}{tag} netId {n:#x} owned by nobody, {d_me:.0} m from us, {} from the nearest other player (interval {:?}): runs here now",
                    if near < f32::MAX { format!("{near:.0} m") } else { "?".to_string() },
                    read::<i32>(c.0 + 0x110)
                ));
            }
            // Host: publish the boss members we hold forced (`host_candidates` strips them
            // from every client's candidate list).
            if me == 0 {
                let held: Vec<u16> = self
                    .ours
                    .iter()
                    .filter(|&(n, v)| crate::bosses::encounter_of(v.0).is_some() || alive_ids.get(n).is_some_and(|&(c, _)| is_boss(c)))
                    .map(|(&n, _)| n)
                    .collect();
                set_boss_forced(held);
            }
        }
    }
}

/// netIds of boss / illusion members the host holds forced (ownfill). The friend's own
/// type-22 candidate list still names them, so the game's distribution kept handing the
/// same boss to the friend (live 2026-10-26: `claimed by both machines`).
static BOSS_FORCED: Mutex<Vec<u16>> = Mutex::new(Vec::new());
static STRIPPED_BOSS: AtomicU32 = AtomicU32::new(0);

fn set_boss_forced(mut v: Vec<u16>) {
    v.sort_unstable();
    if let Ok(mut g) = BOSS_FORCED.lock() {
        *g = v;
    }
}

/// The netIds a client may not keep: the contested / claimed ones (`base`) plus the
/// host's forced boss members. Pure; sorted and deduplicated.
fn blocked_with_bosses(mut base: Vec<u16>, boss_forced: &[u16]) -> Vec<u16> {
    base.extend_from_slice(boss_forced);
    base.sort_unstable();
    base.dedup();
    base
}

/// Type 18 (throw / grab / deathblow, `+0x20` defender netId, `+0x22` attacker) seen lately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ThrowNote {
    defender: u16,
    at: u64,
    /// Sent by us (else received from another machine).
    sent: bool,
    /// The attacker is the sending machine's own player: our player (sent) or that
    /// origin's player (received). A deathblow / backstab / plunge, not an enemy's grab.
    by_player: bool,
    /// The machine whose packet it was: our slot (sent) or the origin slot (received).
    slot: u8,
}

const NO_THROW: ThrowNote = ThrowNote { defender: NO_NET_ID, at: 0, sent: false, by_player: false, slot: 0 };
static THROWS: Mutex<[ThrowNote; 16]> = Mutex::new([NO_THROW; 16]);
static THROW_AT: AtomicUsize = AtomicUsize::new(0);
const THROW_DEFENDER: usize = 0x20;
const THROW_ATTACKER: usize = 0x22;
const THROW_ID: usize = 0x24;
/// A received throw on an enemy our own player started a throw on this recently is not
/// given to the game (IMPLEMENTATION-NOTES §34: two deathblows at once cancel each other).
const THROW_COLLIDE_MS: u64 = 3000;
/// Our player's throw on an enemy claims it this long (the deathblow runs on our AI).
const THROW_CLAIM_MS: u64 = 4000;
static THROW_DROPS: AtomicU32 = AtomicU32::new(0);
static THROW_DROP_LOGS: AtomicU32 = AtomicU32::new(0);
static THROW_CLAIM_LOGS: AtomicU32 = AtomicU32::new(0);

/// An enemy netId (not a player in any form, not "none").
fn enemy_net_id(n: u16) -> bool {
    n != NO_NET_ID && n & 0xFFF0 != WIRE_SLOT && !pnet_map(false).contains(&n) && !pnet_map(true).contains(&n)
}

/// The local player netIds of `slot`'s player (both forms; `NO_NET_ID` = none here).
fn slot_pnets(slot: u8) -> [u16; 2] {
    let s = (slot as usize).min(SLOTS - 1);
    [pnet_map(false)[s], pnet_map(true)[s]]
}

/// A throw with a remote player (any peer's, here a native remote) as attacker or defender
/// keeps that peer's native pin off this long: the game owns both characters' positions
/// for the paired animation (native.rs `follow_peers`; v7.11 puppet.rs for the friend).
const REMOTE_THROW_MS: u64 = 4000;
/// Per slot: `now_ms` until which that slot's player (its remote here) is in a throw
/// (0 = never). Our own slot is never set.
static REMOTE_THROW_UNTIL: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static THROW_EVIDENCE_LOGS: AtomicU32 = AtomicU32::new(0);

/// True while `slot`'s player (its native remote here) is part of a throw.
pub fn remote_in_throw(slot: u8) -> bool {
    REMOTE_THROW_UNTIL
        .get(slot as usize)
        .is_some_and(|u| throw_window_active(u.load(Ordering::Relaxed), crate::game::now_ms()))
}

fn throw_window_active(until: u64, now: u64) -> bool {
    until != 0 && now < until
}

/// Which other slots' players are the attacker or defender (bit per slot; never `me`).
/// Sent packets are in wire form (`WIRE_SLOT | slot`); received ones carry our local
/// netIds, `pnets[slot]` = both forms of that slot's remote here.
fn throw_remote_slots(sent: bool, attacker: Option<u16>, defender: u16, me: u8, pnets: &[[u16; 2]; SLOTS]) -> u8 {
    let slot_of = |n: u16| -> Option<usize> {
        if n == NO_NET_ID {
            None
        } else if sent {
            (n & 0xFFF0 == WIRE_SLOT).then_some((n & 0xF) as usize).filter(|&s| s < SLOTS)
        } else {
            pnets.iter().position(|p| p.contains(&n))
        }
    };
    let mut mask = 0u8;
    for n in [Some(defender), attacker].into_iter().flatten() {
        if let Some(s) = slot_of(n).filter(|&s| s != me as usize) {
            mask |= 1 << s;
        }
    }
    mask
}

/// Send hook / serve (game threads): note the defender of a throw. Never blocks. `d` is
/// the wire form when sent (our player = `WIRE_SLOT | ME`), ours when received from
/// `origin`.
fn note_throw(d: &[u8], sent: bool, origin: u8) {
    let Some(defender) = get_u16(d, THROW_DEFENDER) else { return };
    let attacker = get_u16(d, THROW_ATTACKER);
    let me = ME.load(Ordering::Relaxed);
    let by_player = match attacker {
        Some(a) if sent => a == WIRE_SLOT | me as u16,
        Some(a) => a != NO_NET_ID && slot_pnets(origin).contains(&a),
        None => false,
    };
    let now = crate::game::now_ms();
    let pnets: [[u16; 2]; SLOTS] = std::array::from_fn(|s| slot_pnets(s as u8));
    let remotes = throw_remote_slots(sent, attacker, defender, me, &pnets);
    for (s, until) in REMOTE_THROW_UNTIL.iter().enumerate() {
        if remotes & (1 << s) != 0 {
            until.store(now + REMOTE_THROW_MS, Ordering::Relaxed);
        }
    }
    // 2026-10-24: always logged (throws are rare) so the attacker field can be checked live.
    if THROW_EVIDENCE_LOGS.fetch_add(1, Ordering::Relaxed) < 200 {
        log::line(&format!(
            "throw: type 18 {} defender {defender:#x}, attacker {}, throw id {}{}",
            if sent { "sent".to_string() } else { format!("received from peer {origin}") },
            attacker.map_or("?".to_string(), |a| format!("{a:#x}")),
            get_u16(d, THROW_ID).unwrap_or(0),
            if remotes != 0 { format!(" (remote slots {remotes:#08b} involved: their native pins released)") } else { String::new() }
        ));
    }
    throw_probe::add(defender, sent, now);
    let note = ThrowNote { defender, at: now, sent, by_player, slot: if sent { me } else { origin } };
    if let Ok(mut g) = THROWS.try_lock() {
        let i = THROW_AT.fetch_add(1, Ordering::Relaxed) % 16;
        g[i] = note;
    }
    if sent && by_player && enemy_net_id(defender) && THROW_CLAIMS_ON.load(Ordering::Relaxed) {
        // A new claim (our deathblow): the candidates / the distribution go out now.
        CLAIMS_GREW.store(true, Ordering::Relaxed);
        THROW_CLAIMS.fetch_add(1, Ordering::Relaxed);
        if THROW_CLAIM_LOGS.fetch_add(1, Ordering::Relaxed) < 40 {
            log::line(&format!(
                "stealth: our player's throw {} on enemy netId {defender:#x}: claimed for {} s (the deathblow runs on our AI)",
                get_u16(d, THROW_ID).unwrap_or(0),
                THROW_CLAIM_MS / 1000
            ));
        }
    }
}

/// A throw with `net_id` as defender within `within_ms`: Some(sent by us).
pub fn recent_throw(net_id: u16, within_ms: u64) -> Option<bool> {
    let now = crate::game::now_ms();
    let g = THROWS.lock().ok()?;
    let mut hit = None;
    for t in g.iter() {
        if t.defender == net_id && t.at != 0 && now.saturating_sub(t.at) <= within_ms {
            hit = Some(hit.unwrap_or(false) || t.sent);
        }
    }
    hit
}

/// Age (ms) of the newest player throw on `defender` within `within_ms`, ours (`sent`)
/// or another machine's.
fn player_throw_age(ring: &[ThrowNote], defender: u16, sent: bool, now: u64, within_ms: u64) -> Option<u64> {
    ring.iter()
        .filter(|t| t.defender == defender && t.sent == sent && t.by_player && t.at != 0 && t.at <= now)
        .map(|t| now - t.at)
        .filter(|&age| age <= within_ms)
        .min()
}

/// Age (ms) of the newest throw of `slot`'s player on `defender` within `within_ms`
/// (our own slot = our sent throws).
fn slot_throw_age(ring: &[ThrowNote], defender: u16, slot: u8, now: u64, within_ms: u64) -> Option<u64> {
    ring.iter()
        .filter(|t| t.defender == defender && t.slot == slot && t.by_player && t.at != 0 && t.at <= now)
        .map(|t| now - t.at)
        .filter(|&age| age <= within_ms)
        .min()
}

/// Enemies our own player started a throw on within `within_ms` (sorted, unique).
fn our_throw_targets(ring: &[ThrowNote], now: u64, within_ms: u64) -> Vec<u16> {
    let mut v: Vec<u16> = ring
        .iter()
        .filter(|t| t.sent && t.by_player && t.at != 0 && t.at <= now && now - t.at <= within_ms)
        .map(|t| t.defender)
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// serve, a received throw of another machine's player (`remote` = that player's netIds
/// here): drop it when our own player started a throw on the same enemy within
/// `THROW_COLLIDE_MS` (Some(age of ours)).
fn throw_collides(ring: &[ThrowNote], d: &[u8], remote: &[u16], now: u64) -> Option<u64> {
    let defender = get_u16(d, THROW_DEFENDER)?;
    let attacker = get_u16(d, THROW_ATTACKER)?;
    if attacker == NO_NET_ID || !remote.contains(&attacker) {
        return None;
    }
    player_throw_age(ring, defender, true, now, THROW_COLLIDE_MS)
}

/// One machine claiming a contested enemy (host `contest`).
#[derive(Clone, Copy, Debug)]
struct Contender {
    slot: u8,
    /// Its player's throw (deathblow) on the enemy: ms ago.
    throw_age: Option<u64>,
    /// Its player's horizontal distance to the enemy.
    dist: Option<f32>,
}

/// Host: who runs an enemy several machines claim (host first, then the clients in slot
/// order). The candidates meet one by one: the one kept so far against the next, by
/// `host_wins_contest` (a deathblow first, the earlier one, the one kept so far on a tie;
/// else the nearer player with `CONTEST_HYST_M` for the last winner `held`). Two players:
/// exactly `host_wins_contest` for the host.
fn contest_winner(cands: &[Contender], held: Option<u8>) -> u8 {
    let Some(mut best) = cands.first().copied() else { return 0 };
    for c in &cands[1..] {
        if !host_wins_contest(best.throw_age, c.throw_age, best.dist, c.dist, held == Some(best.slot)) {
            best = *c;
        }
    }
    best.slot
}

// --- Deathblow lock: one deathblow per enemy, first come (IMPLEMENTATION-NOTES §39) ---
//
// Whoever starts a deathblow / backstab / plunge (a player's type 18 on an enemy) first
// holds that enemy's lock for `dblock::LOCK_MS`. Every machine learns of it from the type 18
// the game already sends (no new message): our 18 = our lock, peer s's 18 = s's lock. While
// any other player holds it our game cannot start one: the defender gate `0xBE3810` (IDA,
// see `dblock`) answers "not throwable" for that enemy to our own player only, so no red
// marker and no throw. Collisions (several started inside the latency window), N players
// through the star (PLAN-MULTIPLAYER §5.20):
// - host vs a client: v7.14's pairwise rule on both machines (the earlier start wins, the
//   peer's start = arrival - RTT/2 of that link; within `TIE_MS` the host's);
// - client vs client: the host decides, by arrival at the host (the one clock everybody
//   shares): it applies and relays only the winner's 18, drops the loser's;
// - a client takes a relayed client 18 as the host's verdict (apply it; ours gives way) and
//   drops the host's own 18 while the host has given way to another client.
// The loser applies the winner's 18 (it cuts its own throw off before its hit, §34), so one
// deathblow plays on every screen and one life goes (livesync takes the lower lives).

/// Master switch (no ini key: `ns_throw_collide_drop` is the old, one-sided form).
const DEATHBLOW_LOCK: bool = true;

pub(crate) mod dblock {
    use super::*;

    /// A deathblow holds its enemy this long after the holder's latest type 18 (the two
    /// packets of one deathblow, e.g. 1200 / 1205, come within ~1 s; the animation ~3 s).
    pub const LOCK_MS: u64 = 4000;
    /// Two starts closer than this (after the latency estimate) are a tie: the host's wins.
    pub const TIE_MS: u64 = 120;
    /// RTT assumed until the link has measured one.
    pub const DEFAULT_RTT_MS: u32 = 100;
    /// Client: the host's 18 while another client holds the enemy is a leftover of the
    /// host's cut deathblow only this soon after that client's latest 18 (later it is a new
    /// deathblow after the lock ran out on the host, whose clock runs ahead of ours).
    pub const LEFTOVER_MS: u64 = 2000;
    const MAX_LOCKS: usize = 32;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Holder {
        Us,
        /// That session slot's player (0 = the host).
        Peer(u8),
    }

    /// One enemy's lock: who holds it, when the holder's deathblow started (our clock), its
    /// latest packet and until when it holds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Lock {
        pub net: u16,
        pub holder: Holder,
        pub start: u64,
        pub last: u64,
        pub until: u64,
    }

    /// Our own player's throw on an enemy was sent.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Ours {
        /// The lock is ours now.
        New,
        /// Ours already (the deathblow's next packet): extended.
        Held,
        /// Another player holds it (the gate veto missed: not hooked, or the same frame).
        PeerHolds(Lock),
    }

    /// Another player's throw on an enemy arrived.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Theirs {
        /// The lock is that player's now: apply it.
        New,
        /// That player's already: apply it.
        Held,
        /// Ours started first (`lead` ms, estimated; or a tie the host takes): drop it.
        WeKeep { lead: i64, tie: bool },
        /// Theirs started first: apply it, ours gives way (the lock is theirs now).
        PeerFirst { lead: i64, tie: bool },
        /// Another peer holds the enemy (host: that client reached us first; client: the
        /// host gave way to it): drop it.
        OtherKeeps(u8),
        /// Client: a client's 18 relayed by the host = the host's verdict: apply it (the
        /// previous holder, ours included, gives way).
        Verdict(Holder),
    }

    impl Theirs {
        /// Hand it to the game (and, on the host, relay it).
        pub fn applies(self) -> bool {
            !matches!(self, Theirs::WeKeep { .. } | Theirs::OtherKeeps(_))
        }
    }

    /// Who started first. `lead` = their start - our start (> 0: ours was earlier). Within
    /// `tie_ms` the host's deathblow wins on both machines.
    pub fn first_come(our_start: u64, their_start: u64, host: bool, tie_ms: u64) -> (bool, i64, bool) {
        let lead = their_start as i64 - our_start as i64;
        if lead.unsigned_abs() <= tie_ms {
            (host, lead, true)
        } else {
            (lead > 0, lead, false)
        }
    }

    /// The peer's start in our clock: its packet arrived `now`, one way takes RTT / 2.
    pub fn their_start(now: u64, rtt_ms: Option<u32>) -> u64 {
        now.saturating_sub(rtt_ms.unwrap_or(DEFAULT_RTT_MS) as u64 / 2)
    }

    #[derive(Debug, Default)]
    pub struct Table {
        pub locks: Vec<Lock>,
    }

    impl Table {
        pub const fn new() -> Self {
            Table { locks: Vec::new() }
        }

        pub fn active(&self, net: u16, now: u64) -> Option<Lock> {
            self.locks.iter().find(|l| l.net == net && now < l.until).copied()
        }

        fn put(&mut self, l: Lock) {
            self.locks.retain(|o| o.net != l.net);
            if self.locks.len() >= MAX_LOCKS
                && let Some(i) = self.locks.iter().enumerate().min_by_key(|(_, o)| o.until).map(|(i, _)| i)
            {
                self.locks.swap_remove(i);
            }
            self.locks.push(l);
        }

        fn take(&mut self, net: u16, holder: Holder, start: u64, now: u64) {
            self.put(Lock { net, holder, start, last: now, until: now + LOCK_MS });
        }

        fn extend(&mut self, mut l: Lock, now: u64) {
            l.last = now;
            l.until = l.until.max(now + LOCK_MS);
            self.put(l);
        }

        pub fn on_ours(&mut self, net: u16, now: u64) -> Ours {
            match self.active(net, now) {
                Some(l) if l.holder != Holder::Us => Ours::PeerHolds(l),
                Some(l) => {
                    self.extend(l, now);
                    Ours::Held
                }
                None => {
                    self.take(net, Holder::Us, now, now);
                    Ours::New
                }
            }
        }

        /// Peer `from`'s throw (`start` = its start in our clock, `their_start`). `host` =
        /// we are the host. A client hears every other client's 18 through the host.
        pub fn on_theirs(&mut self, net: u16, from: u8, now: u64, start: u64, host: bool) -> Theirs {
            let peer = Holder::Peer(from);
            let Some(l) = self.active(net, now) else {
                self.take(net, peer, start, now);
                return Theirs::New;
            };
            match l.holder {
                h if h == peer => {
                    self.extend(l, now);
                    Theirs::Held
                }
                // The host's or the host's 18 against our lock: the pairwise rule (both
                // machines of the pair compute the same winner).
                Holder::Us if host || from == 0 => {
                    let (we_keep, lead, tie) = first_come(l.start, start, host, TIE_MS);
                    if we_keep {
                        Theirs::WeKeep { lead, tie }
                    } else {
                        self.take(net, peer, start, now);
                        Theirs::PeerFirst { lead, tie }
                    }
                }
                // Host: another client reached us first and keeps it.
                Holder::Peer(other) if host => Theirs::OtherKeeps(other),
                // Client: the host's own 18 while it has given way to another client (a
                // leftover packet of its cut deathblow).
                Holder::Peer(other) if from == 0 && now.saturating_sub(l.last) < LEFTOVER_MS => Theirs::OtherKeeps(other),
                Holder::Peer(_) if from == 0 => {
                    self.take(net, peer, start, now);
                    Theirs::New
                }
                // Client: a client's 18 relayed by the host = the winner.
                prev => {
                    self.take(net, peer, start, now);
                    Theirs::Verdict(prev)
                }
            }
        }

        /// Drops the locks that ran out; returns them.
        pub fn expire(&mut self, now: u64) -> Vec<Lock> {
            let (gone, kept): (Vec<Lock>, Vec<Lock>) = self.locks.iter().partition(|l| now >= l.until);
            self.locks = kept;
            gone
        }
    }

    // --- Live state ---------------------------------------------------------------------

    static TABLE: Mutex<Table> = Mutex::new(Table::new());
    /// netIds another player holds (read by the gate detour on game threads: atomics only).
    static ANY: AtomicBool = AtomicBool::new(false);
    static BITS: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];
    /// Vetoes already queued for a log line during the current lock.
    static LOGGED: [AtomicU64; 1024] = [const { AtomicU64::new(0) }; 1024];
    static PENDING_LOG: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
    /// Our own player's ChrIns (the only attacker the gate vetoes for).
    static PLAYER: AtomicUsize = AtomicUsize::new(0);
    /// RTT per link slot in ms (0 = not measured yet); a client uses the host link's.
    static RTT: [AtomicU32; SLOTS] = [const { AtomicU32::new(0) }; SLOTS];
    static TRAMP: AtomicUsize = AtomicUsize::new(0);
    static VETOES: AtomicU32 = AtomicU32::new(0);
    static DROPPED: AtomicU32 = AtomicU32::new(0);
    static GAVE_WAY: AtomicU32 = AtomicU32::new(0);
    static OURS_NEW: AtomicU32 = AtomicU32::new(0);
    static PEERS_NEW: AtomicU32 = AtomicU32::new(0);
    static MISSED: AtomicU32 = AtomicU32::new(0);
    static LOGS: AtomicU32 = AtomicU32::new(0);
    static STATS_AT: AtomicU64 = AtomicU64::new(0);
    const MAX_LOGS: u32 = 80;
    const STATS_MS: u64 = 10_000;

    /// `0xBE3810(attacker chr, defender chr, ThrowKindParam row)`: the defender-side gate of
    /// every player / AI throw candidate (only caller `0xBE2A30`, the candidate collection
    /// of `0xBDE5C0` (input path) and `0xBDE1C0`). False = no candidate: no deathblow
    /// marker (`0xBE2D60` applies it only for a candidate) and no throw [VAR, IDA o1/o2].
    const GATE_RVA: usize = 0xBE3810;
    /// `mov [rsp+18h], rbx; push rbp/rsi/rdi/r12/r13/r14/r15` (16 bytes, nothing relative).
    const GATE_PROLOGUE: [u8; 16] = [0x48, 0x89, 0x5C, 0x24, 0x18, 0x55, 0x56, 0x57, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57];
    const CHR_NET_ID: usize = 0x78;

    fn bit(n: u16) -> (usize, u64) {
        ((n >> 6) as usize, 1u64 << (n & 63))
    }

    /// Another player (any peer) holds `n`.
    pub fn peer_locked(n: u16) -> bool {
        let (i, m) = bit(n);
        ANY.load(Ordering::Acquire) && BITS[i].load(Ordering::Relaxed) & m != 0
    }

    fn set_peer(n: u16, on: bool) {
        let (i, m) = bit(n);
        if on {
            BITS[i].fetch_or(m, Ordering::Relaxed);
            ANY.store(true, Ordering::Release);
        } else {
            BITS[i].fetch_and(!m, Ordering::Relaxed);
            LOGGED[i].fetch_and(!m, Ordering::Relaxed);
        }
    }

    fn log_limited(s: String) {
        if LOGS.fetch_add(1, Ordering::Relaxed) < MAX_LOGS {
            log::line(&s);
        }
    }

    fn who(h: Holder) -> String {
        match h {
            Holder::Us => "us".into(),
            Holder::Peer(0) => "the host (peer 0)".into(),
            Holder::Peer(s) => format!("peer {s}"),
        }
    }

    extern "C" fn gate(atk: usize, def: usize, kind: usize) -> u64 {
        let t = TRAMP.load(Ordering::Acquire);
        // SAFETY: the trampoline (or the original once unhooked) has this signature.
        let f: extern "C" fn(usize, usize, usize) -> u64 = unsafe { std::mem::transmute(if t != 0 { t } else { module_base() + GATE_RVA }) };
        let r = f(atk, def, kind);
        if r & 0xFF == 0 || !ANY.load(Ordering::Acquire) || atk == 0 || atk != PLAYER.load(Ordering::Relaxed) || def < 0x10000 || !crate::mod_alive() {
            return r;
        }
        // SAFETY: the original just read this ChrIns (`def+0x1FF8`, `def+8`); `+0x78` is its
        // netId (netai::net_id).
        let n = unsafe { ((def + CHR_NET_ID) as *const u16).read_volatile() };
        if !peer_locked(n) {
            return r;
        }
        VETOES.fetch_add(1, Ordering::Relaxed);
        let (i, m) = bit(n);
        if LOGGED[i].fetch_or(m, Ordering::Relaxed) & m == 0 {
            for s in &PENDING_LOG {
                if s.compare_exchange(0, 0x1_0000 | n as u32, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
                    break;
                }
            }
        }
        0
    }

    /// nsession hook install (title screen). Not fatal: without it only the collision
    /// rule below runs (the second deathblow can still start, it is dropped / cut).
    pub fn install() -> bool {
        if !DEATHBLOW_LOCK {
            return false;
        }
        if TRAMP.load(Ordering::Acquire) != 0 {
            return true;
        }
        let d: extern "C" fn(usize, usize, usize) -> u64 = gate;
        match hook::install_detour(GATE_RVA, &GATE_PROLOGUE, d as usize, "throw defender gate (0xBE3810)") {
            Some(t) => {
                TRAMP.store(t, Ordering::Release);
                true
            }
            None => false,
        }
    }

    /// A player's throw on an enemy in wire form: `(defender netId, throw id)` when the
    /// attacker is `slot`'s player (`WIRE_SLOT | slot`).
    pub fn player_throw_on_enemy(d: &[u8], slot: u8) -> Option<(u16, u16)> {
        let attacker = WIRE_SLOT | slot as u16;
        let defender = get_u16(d, THROW_DEFENDER)?;
        (get_u16(d, THROW_ATTACKER)? == attacker && (slot as usize) < SLOTS && enemy_net_id(defender))
            .then(|| (defender, get_u16(d, THROW_ID).unwrap_or(0)))
    }

    /// route_send (game thread), our outgoing type 18 (wire form).
    pub fn on_sent(d: &[u8]) {
        if !DEATHBLOW_LOCK {
            return;
        }
        let Some((n, id)) = player_throw_on_enemy(d, ME.load(Ordering::Relaxed)) else { return };
        let now = crate::game::now_ms();
        let Ok(mut g) = TABLE.try_lock() else { return };
        match g.on_ours(n, now) {
            Ours::New => {
                OURS_NEW.fetch_add(1, Ordering::Relaxed);
                log_limited(format!("deathblow: enemy {n:#x} locked by us (our throw {id}, {} s; the other players' attempts on it will not start)", LOCK_MS / 1000));
            }
            Ours::Held => {}
            Ours::PeerHolds(l) => {
                MISSED.fetch_add(1, Ordering::Relaxed);
                log_limited(format!(
                    "deathblow: our throw {id} on enemy {n:#x} started although {} holds its lock (since {} ms; gate {}): their side keeps its own",
                    who(l.holder),
                    now.saturating_sub(l.start),
                    if TRAMP.load(Ordering::Acquire) != 0 { "hooked, same frame?" } else { "not hooked" }
                ));
            }
        }
    }

    /// on_game (wire form, before the host relays it): `origin`'s type 18. False = do not
    /// give it to the game and (host) do not relay it.
    pub fn on_received(origin: u8, d: &[u8]) -> bool {
        if !DEATHBLOW_LOCK {
            return true;
        }
        let Some((n, id)) = player_throw_on_enemy(d, origin) else { return true };
        let now = crate::game::now_ms();
        let host = ROLE_HOST.load(Ordering::Relaxed);
        // The link the packet came over: the host hears the client itself; a client hears
        // the host's own player over the host link (relayed clients use the host's verdict).
        let link = if host { origin } else { 0 };
        let rtt = RTT.get(link as usize).map(|r| r.load(Ordering::Relaxed)).filter(|&r| r > 0);
        let mut g = TABLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let d = g.on_theirs(n, origin, now, their_start(now, rtt), host);
        drop(g);
        let tie_text = |lead: i64, tie: bool, ours_first: bool| {
            if tie {
                format!("both within {TIE_MS} ms ({lead:+} ms): the host's goes first")
            } else if ours_first {
                format!("ours started ~{lead} ms earlier")
            } else {
                format!("theirs started ~{} ms earlier", -lead)
            }
        };
        let rtt_ms = rtt.unwrap_or(DEFAULT_RTT_MS);
        match d {
            Theirs::New => {
                set_peer(n, true);
                PEERS_NEW.fetch_add(1, Ordering::Relaxed);
                log_limited(format!("deathblow: enemy {n:#x} locked by peer {origin} (its throw {id}; ours on it cannot start for {} s)", LOCK_MS / 1000));
            }
            Theirs::Held => set_peer(n, true),
            Theirs::WeKeep { lead, tie } => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
                THROW_DROPS.fetch_add(1, Ordering::Relaxed);
                log_limited(format!(
                    "deathblow: enemy {n:#x} locked by us: peer {origin}'s throw {id} not applied here{} ({}, rtt {rtt_ms} ms)",
                    if host { " nor relayed" } else { "" },
                    tie_text(lead, tie, true)
                ));
            }
            Theirs::PeerFirst { lead, tie } => {
                set_peer(n, true);
                GAVE_WAY.fetch_add(1, Ordering::Relaxed);
                log_limited(format!(
                    "deathblow: our attempt on {n:#x} suppressed (peer first): peer {origin}'s throw {id} applied, ours gives way ({}, rtt {rtt_ms} ms)",
                    tie_text(lead, tie, false)
                ));
            }
            Theirs::OtherKeeps(other) => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
                THROW_DROPS.fetch_add(1, Ordering::Relaxed);
                log_limited(format!(
                    "deathblow: enemy {n:#x} locked by {}: peer {origin}'s throw {id} not applied here{} ({})",
                    who(Holder::Peer(other)),
                    if host { " nor relayed" } else { "" },
                    if host { "that client reached the host first" } else { "the host gave way to it" }
                ));
            }
            Theirs::Verdict(prev) => {
                set_peer(n, true);
                if prev == Holder::Us {
                    GAVE_WAY.fetch_add(1, Ordering::Relaxed);
                }
                log_limited(format!(
                    "deathblow: enemy {n:#x}: peer {origin}'s throw {id} relayed by the host (its verdict) applied, {} gives way",
                    if prev == Holder::Us { "ours".to_string() } else { who(prev) }
                ));
            }
        }
        d.applies()
    }

    /// Every frame (nsession::tick): our player, the RTTs, expiry, the gate's log lines.
    pub fn frame(player: usize, peers: &PeerSet) {
        PLAYER.store(player, Ordering::Relaxed);
        for (s, r) in RTT.iter().enumerate() {
            let v = peers.by_id(PeerId(s as u8)).and_then(|p| p.rtt_ms).unwrap_or(0);
            r.store(v, Ordering::Relaxed);
        }
        if !DEATHBLOW_LOCK {
            return;
        }
        let now = crate::game::now_ms();
        if let Ok(mut g) = TABLE.try_lock() {
            for l in g.expire(now) {
                if l.holder != Holder::Us {
                    set_peer(l.net, false);
                }
            }
            if !g.locks.iter().any(|l| l.holder != Holder::Us) {
                ANY.store(false, Ordering::Release);
            }
        }
        for s in &PENDING_LOG {
            let v = s.swap(0, Ordering::Relaxed);
            if v != 0 {
                log_limited(format!("deathblow: our attempt on {:#x} suppressed (peer first): its deathblow marker / throw is off for our player while another player's runs", v & 0xFFFF));
            }
        }
        let at = STATS_AT.load(Ordering::Relaxed);
        if at == 0 {
            STATS_AT.store(now, Ordering::Relaxed);
        } else if now.saturating_sub(at) >= STATS_MS {
            STATS_AT.store(now, Ordering::Relaxed);
            let c = [&OURS_NEW, &PEERS_NEW, &VETOES, &DROPPED, &GAVE_WAY, &MISSED].map(|a| a.swap(0, Ordering::Relaxed));
            if c.iter().any(|&x| x > 0) {
                log::line(&format!(
                    "deathblow: 10 s: locks ours {} / other players' {}, gate vetoes {}, their throws not applied {}, ours gave way {}, ours started during another's lock {} (gate {})",
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    if TRAMP.load(Ordering::Acquire) != 0 { "hooked" } else { "not hooked" }
                ));
            }
        }
    }
}

/// A complete game packet on link `from` (unreliable or from the lane): kept while
/// engaged with that link's current generation. Host: a client's broadcast is applied
/// here and goes on to the other engaged clients.
fn on_game(from: u8, t: u8, body: &[u8]) {
    let host = ROLE_HOST.load(Ordering::Relaxed);
    match decode_raw(body) {
        Some((generation, origin, mode, d))
            if accepts(from, generation) && origin_ok(host, from, origin, ME.load(Ordering::Relaxed)) =>
        {
            // The deathblow lock decides before the host relays: a dropped 18 (another
            // player keeps the enemy) reaches neither our game nor the other clients (§39, §5.20).
            if t == T_THROW && whitelisted(t as u32) && !dblock::on_received(origin, d) {
                return;
            }
            if relays(host, mode) && whitelisted(t as u32) && !targets(true, Dst::All, origin, engaged_links()).is_empty() {
                relay(t, origin, d);
            }
            push_in(origin, t, d.to_vec());
        }
        _ => {
            bump(&DROP, t as u32);
            if let Some(p) = book().peers.get_mut(from as usize).and_then(Option::as_mut) {
                p.stale_packets += 1;
            }
        }
    }
}

/// net.rs: a PlayerRaw packet from `from` (link `epoch`). True if it was one of ours
/// (consumed whatever the setting). One lane per link.
pub fn on_raw(from: PeerId, ptype: u32, data: &[u8], epoch: u64) -> bool {
    if ptype & PTYPE_MASK != PTYPE_BASE {
        return false;
    }
    let s = from.index();
    if s >= SLOTS {
        return true;
    }
    let t = ptype & !PTYPE_MASK;
    match t {
        LANE_DATA | LANE_ACK => {
            let delivered = {
                let Ok(mut g) = LANES.lock() else { return true };
                let l = g[s].get_or_insert_with(new_lane);
                if t == LANE_ACK {
                    l.on_ack(data);
                    return true;
                }
                l.on_data(data, Instant::now())
            };
            for (key, msg) in delivered {
                if key == 0 {
                    on_control(from.slot(), &msg, epoch);
                } else if (key as usize) < TYPES {
                    on_game(from.slot(), key, &msg);
                }
            }
        }
        t if (t as usize) < TYPES => on_game(from.slot(), t as u8, data),
        _ => {}
    }
    true
}

/// net.rs, every frame (loading screens included): handshake messages and reliable
/// game packets through each link's lane, unreliable ones as plain PlayerRaw packets,
/// one copy per target link (each with our generation for it).
pub fn pump(session: &mut Session, now: Instant) {
    let up: [bool; SLOTS] = std::array::from_fn(|s| session.link_state(s as u8) == Some(coop_net::State::Connected));
    let host = ROLE_HOST.load(Ordering::Relaxed);
    let engaged = engaged_links();
    // Taken out under the lock, sent after it: the send hooks only try_lock.
    let (ctrl, unrel, rel) = {
        let Ok(mut o) = OUTBOX.lock() else { return };
        if !up.contains(&true) {
            o.ctrl.clear();
            o.unrel.clear();
            o.rel.clear();
            o.rel_bytes = 0;
            drop(o);
            // A new link gets a new lane (the peer resets its receive side on the new id).
            if let Ok(mut l) = LANES.lock() {
                for x in l.iter_mut() {
                    *x = None;
                }
            }
            return;
        }
        o.rel_bytes = 0;
        let ctrl: Vec<(u8, Vec<u8>)> = o.ctrl.drain(..).filter(|(s, _)| up.get(*s as usize) == Some(&true)).collect();
        (ctrl, o.unrel.drain(..).collect::<Vec<_>>(), o.rel.drain(..).collect::<Vec<_>>())
    };
    let generation = |s: u8| GEN_OUT[s as usize].load(Ordering::Relaxed);
    for o in unrel {
        let mode = wire_mode(host, o.dst);
        let to = targets(host, o.dst, o.origin, engaged);
        if to.is_empty() {
            bump(&DROP, o.t as u32);
        }
        for s in to {
            let frame = encode_raw(generation(s), o.origin, mode, &o.data);
            if !up[s as usize] || !session.send_player_raw_to(s, PTYPE_BASE | o.t as u32, &frame, now) {
                bump(&DROP, o.t as u32);
            }
        }
    }
    let out = {
        let Ok(mut g) = LANES.lock() else { return };
        for (s, x) in g.iter_mut().enumerate() {
            if !up[s] {
                *x = None;
            }
        }
        for (s, c) in &ctrl {
            g[*s as usize].get_or_insert_with(new_lane).queue(0, 0, c);
        }
        for o in &rel {
            let mode = wire_mode(host, o.dst);
            for s in targets(host, o.dst, o.origin, engaged) {
                if !up[s as usize] {
                    bump(&DROP, o.t as u32);
                    continue;
                }
                let l = g[s as usize].get_or_insert_with(new_lane);
                // A link that cannot keep up: behavior (the big one) is dropped first.
                if lane_class(o.t) == 2 && l.backlog() > MAX_LANE_BACKLOG {
                    bump(&DROP, o.t as u32);
                    continue;
                }
                l.queue(o.t, lane_class(o.t), &encode_raw(generation(s), o.origin, mode, &o.data));
            }
        }
        let mut out = Vec::new();
        for (s, x) in g.iter_mut().enumerate() {
            if let Some(l) = x.as_mut() {
                out.extend(l.poll(now).into_iter().map(|(data, bytes)| (s as u8, data, bytes)));
            }
        }
        out
    };
    for (s, data, bytes) in out {
        let ptype = PTYPE_BASE | if data { LANE_DATA } else { LANE_ACK };
        session.send_player_raw_to(s, ptype, &bytes, now);
    }
}

/// Fragments waiting in a lane above which new behavior packets are dropped (about 2 MB).
const MAX_LANE_BACKLOG: usize = 2048;

/// Per link with a lane: (slot, stats since the last call, backlog).
fn lane_stats() -> Vec<(u8, lane::Stats, usize)> {
    let Ok(mut g) = LANES.lock() else { return Vec::new() };
    g.iter_mut()
        .enumerate()
        .filter_map(|(s, x)| {
            x.as_mut().map(|l| {
                let st = l.stats;
                l.stats = lane::Stats::default();
                (s as u8, st, l.backlog())
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Engine (game thread): handshakes, engagements, drop rules, stats
// ---------------------------------------------------------------------------

/// What puppet.rs knows this frame.
pub struct Frame<'a> {
    pub player: Chr,
    /// The other players (their native remotes, link epochs, snapshots).
    pub peers: &'a PeerSet,
    pub connected: bool,
    pub is_host: bool,
    pub in_world_for: Duration,
    pub here: Option<i32>,
}

impl Frame<'_> {
    fn peer(&self, id: PeerId) -> Option<&PeerFrame> {
        self.peers.by_id(id)
    }

    /// That peer's native remote PlayerIns (native.rs), if any.
    fn remote_of(&self, id: PeerId) -> Option<Chr> {
        self.peer(id).and_then(|p| p.native)
    }

    /// That peer's block from its latest visible snapshot.
    fn block_of(&self, id: PeerId) -> Option<i32> {
        self.peer(id).and_then(PeerFrame::block)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Idle,
    Handshake,
    Engaged,
}

const SETTLE: Duration = Duration::from_secs(3);
/// Our world update ran this long without a stall before a handshake engages (v8.6).
const ENGAGE_STEADY: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const COOLDOWN: Duration = Duration::from_secs(5);
const NO_REMOTE_GRACE: Duration = Duration::from_secs(3);
/// Client: the first 23 must come this soon after the host's sync visibly runs (its
/// first 4 / 5 / 70 / 71 of this engagement), not after the engage itself: a host whose
/// world update is paused sends nothing at all (live 7.1, three soft fails -> latched).
const FIRST_23_WITHIN: Duration = Duration::from_secs(5);
/// Client: nothing at all from the host's sync this long after engaging = leave (not a
/// failure of the native session: no soft fail, no fallback).
const HOST_SILENT_MAX: Duration = Duration::from_secs(30);
const SOFT_FAILS_LATCH: u32 = 3;
const OWNED_EVERY: Duration = Duration::from_secs(2);
/// A peer's Owned list counts this long.
const OWNED_FRESH: Duration = Duration::from_secs(5);
/// Frames the client's late chr-loop patch may stay busy before we leave.
const LATE_BUSY_MAX: u32 = 600;
const STATS_EVERY: Duration = Duration::from_secs(10);
/// Host: the Engaged set goes out when it changes and this often.
const ENGAGED_EVERY: Duration = Duration::from_secs(10);
/// Engaged, a peer in another block for this long = that peer disengages (border flicker below).
const ELSEWHERE_GRACE: Duration = Duration::from_secs(3);

/// One handshake partner: on the host each client, on a client the host. Made per
/// link epoch (a new connection = a fresh one).
struct Link {
    id: PeerId,
    phase: Phase,
    /// The peer's link epoch (`PeerFrame::epoch` = net.rs `peer_epoch`).
    epoch: u64,
    generation: u32,
    stage: u8,
    block: i32,
    since: Instant,
    cooldown_until: Option<Instant>,
    /// A failure with this peer this world: no new handshake with it until the next load.
    failed: Option<String>,
    /// Review finding e: a failure switches to our own sync only at the next load
    /// (never mid-fight) and for that one world; the world after it tries again.
    fallback_next: bool,
    fallback_world: bool,
    soft_fails: u32,
    ours: Vec<Pair>,
    ready_sent: bool,
    /// The peer's hello generation we last answered (re-send ours on a new one).
    answered_gen: Option<u32>,
    engaged_gen: u32,
    no_remote_since: Option<Instant>,
    wait_log_at: Option<Instant>,
    /// Frames the patch sites were busy (a game thread inside one).
    busy_frames: u32,
    /// v8.61: suspend phases that ran out of their budget in a row, and when the next try may
    /// run (`busy_retry_gap`).
    busy_timeouts: u32,
    busy_retry_at: Option<Instant>,
    /// The peer was told we switched the setting off (once per handshake).
    said_off: bool,
    /// The peer was told (stage-0 hello) that ours cannot run, on this link.
    said_broken: bool,
    /// The peer said its native session cannot run (refreshed every tick).
    unavailable: bool,
    /// Our handshakes in a row that timed out without a hello from it.
    no_hello: u32,
    /// Handshakes we started with it since the last engagement / new world.
    attempts: u32,
    /// Its last attempt ended in a failure (timeout, no 23, no remote ...), not engaged
    /// since. Not set by a load, rest, our own-sync hold or a setting.
    failed_attempt: bool,
    /// Host: this link is idle because our own-sync gate holds it (last tick): it must not
    /// count as a blocker itself, or two held links would hold each other forever.
    held: bool,
    /// Engaged and the blocks differ: since when (a short border flicker is not a leave).
    elsewhere_since: Option<Instant>,
    /// Idle while the peer is known to be in another block (no handshakes then).
    elsewhere_logged: bool,
    /// Host: the 23 distribution was forced after this client's first 22 of the engagement.
    first22_forced: bool,
    host22_logs: u32,
    /// Client: the counters at engage (the 5 s rule).
    served_23_at_engage: u32,
    sync_in_at_engage: u32,
    /// Client: when the host's sync packets first came in this engagement.
    host_live_at: Option<Instant>,
    owned23_logs: u32,
    /// Host: this client's last candidate list (22, as received), re-settled when the
    /// claims change (§34); its claimed netIds in it; what it may not keep (our claims it
    /// did not win, contests another machine won) as last applied.
    last22: Option<Vec<u8>>,
    claimed: Vec<u16>,
    blocked: Vec<u16>,
}

/// Host: contested claims are checked this often.
const CONTEST_EVERY: Duration = Duration::from_millis(100);

impl Link {
    fn new(id: PeerId, epoch: u64) -> Self {
        Self {
            id,
            phase: Phase::Idle,
            epoch,
            generation: 0,
            stage: 0,
            block: -1,
            since: Instant::now(),
            cooldown_until: None,
            failed: None,
            fallback_next: false,
            fallback_world: BROKEN.load(Ordering::Acquire),
            soft_fails: 0,
            ours: Vec::new(),
            ready_sent: false,
            answered_gen: None,
            engaged_gen: 0,
            no_remote_since: None,
            wait_log_at: None,
            busy_frames: 0,
            busy_timeouts: 0,
            busy_retry_at: None,
            said_off: false,
            said_broken: false,
            unavailable: false,
            no_hello: 0,
            attempts: 0,
            failed_attempt: false,
            held: false,
            elsewhere_since: None,
            elsewhere_logged: false,
            first22_forced: false,
            host22_logs: 0,
            served_23_at_engage: 0,
            sync_in_at_engage: 0,
            host_live_at: None,
            owned23_logs: 0,
            last22: None,
            claimed: Vec::new(),
            blocked: Vec::new(),
        }
    }

    fn slot(&self) -> u8 {
        self.id.slot()
    }

    fn wait_log(&mut self, what: &str) {
        if self.wait_log_at.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
            self.wait_log_at = Some(Instant::now());
            log::line(&format!("nsession: waiting (peer {}): {what}", self.slot()));
        }
    }

    /// Why this peer cannot be in the native session in this world (None = it can,
    /// maybe after a handshake). `count_no_hello` = a missing hello counts (not while the
    /// peer has just been through a loading screen: its game is not answering yet).
    fn cannot_join(&self, count_no_hello: bool) -> Option<String> {
        if let Some(f) = &self.failed {
            Some(format!("failed this world: {f}"))
        } else if self.fallback_world {
            Some("our own sync this world after a failure".into())
        } else if self.unavailable {
            Some("its native session is unavailable".into())
        } else if count_no_hello && self.no_hello > 0 {
            Some("no hello from it (native_session = 0 there?)".into())
        } else {
            None
        }
    }

    /// Host: why this peer, in our block, keeps the whole block on our own sync (mode 4)
    /// instead of the native session. Review finding 1: a block must never be half
    /// native, because the host's mode 4 is off while anyone is engaged and a peer that
    /// is not engaged would get no enemy sync at all. Pure (`x` = the outside facts).
    fn blocks_block(&self, x: &BlockFacts) -> Option<String> {
        if self.phase == Phase::Engaged {
            return None;
        }
        if let Some(why) = self.cannot_join(!x.loaded_recently) {
            return Some(why);
        }
        if !x.has_remote {
            return Some("the host has no native remote of it".into());
        }
        if let Some(s) = x.hello_stage
            && s != 0
            && s != x.our_stage
        {
            return Some(format!("stage mismatch (its native_session_stage {s}, ours {})", x.our_stage));
        }
        if self.held {
            return None;
        }
        match self.phase {
            Phase::Handshake if self.attempts > 1 => Some(format!("still handshaking after {} attempts", self.attempts)),
            Phase::Idle if self.failed_attempt && x.hello_stage.is_some() => {
                Some(format!("not engaged after {} handshake attempt(s) (hello seen)", self.attempts))
            }
            _ => None,
        }
    }
}

/// Pure: do the links in `ids` take every engaged link with them (then none is left that
/// would need its records cleared, and the shared state comes down once)?
fn goes_all_at_once(engaged: &[PeerId], ids: &[PeerId]) -> bool {
    engaged.iter().all(|e| ids.contains(e))
}

/// What `Link::blocks_block` needs from outside the link.
struct BlockFacts {
    /// The stage in the peer's hello on this link (None = no hello seen).
    hello_stage: Option<u8>,
    /// The host has the peer's native remote.
    has_remote: bool,
    /// The peer had a loading screen a moment ago.
    loaded_recently: bool,
    our_stage: u8,
}

/// A blocker counts only after it has stood this long (a peer that just joined, a flicker).
const BLOCKER_GRACE: Duration = Duration::from_secs(3);
/// A missing hello does not count for this many seconds after the peer's loading screen.
const NO_HELLO_LOADED_SECS: u64 = 15;

/// Pure: this tick's raw blockers -> those that have stood `grace` (`since` keeps the first
/// moment each was seen and forgets the ones that went).
fn mature_blockers(
    since: &mut BTreeMap<u8, Instant>,
    raw: BTreeMap<u8, String>,
    now: Instant,
    grace: Duration,
) -> Vec<(u8, String)> {
    since.retain(|s, _| raw.contains_key(s));
    raw.into_iter().filter(|(s, _)| now.saturating_duration_since(*since.entry(*s).or_insert(now)) >= grace).collect()
}

/// Host: who may use the native session now (pure). `players` = the session's player
/// count, `max` = `native_session_max_players`, `wraps` = a type-23 recipient wrap is
/// usable, `blockers` = (slot, why) of the peers in our block that cannot join.
/// None = `link` may; else why it keeps our own sync. A blocker is held back only by the
/// blockers of lower slots (the lowest one always may try), so two blockers never hold
/// each other out.
fn own_sync_rule(players: u32, max: u32, wraps: bool, link: u8, blockers: &[(u8, String)]) -> Option<String> {
    if players > max {
        return Some(format!("{players} players, native_session_max_players = {max}"));
    }
    if players > 2 && !wraps {
        return Some(format!("{players} players and no type-23 recipient wrap (native session for 2 players only)"));
    }
    let is_blocker = blockers.iter().any(|(s, _)| *s == link);
    blockers
        .iter()
        .find(|(s, _)| *s != link && (!is_blocker || *s < link))
        .map(|(s, why)| format!("peer {s} in our block cannot join the native session ({why}); everyone here uses our own sync"))
}

struct Engine {
    links: BTreeMap<PeerId, Link>,
    host: bool,
    /// Handshake generations, one counter for every link.
    next_gen: u32,
    /// Stage of the current engagements (all links engage at the setting's stage).
    stage: u8,
    /// Patches in place while any link is engaged at stage >= 2.
    applied: Vec<Applied>,
    /// `applied` holds the type-23 recipient wraps (host, a world for 3+ players).
    wraps_applied: bool,
    /// Client: the chr-loop patch, applied after the first 23 of the engagement.
    late: Vec<Applied>,
    late_busy: u32,
    /// A failure that concerns every link (`native_remote = 0`).
    failed: Option<String>,
    /// A rest of ours: no handshake before this (links made later too).
    rest_hold_until: Option<Instant>,
    /// `WCM_UPDATES` last seen and when it last moved (our world update runs).
    updates: Option<u32>,
    updates_at: Instant,
    /// Since when our world update has run without a stall (`world_steady`).
    running_since: Option<Instant>,
    /// Engaged and our world update stopped: logged once per stop.
    still_logged: bool,
    engaged_at: Instant,
    owned_sent_at: Option<Instant>,
    /// v8.30b: last Owned list sent and when the list was last compared (change-triggered early send).
    owned_sent_ids: Vec<u16>,
    owned_check_at: Option<Instant>,
    ownerless_at: Option<Instant>,
    stats_at: Option<Instant>,
    timers_logged: bool,
    /// Ownerless enemies near us that we took (`ownfill`).
    fill: ownfill::Fill,
    /// Host: the Engaged set last sent, and when.
    engaged_sent: u8,
    engaged_sent_at: Option<Instant>,
    /// Host: why our block keeps our own sync now (logged when it changes).
    own_sync: Option<String>,
    /// Host: the peers in our block that keep the block on our own sync (stood `BLOCKER_GRACE`).
    blockers: Vec<(u8, String)>,
    /// Host: when each raw blocker (by slot) was first seen.
    blocker_since: BTreeMap<u8, Instant>,
    /// Every engaged link goes in this call (load, rest, our block change ...): the shared
    /// state comes down once at the end, no per-link record clearing (`disengage`).
    all_going: bool,
    /// Host (§34): who runs each contested enemy (slot), the contested set last time, the
    /// last contest check, contest log lines.
    winners: BTreeMap<u16, u8>,
    contested: Vec<u16>,
    contest_at: Option<Instant>,
    contest_logs: u32,
    /// Our world update paused (menu): the enemies handed on (left out of our Owned list).
    pause: PauseTracker,
    handed: Vec<u16>,
    /// Double claims: since when both machines own a claimed enemy; logged once each.
    both_since: HashMap<u16, Instant>,
    double_logged: std::collections::HashSet<u16>,
}

impl Engine {
    fn new() -> Self {
        Self {
            links: BTreeMap::new(),
            host: false,
            next_gen: 0,
            stage: 0,
            applied: Vec::new(),
            wraps_applied: false,
            late: Vec::new(),
            late_busy: 0,
            failed: None,
            rest_hold_until: None,
            updates: None,
            updates_at: Instant::now(),
            running_since: None,
            still_logged: false,
            engaged_at: Instant::now(),
            owned_sent_at: None,
            owned_sent_ids: Vec::new(),
            owned_check_at: None,
            ownerless_at: None,
            stats_at: None,
            timers_logged: false,
            fill: ownfill::Fill::default(),
            engaged_sent: 0,
            engaged_sent_at: None,
            own_sync: None,
            blockers: Vec::new(),
            blocker_since: BTreeMap::new(),
            all_going: false,
            winners: BTreeMap::new(),
            contested: Vec::new(),
            contest_at: None,
            contest_logs: 0,
            pause: PauseTracker::default(),
            handed: Vec::new(),
            both_since: HashMap::new(),
            double_logged: std::collections::HashSet::new(),
        }
    }

    fn bump_gen(&mut self) -> u32 {
        self.next_gen = self.next_gen.wrapping_add(1);
        self.next_gen
    }

    /// Runs `f` with link `id` taken out of the map (so `self.links` holds the others).
    fn with_link<R>(&mut self, id: PeerId, f: impl FnOnce(&mut Self, &mut Link) -> R) -> Option<R> {
        let mut l = self.links.remove(&id)?;
        let r = f(self, &mut l);
        self.links.insert(id, l);
        Some(r)
    }

    fn any_engaged(&self) -> bool {
        self.links.values().any(|l| l.phase == Phase::Engaged)
    }

    /// Every frame: did our world update (WorldChrMan's task, the sync table) run?
    fn observe_world(&mut self) {
        let now = world_updates();
        if now.is_some() && now != self.updates {
            if !self.world_running() {
                self.running_since = Some(Instant::now());
            }
            self.updates = now;
            self.updates_at = Instant::now();
        }
    }

    /// Running for `ENGAGE_STEADY` without a stall: the load is over. The patches freeze
    /// every thread, and threads still streaming the world take seconds to stop (live
    /// 2026-10-25: engage 7.4 s + restore 5.1 s of frozen game behind the loading screen,
    /// the update had run 0.5 s between two stalls).
    fn world_steady(&self) -> bool {
        self.world_running() && self.running_since.is_some_and(|t| t.elapsed() >= ENGAGE_STEADY)
    }

    fn world_running(&self) -> bool {
        self.updates.is_some() && self.updates_at.elapsed() < WORLD_STILL_AFTER
    }

    fn hello(&self, l: &Link) -> Msg {
        Msg::Hello { proto: PROTO, stage: l.stage, host: self.host, block: l.block, generation: l.generation }
    }

    /// Everything shared goes when no link is engaged any more: routing off first (the
    /// hooks pass through from here), then the patches out, the queues empty.
    fn teardown(&mut self) {
        set_owns(false);
        ROUTE.store(false, Ordering::Release);
        ANY_MEMBER.store(false, Ordering::Release);
        set_whitelist(0);
        for a in &ACCEPT {
            a.store(0, Ordering::Release);
        }
        ENGAGED_LINKS.store(0, Ordering::Release);
        PEER_ENGAGED.store(0, Ordering::Release);
        restore_patches(&mut self.applied);
        // Never written (the client's chr loop before its first 23): nothing to restore.
        self.late.clear();
        clear_queues();
        let _ = take_last23();
        if !self.fill.ours.is_empty() {
            log::line(&format!(
                "nsession: ownfill: {} enemies we had taken go back to the game's rules (taken {}, given back {} this engagement)",
                self.fill.ours.len(),
                self.fill.taken,
                self.fill.given
            ));
        }
        self.fill.reset();
        self.pause = PauseTracker::default();
        self.handed.clear();
        self.both_since.clear();
        self.double_logged.clear();
        if let Ok(mut d) = DROPPED_CLAIMS.lock() {
            d.clear();
        }
        self.engaged_sent = 0;
        self.engaged_sent_at = None;
    }

    /// Link back to Idle: `leave` tells that peer, `latch` keeps it off until the next
    /// load. Only this peer leaves while others stay engaged (its ownership record is
    /// emptied so the game hands its enemies on); the last one takes the shared state
    /// down (`teardown`).
    fn disengage(&mut self, l: &mut Link, why: &str, reason: Reason, leave: bool, latch: bool) {
        let was = l.phase;
        let slot = l.slot();
        ACCEPT[slot as usize].store(0, Ordering::Release);
        ENGAGED_LINKS.fetch_and(!(1u8 << slot), Ordering::AcqRel);
        // All links going at once (load, rest, our block change): the other links' inbox
        // entries and ownership records are not touched (v7.5 never did), the shared state
        // comes down once afterwards. Clearing a record calls game table functions, which
        // must not run during a load.
        let others = !self.all_going && self.any_engaged();
        if was == Phase::Engaged && others {
            drop_inbound_from(slot);
            if self.host && self.stage >= 2 {
                let cleared = clear_record(slot);
                log::line(&format!(
                    "nsession: peer {slot} leaves the native session alone ({} engaged stay): its ownership record emptied {cleared:?}, 23 distribution forced",
                    self.links.values().filter(|o| o.phase == Phase::Engaged).count()
                ));
            }
        }
        if !others && !self.all_going {
            self.teardown();
        }
        l.elsewhere_since = None;
        if leave && was != Phase::Idle {
            queue_ctrl(slot, &Msg::Leave { generation: l.generation, latch, reason: reason as u8 });
        }
        if latch && l.failed.is_none() {
            l.failed = Some(why.to_string());
            l.fallback_next = true;
        }
        l.phase = Phase::Idle;
        l.ready_sent = false;
        l.no_remote_since = None;
        l.cooldown_until = Some(Instant::now() + COOLDOWN);
        if was != Phase::Idle || latch {
            log::line(&format!(
                "nsession: {} with peer {slot} ({why}){}",
                if was == Phase::Engaged { "DISENGAGED" } else { "handshake dropped" },
                if latch {
                    ", fallback latched: enemies stay local here, our own sync (mode 4) with it for the next world"
                } else {
                    ""
                }
            ));
        }
    }

    fn disengage_id(&mut self, id: PeerId, why: &str, reason: Reason, leave: bool, latch: bool) {
        self.with_link(id, |e, l| e.disengage(l, why, reason, leave, latch));
    }

    /// Every link that is not Idle (every link when latching).
    fn disengage_all(&mut self, why: &str, reason: Reason, leave: bool, latch: bool) {
        let ids: Vec<PeerId> = self.links.iter().filter(|(_, l)| latch || l.phase != Phase::Idle).map(|(id, _)| *id).collect();
        self.disengage_many(&ids, why, reason, leave, latch);
    }

    /// Links `ids` go. When that is every engaged link the shared state comes down once at the
    /// end and no link clears records or queue entries of the others (`all_going`); when some
    /// stay engaged each one goes alone as `disengage` describes.
    fn disengage_many(&mut self, ids: &[PeerId], why: &str, reason: Reason, leave: bool, latch: bool) {
        if ids.is_empty() {
            return;
        }
        let engaged: Vec<PeerId> = self.links.iter().filter(|(_, l)| l.phase == Phase::Engaged).map(|(id, _)| *id).collect();
        let all = goes_all_at_once(&engaged, ids);
        self.all_going = all;
        for &id in ids {
            self.disengage_id(id, why, reason, leave, latch);
        }
        self.all_going = false;
        if all {
            self.teardown();
        }
    }

    /// Host: why link `id` keeps our own sync now (None = it may use the native session).
    fn own_sync_reason(&self, f: &Frame, id: PeerId) -> Option<String> {
        if !self.host {
            return None;
        }
        let max = (MAX_NATIVE_PLAYERS.load(Ordering::Relaxed) as u32).clamp(2, SLOTS as u32);
        let players = f.peers.players();
        // 3+ players need a recipient wrap: in the patches now, or in the next plan.
        let wraps = players <= 2
            || if self.applied.is_empty() { tuning::max_players() > 2 && wrap_mask() != 0 } else { self.wraps_applied };
        own_sync_rule(players, max, wraps, id.slot(), &self.blockers)
    }

    /// Host, every tick: the peers in our block that cannot be in the native session now
    /// (see `Link::blocks_block`), kept in `blockers` once they have stood `BLOCKER_GRACE`.
    fn update_blockers(&mut self, f: &Frame, stage: u8) {
        let mut raw: BTreeMap<u8, String> = BTreeMap::new();
        if self.host {
            for p in f.peers.in_block(f.here) {
                let Some(l) = self.links.get(&p.id) else { continue };
                let x = BlockFacts {
                    hello_stage: peer_hello_stage(l.slot(), l.epoch),
                    has_remote: p.native.is_some(),
                    loaded_recently: p.loaded_recently(NO_HELLO_LOADED_SECS),
                    our_stage: stage,
                };
                if let Some(why) = l.blocks_block(&x) {
                    raw.insert(p.id.slot(), why);
                }
            }
        }
        self.blockers = mature_blockers(&mut self.blocker_since, raw, Instant::now(), BLOCKER_GRACE);
    }

    fn maybe_start(&mut self, l: &mut Link, f: &Frame, stage: u8, broken: bool) {
        let now = Instant::now();
        let ready = self.failed.is_none()
            && l.failed.is_none()
            && !l.fallback_world
            && !broken
            && !l.unavailable
            && f.in_world_for >= SETTLE
            && f.here.is_some()
            && l.cooldown_until.is_none_or(|t| now >= t)
            && self.rest_hold_until.is_none_or(|t| now >= t);
        // Live 7.4: the friend in another block for 5 minutes = a handshake every 35 s
        // that could only time out ("blocks differ"), generations 4 -> 12. Wait instead.
        let elsewhere = f.block_of(l.id).is_some_and(|b| Some(b) != f.here);
        if ready && elsewhere {
            if !l.elsewhere_logged {
                l.elsewhere_logged = true;
                log::line(&format!(
                    "nsession: peer {} is in another block (ours {:?}, its {:?}): no handshake until we share one",
                    l.slot(),
                    f.here,
                    f.block_of(l.id)
                ));
            }
        } else if ready {
            l.elsewhere_logged = false;
            self.start_handshake(l, f, stage);
        }
    }

    fn start_handshake(&mut self, l: &mut Link, f: &Frame, stage: u8) {
        l.attempts += 1;
        l.generation = self.bump_gen();
        l.stage = stage;
        l.block = f.here.unwrap_or(-1);
        l.ours = our_pairs();
        l.ready_sent = false;
        l.answered_gen = None;
        l.since = Instant::now();
        l.phase = Phase::Handshake;
        l.wait_log_at = None;
        l.said_off = false;
        queue_ctrl(l.slot(), &self.hello(l));
        queue_ctrl(l.slot(), &Msg::List { generation: l.generation, block: l.block, pairs: l.ours.clone() });
        queue_ctrl(l.slot(), &Msg::MaxHp { generation: l.generation, max: our_max_hp() });
        log::line(&format!(
            "nsession: handshake generation {} stage {stage} with peer {} as {} in block {}: {} enemies with entity id + netId",
            l.generation,
            l.slot(),
            if self.host { "host" } else { "client" },
            l.block,
            l.ours.len()
        ));
    }

    fn handshake(&mut self, l: &mut Link, f: &Frame) {
        let peer = {
            let g = book();
            g.peers[l.slot() as usize].as_ref().filter(|p| p.epoch == l.epoch).map(|p| (p.hello, p.list.clone(), p.ready, p.leave, p.newest_gen))
        };
        let Some((Some((proto, stage, host, block, pgen)), list, ready, leave, newest)) = peer else {
            if l.since.elapsed() >= HANDSHAKE_TIMEOUT {
                l.no_hello += 1;
                self.disengage(l, "no hello from the peer (native_session = 0 there?)", Reason::Other, true, false);
            } else {
                l.wait_log("its hello (it needs native_session = 1 too)");
            }
            return;
        };
        // A (new) handshake of the peer: make sure it has ours, and check its new
        // list again before we say ready.
        if l.answered_gen != Some(pgen) {
            if l.answered_gen.is_some() {
                queue_ctrl(l.slot(), &self.hello(l));
                queue_ctrl(l.slot(), &Msg::List { generation: l.generation, block: l.block, pairs: l.ours.clone() });
                queue_ctrl(l.slot(), &Msg::MaxHp { generation: l.generation, max: our_max_hp() });
            }
            l.answered_gen = Some(pgen);
            l.ready_sent = false;
        }
        if let Some((lgen, latch, reason)) = leave
            && lgen == pgen
            && latch
        {
            self.disengage(l, &format!("peer fell back ({:?})", Reason::from_u8(reason)), Reason::from_u8(reason), false, true);
            return;
        }
        if leave.is_some_and(|(lgen, ..)| lgen == pgen) {
            // That handshake of the peer is over; its next one brings a new generation.
            l.wait_log("its next handshake (it left the last one)");
            return;
        }
        if !self.world_steady() {
            // Our world update is not running (pause menu, death / respawn screen): the
            // game's sync table would not run either, so no 22 / 23 would ever go out.
            // Live 7.1: the host engaged three times in that state (timers frozen at
            // [3.0, 2.98, 5.0, 4.98]), the client's 5 s rule fired three times and
            // latched the fallback. Wait (no timeout while paused).
            l.since = Instant::now();
            l.wait_log("our world update is paused (menu / death screen?): engaging when it runs again");
            return;
        }
        let peer_block = f.block_of(l.id);
        let problem = if proto != PROTO {
            Some(format!("peer's nsession protocol {proto}, ours {PROTO} (different builds)"))
        } else if stage != l.stage {
            Some(format!("peer uses native_session_stage {stage}, we {}", l.stage))
        } else if host == self.host {
            Some("both sides think they are the host / the client".into())
        } else if block != l.block || f.here != Some(l.block) || peer_block.is_some_and(|b| b != l.block) {
            Some(format!("blocks differ (ours {:?} at hello {}, peer's hello {block}, snapshot {peer_block:?})", f.here, l.block))
        } else if f.remote_of(l.id).is_none() {
            Some("the peer's native remote (native_remote = 1, same block)".into())
        } else {
            None
        };
        if let Some(p) = problem {
            if l.since.elapsed() >= HANDSHAKE_TIMEOUT {
                l.failed_attempt = true;
                self.disengage(l, &format!("handshake timed out: {p}"), Reason::Other, true, false);
            } else {
                l.wait_log(&p);
            }
            return;
        }
        let Some((_, _, theirs)) = list.filter(|x| x.0 == pgen) else {
            l.wait_log("its netId list");
            return;
        };
        if !l.ready_sent {
            let (common, bad) = netid_check(&l.ours, &theirs);
            log::line(&format!(
                "nsession: netId check with peer {}: {}/{common} match ({} ours, {} theirs){}",
                l.slot(),
                common - bad.len(),
                l.ours.len(),
                theirs.len(),
                if bad.is_empty() { String::new() } else { format!(", first mismatches {:?}", &bad[..bad.len().min(5)]) }
            ));
            if !bad.is_empty() && l.stage >= 2 {
                self.disengage(l, "netId check failed", Reason::NetIdMismatch, true, true);
                return;
            }
            l.ready_sent = true;
            queue_ctrl(l.slot(), &Msg::Ready { generation: l.generation, peer_generation: pgen });
        }
        // The peer's ready must name this handshake of ours (an old one is stale).
        // Also nothing newer than that hello may have come from it since (it moved on:
        // our engage would be one-sided, the peer having left this generation already).
        if let Some(why) = engage_block_reason(l.generation, pgen, ready, newest) {
            if l.since.elapsed() >= HANDSHAKE_TIMEOUT {
                l.failed_attempt = true;
                self.disengage(l, &format!("peer never got ready ({why})"), Reason::Other, true, false);
            } else {
                l.wait_log(&format!("its ready ({why})"));
            }
            return;
        }
        self.engage(l, f, pgen, &theirs);
    }

    /// A check failed for good (hooks, patch bytes): this link and every other one out,
    /// latched; BROKEN keeps the mode off for the run.
    fn broke(&mut self, l: &mut Link, what: &str, e: &str) {
        BROKEN.store(true, Ordering::Release);
        log::line(&format!("nsession: {what} failed: {e}; native session OFF for this run"));
        unavailable(&format!("{what} failed: {e}"));
        // Every link goes (this one is out of the map): the shared state comes down once.
        self.all_going = true;
        self.disengage(l, what, Reason::CheckFailed, true, true);
        self.disengage_all(what, Reason::CheckFailed, true, true);
        self.all_going = false;
        self.teardown();
    }

    fn engage(&mut self, l: &mut Link, f: &Frame, pgen: u32, theirs: &[Pair]) {
        if let Err(e) = install_hooks() {
            self.broke(l, "hook check", &e);
            return;
        }
        let first = !self.any_engaged();
        if l.stage >= 2 && self.applied.is_empty() {
            // v8.61: after a timed-out suspend phase, wait before the next one (each costs the
            // game a 250 ms+ frame; live 2026-10-28: 12 tries in 7 s, 5.5 s frozen at most).
            if l.busy_retry_at.is_some_and(|t| Instant::now() < t) {
                return;
            }
            let host = self.host;
            // The recipient wraps only in a world for 3+ players: with max_players = 2 the
            // host's patches are exactly the 2-player build's.
            let wraps = host && tuning::max_players() > 2 && wrap_mask() != 0;
            self.wraps_applied = false;
            let applied = plan_patches(host, l.stage, wraps).map_err(|e| (false, e)).and_then(|plan| {
                // Client: the chr loop waits for the first 23 (CHR_LOOP_SITE): until then
                // every enemy here keeps its own AI instead of NetAI without data.
                let (late, now): (Vec<Applied>, Vec<Applied>) = plan.into_iter().partition(|p| !host && p.rva == CHR_LOOP_SITE);
                apply_patches(now).map(|a| (a, late))
            });
            match applied {
                Ok((a, late)) => {
                    log::line(&format!(
                        "nsession: patches role={} applied {}/{}{}",
                        if self.host { "host" } else { "client" },
                        a.len(),
                        a.len() + late.len(),
                        if late.is_empty() {
                            String::new()
                        } else {
                            ", the members chr loop waits for the first 23 (enemies keep their own AI here until then)".into()
                        }
                    ));
                    self.applied = a;
                    self.late = late;
                    self.late_busy = 0;
                    self.wraps_applied = wraps;
                }
                Err((true, e)) => {
                    // A game thread sat inside a site: try again next frame, a few seconds at most.
                    // The suspend phase out of time: try again after a growing gap (v8.61).
                    l.busy_frames += 1;
                    let timed_out = e.starts_with(TIMED_OUT_MSG);
                    if timed_out {
                        l.busy_timeouts += 1;
                        let gap = busy_retry_gap(l.busy_timeouts);
                        l.busy_retry_at = Some(Instant::now() + gap);
                        log::line(&format!(
                            "nsession: patches postponed: {e} (timeout {} in a row, next try in {} ms)",
                            l.busy_timeouts,
                            gap.as_millis()
                        ));
                    } else if l.busy_frames == 1 || l.busy_frames.is_multiple_of(60) {
                        log::line(&format!("nsession: patches postponed: {e} (try {})", l.busy_frames));
                    }
                    if l.busy_frames >= 600 || l.busy_timeouts >= BUSY_TIMEOUTS_MAX {
                        l.busy_frames = 0;
                        l.busy_timeouts = 0;
                        l.busy_retry_at = None;
                        l.failed_attempt = true;
                        self.disengage(l, "patch sites stayed busy", Reason::Other, true, false);
                    }
                    return;
                }
                Err((false, e)) => {
                    self.broke(l, "patch check", &e);
                    return;
                }
            }
        }
        l.busy_frames = 0;
        l.busy_timeouts = 0;
        l.busy_retry_at = None;
        // Host death rule: alive on both = the lower HP on both (nothing is healed).
        if l.stage >= 2 {
            let ours = our_pairs();
            let our_max: HashMap<i32, i32> = our_max_hp().into_iter().collect();
            // This link's MaxHp for its List of this generation (§34).
            let their_max: Option<HashMap<i32, i32>> = book().peers[l.slot() as usize]
                .as_ref()
                .and_then(|p| p.max_hp.as_ref())
                .filter(|(g, _)| *g == pgen)
                .map(|(_, m)| m.iter().copied().collect());
            let raw = hp_adoptions_raw(&ours, theirs).len();
            let adopt = hp_adoptions(&ours, &our_max, theirs, their_max.as_ref());
            if raw != adopt.len() {
                log::line(&format!(
                    "nsession: HP check on engage with peer {}: {} of {raw} lower HPs not taken (max HP differs or HP above max: scaling not settled on one side{})",
                    l.slot(),
                    raw - adopt.len(),
                    if their_max.is_none() { "; it sent no max HP" } else { "" }
                ));
            }
            let mut n = 0;
            for &(e, hp) in &adopt {
                if let Some(c) = crate::game::chr_by_entity_id(e)
                    && c.hp().is_some_and(|cur| cur > hp)
                    // The game's setter (0xBD64E0, posture.rs), so a multi-life boss keeps its floor.
                    && crate::posture::set_hp(c, hp)
                {
                    n += 1;
                }
            }
            log::line(&format!(
                "nsession: HP check on engage with peer {}: {n} enemies took its lower HP{}",
                l.slot(),
                if self.host { "" } else { "; from now on their HP follows the host (type 5)" }
            ));
        }
        if first {
            clear_queues();
            self.stage = l.stage;
            self.engaged_at = Instant::now();
            self.still_logged = false;
            self.owned_sent_at = None;
            self.ownerless_at = Some(Instant::now());
            self.timers_logged = false;
            self.stats_at = Some(Instant::now());
        }
        ROLE_HOST.store(self.host, Ordering::Relaxed);
        update_pnets(f);
        let slot = l.slot();
        GEN_OUT[slot as usize].store(l.generation, Ordering::Relaxed);
        ACCEPT[slot as usize].store(1 << 32 | pgen as u64, Ordering::Release);
        ENGAGED_LINKS.fetch_or(1 << slot, Ordering::AcqRel);
        set_whitelist(whitelist(l.stage));
        ANY_MEMBER.store(l.stage >= 2, Ordering::Release);
        ROUTE.store(l.stage >= 2, Ordering::Release);
        l.phase = Phase::Engaged;
        l.since = Instant::now();
        l.attempts = 0;
        l.failed_attempt = false;
        l.engaged_gen = pgen;
        l.served_23_at_engage = SERVED_23.load(Ordering::Relaxed);
        l.sync_in_at_engage = SYNC_IN.load(Ordering::Relaxed);
        l.host_live_at = None;
        l.first22_forced = false;
        // Host (§34): this client's candidates and claims start over; contests are settled
        // again from the next lists (`host_candidates`).
        l.last22 = None;
        l.claimed.clear();
        l.blocked.clear();
        if first {
            self.winners.clear();
            self.contested.clear();
            self.contest_at = None;
        }
        if l.stage >= 2 {
            // Do not wait for the game's own timers (up to 3 s for 23, 5 s for 22): the
            // host distributes ownership on its next update, the client sends its
            // candidates on its next one (the host distributes again after that 22).
            let before = timers();
            let forced = make_due(if self.host { T_DUE_23 } else { T_DUE_22 });
            log::line(&format!(
                "nsession: sync table timers at engage {before:?}; {} forced to the next update: {forced}",
                if self.host { "23 distribution" } else { "22 send" }
            ));
        }
        let map: Vec<String> = pnet_map(false).iter().map(|n| format!("{n:#x}")).collect();
        log::line(&format!(
            "nsession: ENGAGED stage {} as {} with peer {slot} (generation {} / peer {}), routing {}, player netIds by slot [{}], engaged {:?}, bullets {}",
            l.stage,
            if self.host { "host" } else { "client" },
            l.generation,
            pgen,
            if l.stage >= 2 { "on" } else { "off (counters only)" },
            map.join(" "),
            mask_slots(engaged_links()),
            if step_aside::NATIVE_BULLETS { "native" } else { "artfx.rs" }
        ));
    }

    /// One engaged link's drop rules (its leave, restart, block, remote; client: the
    /// host's 23 rules and the late chr-loop patch).
    fn engaged_link(&mut self, l: &mut Link, f: &Frame) {
        let peer = {
            let g = book();
            g.peers[l.slot() as usize].as_ref().filter(|p| p.epoch == l.epoch).map(|p| (p.hello.map(|h| h.4), p.leave))
        };
        let (pgen, leave) = peer.unwrap_or((None, None));
        if let Some((lgen, latch, reason)) = leave
            && lgen == l.engaged_gen
        {
            self.disengage(l, &format!("peer left ({:?})", Reason::from_u8(reason)), Reason::from_u8(reason), false, latch);
            return;
        }
        if pgen != Some(l.engaged_gen) {
            self.disengage(l, "peer restarted its handshake", Reason::PeerRestarted, true, false);
            return;
        }
        let peer_block = f.block_of(l.id);
        if f.here != Some(l.block) || peer_block.is_some_and(|b| b != l.block) {
            // Our own block changed = a load is coming (it disengages anyway; every link
            // sees it); the peer's block flickering at a border is not a reason to drop
            // the ownership tables.
            let since = *l.elsewhere_since.get_or_insert_with(Instant::now);
            if f.here != Some(l.block) || since.elapsed() >= ELSEWHERE_GRACE {
                let why = format!(
                    "peer in another block (ours {:?}, its {peer_block:?}, for {} ms)",
                    f.here,
                    since.elapsed().as_millis()
                );
                self.disengage(l, &why, Reason::FriendElsewhere, true, false);
                return;
            }
        } else {
            l.elsewhere_since = None;
        }
        match f.remote_of(l.id) {
            Some(_) => l.no_remote_since = None,
            None => {
                let t = *l.no_remote_since.get_or_insert_with(Instant::now);
                if t.elapsed() >= NO_REMOTE_GRACE {
                    l.failed_attempt = true;
                    self.disengage(l, "no native remote of the peer", Reason::NoRemote, true, false);
                    return;
                }
            }
        }
        if self.host || l.stage < 2 {
            return;
        }
        let served_23 = SERVED_23.load(Ordering::Relaxed) != l.served_23_at_engage;
        if !served_23 {
            // The 5 s rule counts from the first packet of the host's sync (its 4 / 5 /
            // 70 / 71 come from the same update as its 23), not from the engage.
            let fed = SYNC_IN.load(Ordering::Relaxed).wrapping_sub(l.sync_in_at_engage);
            if fed > 0 && l.host_live_at.is_none() {
                l.host_live_at = Some(Instant::now());
                log::line(&format!(
                    "nsession: the host's sync runs ({fed} packets, {} ms after engage); its first 23 is due within 5 s",
                    l.since.elapsed().as_millis()
                ));
            }
            match l.host_live_at {
                Some(t) if t.elapsed() >= FIRST_23_WITHIN => {
                    l.soft_fails += 1;
                    let latch = l.soft_fails >= SOFT_FAILS_LATCH;
                    let why = format!(
                        "no ownership list (23) within 5 s of the host's sync packets ({fed} of them, timers here {:?}), failure {}",
                        timers(),
                        l.soft_fails
                    );
                    l.failed_attempt = true;
                    self.disengage(l, &why, Reason::No23, true, latch);
                    return;
                }
                None if l.since.elapsed() >= HOST_SILENT_MAX => {
                    // The host's world is not running (paused); enemies kept their own AI
                    // here meanwhile (the chr loop waits for the first 23). Not a failure.
                    self.disengage(l, "the host's sync sent nothing for 30 s (its world paused?)", Reason::Other, true, false);
                    return;
                }
                _ => {}
            }
        }
        if served_23 && !self.late.is_empty() {
            // First 23 of this engagement: ownership is known now, enemies not owned here
            // go to NetAI (fed by the host) from the next chr loop.
            match apply_patches(self.late.clone()) {
                Ok(a) => {
                    let (_, _, _, here) = owners();
                    log::line(&format!(
                        "nsession: first 23 {} ms after engage, owned here {here}: members chr loop patched, enemies not owned here go to NetAI now",
                        l.since.elapsed().as_millis()
                    ));
                    self.applied.extend(a);
                    self.late.clear();
                }
                Err((true, e)) => {
                    self.late_busy += 1;
                    if self.late_busy == 1 || self.late_busy % 60 == 0 {
                        log::line(&format!("nsession: chr loop patch postponed: {e} (try {})", self.late_busy));
                    }
                    if self.late_busy >= LATE_BUSY_MAX {
                        l.failed_attempt = true;
                        self.disengage(l, "chr loop patch site stayed busy", Reason::Other, true, false);
                        return;
                    }
                }
                Err((false, e)) => {
                    self.broke(l, "chr loop patch", &e);
                    return;
                }
            }
        }
        if l.owned23_logs < 5 && served_23 {
            l.owned23_logs += 1;
            let (_, _, _, here) = owners();
            log::line(&format!("nsession: 23 rx: owned here {here} ({} s after engage)", l.since.elapsed().as_secs_f32() as u32));
        }
    }

    /// The other players that own enemies natively, with their native remotes' positions:
    /// host = its engaged clients; client = the host and the clients the host says are engaged.
    fn competitors(&self, f: &Frame) -> Vec<(u8, Option<[f32; 3]>)> {
        let me = f.peers.me().slot();
        let mask = if self.host { engaged_links() } else { 1 | PEER_ENGAGED.load(Ordering::Relaxed) };
        mask_slots(mask)
            .into_iter()
            .filter(|&s| s != me)
            .map(|s| (s, PeerId::new(s).and_then(|id| f.remote_of(id)).and_then(|c| c.position())))
            .collect()
    }

    /// Once per frame while any link is engaged: Owned lists, the Engaged set, the
    /// ownerless report, ownfill, the host's 22 feed, timers.
    fn engaged_common(&mut self, f: &Frame) {
        update_pnets(f);
        let now = Instant::now();
        if !self.host {
            let g = book();
            let mask = g.engaged.as_ref().filter(|(generation, _)| accepts(0, *generation)).map_or(0, |(_, s)| {
                s.iter().fold(0u8, |m, &x| m | 1 << x)
            });
            PEER_ENGAGED.store(mask, Ordering::Release);
        }
        // Our world update stopped while engaged (pause menu): what we own stands still
        // on every screen until it runs again. Logged, not a reason to leave.
        if !self.world_running() && !self.still_logged {
            self.still_logged = true;
            log::line(&format!(
                "nsession: our world update stopped while engaged (menu?): enemies owned here stand still on every screen until it runs; timers {:?}",
                timers()
            ));
        } else if self.world_running() && self.still_logged {
            self.still_logged = false;
            log::line("nsession: our world update runs again");
        }
        let me = f.peers.me().slot();
        if self.host {
            let mask = engaged_links();
            if mask != self.engaged_sent || self.engaged_sent_at.is_none_or(|t| t.elapsed() >= ENGAGED_EVERY) {
                self.engaged_sent = mask;
                self.engaged_sent_at = Some(now);
                let slots = mask_slots(mask);
                queue_ctrl_engaged(|generation| Msg::Engaged { generation, slots: slots.clone() });
            }
        }
        let comps = self.competitors(f);
        let comp_slots: Vec<u8> = comps.iter().map(|c| c.0).collect();
        // Paused peers (marker in their Owned list) do not compete for enemies.
        let paused_peers: Vec<u8> = owned_lists(&comp_slots)
            .map(|l| l.iter().filter(|(_, ids)| owned_is_paused(ids)).map(|(s, _)| *s).collect())
            .unwrap_or_default();
        let active: Vec<(u8, Option<[f32; 3]>)> = comps.iter().filter(|c| !paused_peers.contains(&c.0)).cloned().collect();
        self.pause_step(me, &active);
        // The 2 s heartbeat, plus (v8.30b) an early send when the list changed (at most every 250 ms).
        let owned_due = self.owned_sent_at.is_none_or(|t| t.elapsed() >= OWNED_EVERY);
        let owned_check = !owned_due
            && self.owned_sent_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(250))
            && self.owned_check_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
        if self.stage >= 2 && (owned_due || owned_check) {
            self.owned_check_at = Some(now);
            let mut ids = owned_ids();
            ids.retain(|n| !self.handed.contains(n));
            // v8.57: dead in a boss fight: the boss leaves our list like a paused hand-over, so a
            // living player's ownfill takes it (the game kept it ours natively, live 2026-10-27).
            if crate::bossrules::spectating() {
                let bosses = boss_net_ids();
                ids.retain(|n| !bosses.contains(n));
            }
            if self.pause.active {
                ids.push(OWNED_PAUSED_MARK);
            }
            if owned_due || ids != self.owned_sent_ids {
                self.owned_sent_at = Some(now);
                self.owned_sent_ids = ids.clone();
                queue_ctrl_engaged(|generation| Msg::Owned { generation, origin: me, ids: ids.clone() });
            }
        }
        if self.stage >= 2 && self.owned_sent_at.is_some() {
            self.resolve_double_claims(me, &comp_slots);
        }
        if self.stage >= 2 && self.ownerless_at.is_none_or(|t| t.elapsed() >= STATS_EVERY) {
            self.ownerless_at = Some(now);
            ownerless_report(f.player, &comps, owned_lists(&comp_slots).as_deref());
            if !self.fill.ours.is_empty() || self.fill.taken > 0 {
                log::line(&format!(
                    "nsession: ownfill (engagement): {} ownerless enemies run here now, taken {}, given back {}",
                    self.fill.ours.len(),
                    self.fill.taken,
                    self.fill.given
                ));
            }
        }
        // Enemies nobody owns near the players get one (the client once its first 23 is in;
        // before that every enemy keeps its own AI there anyway).
        if self.stage >= 2
            && !self.pause.active
            && (self.host || self.late.is_empty())
            && self.fill.at.is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            let theirs = owned_lists(&comp_slots);
            self.fill.step(me, f.player, &active, theirs.as_deref());
        }
        if self.host && self.stage >= 2 {
            self.host_candidates(f);
        } else if !self.host && self.stage >= 2 && CLAIMS_GREW.swap(false, Ordering::Relaxed) {
            // A new claim (stealth approach / our deathblow): our candidates go out on the
            // next update instead of up to 5 s later (§34). ns_force_distribution only.
            force_due(T_DUE_22);
        }
        if !self.timers_logged && self.engaged_at.elapsed() >= Duration::from_secs(3) {
            self.timers_logged = true;
            let t = sync_table();
            log::line(&format!("nsession: sync table {t:x?} timers +0x398.. {:?}", t.and_then(|t| read::<[f32; 4]>(t + T_TIMERS))));
        }
    }

    /// Our world update paused while engaged (menu): after `WORLD_PAUSED_AFTER` the enemies
    /// we own near other players are left out of our Owned list (with the paused marker), so
    /// their ownfill takes them (nearest peer); back when our update runs again, where the
    /// peers' lower-slot / dwell rules settle who keeps what.
    fn pause_step(&mut self, me: u8, active: &[(u8, Option<[f32; 3]>)]) {
        if self.stage < 2 {
            return;
        }
        let stalled = if self.updates.is_some() { self.updates_at.elapsed() } else { Duration::ZERO };
        match self.pause.step(stalled) {
            PauseStep::Begin => {
                let Some(w) = wcm() else { return };
                let owned: Vec<(u16, [f32; 3])> = enemies()
                    .into_iter()
                    .filter(|c| c.hp().is_some_and(|hp| hp > 0))
                    .filter_map(|c| Some((netai::net_id(c)?, c.position()?)))
                    .filter(|&(n, _)| ownfill::game_owned(w, n) != 0 || ownfill::forced(n))
                    .collect();
                let targets = handover_targets(&owned, active);
                self.handed = targets.iter().map(|t| t.0).collect();
                self.handed.sort_unstable();
                self.handed.dedup();
                let mut per: BTreeMap<u8, u32> = BTreeMap::new();
                for &(_, s) in &targets {
                    *per.entry(s).or_default() += 1;
                }
                if per.is_empty() {
                    log::line("nsession: our world update paused: 0 enemies handed to a peer (none near another player)");
                }
                for (s, n) in per {
                    log::line(&format!("nsession: our world update paused: {n} enemies handed to peer {s}"));
                }
                self.owned_sent_at = None;
                let _ = me;
            }
            PauseStep::End => {
                log::line(&format!("nsession: our world update paused: ended, {} handed enemies back to the usual rules", self.handed.len()));
                self.handed.clear();
                self.owned_sent_at = None;
            }
            PauseStep::Idle => {}
        }
    }

    /// Double claims (live 2026-10-24: "claimed by both machines", up to 6): a claim of ours
    /// on an enemy that a lower slot (the host first) owns too, for `DOUBLE_CLAIM_AFTER`,
    /// is dropped; logged once per enemy per engagement. The host keeps its claims.
    fn resolve_double_claims(&mut self, me: u8, comp_slots: &[u8]) {
        let raw = claims_raw();
        if me == 0 || raw.is_empty() {
            self.both_since.clear();
            if let Ok(mut d) = DROPPED_CLAIMS.lock() {
                d.clear();
            }
            return;
        }
        let Some(theirs) = owned_lists(comp_slots) else { return };
        let now = Instant::now();
        let dropped: Vec<u16> = DROPPED_CLAIMS.lock().map(|d| d.clone()).unwrap_or_default();
        let mut new_drop = Vec::new();
        self.both_since.retain(|n, _| raw.contains(n));
        for &n in &raw {
            let owners: Vec<u8> = theirs.iter().filter(|(_, ids)| ids.binary_search(&n).is_ok()).map(|(s, _)| *s).collect();
            let we_own = owned_here(n);
            if dropped.contains(&n) || owners.is_empty() || !we_own {
                self.both_since.remove(&n);
                continue;
            }
            let since = *self.both_since.entry(n).or_insert(now);
            if double_claim_drop(me, true, we_own, &owners, now.duration_since(since)) {
                new_drop.push(n);
                if self.double_logged.insert(n) {
                    log::line(&format!(
                        "nsession: enemy netId {n:#x} claimed by both machines: slot {} owns it, our claim is dropped",
                        owners.iter().min().copied().unwrap_or(0)
                    ));
                }
            }
        }
        if let Ok(mut d) = DROPPED_CLAIMS.lock() {
            d.retain(|n| raw.contains(n));
            for n in new_drop {
                if !d.contains(&n) {
                    d.push(n);
                }
            }
        }
    }

    /// Host: each engaged client's candidate lists (22) with the claims applied (§27, §34).
    /// A netId claimed by two or more machines (us and / or clients) is settled here
    /// (`contest_winner`); the losers' entries for it go, as do every client's ordinary
    /// entries for our own claims. A change of what a client may keep re-feeds its last list
    /// at once and distributes (23) on the next update; a new claim of a client's too.
    fn host_candidates(&mut self, f: &Frame) {
        let inbound = take_inbound(T_CAND);
        let grew = CLAIMS_GREW.swap(false, Ordering::Relaxed);
        let mut fresh: BTreeMap<PeerId, Vec<Vec<u8>>> = BTreeMap::new();
        for (origin, p) in inbound {
            let Some(id) = PeerId::new(origin) else { continue };
            match self.links.get_mut(&id).filter(|l| l.phase == Phase::Engaged) {
                Some(l) => {
                    l.last22 = Some(p.clone());
                    fresh.entry(id).or_default().push(p);
                }
                None => bump(&DROP, T_CAND as u32),
            }
        }
        let ours = claims_now();
        let boss_forced: Vec<u16> = BOSS_FORCED.try_lock().map(|g| g.clone()).unwrap_or_default();
        // Who claims what: us (slot 0) and each engaged client from its last list.
        let mut claimants: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
        for &n in &ours {
            claimants.entry(n).or_default().push(PeerId::HOST.slot());
        }
        let mut friend_new = false;
        for l in self.links.values_mut().filter(|l| l.phase == Phase::Engaged) {
            let c = l.last22.as_deref().map(claimed_in_22).unwrap_or_default();
            friend_new |= c.iter().any(|n| !l.claimed.contains(n));
            for &n in &c {
                claimants.entry(n).or_default().push(l.slot());
            }
            l.claimed = c;
        }
        let contested: Vec<u16> = claimants.iter().filter(|(_, s)| s.len() >= 2).map(|(&n, _)| n).collect();
        let due = !fresh.is_empty() || grew || self.contest_at.is_none_or(|t| t.elapsed() >= CONTEST_EVERY);
        if due {
            self.contest_at = Some(Instant::now());
            let w = self.contest(f, &claimants, &contested);
            self.winners = w;
        } else {
            self.winners.retain(|n, _| contested.contains(n));
        }
        self.contested = contested;
        let mut any_change = false;
        let ids: Vec<PeerId> = self.links.iter().filter(|(_, l)| l.phase == Phase::Engaged).map(|(&id, _)| id).collect();
        for id in ids {
            let Some(remote) = f.remote_of(id) else { continue };
            let slot = id.slot();
            // What this client may not keep: our claims it did not win, and contested
            // netIds another machine won.
            let mut blocked: Vec<u16> = ours.iter().copied().filter(|n| self.winners.get(n).is_none_or(|&w| w != slot)).collect();
            blocked.extend(self.winners.iter().filter(|&(n, &w)| w != slot && !ours.contains(n)).map(|(&n, _)| n));
            // The host's forced boss members: the client's list must not keep them either.
            let blocked = blocked_with_bosses(blocked, &boss_forced);
            let Some(l) = self.links.get_mut(&id) else { continue };
            let changed = blocked != l.blocked;
            any_change |= changed;
            l.blocked = blocked;
            let lists = fresh.remove(&id).unwrap_or_default();
            // Re-settling this client's last list at once: ns_force_distribution only (v7.6:
            // our claims apply to its next 22).
            let refeed = lists.is_empty() && (grew || changed) && FORCE_DISTRIBUTION.load(Ordering::Relaxed);
            let lists: Vec<Vec<u8>> = if refeed { l.last22.clone().into_iter().collect() } else { lists };
            for mut p in lists {
                if !l.blocked.is_empty()
                    && let Some((kept, n)) = strip_22(&p, &l.blocked, &l.blocked)
                {
                    if !boss_forced.is_empty() {
                        let nb = list_22(&p).map_or(0, |v| v.iter().filter(|&&x| boss_forced.contains(&((x & 0xFFFF) as u16))).count());
                        STRIPPED_BOSS.fetch_add(nb as u32, Ordering::Relaxed);
                    }
                    p = kept;
                    STRIPPED.fetch_add(n as u32, Ordering::Relaxed);
                }
                match feed_22(&p, remote) {
                    Ok(n) if refeed => {
                        if l.host22_logs < 30 {
                            l.host22_logs += 1;
                            log::line(&format!("nsession: host: our claims changed: peer {slot}'s last candidates ({n}) settled again"));
                        }
                    }
                    Ok(n) => {
                        bump(&SRV, T_CAND as u32);
                        if !l.first22_forced {
                            // Give this client its candidates now, not at the next 3 s tick.
                            l.first22_forced = true;
                            let forced = make_due(T_DUE_23);
                            log::line(&format!(
                                "nsession: host: first 22 of peer {slot} this engagement ({n} cand, {} ms after engage): 23 distribution forced to the next update: {forced}",
                                l.since.elapsed().as_millis()
                            ));
                        }
                        if l.host22_logs < 10 {
                            l.host22_logs += 1;
                            log::line(&format!("nsession: host 22 from peer {slot}: {n} cand"));
                        }
                    }
                    Err(e) => {
                        if !refeed {
                            bump(&DROP, T_CAND as u32);
                        }
                        if l.host22_logs < 10 {
                            l.host22_logs += 1;
                            log::line(&format!("nsession: host 22 from peer {slot} dropped: {e}"));
                        }
                    }
                }
            }
        }
        if grew || any_change || friend_new {
            // Ownership moves on the next update, not at the game's next 3 s tick
            // (ns_force_distribution only, at most once per second).
            force_due(T_DUE_23);
        }
    }

    /// Host: who runs each contested enemy (two or more machines claim it): slot.
    fn contest(&mut self, f: &Frame, claimants: &BTreeMap<u16, Vec<u8>>, contested: &[u16]) -> BTreeMap<u16, u8> {
        let mut out = BTreeMap::new();
        if contested.is_empty() {
            return out;
        }
        let now = crate::game::now_ms();
        let ring = THROWS.lock().map(|g| *g).unwrap_or([NO_THROW; 16]);
        let horiz = |a: [f32; 3], b: [f32; 3]| ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        let pos_of = |s: u8| -> Option<[f32; 3]> {
            if s == PeerId::HOST.slot() { f.player.position() } else { PeerId::new(s).and_then(|id| f.remote_of(id)).and_then(|c| c.position()) }
        };
        let chrs: HashMap<u16, Chr> =
            enemies().into_iter().filter_map(|c| Some((netai::net_id(c)?, c))).filter(|(n, _)| contested.contains(n)).collect();
        for &n in contested {
            let Some(slots) = claimants.get(&n) else { continue };
            let ep = chrs.get(&n).and_then(|c| c.position());
            let cands: Vec<Contender> = slots
                .iter()
                .map(|&s| Contender {
                    slot: s,
                    throw_age: slot_throw_age(&ring, n, s, now, THROW_CLAIM_MS),
                    dist: pos_of(s).zip(ep).map(|(a, b)| horiz(a, b)),
                })
                .collect();
            let held = self.winners.get(&n).copied();
            let w = contest_winner(&cands, held);
            out.insert(n, w);
            if Some(w) != held || !self.contested.contains(&n) {
                if w == PeerId::HOST.slot() { &CONTESTS_WON } else { &CONTESTS_LOST }.fetch_add(1, Ordering::Relaxed);
                if self.contest_logs < 40 {
                    self.contest_logs += 1;
                    let e = chrs.get(&n).and_then(|c| c.entity_id()).unwrap_or(-1);
                    let m = |d: Option<f32>| d.map_or("?".into(), |d| format!("{d:.1}"));
                    if let [h, c] = cands.as_slice()
                        && h.slot == PeerId::HOST.slot()
                    {
                        // Two machines, the host one of them: the 2-player line (§34).
                        let why = match (h.throw_age, c.throw_age) {
                            (Some(a), Some(b)) => format!("both deathblowed it, ours {a} ms ago, the friend's {b} ms ago"),
                            (Some(a), None) => format!("our deathblow {a} ms ago"),
                            (None, Some(b)) => format!("the friend's deathblow {b} ms ago"),
                            (None, None) => format!("us {} m, the friend {} m", m(h.dist), m(c.dist)),
                        };
                        let runs = if w == PeerId::HOST.slot() { "the host".to_owned() } else { format!("the friend (peer {w})") };
                        log::line(&format!("stealth: both claim enemy {e} netId {n:#x}: {runs} runs it ({why})"));
                    } else {
                        let who = |s: u8| if s == PeerId::HOST.slot() { "the host".to_owned() } else { format!("peer {s}") };
                        let why: Vec<String> = cands
                            .iter()
                            .map(|c| match c.throw_age {
                                Some(a) => format!("{} deathblow {a} ms ago", who(c.slot)),
                                None => format!("{} {} m", who(c.slot), m(c.dist)),
                            })
                            .collect();
                        log::line(&format!("stealth: {} machines claim enemy {e} netId {n:#x}: {} runs it ({})", cands.len(), who(w), why.join(", ")));
                    }
                }
            }
        }
        out
    }

    fn stats(&mut self) {
        if self.stats_at.is_none_or(|t| t.elapsed() < STATS_EVERY) {
            return;
        }
        self.stats_at = Some(Instant::now());
        let take = |a: &[AtomicU32; TYPES], t: usize| a[t].swap(0, Ordering::Relaxed);
        let mut parts = Vec::new();
        for t in 0..TYPES {
            let (tx, rx, srv, dr, big) = (take(&TX, t), take(&RX, t), take(&SRV, t), take(&DROP, t), take(&BIG, t));
            let (rl, un, pd) = (take(&RELAY, t), take(&UNMAPPED, t), take(&PNET_DROP, t));
            if tx + rx + srv + dr + big + rl + un + pd > 0 {
                let mut s = format!("{t}: tx {tx} rx {rx} srv {srv} drop {dr} big {big}");
                if rl > 0 {
                    s.push_str(&format!(" relay {rl}"));
                }
                if un > 0 {
                    s.push_str(&format!(" unmapped {un}"));
                }
                if pd > 0 {
                    s.push_str(&format!(" player-not-here {pd}"));
                }
                parts.push(s);
            }
        }
        let phases: Vec<String> = self.links.values().map(|l| format!("peer {} {:?}", l.slot(), l.phase)).collect();
        log::line(&format!(
            "nsession: types (10 s, [{}] stage {}) {}",
            phases.join(", "),
            self.stage,
            if parts.is_empty() { "none".into() } else { parts.join(" | ") }
        ));
        let single = SINGLE_23.swap(0, Ordering::Relaxed);
        // Only meaningful with the recipient wraps patched; without them (max_players = 2) every 23
        // takes this path by design.
        if single > 0 && self.wraps_applied {
            log::line(&format!("nsession: {single} type-23 sends without a known recipient went to the one engaged peer"));
        }
        let mut hooks = Vec::new();
        for (h, d) in HOOK_DEFS.iter().enumerate() {
            let (c, r) = (CALLS[h].swap(0, Ordering::Relaxed), ROUTED[h].swap(0, Ordering::Relaxed));
            let types: Vec<String> = (0..TYPES)
                .filter_map(|t| {
                    let n = SEEN[h][t].swap(0, Ordering::Relaxed);
                    (n > 0).then(|| format!("{t}:{n}"))
                })
                .collect();
            if c > 0 {
                hooks.push(format!("{} {c} (routed {r}) [{}]", &d.name[3..], types.join(" ")));
            }
        }
        if !hooks.is_empty() {
            log::line(&format!("nsession: hook calls {}", hooks.join("; ")));
        }
        for (s, st, backlog) in lane_stats() {
            let stale = book().peers[s as usize].as_mut().map_or(0, |p| std::mem::take(&mut p.stale_packets));
            log::line(&format!(
                "nsession: lane peer {s} (10 s): sent {} resent {} acked {} given up {}; received {} dup {} delivered {} gaps skipped {} bad {}; backlog {backlog} fragments, stale from it {stale}",
                st.sent, st.resent, st.acked, st.given_up, st.received, st.duplicates, st.delivered, st.gaps_skipped, st.bad
            ));
        }
        if self.any_engaged() {
            let (com, net, other, here) = owners();
            log::line(&format!("nsession: owners: Com {com} / NetAI {net} / other {other} / owned-here {here}"));
            let claims = claims_now().len();
            let (pk, st) = (CLAIM_PACKETS.swap(0, Ordering::Relaxed), STRIPPED.swap(0, Ordering::Relaxed));
            let (cw, cl) = (CONTESTS_WON.swap(0, Ordering::Relaxed), CONTESTS_LOST.swap(0, Ordering::Relaxed));
            let (drops, tc) = (THROW_DROPS.swap(0, Ordering::Relaxed), THROW_CLAIMS.swap(0, Ordering::Relaxed));
            let fskip = FORCES_SKIPPED.swap(0, Ordering::Relaxed);
            let sb = STRIPPED_BOSS.swap(0, Ordering::Relaxed);
            if sb > 0 {
                log::line(&format!("nsession: stripped boss members from the friend's candidates: {sb} (10 s)"));
            }
            if claims > 0 || pk > 0 || st > 0 || cw + cl + drops + tc + fskip > 0 {
                log::line(&format!(
                    "nsession: stealth claims (10 s): ours now {claims}; {}; our deathblows claimed {tc}, the friend's throws on them not applied {drops}; forced distributions rate-limited {fskip} (ns_force_distribution {}, ns_throw_claims {}, ns_throw_collide_drop {})",
                    if self.host {
                        format!("clients' candidates for them removed {st}, contests kept {cw} / given to a client {cl}")
                    } else {
                        format!("candidate lists sent with a claim {pk}, last with {} claimed entries", CLAIM_ENTRIES.load(Ordering::Relaxed))
                    },
                    FORCE_DISTRIBUTION.load(Ordering::Relaxed) as u8,
                    THROW_CLAIMS_ON.load(Ordering::Relaxed) as u8,
                    THROW_COLLIDE_DROP.load(Ordering::Relaxed) as u8
                ));
            }
        }
    }

    /// `native_session = 0`: every link that had a handshake on its link is told once (a
    /// latched leave), so its own enemy sync comes back too.
    fn switched_off(&mut self, f: &Frame) {
        let ids: Vec<PeerId> = self.links.keys().copied().collect();
        for id in ids {
            let epoch = f.peer(id).map(|p| p.epoch);
            self.with_link(id, |e, l| {
                if l.said_off || epoch != Some(l.epoch) || l.generation == 0 {
                    return;
                }
                l.said_off = true;
                if l.phase != Phase::Idle {
                    e.disengage(l, "native_session = 0", Reason::SettingOff, false, false);
                }
                if f.connected {
                    queue_ctrl(l.slot(), &Msg::Leave { generation: l.generation, latch: true, reason: Reason::SettingOff as u8 });
                }
                log::line(&format!("nsession: native_session = 0, peer {} was told to use our own sync", l.slot()));
            });
        }
        self.stats();
    }

    fn tick(&mut self, f: &Frame) {
        let stage = tuning::native_session_stage();
        if f.is_host != self.host {
            if self.links.values().any(|l| l.phase != Phase::Idle) {
                self.disengage_all("our role changed", Reason::Other, false, false);
            }
            self.links.clear();
            self.host = f.is_host;
        }
        ROLE_HOST.store(self.host, Ordering::Relaxed);
        // The handshake partners this frame with their link epochs: host = every client,
        // client = the host.
        let partners: Vec<(PeerId, u64)> = if f.is_host {
            f.peers.iter().filter(|p| !p.id.is_host()).map(|p| (p.id, p.epoch)).collect()
        } else {
            f.peers.host().map(|p| (p.id, p.epoch)).into_iter().collect()
        };
        let gone: Vec<PeerId> = self.links.keys().copied().filter(|id| partners.iter().all(|(p, _)| p != id)).collect();
        for id in gone {
            if self.links.get(&id).is_some_and(|l| l.phase != Phase::Idle) {
                self.disengage_id(id, "link down", Reason::Other, false, false);
            }
            self.links.remove(&id);
        }
        for &(id, epoch) in &partners {
            match self.links.get(&id).map(|l| l.epoch) {
                Some(ep) if ep == epoch => {}
                Some(_) => {
                    if self.links.get(&id).is_some_and(|l| l.phase != Phase::Idle) {
                        self.disengage_id(id, "new connection", Reason::Other, false, false);
                    }
                    self.links.insert(id, Link::new(id, epoch));
                    // A new connection starts clean (`native_remote = 0` is checked again below).
                    self.failed = None;
                }
                None => {
                    self.links.insert(id, Link::new(id, epoch));
                    self.failed = None;
                }
            }
        }
        if !f.connected {
            if self.links.values().any(|l| l.phase != Phase::Idle) {
                self.disengage_all("link down", Reason::Other, false, false);
            }
            OWNS_ENEMIES.store(false, Ordering::Relaxed);
            return;
        }
        if !tuning::native_remote() && self.failed.is_none() {
            self.failed = Some("native_remote = 0".into());
            log::line("nsession: needs native_remote = 1 (the other players as real players); off");
        }
        let changed: Vec<PeerId> = self.links.iter().filter(|(_, l)| l.phase != Phase::Idle && l.stage != stage).map(|(id, _)| *id).collect();
        self.disengage_many(&changed, "native_session_stage changed", Reason::Other, true, false);
        // Our own block changed (a load is coming): every engaged link goes in one call, no
        // record clearing for the links still to go (`disengage_many`).
        let moved: Vec<PeerId> = self.links.iter().filter(|(_, l)| l.phase == Phase::Engaged && f.here != Some(l.block)).map(|(id, _)| *id).collect();
        if !moved.is_empty() {
            let why = format!("our block changed (ours {:?}, was {:?})", f.here, self.links.get(&moved[0]).map(|l| l.block));
            self.disengage_many(&moved, &why, Reason::FriendElsewhere, true, false);
        }
        let broken = BROKEN.load(Ordering::Acquire);
        let ids: Vec<PeerId> = self.links.keys().copied().collect();
        for &id in &ids {
            self.with_link(id, |e, l| {
                // Ours cannot run this time: tell the peer once per link (a stage-0 hello),
                // so it keeps its own sync instead of waiting for handshakes.
                if broken && !l.said_broken {
                    l.said_broken = true;
                    l.generation = e.bump_gen();
                    queue_ctrl(
                        l.slot(),
                        &Msg::Hello { proto: PROTO, stage: 0, host: e.host, block: f.here.unwrap_or(-1), generation: l.generation },
                    );
                    log::line(&format!("nsession: told peer {} our native session is unavailable", l.slot()));
                }
                l.unavailable = peer_unavailable(l.slot(), l.epoch);
                if book().peers[l.slot() as usize].as_ref().is_some_and(|p| p.epoch == l.epoch && p.hello.is_some()) {
                    l.no_hello = 0;
                }
            });
        }
        self.observe_world();
        self.update_blockers(f, stage);
        let mut own_sync: Option<String> = None;
        for &id in &ids {
            let blocked = self.own_sync_reason(f, id);
            self.with_link(id, |e, l| {
                l.held = blocked.is_some();
                if let Some(why) = &blocked
                    && l.phase != Phase::Idle
                {
                    e.disengage(l, &format!("our own sync for this block: {why}"), Reason::OwnSync, true, false);
                }
                match l.phase {
                    Phase::Idle if blocked.is_none() => e.maybe_start(l, f, stage, broken),
                    Phase::Idle => {}
                    Phase::Handshake => e.handshake(l, f),
                    Phase::Engaged => e.engaged_link(l, f),
                }
            });
            if own_sync.is_none() {
                own_sync = blocked;
            }
        }
        if own_sync != self.own_sync {
            match &own_sync {
                Some(why) => log::line(&format!("nsession: host keeps our own enemy sync: {why}")),
                None => log::line("nsession: the native session may run again for every peer here"),
            }
            self.own_sync = own_sync;
        }
        if self.any_engaged() {
            self.engaged_common(f);
        }
        // Review #5: our own enemy sync runs until the session is ENGAGED (and from the
        // moment it disengages), so there is never a window with neither. Handover: the
        // frame after engaging, enemies.rs releases its NetAI enemies (release_all).
        set_owns(self.any_engaged() && self.stage >= 2);
        self.stats();
    }
}

/// Our player and every peer's native remote: player netIds (both forms) and record handles.
fn update_pnets(f: &Frame) {
    let raw = |c: Chr| read::<u16>(c.0 + CHR_NET_ID).filter(|&n| n != NO_NET_ID);
    let mut pnet = [NO_NET_ID; SLOTS];
    let mut alt = [NO_NET_ID; SLOTS];
    let mut handle = [0u32; SLOTS];
    let me = f.peers.me().index();
    if me < SLOTS {
        let a = netai::net_id_of(f.player);
        pnet[me] = raw(f.player).or(a).unwrap_or(NO_NET_ID);
        alt[me] = a.unwrap_or(NO_NET_ID);
    }
    for p in f.peers.iter() {
        let (s, Some(r)) = (p.id.index(), p.native) else { continue };
        if s >= SLOTS || s == me {
            continue;
        }
        let a = netai::net_id_of(r);
        pnet[s] = raw(r).or(a).unwrap_or(NO_NET_ID);
        alt[s] = a.unwrap_or(NO_NET_ID);
        handle[s] = read::<u32>(r.0 + 8).filter(|&h| h != 0 && h != u32::MAX).unwrap_or(0);
    }
    for s in 0..SLOTS {
        PNET[s].store(pnet[s], Ordering::Relaxed);
        PNET_ALT[s].store(alt[s], Ordering::Relaxed);
        PEER_HANDLE[s].store(handle[s], Ordering::Relaxed);
    }
}

/// Host: a client that left alone gets an empty candidate list in its ownership record
/// (its enemies go to the others at the next distribution, forced now). Some(handle)
/// when a record was emptied.
fn clear_record(slot: u8) -> Option<u32> {
    let h = PEER_HANDLE.get(slot as usize)?.load(Ordering::Relaxed);
    if h == 0 {
        return None;
    }
    let t = sync_table()?;
    let base = module_base();
    let find: extern "C" fn(usize, u32) -> usize = unsafe { std::mem::transmute(base + FIND_RECORD_RVA) };
    let set: extern "C" fn(usize, i32, *const u32) = unsafe { std::mem::transmute(base + SET_CANDIDATES_RVA) };
    let rec = find(t, h);
    if rec < 0x10000 {
        return None;
    }
    let none: [u32; 1] = [0];
    set(rec, 0, none.as_ptr());
    make_due(T_DUE_23);
    Some(h)
}

static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

/// The stage in the hello of the peer on link `slot` (this link epoch), if one came in.
fn peer_hello_stage(slot: u8, epoch: u64) -> Option<u8> {
    book().peers.get(slot as usize).and_then(Option::as_ref).filter(|p| p.epoch == epoch).and_then(|p| p.hello).map(|h| h.1)
}

/// The peer on link `slot` said its native session cannot run (a stage-0 hello, or a
/// latched "check failed" leave for its current hello): no handshakes with it on this link.
fn peer_unavailable(slot: u8, epoch: u64) -> bool {
    static SHOWN: [AtomicU64; SLOTS] = [const { AtomicU64::new(u64::MAX) }; SLOTS];
    let out = book().peers.get(slot as usize).and_then(Option::as_ref).filter(|p| p.epoch == epoch).is_some_and(|p| match p.hello {
        Some((_, 0, ..)) => true,
        Some((.., pg)) => p.leave.is_some_and(|(lg, latch, r)| lg == pg && latch && r == Reason::CheckFailed as u8),
        None => false,
    });
    if out && SHOWN[slot as usize % SLOTS].swap(epoch, Ordering::Relaxed) != epoch {
        log::line(&format!("nsession: peer {slot}'s native session is unavailable: our own enemy sync with it"));
        crate::bossrules::notice("Native session unavailable on a companion's side".into(), 10);
    }
    out
}

/// `OWNS_ENEMIES`, with a log line when it changes.
fn set_owns(owns: bool) {
    if OWNS_ENEMIES.swap(owns, Ordering::Relaxed) != owns {
        log::line(&format!(
            "nsession: enemies are {}",
            if owns { "the game's own (native session engaged); our enemy sync steps aside" } else { "ours (our enemy sync runs)" }
        ));
    }
}

/// The engine, also after a panic inside it (the patches must still come back out).
fn engine() -> std::sync::MutexGuard<'static, Option<Engine>> {
    ENGINE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// puppet.rs, every in-world frame (game thread, FrameBegin).
pub fn tick(f: &Frame) {
    throw_probe::tick();
    ME.store(f.peers.me().slot(), Ordering::Relaxed);
    dblock::frame(f.player.0, f.peers);
    let on = tuning::native_session();
    let mut g = engine();
    if !on {
        OWNS_ENEMIES.store(false, Ordering::Relaxed);
        if let Some(e) = g.as_mut() {
            e.switched_off(f);
        }
        return;
    }
    g.get_or_insert_with(Engine::new).tick(f);
}

/// A new handshake waits this long after a rest of ours (restsync.rs). Must outlast
/// restsync.rs `HOLD_KILLS_FOR` (12 s) by at least 1 s, so the handshake never starts
/// inside the window in which the host's kills are held back.
const REST_HOLD: Duration = Duration::from_secs(13);

/// restsync.rs, the frame after a rest of ours (our own, mirrored, resync): our enemies
/// were respawned in place while the game's ownership / HP tables (types 5 / 22 / 23,
/// the owner's records) still describe the world before the rest; the other side of the
/// same rest reaches it a round trip plus the idol's 1.333 s later. Engaged, the
/// owner's type-5 HP of a still-dead enemy kills our respawned one again (`0xA4B830` ->
/// `0x9EE7E0`, SPEC §HP). So: out with every peer (each gets Leave(Rest)), our own sync
/// with its rest window runs meanwhile, and a fresh handshake (netId list with the new
/// HPs, HP adoption) only starts `REST_HOLD` after the rest, when both resets are done.
pub fn on_rest(why: &str) {
    let mut g = engine();
    let Some(e) = g.as_mut() else { return };
    let was = e.links.values().any(|l| l.phase != Phase::Idle);
    if was {
        e.disengage_all(&format!("{why} rest: enemies reset"), Reason::Rest, true, false);
    }
    let until = Instant::now() + REST_HOLD;
    e.rest_hold_until = Some(until);
    for l in e.links.values_mut() {
        l.cooldown_until = Some(until);
    }
    if was || tuning::native_session() {
        log::line(&format!(
            "nsession: {why} rest: {}a new handshake in {} s (ownership and HP tables from before the rest are dropped)",
            if was { "disengaged, " } else { "" },
            REST_HOLD.as_secs()
        ));
    }
}

/// puppet.rs `invalidate_world` (load, our warp, player re-created): everything out
/// before the world goes; the next world starts new handshakes (and netId checks).
pub fn on_world_reset(why: &str) {
    let mut g = engine();
    let Some(e) = g.as_mut() else { return };
    e.disengage_all(why, Reason::Load, true, false);
    e.failed = None;
    e.rest_hold_until = None;
    let broken = BROKEN.load(Ordering::Acquire);
    for l in e.links.values_mut() {
        l.failed = None;
        let was = l.fallback_world;
        l.fallback_world = l.fallback_next || broken;
        l.fallback_next = false;
        if l.fallback_world != was {
            log::line(&if l.fallback_world {
                format!("nsession: new world uses our own enemy sync with peer {} (fallback after a failure)", l.slot())
            } else {
                format!("nsession: new world, the native session with peer {} is tried again", l.slot())
            });
        }
        l.soft_fails = 0;
        l.cooldown_until = None;
        l.attempts = 0;
        l.failed_attempt = false;
        l.held = false;
    }
}

/// lib.rs, the mod gives up after repeated panics: every patch back, routing off.
pub fn shutdown() {
    OWNS_ENEMIES.store(false, Ordering::Relaxed);
    BROKEN.store(true, Ordering::Release);
    ROUTE.store(false, Ordering::Release);
    ANY_MEMBER.store(false, Ordering::Release);
    set_whitelist(0);
    for a in &ACCEPT {
        a.store(0, Ordering::Release);
    }
    ENGAGED_LINKS.store(0, Ordering::Release);
    if let Some(e) = engine().as_mut() {
        restore_patches(&mut e.applied);
        for l in e.links.values_mut() {
            l.phase = Phase::Idle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timed_out_patch_try_waits_longer_each_time() {
        let ms = |n| busy_retry_gap(n).as_millis();
        assert_eq!([ms(1), ms(2), ms(3), ms(4), ms(5), ms(40)], [1000, 2000, 4000, 8000, 8000, 8000]);
        assert_eq!(ms(0), 1000, "never zero (no every-frame retry)");
        // 12 tries take over a minute, as the 600 busy frames did.
        let total: u128 = (1..BUSY_TIMEOUTS_MAX).map(ms).sum();
        assert!(total >= 60_000, "{total}");
    }

    #[test]
    fn stale_generations_are_ignored() {
        assert!(!hello_is_stale(None, 5));
        assert!(hello_is_stale(Some(4), 3));
        assert!(!hello_is_stale(Some(4), 4));
        assert!(!hello_is_stale(Some(4), 5));
        // 0 = unknown generation: never stale, never raises the mark
        assert!(!hello_is_stale(Some(4), 0));
        assert_eq!(raise_newest(Some(4), 0), Some(4));
        assert_eq!(raise_newest(None, 3), Some(3));
        assert_eq!(raise_newest(Some(4), 5), Some(5));
        assert_eq!(raise_newest(Some(4), 4), Some(4));
        // 3 and 4 arriving after 12: both ignored, 12 stays
        let mut n = Some(12);
        for g in [3, 4] {
            assert!(hello_is_stale(n, g));
        }
        n = raise_newest(n, 13);
        assert_eq!(n, Some(13));
    }

    #[test]
    fn generation_compare_wraps() {
        assert!(hello_is_stale(Some(2), u32::MAX - 1)); // counter wrapped: MAX-1 is older than 2
        assert!(!hello_is_stale(Some(u32::MAX - 1), 2)); // and 2 is newer than MAX-1
        assert_eq!(raise_newest(Some(u32::MAX), 1), Some(1));
        assert_eq!(raise_newest(Some(1), u32::MAX), Some(1));
        assert!(!stale_log_due(6) && stale_log_due(5) && stale_log_due(50));
    }

    #[test]
    fn engage_only_on_exact_generations() {
        // ready names the peer's hello generation (7) and our current one (12)
        assert_eq!(engage_block_reason(12, 7, Some((7, 12)), Some(7)), None);
        assert_eq!(engage_block_reason(12, 7, Some((7, 12)), None), None);
        // ready for our older generation 11, accepted before
        assert!(engage_block_reason(12, 7, Some((7, 11)), Some(7)).is_some());
        // ready from an older hello of the peer
        assert!(engage_block_reason(12, 7, Some((4, 12)), Some(7)).is_some());
        assert!(engage_block_reason(12, 7, None, Some(7)).is_some());
        // the peer has moved on to generation 8 since
        assert!(engage_block_reason(12, 7, Some((7, 12)), Some(8)).is_some());
    }

    #[test]
    fn throw_window_and_remote_involvement() {
        assert!(!throw_window_active(0, 100));
        assert!(throw_window_active(5000, 4999));
        assert!(!throw_window_active(5000, 5000));
        // sent (wire form): another slot's player as attacker or defender; never our own
        let none = [[NO_NET_ID; 2]; SLOTS];
        assert_eq!(throw_remote_slots(true, Some(WIRE_SLOT | 1), 0x100, 0, &none), 0b10);
        assert_eq!(throw_remote_slots(true, Some(0x100), WIRE_SLOT | 3, 0, &none), 0b1000);
        assert_eq!(throw_remote_slots(true, Some(WIRE_SLOT), 0x100, 0, &none), 0);
        assert_eq!(throw_remote_slots(true, Some(WIRE_SLOT | 2), WIRE_SLOT | 4, 1, &none), 0b10100);
        // received (local netIds): the slot whose remote has that netId (either form)
        let mut pnets = none;
        pnets[0] = [0xA000, NO_NET_ID]; // our player (we are slot 0)
        pnets[1] = [7, NO_NET_ID];
        pnets[2] = [8, 9];
        assert_eq!(throw_remote_slots(false, Some(7), 0x100, 0, &pnets), 0b10);
        assert_eq!(throw_remote_slots(false, Some(0x100), 9, 0, &pnets), 0b100);
        assert_eq!(throw_remote_slots(false, Some(0xA000), 0x101, 0, &pnets), 0);
        assert_eq!(throw_remote_slots(false, Some(NO_NET_ID), NO_NET_ID, 0, &pnets), 0);
    }

    #[test]
    fn messages_roundtrip_and_reject_truncation_and_trailing_bytes() {
        let all = [
            Msg::Hello { proto: PROTO, stage: 2, host: true, block: 1100, generation: 7 },
            Msg::List { generation: 3, block: -1, pairs: vec![(1_100_000, 0x0801, 450), (2_000_123, 0xFFFE, 0)] },
            Msg::List { generation: 3, block: 5, pairs: vec![] },
            Msg::Ready { generation: u32::MAX, peer_generation: 4 },
            Msg::Leave { generation: 9, latch: true, reason: Reason::NetIdMismatch as u8 },
            Msg::Owned { generation: 5, origin: 0, ids: vec![0x0801, 0x0FFF, 0x1800] },
            Msg::Owned { generation: 6, origin: 3, ids: vec![] },
            Msg::Lives {
                generation: 7,
                origin: 2,
                entries: vec![(0x083F, 1, 0), (0x0821, 0, LIFE_DEAD | LIFE_FRESH), (0x0800, -1, LIFE_DEAD)],
            },
            Msg::Lives { generation: 8, origin: 1, entries: vec![] },
            Msg::Engaged { generation: 9, slots: vec![1, 2, 5] },
            Msg::Engaged { generation: 10, slots: vec![] },
            Msg::MaxHp { generation: 9, max: vec![(1_120_353, 349), (1_100_301, 9999)] },
            Msg::MaxHp { generation: 9, max: vec![] },
        ];
        for m in all {
            let b = encode(&m);
            assert_eq!(decode(&b), Some(m.clone()), "{m:?}");
            for cut in 0..b.len() {
                assert_eq!(decode(&b[..cut]), None, "{m:?} cut at {cut}");
            }
            let mut extra = b.clone();
            extra.push(0);
            assert_eq!(decode(&extra), None, "{m:?} trailing byte");
        }
        assert_eq!(decode(&[99, 0, 0]), None);
        // An origin or a slot past the six players is malformed.
        assert_eq!(decode(&encode(&Msg::Owned { generation: 1, origin: 6, ids: vec![] })), None);
        assert_eq!(decode(&encode(&Msg::Engaged { generation: 1, slots: vec![6] })), None);
    }

    #[test]
    fn raw_framing_carries_the_generation_origin_and_mode() {
        let b = encode_raw(0xDEAD_BEEF, 2, MODE_ALL, &[1, 2, 3]);
        assert_eq!(decode_raw(&b), Some((0xDEAD_BEEF, 2, MODE_ALL, &[1u8, 2, 3][..])));
        let b = encode_raw(7, 0, MODE_PEER, &[9]);
        assert_eq!(decode_raw(&b), Some((7, 0, MODE_PEER, &[9u8][..])));
        assert_eq!(decode_raw(&b[..RAW_HEADER]), None, "no game bytes");
        assert_eq!(decode_raw(&[1, 2]), None);
        assert_eq!(decode_raw(&encode_raw(1, 6, MODE_ALL, &[1])), None, "origin past the slots");
        assert_eq!(decode_raw(&encode_raw(1, 1, 3, &[1])), None, "unknown mode");
        // The biggest unreliable game packets fit a PlayerRaw packet, and a lane fragment too.
        assert!(RAW_HEADER + 256 <= MAX_PLAYER_RAW);
        assert!(lane::HEADER + lane::FRAG <= MAX_PLAYER_RAW);
    }

    #[test]
    fn lane_relay_goes_hop_by_hop_with_the_origin() {
        // Host 0 engaged with clients 1, 2, 3 (bits 1..3).
        let all = 0b1110u8;
        // Client 2 broadcasts a 70: to the host only, mode "all"; the host takes it from link
        // 2 with origin 2 and relays it to 1 and 3 (never back to 2), origin kept.
        assert_eq!(targets(false, Dst::All, 2, 0b1), vec![0]);
        assert_eq!(wire_mode(false, Dst::All), MODE_ALL);
        assert!(origin_ok(true, 2, 2, 0) && relays(true, MODE_ALL));
        assert_eq!(targets(true, Dst::All, 2, all), vec![1, 3]);
        // The relayed frame: the host's generation for each link, origin 2, the same bytes.
        let wire = encode_raw(41, 2, wire_mode(true, Dst::All), &[0x46, 0xF2, 0xFF]);
        assert_eq!(decode_raw(&wire), Some((41, 2, MODE_ALL, &[0x46u8, 0xF2, 0xFF][..])));
        // Client 1 takes it: from the host link, origin 2 (not itself); a client never relays.
        assert!(origin_ok(false, 0, 2, 1) && !relays(false, MODE_ALL));
        assert!(!origin_ok(false, 0, 1, 1), "our own packet back");
        assert!(!origin_ok(false, 3, 3, 1), "a client hears only the host");
        assert!(!origin_ok(true, 2, 3, 0), "a client cannot speak for another");
        assert!(!origin_ok(true, 0, 0, 0));
        // 22 (to the host) and a client-owned hit to the host: consumed, never relayed.
        assert_eq!(wire_mode(false, Dst::Host), MODE_HOST);
        assert!(!relays(true, MODE_HOST));
        // The host's 23 for client 3 only; its own broadcast to every engaged client.
        assert_eq!(wire_mode(true, Dst::Peer(3)), MODE_PEER);
        assert_eq!(targets(true, Dst::Peer(3), 0, all), vec![3]);
        assert_eq!(targets(true, Dst::Peer(4), 0, all), Vec::<u8>::new(), "not engaged");
        assert_eq!(targets(true, Dst::All, 0, all), vec![1, 2, 3]);
        assert_eq!(targets(true, Dst::Host, 0, all), Vec::<u8>::new());
        // Not engaged with the host: a client sends nothing.
        assert_eq!(targets(false, Dst::Host, 1, 0), Vec::<u8>::new());
        // Two players: the host's broadcast reaches its one client, a client's reaches the
        // host and nobody else (nothing to relay).
        assert_eq!(targets(true, Dst::All, 0, 0b10), vec![1]);
        assert_eq!(targets(true, Dst::All, 1, 0b10), Vec::<u8>::new());
    }

    #[test]
    fn a_player_netid_without_a_player_here_is_detected_before_it_becomes_0xffff() {
        let mut map: PnetMap = [NO_NET_ID; SLOTS];
        map[0] = 0xA000;
        map[1] = 0xA001;
        // Type 21: attacker slot 0, defender slot 1 -> both here.
        let mut p = vec![0u8; 0x30];
        put_u16(&mut p, 0x00, WIRE_SLOT);
        put_u16(&mut p, 0x0C, WIRE_SLOT | 1);
        assert!(!pnet_unmapped(&p, T_DAMAGE, &map));
        // The defender is slot 3: no remote of it here.
        put_u16(&mut p, 0x0C, WIRE_SLOT | 3);
        assert!(pnet_unmapped(&p, T_DAMAGE, &map));
        // A plain enemy netId, or a type without player fields, never counts.
        put_u16(&mut p, 0x0C, 0x0801);
        assert!(!pnet_unmapped(&p, T_DAMAGE, &map));
        put_u16(&mut p, 0x0C, WIRE_SLOT | 3);
        assert!(!pnet_unmapped(&p, T_MOVE, &map));
        // Throw (18), second hit (28), ObjAct (45): their fields count too.
        put_u16(&mut p, 0x22, WIRE_SLOT | 4);
        assert!(pnet_unmapped(&p, T_THROW, &map));
        put_u16(&mut p, 0x04, WIRE_SLOT | 5);
        assert!(pnet_unmapped(&p, T_HIT2, &map));
        put_u16(&mut p, 0x00, WIRE_SLOT | 2);
        assert!(pnet_unmapped(&p, T_OBJ_EVENT, &map));
        // Too short to hold the field: nothing to say.
        assert!(!pnet_unmapped(&[0, 0], T_THROW, &map));
    }

    #[test]
    fn type_23_goes_to_its_recipient_only() {
        let engaged = 0b1010u8; // clients 1 and 3
        // The wraps name the recipient (chr first, the record as a fallback).
        assert_eq!(resolve_member(true, Some(3), None, engaged), Some(3));
        assert_eq!(resolve_member(true, None, Some(1), engaged), Some(1));
        assert_eq!(resolve_member(true, Some(1), Some(1), engaged), Some(1));
        // The wraps disagree: neither is trusted, the 23 is dropped.
        assert_eq!(resolve_member(true, Some(1), Some(3), engaged), None);
        assert_eq!(resolve_member(true, Some(1), Some(3), 0b10), None);
        // A recipient that is not engaged (its record outlived its engagement): nobody.
        assert_eq!(resolve_member(true, Some(2), None, engaged), None);
        // Unknown with two engaged: dropped, never broadcast.
        assert_eq!(resolve_member(true, None, None, engaged), None);
        // Two players: unknown = the one peer (the PROTO 4 "empty member = the friend").
        assert_eq!(resolve_member(true, None, None, 0b10), Some(1));
        assert_eq!(resolve_member(false, None, None, 0b10), Some(1));
        assert_eq!(resolve_member(false, None, None, engaged), None);
        assert_eq!(resolve_member(true, None, None, 0), None);
    }

    #[test]
    fn engaged_set_and_key() {
        // One engagement: the 2-player key (1 << 32 | the peer's generation).
        assert_eq!(engaged_key_of(&[]), 0);
        assert_eq!(engaged_key_of(&[(1, 1 << 32 | 7)]), 1 << 32 | 7);
        // Several: nonzero, and it changes when one of them changes or one more engages.
        let a = engaged_key_of(&[(1, 1 << 32 | 7), (2, 1 << 32 | 3)]);
        let b = engaged_key_of(&[(1, 1 << 32 | 7), (2, 1 << 32 | 4)]);
        let c = engaged_key_of(&[(1, 1 << 32 | 7), (2, 1 << 32 | 3), (3, 1 << 32 | 1)]);
        assert!(a != 0 && b != 0 && c != 0 && a != b && a != c);
        assert_eq!(a >> 32, 1);
        assert_eq!(mask_slots(0b10_0110), vec![1, 2, 5]);
        // The Engaged message carries exactly the slots of the mask.
        let m = Msg::Engaged { generation: 4, slots: mask_slots(0b1010) };
        assert_eq!(decode(&encode(&m)), Some(Msg::Engaged { generation: 4, slots: vec![1, 3] }));
    }

    #[test]
    fn own_sync_for_the_whole_block_when_a_peer_cannot_join() {
        let none: Vec<(u8, String)> = Vec::new();
        // Two players: nothing in the way (the 2-player build), even without a wrap.
        assert_eq!(own_sync_rule(2, 6, false, 1, &none), None);
        // A peer that cannot join keeps the others out too, but not itself (its own state
        // machine decides for it).
        let blockers = vec![(2u8, "failed this world".to_string())];
        assert!(own_sync_rule(3, 6, true, 1, &blockers).is_some_and(|w| w.contains("peer 2")));
        assert_eq!(own_sync_rule(3, 6, true, 2, &blockers), None);
        // Too many players, or 3+ without a type-23 recipient wrap: our own sync.
        assert!(own_sync_rule(4, 3, true, 1, &none).is_some());
        assert!(own_sync_rule(3, 6, false, 1, &none).is_some());
        assert_eq!(own_sync_rule(3, 3, true, 1, &none), None);
        // native_session_max_players = 2: native only in 2-player co-op.
        assert_eq!(own_sync_rule(2, 2, true, 1, &none), None);
        assert!(own_sync_rule(3, 2, true, 1, &none).is_some());
    }

    #[test]
    fn links_going_together_are_one_teardown_not_a_clearing_each() {
        let id = |s: u8| PeerId::new(s).unwrap();
        let engaged = [id(1), id(2), id(3)];
        assert!(goes_all_at_once(&engaged, &[id(1), id(2), id(3)]));
        assert!(goes_all_at_once(&engaged, &[id(3), id(1), id(2), id(4)]), "idle links in the list do not matter");
        assert!(!goes_all_at_once(&engaged, &[id(1), id(2)]), "one stays: the others go one by one");
        assert!(goes_all_at_once(&[], &[id(1)]), "nothing engaged: nothing to clear");
    }

    #[test]
    fn a_client_counts_only_the_hosts_packets_as_its_sync() {
        assert!(counts_as_sync_in(false, T_MOVE, 0));
        assert!(counts_as_sync_in(false, T_HP, 0));
        assert!(!counts_as_sync_in(false, T_MOVE, 2), "a relayed client's packet");
        assert!(!counts_as_sync_in(false, T_OWNERS, 0), "not a sync type");
        assert!(counts_as_sync_in(true, T_ACTION, 2), "the host has no such rule");
    }

    #[test]
    fn two_blockers_never_hold_each_other_out() {
        let both = vec![(1u8, "a".to_string()), (2u8, "b".to_string())];
        // The lowest blocker may try; the higher one waits for it; a non-blocker waits for all.
        assert_eq!(own_sync_rule(4, 6, true, 1, &both), None);
        assert!(own_sync_rule(4, 6, true, 2, &both).is_some_and(|w| w.contains("peer 1")));
        assert!(own_sync_rule(4, 6, true, 3, &both).is_some());
    }

    fn facts() -> BlockFacts {
        BlockFacts { hello_stage: Some(2), has_remote: true, loaded_recently: false, our_stage: 2 }
    }

    fn link(slot: u8) -> Link {
        Link::new(PeerId::new(slot).unwrap(), 1)
    }

    #[test]
    fn a_peer_that_is_not_engaged_after_an_attempt_blocks_the_block() {
        let x = facts();
        // Engaged, or never tried yet: not a blocker.
        let mut l = link(2);
        assert_eq!(l.blocks_block(&x), None);
        l.attempts = 1;
        l.phase = Phase::Handshake;
        assert_eq!(l.blocks_block(&x), None, "the first attempt is still running");
        l.attempts = 2;
        assert!(l.blocks_block(&x).is_some(), "still handshaking after more than one attempt");
        l.phase = Phase::Engaged;
        assert_eq!(l.blocks_block(&x), None);
        // Idle after a failed attempt with its hello seen = between soft fails.
        l.phase = Phase::Idle;
        l.failed_attempt = true;
        assert!(l.blocks_block(&x).is_some());
        // Without a hello it is the no_hello rule's business, not this one.
        assert_eq!(l.blocks_block(&BlockFacts { hello_stage: None, ..facts() }), None);
        // A load / rest / own-sync hold is not a failure.
        l.failed_attempt = false;
        assert_eq!(l.blocks_block(&x), None);
        // Held by the gate: never a blocker for the attempt rules.
        l.failed_attempt = true;
        l.held = true;
        assert_eq!(l.blocks_block(&x), None);
        l.phase = Phase::Handshake;
        l.attempts = 3;
        assert_eq!(l.blocks_block(&x), None);
    }

    #[test]
    fn stage_mismatch_missing_remote_and_hard_failures_block_even_when_held() {
        let mut l = link(1);
        l.held = true;
        assert!(l.blocks_block(&BlockFacts { has_remote: false, ..facts() }).is_some());
        assert!(l.blocks_block(&BlockFacts { hello_stage: Some(3), ..facts() }).is_some());
        // Stage 0 = "unavailable", reported by its own flag.
        assert_eq!(l.blocks_block(&BlockFacts { hello_stage: Some(0), ..facts() }), None);
        l.unavailable = true;
        assert!(l.blocks_block(&facts()).is_some());
        l.unavailable = false;
        l.failed = Some("x".into());
        assert!(l.blocks_block(&facts()).is_some());
        l.failed = None;
        l.fallback_world = true;
        assert!(l.blocks_block(&facts()).is_some());
    }

    #[test]
    fn no_hello_does_not_count_right_after_the_peers_loading_screen() {
        let mut l = link(1);
        l.no_hello = 1;
        assert!(l.blocks_block(&facts()).is_some());
        assert_eq!(l.blocks_block(&BlockFacts { loaded_recently: true, ..facts() }), None);
        // Engaged peers never block, whatever is left over.
        l.phase = Phase::Engaged;
        assert_eq!(l.blocks_block(&facts()), None);
    }

    #[test]
    fn blockers_count_after_the_grace_and_forget_when_gone() {
        let t0 = Instant::now();
        let mut since = BTreeMap::new();
        let raw = |slots: &[u8]| slots.iter().map(|&s| (s, "why".to_string())).collect::<BTreeMap<_, _>>();
        let g = BLOCKER_GRACE;
        assert!(mature_blockers(&mut since, raw(&[2]), t0, g).is_empty());
        assert!(mature_blockers(&mut since, raw(&[2]), t0 + g / 2, g).is_empty());
        assert_eq!(mature_blockers(&mut since, raw(&[2]), t0 + g, g).len(), 1);
        // Gone for one tick: the clock starts over.
        assert!(mature_blockers(&mut since, raw(&[]), t0 + g, g).is_empty());
        assert!(mature_blockers(&mut since, raw(&[2]), t0 + g + g / 2, g).is_empty());
        // A second blocker has its own clock.
        let m = mature_blockers(&mut since, raw(&[2, 3]), t0 + g * 3, g);
        assert_eq!(m.iter().map(|b| b.0).collect::<Vec<_>>(), vec![2]);
    }

    /// Runs two lanes against each other through a lossy, reordering "wire".
    fn shuttle(
        a: &mut lane::Lane,
        b: &mut lane::Lane,
        now: Instant,
        drop_every: usize,
        reverse: bool,
    ) -> (Vec<(u8, Vec<u8>)>, Vec<(u8, Vec<u8>)>) {
        let (mut got_a, mut got_b) = (Vec::new(), Vec::new());
        let mut n = 0;
        for (from_a, out) in [(true, a.poll(now)), (false, b.poll(now))] {
            let mut out = out;
            if reverse {
                out.reverse();
            }
            for (data, bytes) in out {
                n += 1;
                if drop_every > 0 && n % drop_every == 0 {
                    continue;
                }
                let (to, got) = if from_a { (&mut *b, &mut got_b) } else { (&mut *a, &mut got_a) };
                if data {
                    got.extend(to.on_data(&bytes, now));
                } else {
                    to.on_ack(&bytes);
                }
            }
        }
        (got_a, got_b)
    }

    #[test]
    fn lane_delivers_everything_once_in_order_per_key_despite_loss_and_reorder() {
        let mut a = lane::Lane::new(11);
        let mut b = lane::Lane::new(22);
        let big: Vec<u8> = (0..20_000u32).map(|i| i as u8).collect();
        a.queue(T_BEHAVIOR, 2, &big);
        for i in 0..20u8 {
            a.queue(T_OWNERS, 0, &[i; 9]);
            a.queue(T_ACTION, 1, &[i; 300]);
        }
        a.queue(0, 0, &[]);
        let mut now = Instant::now();
        let mut got = Vec::new();
        for round in 0..60 {
            let (_, g) = shuttle(&mut a, &mut b, now, if round < 30 { 3 } else { 0 }, round % 2 == 1);
            got.extend(g);
            now += lane::RTO;
        }
        assert_eq!(a.backlog(), 0, "everything acked");
        let of = |k: u8| got.iter().filter(|(key, _)| *key == k).map(|(_, m)| m.clone()).collect::<Vec<_>>();
        assert_eq!(of(T_BEHAVIOR), vec![big]);
        assert_eq!(of(T_OWNERS), (0..20u8).map(|i| vec![i; 9]).collect::<Vec<_>>());
        assert_eq!(of(T_ACTION), (0..20u8).map(|i| vec![i; 300]).collect::<Vec<_>>());
        assert_eq!(of(0), vec![Vec::<u8>::new()]);
        assert_eq!(b.stats.gaps_skipped, 0);
    }

    #[test]
    fn lane_lets_ownership_overtake_a_big_behavior_packet() {
        let mut a = lane::Lane::new(1);
        let mut b = lane::Lane::new(2);
        // More behavior than the whole window, queued first.
        for _ in 0..8 {
            a.queue(T_BEHAVIOR, 2, &vec![7u8; 20_000]);
        }
        a.queue(T_OWNERS, 0, &[1, 0, 0, 0, 5, 0, 0, 0]);
        let now = Instant::now();
        let (_, got) = shuttle(&mut a, &mut b, now, 0, false);
        assert!(got.iter().any(|(k, _)| *k == T_OWNERS), "23 must not wait behind 70");
    }

    #[test]
    fn lane_ignores_garbage_foreign_acks_and_resets_for_a_new_peer() {
        let mut a = lane::Lane::new(5);
        let mut b = lane::Lane::new(6);
        let now = Instant::now();
        assert!(b.on_data(&[1, 2, 3], now).is_empty());
        b.on_ack(&[9, 9]);
        a.queue(T_ACTION, 1, &[1; 10]);
        let out = a.poll(now);
        // An ack for another lane id frees nothing.
        let mut foreign = 77u32.to_le_bytes().to_vec();
        foreign.extend_from_slice(&1u16.to_le_bytes());
        foreign.extend_from_slice(&0u32.to_le_bytes());
        a.on_ack(&foreign);
        assert_eq!(a.backlog(), 1);
        let got: Vec<_> = out.iter().filter(|(d, _)| *d).flat_map(|(_, x)| b.on_data(x, now)).collect();
        assert_eq!(got, vec![(T_ACTION, vec![1; 10])]);
        // The same datagram again is a duplicate, not a second delivery.
        let again: Vec<_> = out.iter().filter(|(d, _)| *d).flat_map(|(_, x)| b.on_data(x, now)).collect();
        assert!(again.is_empty());
        // A restarted peer (new id) starts a fresh stream at kseq 0.
        let mut a2 = lane::Lane::new(55);
        a2.queue(T_ACTION, 1, &[2; 4]);
        let got: Vec<_> = a2.poll(now).iter().filter(|(d, _)| *d).flat_map(|(_, x)| b.on_data(x, now)).collect();
        assert_eq!(got, vec![(T_ACTION, vec![2; 4])]);
    }

    #[test]
    fn lane_skips_a_gap_that_never_fills() {
        let mut a = lane::Lane::new(3);
        let mut b = lane::Lane::new(4);
        let now = Instant::now();
        a.queue(T_ACTION, 1, &[1]);
        a.queue(T_ACTION, 1, &[2]);
        let out = a.poll(now);
        let data: Vec<_> = out.into_iter().filter(|(d, _)| *d).map(|(_, x)| x).collect();
        // The first one is lost for good: the second waits, then is handed out.
        assert!(b.on_data(&data[1], now).is_empty());
        let later = now + lane::GAP_SKIP;
        assert!(b.on_data(&data[1], later).is_empty(), "a duplicate does not release");
        // Any later new datagram of the same peer lets the aged gap go.
        a.queue(T_HP, 1, &[9]);
        let x: Vec<_> = a.poll(now).into_iter().filter(|(d, _)| *d).map(|(_, x)| x).collect();
        let got = b.on_data(x.last().unwrap(), later);
        assert!(got.contains(&(T_ACTION, vec![2])), "{got:?}");
    }

    #[test]
    fn whitelists_per_stage() {
        assert_eq!(whitelist(1), 0);
        let s2 = whitelist(2);
        for t in STAGE2_TYPES {
            assert!(s2 >> t & 1 == 1);
        }
        assert_eq!(s2 >> 26 & 1, 0, "flags (26) stay with world.rs");
        assert_eq!(s2 >> T_DAMAGE & 1, 1, "hits are part of the enemy stage");
        assert_eq!(s2 >> T_HIT2 & 1, 0);
        let s3 = whitelist(3);
        for t in STAGE2_TYPES.iter().chain(STAGE3_TYPES.iter()) {
            assert!(s3 >> t & 1 == 1);
        }
        assert_eq!(s3 >> T_OBJ_EVENT & 1, 0, "object events (45) are stage 4");
        assert_eq!(whitelist(4) >> T_OBJ_EVENT & 1, 1);
        assert!(!reliable(T_MOVE) && !reliable(T_HP) && reliable(T_BEHAVIOR) && reliable(T_OWNERS) && reliable(T_DAMAGE));
        assert_eq!((lane_class(T_OWNERS), lane_class(T_DAMAGE), lane_class(T_BEHAVIOR)), (0, 1, 2));
        assert_eq!(queue_cap(T_MOVE), 32);
        assert_eq!(queue_cap(T_CAND), 4);
    }

    #[test]
    fn bullets_stay_with_artfx() {
        assert!(!step_aside::NATIVE_BULLETS);
        for stage in 1..=MAX_STAGE {
            assert_eq!(whitelist(stage) >> 63 & 1, 0, "type 63 is artfx.rs's at stage {stage}");
        }
    }

    /// A machine's slot -> player netId map: `own` at `me`, the remotes at theirs.
    fn map_of(me: usize, own: u16, remotes: &[(usize, u16)]) -> PnetMap {
        let mut m = [NO_NET_ID; SLOTS];
        m[me] = own;
        for &(s, n) in remotes {
            m[s] = n;
        }
        m
    }

    #[test]
    fn player_net_ids_are_rewritten_both_ways() {
        // Two players. Host (slot 0): own 0x0801, remote of the client 0x0802. Client
        // (slot 1): own 0x1003, remote of the host 0x1001.
        let host = map_of(0, 0x0801, &[(1, 0x0802)]);
        let client = map_of(1, 0x1003, &[(0, 0x1001)]);
        // The host's enemy 0x1234 hits the client's remote on the host.
        let mut d = vec![0u8; 0xD0];
        put_u16(&mut d, 0x00, 0x0802);
        put_u16(&mut d, 0x0C, 0x1234);
        pnet_to_wire(&mut d, T_DAMAGE, &host);
        assert_eq!(get_u16(&d, 0), Some(WIRE_SLOT | 1));
        assert_eq!(get_u16(&d, 0x0C), Some(0x1234));
        pnet_from_wire(&mut d, T_DAMAGE, &client);
        assert_eq!(get_u16(&d, 0), Some(0x1003));
        assert_eq!(get_u16(&d, 0x0C), Some(0x1234));
        // The client player hits a host-owned enemy: the victim (an enemy) stays, the
        // attacker becomes the host's remote of the client.
        let mut d = vec![0u8; 0xD0];
        put_u16(&mut d, 0x00, 0x1234);
        put_u16(&mut d, 0x0C, 0x1003);
        pnet_to_wire(&mut d, T_DAMAGE, &client);
        pnet_from_wire(&mut d, T_DAMAGE, &host);
        assert_eq!((get_u16(&d, 0), get_u16(&d, 0x0C)), (Some(0x1234), Some(0x0802)));
        // ObjAct: the actor at +0.
        let mut d = vec![0u8; 6];
        put_u16(&mut d, 0, 0x0801);
        pnet_to_wire(&mut d, T_OBJ_EVENT, &host);
        pnet_from_wire(&mut d, T_OBJ_EVENT, &client);
        assert_eq!(get_u16(&d, 0), Some(0x1001));
        // Throw (live 7.1 evidence): the host's player (0xA000 on both machines)
        // deathblows enemy 0x0821. On the client it must be the host's remote, never
        // the client's own player (that pulled the client onto the host).
        let (h, c) = (map_of(0, 0xA000, &[(1, 0xA001)]), map_of(1, 0xA000, &[(0, 0xA001)]));
        let mut d = vec![0u8; 0x2C];
        put_u16(&mut d, 0x20, 0x0821);
        put_u16(&mut d, 0x22, 0xA000);
        pnet_to_wire(&mut d, T_THROW, &h);
        assert_eq!((get_u16(&d, 0x20), get_u16(&d, 0x22)), (Some(0x0821), Some(WIRE_SLOT)));
        pnet_from_wire(&mut d, T_THROW, &c);
        assert_eq!((get_u16(&d, 0x20), get_u16(&d, 0x22)), (Some(0x0821), Some(0xA001)));
        // An enemy grabs the sender's player: the defender at +0x20.
        let mut d = vec![0u8; 0x2C];
        put_u16(&mut d, 0x20, 0xA000);
        put_u16(&mut d, 0x22, 0x0879);
        pnet_to_wire(&mut d, T_THROW, &h);
        pnet_from_wire(&mut d, T_THROW, &c);
        assert_eq!((get_u16(&d, 0x20), get_u16(&d, 0x22)), (Some(0xA001), Some(0x0879)));
        // Second hit record: chrs at +4 / +8, the kind at +0 untouched.
        let mut d = vec![0u8; 0x80];
        put_u16(&mut d, 0x00, 4);
        put_u16(&mut d, 0x04, 0x0821);
        put_u16(&mut d, 0x08, 0xA000);
        pnet_to_wire(&mut d, T_HIT2, &h);
        pnet_from_wire(&mut d, T_HIT2, &c);
        assert_eq!((get_u16(&d, 0), get_u16(&d, 4), get_u16(&d, 8)), (Some(4), Some(0x0821), Some(0xA001)));
        // Unknown ids and other types are left alone.
        let mut d = vec![0xFF; 0x80];
        pnet_to_wire(&mut d, T_HIT2, &[NO_NET_ID; SLOTS]);
        assert_eq!(get_u16(&d, 0), Some(NO_NET_ID));
        pnet_from_wire(&mut d, T_HIT2, &h);
        assert_eq!(get_u16(&d, 4), Some(NO_NET_ID), "0xFFFF is not a slot");
        let mut d = vec![1u8; 8];
        pnet_to_wire(&mut d, 52, &map_of(0, 0x0101, &[(1, 0x0101)]));
        assert_eq!(d, vec![1u8; 8]);
        // Short buffers do not panic.
        pnet_from_wire(&mut [0u8; 3], T_DAMAGE, &h);
    }

    #[test]
    fn player_net_ids_name_slots_across_the_relay() {
        // Three players; playerNo order differs per machine. Host 0: own 0xA000, client 1
        // = 0xA001, client 2 = 0xA002. Client 1: own 0xA000, host = 0xA002, client 2 =
        // 0xA001. Client 2: own 0xA000, host = 0xA001, client 1 = 0xA003.
        let host = map_of(0, 0xA000, &[(1, 0xA001), (2, 0xA002)]);
        let c1 = map_of(1, 0xA000, &[(0, 0xA002), (2, 0xA001)]);
        let c2 = map_of(2, 0xA000, &[(0, 0xA001), (1, 0xA003)]);
        // Client 2's player hits enemy 0x0850 owned by client 1 (broadcast 21).
        let mut wire = vec![0u8; 0xD0];
        put_u16(&mut wire, 0x00, 0x0850);
        put_u16(&mut wire, 0x0C, 0xA000);
        pnet_to_wire(&mut wire, T_DAMAGE, &c2);
        assert_eq!(get_u16(&wire, 0x0C), Some(WIRE_SLOT | 2));
        // The host relays the wire bytes unchanged and applies its own copy: client 2's remote.
        let mut on_host = wire.clone();
        pnet_from_wire(&mut on_host, T_DAMAGE, &host);
        assert_eq!((get_u16(&on_host, 0), get_u16(&on_host, 0x0C)), (Some(0x0850), Some(0xA002)));
        // Client 1 gets the relayed copy: its remote of client 2.
        let mut on_c1 = wire.clone();
        pnet_from_wire(&mut on_c1, T_DAMAGE, &c1);
        assert_eq!(get_u16(&on_c1, 0x0C), Some(0xA001));
        // Client 1's own player as a victim (an enemy of the host hits it, seen on client 2's
        // machine and broadcast from there) arrives as client 1's own player.
        let mut wire = vec![0u8; 0xD0];
        put_u16(&mut wire, 0x00, 0xA003);
        pnet_to_wire(&mut wire, T_DAMAGE, &c2);
        assert_eq!(get_u16(&wire, 0), Some(WIRE_SLOT | 1));
        pnet_from_wire(&mut wire, T_DAMAGE, &c1);
        assert_eq!(get_u16(&wire, 0), Some(0xA000));
        // A slot we have no remote for: 0xFFFF (the game finds no chr), never a wrong player.
        let partial = map_of(1, 0xA000, &[(0, 0xA002)]);
        let mut d = vec![0u8; 0xD0];
        put_u16(&mut d, 0x0C, WIRE_SLOT | 2);
        pnet_from_wire(&mut d, T_DAMAGE, &partial);
        assert_eq!(get_u16(&d, 0x0C), Some(NO_NET_ID));
    }

    #[test]
    fn netid_check_compares_common_unique_entities() {
        let ours = [(10, 0x0801, 1), (11, 0x0802, 1), (12, 0x0803, 1), (13, 0x0804, 1), (13, 0x0805, 1)];
        let theirs = [(10, 0x0801, 1), (11, 0x0802, 1), (14, 0x0900, 1), (13, 0x0804, 1)];
        assert_eq!(netid_check(&ours, &theirs), (2, vec![]));
        let bad = [(10, 0x0801, 1), (11, 0x0809, 1)];
        assert_eq!(netid_check(&ours, &bad), (2, vec![(11, 0x0802, 0x0809)]));
        assert_eq!(netid_check(&[], &theirs), (0, vec![]));
    }

    #[test]
    fn host_death_reload_never_heals_or_kills() {
        // Host after its death reload (full HP) vs the client's damaged / dead enemies.
        let host = [(1, 0x801, 1000), (2, 0x802, 1000), (3, 0x803, 1000), (4, 0x804, 500), (5, 0x805, 800), (5, 0x806, 800)];
        let client = [(1, 0x801, 300), (2, 0x802, 0), (3, 0x803, 1000), (4, 0x804, 700), (5, 0x805, 100)];
        // Host takes the client's lower HP; the dead one is P3's (no loot twice), a
        // duplicate id is left out, nothing goes up.
        assert_eq!(hp_adoptions_raw(&host, &client), vec![(1, 300)]);
        // The client keeps its own lower values.
        assert_eq!(hp_adoptions_raw(&client, &host), vec![(4, 500)]);
    }

    #[test]
    fn engage_hp_check_skips_enemies_whose_max_differs() {
        // Live 2026-10-24: 1120353 at 558 HP on the host (scaled) vs max 349 on the friend
        // (scaling not applied yet after the rest). 1120354: same max, a real lower HP.
        let ours = [(1_120_353, 0x180b, 349), (1_120_354, 0x180c, 292), (1_120_355, 0x180d, 600), (1_120_356, 0x180e, 300)];
        let theirs = [(1_120_353, 0x180b, 300), (1_120_354, 0x180c, 100), (1_120_355, 0x180d, 500), (1_120_356, 0x180e, 200)];
        let our_max: HashMap<i32, i32> = [(1_120_353, 349), (1_120_354, 292), (1_120_355, 468), (1_120_356, 300)].into();
        let their_max: HashMap<i32, i32> = [(1_120_353, 558), (1_120_354, 292), (1_120_355, 468), (1_120_356, 301)].into();
        // 353: max differs; 355: our HP above our max (not settled); 356: 1 HP of max apart = same.
        assert_eq!(hp_adoptions(&ours, &our_max, &theirs, Some(&their_max)), vec![(1_120_354, 100), (1_120_356, 200)]);
        // A 7.6 friend (no MaxHp): only the "HP above our max" guard.
        assert_eq!(
            hp_adoptions(&ours, &our_max, &theirs, None),
            vec![(1_120_353, 300), (1_120_354, 100), (1_120_356, 200)]
        );
        // The friend's HP above our max (its scale is bigger): never taken.
        let big = [(1_120_354, 0x180c, 400)];
        assert_eq!(hp_adoptions(&[(1_120_354, 0x180c, 500)], &[(1_120_354, 292)].into(), &big, None), vec![]);
        // Unknown max here: skipped.
        assert_eq!(hp_adoptions(&ours, &HashMap::new(), &theirs, None), vec![]);
    }

    #[test]
    fn type_22_payload_shape() {
        let mut d = 2i32.to_le_bytes().to_vec();
        d.extend_from_slice(&[0; 8]);
        assert_eq!(parse_22(&d), Some(2));
        assert_eq!(parse_22(&0i32.to_le_bytes()), Some(0));
        assert_eq!(parse_22(&d[..11]), None);
        assert_eq!(parse_22(&(-1i32).to_le_bytes()), None);
        let mut big = 1024i32.to_le_bytes().to_vec();
        big.extend(std::iter::repeat_n(0u8, 4096));
        assert_eq!(parse_22(&big), None);
        assert_eq!(parse_22(&[1, 0]), None);
    }

    #[test]
    fn stealth_claims_raise_our_prio_and_strip_the_friends() {
        // {n; netId | prio << 16}: 0x850 prio 7, 0x851 prio 3.
        let d = encode_22(&[0x850 | 7 << 16, 0x851 | 3 << 16]);
        assert_eq!(list_22(&d), Some(vec![0x850 | 7 << 16, 0x851 | 3 << 16]));
        // Client: a listed claim gets 0xFFFF, a missing one is appended.
        let (c, n) = claim_22(&d, &[0x851, 0x900]).unwrap();
        assert_eq!(n, 2);
        assert_eq!(list_22(&c), Some(vec![0x850 | 7 << 16, 0x851 | CLAIM_PRIO << 16, 0x900 | CLAIM_PRIO << 16]));
        assert_eq!(parse_22(&c), Some(3));
        assert!(claim_22(&d[..7], &[0x850]).is_none());
        // Host: the friend's ordinary entry for our claim goes, its own claim stays.
        let (s, removed) = strip_22(&c, &[0x850, 0x851], &[]).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(list_22(&s), Some(vec![0x851 | CLAIM_PRIO << 16, 0x900 | CLAIM_PRIO << 16]));
        // ... unless the host won the contest for it (§34): then the friend's claim goes too.
        let (w, removed) = strip_22(&c, &[0x850, 0x851], &[0x851]).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(list_22(&w), Some(vec![0x900 | CLAIM_PRIO << 16]));
        assert_eq!(claimed_in_22(&c), vec![0x851, 0x900]);
        let (same, none) = strip_22(&d, &[], &[]).unwrap();
        assert_eq!((same, none), (d.clone(), 0));
        // A full list takes no new entry.
        let full = encode_22(&(0..1023u32).collect::<Vec<_>>());
        let (f, n) = claim_22(&full, &[0x7FFF]).unwrap();
        assert_eq!((parse_22(&f), n), (Some(1023), 0));
    }

    #[test]
    fn forced_boss_members_leave_the_friends_candidates() {
        // Blocked set = base plus the host's forced bosses, sorted, no duplicates.
        let b = blocked_with_bosses(vec![0x900, 0x850], &[0x852, 0x850]);
        assert_eq!(b, vec![0x850, 0x852, 0x900]);
        assert_eq!(blocked_with_bosses(Vec::new(), &[]), Vec::<u16>::new());
        // Even the friend's own prio-0xFFFF entry for a boss goes (blocked is also `won`).
        let d = encode_22(&[0x850 | 7 << 16, 0x852 | CLAIM_PRIO << 16, 0x700 | 2 << 16]);
        let (s, removed) = strip_22(&d, &b, &b).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(list_22(&s), Some(vec![0x700 | 2 << 16]));
    }

    #[test]
    fn two_deathblows_at_once_do_not_cancel_each_other() {
        // Live 2026-10-24 host 1792813226 (bytes from the log): the friend's throw 1200 on
        // 0x182e (attacker = the friend, our remote 0xa001 after the rewrite); both players
        // threw that enemy within a second, twice.
        let rx = |hex: &str| -> Vec<u8> { hex.split(' ').map(|b| u8::from_str_radix(b, 16).unwrap()).collect() };
        let friends = rx("d5 a7 98 c3 6b d7 56 c2 ff 6b 94 43 35 04 0b 40 88 5c 99 c3 de 75 56 c2 14 e7 94 43 b2 42 71 bf 2e 18 01 a0 b0 04 af 00 00 00 00 00");
        assert_eq!((get_u16(&friends, THROW_DEFENDER), get_u16(&friends, THROW_ATTACKER), get_u16(&friends, THROW_ID)), (Some(0x182e), Some(0xa001), Some(1200)));
        let ours = |at: u64| ThrowNote { defender: 0x182e, at, sent: true, by_player: true, slot: 0 };
        let mut ring = [NO_THROW; 16];
        // Nothing of ours: applied.
        assert_eq!(throw_collides(&ring, &friends, &[0xa001], 10_000), None);
        ring[0] = ours(9_200);
        assert_eq!(throw_collides(&ring, &friends, &[0xa001], 10_000), Some(800));
        // Too old, another enemy, an enemy's grab (attacker not the friend's player), or a
        // throw that was not our player's: applied.
        assert_eq!(throw_collides(&ring, &friends, &[0xa001], 9_200 + THROW_COLLIDE_MS + 1), None);
        assert_eq!(throw_collides(&ring, &friends, &[0xa002], 10_000), None);
        ring[0] = ThrowNote { by_player: false, ..ours(9_200) };
        assert_eq!(throw_collides(&ring, &friends, &[0xa001], 10_000), None);
        ring[0] = ThrowNote { defender: 0x182f, ..ours(9_200) };
        assert_eq!(throw_collides(&ring, &friends, &[0xa001], 10_000), None);
        // Our throw targets (claims) and ages.
        ring[0] = ours(9_000);
        ring[1] = ThrowNote { defender: 0x0900, at: 9_500, sent: true, by_player: true, slot: 0 };
        ring[2] = ThrowNote { defender: 0x0901, at: 9_500, sent: false, by_player: true, slot: 1 };
        ring[3] = ThrowNote { defender: 0x0902, at: 1_000, sent: true, by_player: true, slot: 0 };
        assert_eq!(our_throw_targets(&ring, 10_000, THROW_CLAIM_MS), vec![0x0900, 0x182e]);
        assert_eq!(player_throw_age(&ring, 0x0901, false, 10_000, THROW_CLAIM_MS), Some(500));
        assert_eq!(player_throw_age(&ring, 0x0901, true, 10_000, THROW_CLAIM_MS), None);
        // A note "in the future" (clock skew between threads) is ignored, never underflows.
        ring[4] = ThrowNote { defender: 0x0903, at: 20_000, sent: true, by_player: true, slot: 0 };
        assert_eq!(player_throw_age(&ring, 0x0903, true, 10_000, THROW_CLAIM_MS), None);
    }

    #[test]
    fn contested_claims_go_to_the_deathblow_then_the_nearer_player() {
        // Live 2026-10-24: host 13 m / friend 8 m from 1120300, both "behind": the friend.
        assert!(!host_wins_contest(None, None, Some(13.0), Some(8.0), false));
        // The host nearer by more than the margin takes it, and keeps it within the margin.
        assert!(host_wins_contest(None, None, Some(3.0), Some(8.0), false));
        assert!(!host_wins_contest(None, None, Some(7.0), Some(8.0), false));
        assert!(host_wins_contest(None, None, Some(9.0), Some(8.0), true));
        assert!(!host_wins_contest(None, None, Some(10.0), Some(8.0), true));
        // A deathblow beats distance; two of them: the earlier one (the host on a tie).
        assert!(host_wins_contest(Some(200), None, Some(13.0), Some(1.0), false));
        assert!(!host_wins_contest(None, Some(200), Some(1.0), Some(13.0), true));
        assert!(host_wins_contest(Some(900), Some(300), None, None, false));
        assert!(!host_wins_contest(Some(300), Some(900), None, None, true));
        assert!(host_wins_contest(Some(500), Some(500), None, None, false));
        // No friend position: the host; no position of ours: whoever has it now.
        assert!(host_wins_contest(None, None, Some(5.0), None, false));
        assert!(host_wins_contest(None, None, None, Some(5.0), true) && !host_wins_contest(None, None, None, Some(5.0), false));
    }

    #[test]
    fn contests_between_any_machines_are_settled_by_the_host() {
        let c = |slot: u8, throw_age: Option<u64>, dist: Option<f32>| Contender { slot, throw_age, dist };
        // Two players: exactly host_wins_contest for the host (held = the host won last time).
        for (h, f, held) in [(13.0, 8.0, false), (3.0, 8.0, false), (9.0, 8.0, true), (10.0, 8.0, true)] {
            let w = contest_winner(&[c(0, None, Some(h)), c(1, None, Some(f))], held.then_some(0));
            assert_eq!(w == 0, host_wins_contest(None, None, Some(h), Some(f), held), "{h} {f} {held}");
        }
        // Two clients (the host does not claim it): the nearer one, the holder keeps within 1.5 m.
        assert_eq!(contest_winner(&[c(1, None, Some(9.0)), c(2, None, Some(4.0))], None), 2);
        assert_eq!(contest_winner(&[c(1, None, Some(5.0)), c(2, None, Some(4.0))], Some(1)), 1);
        // Three machines: a deathblow beats distance; two of them: the earlier one.
        assert_eq!(contest_winner(&[c(0, None, Some(1.0)), c(1, Some(300), Some(9.0)), c(2, None, Some(2.0))], None), 1);
        assert_eq!(contest_winner(&[c(0, Some(200), None), c(1, Some(300), None), c(2, Some(900), None)], Some(0)), 2);
        // The throw ring per machine.
        let mut ring = [NO_THROW; 16];
        ring[0] = ThrowNote { defender: 0x0900, at: 9_000, sent: false, by_player: true, slot: 2 };
        ring[1] = ThrowNote { defender: 0x0900, at: 9_800, sent: true, by_player: true, slot: 0 };
        assert_eq!(slot_throw_age(&ring, 0x0900, 2, 10_000, THROW_CLAIM_MS), Some(1_000));
        assert_eq!(slot_throw_age(&ring, 0x0900, 0, 10_000, THROW_CLAIM_MS), Some(200));
        assert_eq!(slot_throw_age(&ring, 0x0900, 1, 10_000, THROW_CLAIM_MS), None);
    }

    #[test]
    fn ownerless_enemies_go_to_exactly_one_machine() {
        use super::ownfill::{NEAR_M, keep, wanted};
        // Two players (host 0, client 1), the 7.4 rule. The host / the client asks:
        let host = |dh: f32, dc: Option<f32>| wanted(0, dh, &[(1, dc)]);
        let client = |dc: f32, dh: Option<f32>| wanted(1, dc, &[(0, dh)]);
        // Live 7.4 friend line: 1100237 NetAI 7 m from the friend (client), 394 m from the host.
        assert!(client(7.0, Some(394.0)));
        assert!(!host(394.0, Some(7.0)));
        // Both near: the nearer one; within 5 m of each other the host.
        for (dh, dc) in [(10.0, 30.0), (30.0, 10.0), (20.0, 22.0), (22.0, 20.0), (20.0, 25.5), (35.0, 35.0), (25.0, 20.0)] {
            let (h, c) = (host(dh, Some(dc)), client(dc, Some(dh)));
            assert!(!(h && c), "both take it at host {dh} m / client {dc} m");
            assert!(h || c || dh.min(dc) > NEAR_M, "nobody takes it at host {dh} m / client {dc} m");
            assert_eq!(h, dh <= NEAR_M && dh <= dc + 5.0, "host at {dh} / {dc}");
            assert_eq!(c, dc <= NEAR_M && dc + 5.0 < dh, "client at {dc} / {dh}");
        }
        // Too far from both: nobody (the game's own LOD).
        assert!(!host(60.0, Some(80.0)) && !client(80.0, Some(60.0)));
        // No position of the other: only the host.
        assert!(host(10.0, None) && !client(10.0, None));
        // Hysteresis: a taken enemy stays a little past the rule (host up to 10 m farther,
        // the client while nearer).
        assert!(keep(0, 28.0, &[(1, Some(20.0))]) && !host(28.0, Some(20.0)));
        assert!(!keep(0, 31.0, &[(1, Some(20.0))]));
        assert!(keep(1, 19.0, &[(0, Some(20.0))]) && !keep(1, 20.0, &[(0, Some(20.0))]));
        assert!(!keep(0, 55.0, &[(1, Some(70.0))]));
    }

    #[test]
    fn ownfill_nearest_player_takes_it_tie_lowest_slot() {
        use super::ownfill::{conflict_drop, wanted};
        // Every slot asks with the same distances: exactly one may take it (within 40 m).
        let takers = |d: &[(u8, f32)]| -> Vec<u8> {
            d.iter()
                .filter(|&&(me, dm)| {
                    let others: Vec<(u8, Option<f32>)> = d.iter().filter(|(s, _)| *s != me).map(|&(s, x)| (s, Some(x))).collect();
                    wanted(me, dm, &others)
                })
                .map(|&(s, _)| s)
                .collect()
        };
        // Clients only: the nearest.
        assert_eq!(takers(&[(0, 90.0), (1, 12.0), (2, 8.0), (3, 30.0)]), vec![2]);
        // A tie between clients: the lowest slot.
        assert_eq!(takers(&[(0, 90.0), (2, 10.0), (3, 10.0), (5, 10.0)]), vec![2]);
        // The host's 5 m bias: it wins a tie band against every client.
        assert_eq!(takers(&[(0, 14.0), (1, 10.0), (2, 9.5)]), vec![0]);
        assert_eq!(takers(&[(0, 15.0), (1, 10.0), (2, 9.9)]), vec![2]);
        // The 3-player cycle a pairwise rule would have (nobody takes it): one taker here.
        assert_eq!(takers(&[(0, 10.0), (1, 6.0), (2, 4.0)]).len(), 1);
        // A grid of distances: never two, never none within range.
        let ds = [3.0f32, 5.0, 8.0, 10.0, 13.0, 15.0, 20.0, 35.0, 45.0];
        for &a in &ds {
            for &b in &ds {
                for &c in &ds {
                    let t = takers(&[(0, a), (1, b), (4, c)]);
                    assert!(t.len() <= 1, "{a} {b} {c}: {t:?}");
                    assert!(!t.is_empty() || a.min(b).min(c) > 40.0, "{a} {b} {c}: nobody");
                }
            }
        }
        // A client without the host's position never takes one; an unknown client does not compete.
        assert!(!wanted(2, 5.0, &[(0, None), (1, Some(30.0))]));
        assert!(wanted(0, 5.0, &[(1, None), (2, Some(30.0))]));
        assert!(wanted(1, 5.0, &[(0, Some(30.0)), (2, None)]));
        // Conflicts: a lower slot also owns it = we drop at once; we are the lowest = after 3 s.
        assert!(conflict_drop(2, &[1], Duration::ZERO, false));
        // v8.30b: the host yields a non-boss at once; a boss keeps the old rule.
        assert!(conflict_drop(0, &[1], Duration::ZERO, false));
        assert!(!conflict_drop(0, &[1], Duration::ZERO, true));
        assert!(!conflict_drop(0, &[], Duration::ZERO, false));
        assert!(!conflict_drop(1, &[2, 3], Duration::from_secs(1), false));
        assert!(conflict_drop(1, &[2, 3], Duration::from_secs(3), false));
        assert!(!conflict_drop(0, &[1], Duration::from_secs(2), true) && conflict_drop(1, &[0], Duration::ZERO, false));
    }

    #[test]
    fn a_dead_spectating_host_leaves_the_boss_to_the_living_friend() {
        use super::ownfill::{boss_rivals, keep_boss, wanted_boss};
        // Live 2026-10-27 Divine Dragon: host dead 50 m from the dragon, the friend 40 m.
        let host_near = [(0u8, Some(50.0f32))];
        assert!(!wanted_boss(1, 40.0, &host_near));
        // Host down (EV_DOWN, bit 0): it no longer counts, the friend takes and keeps the boss.
        let rivals = boss_rivals(&host_near, 1);
        assert!(rivals.is_empty());
        assert!(wanted_boss(1, 40.0, &rivals) && keep_boss(1, 40.0, &rivals));
        // v8.57: at the dragon's fighting range (the friend was 84 m away) too, not beyond 150 m.
        assert!(wanted_boss(1, 84.0, &rivals) && keep_boss(1, 160.0, &rivals));
        assert!(!wanted_boss(1, 151.0, &rivals) && !keep_boss(1, 171.0, &rivals));
        // The host still listed (alive, 50 m): the client keeps the old 40 m rule.
        assert!(!wanted_boss(1, 84.0, &[(0, Some(400.0))]));
        // Back on its feet (bit cleared): the host's rule again, the friend lets it go.
        assert_eq!(boss_rivals(&host_near, 0), host_near.to_vec());
        assert!(!keep_boss(1, 40.0, &boss_rivals(&host_near, 0)));
        // 3 players: only the down one drops out.
        let three = [(0u8, Some(50.0f32)), (2, Some(10.0))];
        assert_eq!(boss_rivals(&three, 0b100), vec![(0, Some(50.0))]);
        assert_eq!(boss_rivals(&three, 0b001), vec![(2, Some(10.0))]);
    }

    #[test]
    fn a_boss_is_the_hosts_not_the_nearest_players() {
        use super::ownfill::{keep_boss, wanted_boss};
        // The client stands 2 m from the boss and the host 16 m: the nearest-player rule would
        // give it to the client; a boss stays with the host.
        assert!(wanted_boss(0, 16.0, &[(1, Some(2.0))]));
        assert!(!wanted_boss(1, 2.0, &[(0, Some(16.0))]));
        assert!(keep_boss(0, 100.0, &[(1, Some(2.0))]));
        assert!(!keep_boss(1, 2.0, &[(0, Some(16.0))]));
        // The host far away (another area) or unknown: the client only with a known far host.
        assert!(wanted_boss(1, 20.0, &[(0, Some(400.0))]));
        assert!(!wanted_boss(1, 20.0, &[(0, None)]));
        assert!(keep_boss(1, 30.0, &[(0, Some(400.0))]));
        assert!(!keep_boss(1, 30.0, &[(0, Some(125.0))]));
        // Too far from the boss for anybody to take it.
        assert!(!wanted_boss(0, 200.0, &[]));
    }

    #[test]
    fn a_host_boss_is_not_dropped_on_a_native_owner_answer() {
        use super::ownfill::drop_on_native;
        assert!(!drop_on_native(0, true));
        assert!(drop_on_native(0, false));
        assert!(drop_on_native(1, true));
        assert!(drop_on_native(1, false));
    }

    #[test]
    fn ownfill_hysteresis_no_flapping_at_the_threshold() {
        use super::ownfill::{GIVE_BACK_AFTER, give_back_due, keep, wanted};
        // Alternating 39 / 41 m (250 ms steps, 10 s): taken once, never given back.
        let (mut held, mut beyond, mut transfers) = (false, None::<Duration>, 0u32);
        for i in 0..40u32 {
            let d = if i % 2 == 0 { 39.0 } else { 41.0 };
            let others = [(1u8, Some(100.0f32))];
            if held {
                if keep(0, d, &others) {
                    beyond = None;
                } else {
                    let b = beyond.unwrap_or(Duration::ZERO) + Duration::from_millis(250);
                    beyond = Some(b);
                    if give_back_due(beyond) {
                        held = false;
                        transfers += 1;
                    }
                }
            } else if wanted(0, d, &others) {
                held = true;
                transfers += 1;
            }
        }
        assert!(transfers <= 1, "{transfers} transfers");
        // Beyond 48 m only counts after 5 s without a break.
        assert!(!keep(0, 49.0, &[(1, Some(100.0))]) && keep(0, 47.0, &[(1, Some(100.0))]));
        assert!(!give_back_due(None) && !give_back_due(Some(Duration::from_millis(4750))));
        assert!(give_back_due(Some(GIVE_BACK_AFTER)));
    }

    #[test]
    fn double_claim_host_wins() {
        let s = |m| Duration::from_millis(m);
        // Client claims, the host owns too for 2.5 s: the client drops.
        assert!(double_claim_drop(1, true, true, &[0], s(2500)));
        assert!(!double_claim_drop(1, true, true, &[0], s(1000)));
        // Not owned by the host / not ours / no claim: nothing to drop.
        assert!(!double_claim_drop(1, true, true, &[], s(9000)));
        assert!(!double_claim_drop(1, true, false, &[0], s(9000)));
        assert!(!double_claim_drop(1, false, true, &[0], s(9000)));
        // The host keeps, even when a client owns it too.
        assert!(!double_claim_drop(0, true, true, &[1, 2], s(9000)));
        // Between clients the lower slot wins.
        assert!(double_claim_drop(2, true, true, &[1], s(2500)) && !double_claim_drop(1, true, true, &[2], s(9000)));
    }

    #[test]
    fn ownerless_count_is_per_net_id() {
        let mut t = OwnerTally::default();
        // 232 enemies, each listed twice (two chr sets): 232 counted, not 464.
        for rep in 0..2 {
            for n in 0..232u16 {
                let counted = t.add(n, n % 7 == 0, n % 11 == 0);
                assert_eq!(counted, rep == 0 && n % 7 != 0 && n % 11 != 0);
            }
        }
        let distinct = (0..232u16).filter(|n| n % 7 != 0 && n % 11 != 0).count() as u32;
        assert_eq!(t.nobody, distinct);
        assert!(t.nobody <= 232);
        assert_eq!(t.both, (0..232u16).filter(|n| n % 7 == 0 && n % 11 == 0).count() as u32);
    }

    #[test]
    fn world_pause_hands_enemies_on_and_back() {
        // Stalled time grows 100 ms per tick; a pause begins at 1.5 s, ends when it runs.
        let mut p = PauseTracker::default();
        let mut began = None;
        for ms in (0..3000u64).step_by(100) {
            if p.step(Duration::from_millis(ms)) == PauseStep::Begin {
                began = Some(ms);
            }
        }
        assert_eq!(began, Some(1500));
        assert!(p.active);
        // Our enemies near the friend (client slot 1, 20 m) go to it; the far one stays; the
        // paused marker sorts last and is recognised.
        let owned = [(0x10u16, [0.0f32, 0.0, 0.0]), (0x20, [30.0, 0.0, 0.0]), (0x30, [300.0, 0.0, 0.0])];
        let peers = [(1u8, Some([20.0f32, 0.0, 0.0]))];
        assert_eq!(handover_targets(&owned, &peers), vec![(0x10, 1), (0x20, 1)]);
        // With two peers the nearer one takes it.
        let peers2 = [(1u8, Some([20.0f32, 0.0, 0.0])), (2, Some([-3.0f32, 0.0, 0.0]))];
        assert_eq!(handover_targets(&owned[..1], &peers2), vec![(0x10, 2)]);
        let mut ids = vec![0x10u16, 0x20, OWNED_PAUSED_MARK];
        ids.sort_unstable();
        assert!(owned_is_paused(&ids) && !owned_is_paused(&[0x10, 0x20]));
        // Resumed: ends at once; a short stall (menu flicker < 1.5 s) never begins one.
        assert_eq!(p.step(Duration::from_millis(100)), PauseStep::End);
        assert!(!p.active);
        let mut q = PauseTracker::default();
        assert!((0..1500u64).step_by(100).all(|ms| q.step(Duration::from_millis(ms)) == PauseStep::Idle));
    }

    #[test]
    fn ownfill_bits_set_and_clear() {
        use super::ownfill::{clear_all, forced, set};
        clear_all();
        assert!(!forced(0x87b));
        set(0x87b, true);
        set(0x1036, true);
        assert!(forced(0x87b) && forced(0x1036) && !forced(0x87c));
        set(0x87b, false);
        assert!(!forced(0x87b) && forced(0x1036));
        clear_all();
        assert!(!forced(0x1036));
    }

    #[test]
    fn rest_reason_round_trips() {
        assert_eq!(Reason::from_u8(Reason::Rest as u8), Reason::Rest);
        assert_eq!(Reason::from_u8(200), Reason::Other);
    }

    #[test]
    fn patch_tables_are_consistent() {
        for s in SITES {
            for new in [s.host, s.client].into_iter().flatten() {
                assert_eq!(new.len(), s.orig.len(), "{}", s.name);
            }
        }
        // The fake vector holds exactly one member (N - 1 = 1) and the count patch says 2.
        assert_eq!(ECX_2M[1] as usize, 2 * SET_ENTRY);
        assert!(FAKE_MEMBERS_OFF >= FAKE_S_OFF + S_MEMBERS_END + 8);
        assert!(FAKE_MEMBERS_OFF + FAKE_MEMBERS_N * SET_ENTRY <= STUB_OFF);
        let mut rvas: Vec<usize> = SITES.iter().map(|s| s.rva).chain(LEA_SITES.iter().map(|l| l.rva)).collect();
        rvas.push(VEC_CALL_SITE);
        rvas.push(HP_OFFLINE_SITE);
        rvas.push(OBJ_EVENT_SITE);
        rvas.extend(WRAPS_23.iter().map(|w| w.rva));
        let n = rvas.len();
        rvas.sort_unstable();
        rvas.dedup();
        assert_eq!(rvas.len(), n, "a site is listed twice");
        assert_eq!(ANY_TYPES.len(), HOOKS - H_ANY);
        // The wraps' original calls go where IDA says (site + 5 + rel32), and the stubs and
        // their u64 slots sit inside the fake page without overlapping the member stub.
        for w in &WRAPS_23 {
            assert_eq!(w.orig[0], 0xE8, "{}", w.name);
            let rel = i32::from_le_bytes([w.orig[1], w.orig[2], w.orig[3], w.orig[4]]);
            assert_eq!((w.rva as i64 + 5 + rel as i64) as usize, w.target, "{}", w.name);
            assert!(w.stub_off >= STUB_OFF + 8 && w.stub_off + 12 <= LAST23_CHR_OFF);
            assert!(w.var_off % 8 == 0 && w.var_off + 8 <= 0x1200);
        }
        assert!(STUB23_CHR_OFF + 12 <= STUB23_REC_OFF);
        // The stub: mov [rip+var], rcx; jmp target.
        let s = wrap_stub(0x1_4000_0000, 0x1_4000_0100, 0x1_4000_1000).unwrap();
        assert_eq!(&s[..3], &[0x48, 0x89, 0x0D]);
        assert_eq!(i32::from_le_bytes([s[3], s[4], s[5], s[6]]), 0x100 - 7);
        assert_eq!(s[7], 0xE9);
        assert_eq!(i32::from_le_bytes([s[8], s[9], s[10], s[11]]), 0x1000 - 12);
        assert_eq!(s.len(), 12);
    }
}

#[cfg(test)]
mod dblock_tests {
    use super::dblock::*;
    use super::{NO_NET_ID, THROW_ATTACKER, THROW_DEFENDER, THROW_ID, WIRE_SLOT, put_u16};

    #[test]
    fn first_come_and_the_host_tie_break() {
        // Ours 300 ms earlier / later; within TIE_MS the host's on both machines.
        assert_eq!(first_come(1000, 1300, false, TIE_MS), (true, 300, false));
        assert_eq!(first_come(1300, 1000, true, TIE_MS), (false, -300, false));
        assert_eq!(first_come(1000, 1050, true, TIE_MS), (true, 50, true));
        assert_eq!(first_come(1000, 1050, false, TIE_MS), (false, 50, true));
        assert!(!first_come(1000, 1000 + TIE_MS, false, TIE_MS).0);
        assert!(first_come(1000, 1001 + TIE_MS, false, TIE_MS).0);
        // The peer's start = arrival - RTT / 2 (100 ms assumed until measured).
        assert_eq!(their_start(5000, Some(80)), 4960);
        assert_eq!(their_start(5000, None), 5000 - DEFAULT_RTT_MS as u64 / 2);
        assert_eq!(their_start(10, Some(400)), 0);
    }

    #[test]
    fn a_peers_lock_blocks_ours_until_it_expires() {
        let mut t = Table::new();
        assert_eq!(t.on_theirs(0x182e, 0, 1000, 950, false), Theirs::New);
        let l = t.active(0x182e, 1000).unwrap();
        assert_eq!((l.holder, l.start, l.until), (Holder::Peer(0), 950, 1000 + LOCK_MS));
        // The second packet of the same deathblow (1205) extends it.
        assert_eq!(t.on_theirs(0x182e, 0, 1800, 1750, false), Theirs::Held);
        assert_eq!(t.active(0x182e, 1800).unwrap().until, 1800 + LOCK_MS);
        // Our throw during it (the gate missed): the peer keeps the lock.
        assert!(matches!(t.on_ours(0x182e, 2000), Ours::PeerHolds(_)));
        assert_eq!(t.active(0x182e, 2000).unwrap().holder, Holder::Peer(0));
        // Another enemy is free.
        assert_eq!(t.on_ours(0x0900, 2000), Ours::New);
        // Expiry: gone at `until`, then ours is new.
        assert!(t.active(0x182e, 1800 + LOCK_MS).is_none());
        let gone = t.expire(1800 + LOCK_MS);
        assert_eq!(gone.iter().map(|l| l.net).collect::<Vec<_>>(), vec![0x182e]);
        assert_eq!(t.locks.len(), 1);
        assert_eq!(t.on_ours(0x182e, 1800 + LOCK_MS), Ours::New);
    }

    #[test]
    fn our_lock_drops_a_later_throw_and_extends() {
        let mut t = Table::new();
        assert_eq!(t.on_ours(0x182e, 1000), Ours::New);
        assert_eq!(t.on_ours(0x182e, 1900), Ours::Held);
        assert_eq!(t.active(0x182e, 1900).unwrap().until, 1900 + LOCK_MS);
        assert_eq!(t.active(0x182e, 1900).unwrap().start, 1000);
        // The host pressed 400 ms after us (its packet here 450 ms later): dropped, ours stays.
        assert_eq!(t.on_theirs(0x182e, 0, 1450, 1400, false), Theirs::WeKeep { lead: 400, tie: false });
        assert_eq!(t.active(0x182e, 1450).unwrap().holder, Holder::Us);
        // Its next packet too.
        assert!(matches!(t.on_theirs(0x182e, 0, 2400, 2350, false), Theirs::WeKeep { .. }));
    }

    #[test]
    fn a_collision_gives_the_lock_to_the_earlier_start() {
        let mut t = Table::new();
        assert_eq!(t.on_ours(0x182e, 1000), Ours::New);
        // Client 3 started 200 ms before the host (its packet was still on the way).
        assert_eq!(t.on_theirs(0x182e, 3, 1050, 800, true), Theirs::PeerFirst { lead: -200, tie: false });
        let l = t.active(0x182e, 1050).unwrap();
        assert_eq!((l.holder, l.start), (Holder::Peer(3), 800));
        // Our deathblow's next packet does not take it back.
        assert!(matches!(t.on_ours(0x182e, 1500), Ours::PeerHolds(_)));
    }

    #[test]
    fn the_host_keeps_the_first_client_and_clients_follow_its_verdict() {
        // Host: client 1 reached it first; client 2's is dropped (not relayed) whatever its start.
        let mut h = Table::new();
        assert_eq!(h.on_theirs(7, 1, 1000, 900, true), Theirs::New);
        assert_eq!(h.on_theirs(7, 2, 1010, 500, true), Theirs::OtherKeeps(1));
        assert_eq!(h.active(7, 1010).unwrap().holder, Holder::Peer(1));
        // Client 2 had started its own: client 1's relayed 18 is the verdict, ours gives way.
        let mut c = Table::new();
        assert_eq!(c.on_ours(7, 1000), Ours::New);
        assert_eq!(c.on_theirs(7, 1, 1100, 1000, false), Theirs::Verdict(Holder::Us));
        assert_eq!(c.active(7, 1100).unwrap().holder, Holder::Peer(1));
        // A third client: the host's 18, then client 1's relayed one (the host gave way).
        let mut o = Table::new();
        assert_eq!(o.on_theirs(7, 0, 1000, 950, false), Theirs::New);
        assert_eq!(o.on_theirs(7, 1, 1100, 1050, false), Theirs::Verdict(Holder::Peer(0)));
        // The host's leftover packet after giving way is not applied ...
        assert_eq!(o.on_theirs(7, 0, 1200, 1150, false), Theirs::OtherKeeps(1));
        assert_eq!(o.active(7, 1200).unwrap().holder, Holder::Peer(1));
        // ... a new deathblow of the host's once client 1 went quiet is.
        assert_eq!(o.on_theirs(7, 0, 1100 + LEFTOVER_MS, 1100 + LEFTOVER_MS, false), Theirs::New);
        assert!(!Theirs::OtherKeeps(1).applies() && !Theirs::WeKeep { lead: 0, tie: true }.applies());
        assert!(Theirs::Verdict(Holder::Us).applies() && Theirs::PeerFirst { lead: 0, tie: true }.applies());
    }

    /// Host A and client B (own clocks), true one-way latency `owl`, both machines' RTT
    /// estimate off by `err`. Returns (A's deathblow survives, B's survives, the loser
    /// shows the winner's).
    fn duel(gap: i64, owl: u64, err: i64) -> (bool, bool, bool) {
        let a0 = 10_000u64;
        let b0 = (a0 as i64 + gap) as u64;
        let skew = 7777u64; // B's clock = A's + skew
        let rtt_est = Some((2 * owl as i64 + err).max(1) as u32);
        let (mut a, mut b) = (Table::new(), Table::new());
        // A throw starts only if the other's packet has not arrived yet (else the gate vetoes it).
        let a_starts = a0 < b0 + owl;
        let b_starts = b0 < a0 + owl;
        if a_starts {
            assert_eq!(a.on_ours(1, a0), Ours::New);
        }
        if b_starts {
            assert_eq!(b.on_ours(1, b0 + skew), Ours::New);
        }
        let a_applies_b = b_starts && {
            let now = b0 + owl;
            a.on_theirs(1, 1, now, their_start(now, rtt_est), true).applies()
        };
        let b_applies_a = a_starts && {
            let now = a0 + owl + skew;
            b.on_theirs(1, 0, now, their_start(now, rtt_est), false).applies()
        };
        // Applying the peer's throw cuts our own running one off (§34).
        let a_survives = a_starts && !a_applies_b;
        let b_survives = b_starts && !b_applies_a;
        let shown = (a_survives && (b_applies_a || !b_starts)) || (b_survives && (a_applies_b || !a_starts));
        (a_survives, b_survives, shown)
    }

    /// Two players (host + one client): exactly one deathblow survives and both screens show
    /// it: always with an exact RTT, and for any estimate error while the latency stays
    /// inside the tie window (every possible collision is then a tie, which the host takes).
    #[test]
    fn both_machines_agree_on_one_winner() {
        let gaps = [-1000i64, -300, -150, -121, -100, -60, -30, -10, 0, 10, 30, 60, 100, 121, 150, 300, 1000];
        for owl in [5u64, 20, 40, 55, 100, 200, 300] {
            let errs: &[i64] = if owl <= 55 { &[-60, -20, 0, 20, 60] } else { &[0] };
            for &err in errs {
                for gap in gaps {
                    let (a, b, shown) = duel(gap, owl, err);
                    assert!(a ^ b, "owl {owl} err {err} gap {gap}: a {a} b {b}");
                    assert!(shown, "owl {owl} err {err} gap {gap}");
                    if gap.unsigned_abs() > TIE_MS {
                        // Clearly apart: the earlier start wins.
                        assert_eq!(a, gap > 0, "owl {owl} err {err} gap {gap}");
                    } else if gap.unsigned_abs() < owl {
                        // A real collision inside the tie window: the host's.
                        assert!(a, "owl {owl} err {err} gap {gap}");
                    }
                }
            }
        }
        // Long latency with an RTT estimated too short: never both cancelled (at worst
        // both run, livesync still takes one life).
        for gap in gaps {
            let (a, b, _) = duel(gap, 300, -100);
            assert!(a || b, "gap {gap}");
        }
    }

    /// Star of a host (0) and clients 1..: each machine its own table, exact RTTs, every
    /// client's 18 through the host (which relays only what it applied). `starts[m]` =
    /// when machine m presses (None = never), `owl[m]` = one way host <-> m. Returns per
    /// machine: (its own deathblow survived, whose deathblow its screen shows last).
    fn star(starts: &[Option<u64>], owl: &[u64]) -> Vec<(bool, Option<u8>)> {
        let n = starts.len();
        let mut tables: Vec<Table> = (0..n).map(|_| Table::new()).collect();
        let mut started = vec![false; n];
        let mut cut = vec![false; n];
        let mut shown: Vec<Option<u8>> = vec![None; n];
        // (time, kind 0 = press / 1 = delivery, machine, origin)
        let mut ev: Vec<(u64, u8, usize, u8)> = starts.iter().enumerate().filter_map(|(m, s)| s.map(|t| (t, 0, m, m as u8))).collect();
        while !ev.is_empty() {
            ev.sort();
            let (t, kind, m, origin) = ev.remove(0);
            if kind == 0 {
                if tables[m].active(1, t).is_some_and(|l| l.holder != Holder::Us) {
                    continue; // the gate vetoes it
                }
                assert_eq!(tables[m].on_ours(1, t), Ours::New);
                started[m] = true;
                shown[m] = Some(m as u8);
                if m == 0 {
                    ev.extend((1..n).map(|k| (t + owl[k], 1, k, 0)));
                } else {
                    ev.push((t + owl[m], 1, 0, m as u8));
                }
                continue;
            }
            let host = m == 0;
            let link = if host { origin as usize } else { 0 };
            let rtt = Some(2 * owl[link.max(m)] as u32);
            if !tables[m].on_theirs(1, origin, t, their_start(t, rtt), host).applies() {
                continue;
            }
            if started[m] && shown[m] == Some(m as u8) {
                cut[m] = true;
            }
            shown[m] = Some(origin);
            if host {
                ev.extend((1..n).filter(|&k| k != origin as usize).map(|k| (t + owl[k], 1, k, origin)));
            }
        }
        (0..n).map(|m| (started[m] && !cut[m], shown[m])).collect()
    }

    /// 3-4 players: whoever collides with whom, exactly one deathblow survives and every
    /// screen shows it last; a host vs client collision clearly apart goes to the earlier start.
    #[test]
    fn every_machine_agrees_on_one_winner_through_the_host() {
        let offsets = [None, Some(0u64), Some(30), Some(90), Some(150), Some(400)];
        for owl in [[0u64, 20, 60, 35], [0, 100, 100, 100], [0, 5, 200, 50]] {
            for &h in &offsets {
                for &c1 in &offsets {
                    for &c2 in &offsets {
                        for &c3 in &[None, Some(60u64)] {
                            let starts: Vec<Option<u64>> = [h, c1, c2, c3].iter().map(|o| o.map(|x| 1000 + x)).collect();
                            if starts.iter().all(Option::is_none) {
                                continue;
                            }
                            let r = star(&starts, &owl);
                            let winners: Vec<usize> = (0..4).filter(|&m| r[m].0).collect();
                            assert_eq!(winners.len(), 1, "owl {owl:?} starts {starts:?}: {r:?}");
                            let w = winners[0] as u8;
                            for (m, (_, s)) in r.iter().enumerate() {
                                assert_eq!(*s, Some(w), "machine {m} owl {owl:?} starts {starts:?}: {r:?}");
                            }
                            // Host vs one client only, clearly apart: the earlier start.
                            if let (Some(a), Some(b), None, None) = (starts[0], starts[1], starts[2], starts[3])
                                && a.abs_diff(b) > TIE_MS
                            {
                                assert_eq!(w, if a < b { 0 } else { 1 }, "owl {owl:?} starts {starts:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn only_a_players_throw_on_an_enemy_counts() {
        let mut d = vec![0u8; 0x2C];
        put_u16(&mut d, THROW_DEFENDER, 0x182e);
        put_u16(&mut d, THROW_ATTACKER, WIRE_SLOT | 2);
        put_u16(&mut d, THROW_ID, 1200);
        assert_eq!(player_throw_on_enemy(&d, 2), Some((0x182e, 1200)));
        // Another slot's player, an enemy's grab (attacker = the enemy), a player as defender.
        assert_eq!(player_throw_on_enemy(&d, 1), None);
        put_u16(&mut d, THROW_ATTACKER, 0xA001);
        assert_eq!(player_throw_on_enemy(&d, 2), None);
        put_u16(&mut d, THROW_ATTACKER, WIRE_SLOT | 2);
        put_u16(&mut d, THROW_DEFENDER, WIRE_SLOT | 1);
        assert_eq!(player_throw_on_enemy(&d, 2), None);
        put_u16(&mut d, THROW_DEFENDER, NO_NET_ID);
        assert_eq!(player_throw_on_enemy(&d, 2), None);
        put_u16(&mut d, THROW_DEFENDER, 0x182e);
        assert_eq!(player_throw_on_enemy(&d[..0x21], 2), None);
    }

    #[test]
    fn the_table_stays_bounded() {
        let mut t = Table::new();
        for n in 0..100u16 {
            t.on_ours(n, 1000 + n as u64);
        }
        assert!(t.locks.len() <= 32);
        assert!(t.active(99, 1100).is_some());
    }
}

/// v8.46 diagnostics: what became of the enemy a throw (type 18) was aimed at. A friend's deathblow on a
/// last-life boss twice left it at 1 HP on both machines (live 2026-10-26, enemy 1110850, third throw
/// killed it); this logs the defender's HP / lives / owner at +1 s, +3 s and +6 s after every throw, sent or
/// received, so the next time shows which machine did not take the kill. Game thread only; at most 400 lines.
mod throw_probe {
    use super::*;

    const AFTER_MS: [u64; 3] = [1000, 3000, 6000];
    const MAX_LINES: u32 = 400;

    struct Probe {
        defender: u16,
        sent: bool,
        base: u64,
        stage: usize,
    }

    static PROBES: Mutex<Vec<Probe>> = Mutex::new(Vec::new());
    static LINES: AtomicU32 = AtomicU32::new(0);

    /// Send hook / serve (any game thread): queue a probe. A throw is sent as two packets within a second
    /// (e.g. 52256 + 52257); the second one does not start another probe.
    pub fn add(defender: u16, sent: bool, now: u64) {
        if !enemy_net_id(defender) || LINES.load(Ordering::Relaxed) >= MAX_LINES {
            return;
        }
        let Ok(mut g) = PROBES.try_lock() else { return };
        if g.iter().any(|p| p.defender == defender && now.saturating_sub(p.base) < 1500) || g.len() >= 16 {
            return;
        }
        g.push(Probe { defender, sent, base: now, stage: 0 });
    }

    fn describe(defender: u16) -> String {
        let chr = enemy_chrs().into_iter().find(|&c| crate::netai::net_id_of(c) == Some(defender));
        let Some(chr) = chr else {
            return "no enemy with this netId is loaded here (dead or unloaded)".into();
        };
        let entity = chr.entity_id().unwrap_or(-1);
        let owned = owned_here(defender);
        let forced = ownfill::forced(defender);
        match crate::livesync::read_data(chr) {
            Some((_, hp, max_hp, max_posture, lives)) => format!(
                "entity {entity} hp {hp}/{max_hp} lives {lives} (of {}) max posture {max_posture} owned here {owned} forced {forced}",
                crate::livesync::lives0_of(chr)
            ),
            None => format!("entity {entity} (data unreadable) owned here {owned} forced {forced}"),
        }
    }

    /// Frame tick (game thread).
    pub fn tick() {
        let now = crate::game::now_ms();
        let due: Vec<(u16, bool, u64, usize)> = {
            let Ok(mut g) = PROBES.try_lock() else { return };
            let mut out = Vec::new();
            for p in g.iter_mut() {
                if p.stage < AFTER_MS.len() && now >= p.base + AFTER_MS[p.stage] {
                    out.push((p.defender, p.sent, p.base, p.stage));
                    p.stage += 1;
                }
            }
            g.retain(|p| p.stage < AFTER_MS.len());
            out
        };
        for (defender, sent, _, stage) in due {
            if LINES.fetch_add(1, Ordering::Relaxed) >= MAX_LINES {
                return;
            }
            log::line(&format!(
                "throwprobe: +{} s after a throw ({}) on {defender:#x}: {}",
                AFTER_MS[stage] / 1000,
                if sent { "ours" } else { "the friend's" },
                describe(defender)
            ));
        }
    }
}
