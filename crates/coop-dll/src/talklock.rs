//! NPC talk lock (docs/IMPLEMENTATION-NOTES.md § NPC talk lock, v8.3, NOT verified live).
//!
//! Two players talking to the same NPC at once left one of them with a half-written quest
//! state. While a conversation of ours runs we tell everybody which NPC it is (`EV_TALK_LOCK`,
//! the session relays an event to every peer); a peer who presses the talk button on an NPC
//! somebody else is talking to gets no conversation and a notice instead.
//!
//! Game side [VAR, static IDA]: `TALK_MAN = base+0x3EA0CE0` (SprjWorldTalkManImpl). The press
//! handler `0x140A775B0(this, dt)` (called every frame from `0x140A746F0`) ends by writing
//! `TalkMan+0x20E0 = pressed` and `TalkMan+0x20E4 = talk id` (-1 when none); the conversation
//! starts later in the same frame from those two. So the veto detours the handler, calls the
//! original and clears both when the NPC is locked by another peer. Talk instances
//! (`SprjNpcTalkIns`, id at `+0x98`) are found through the manager (count `+0x28`, blocks at
//! `+0x30`, 0xB0 each, per block a pointer array `+0x80` with count `+0x88`); the NPC's
//! `ChrIns` comes from `0x1411C9D80`, its entity id from `ChrIns+0x1A24`.
//!
//! The lock key is the entity id; when the chr cannot be resolved it is `FALLBACK | talk id`
//! (the talk id is the same parameter on every machine). Locks live 20 s without a refresh,
//! the owner refreshes every 5 s, a peer that leaves loses its locks.
//!
//! `[GAMEPLAY] quest_choices_host_only = 1`: a client in the host's world cannot talk to quest
//! NPCs (an NPC whose conversation wrote talk flags, learned: `npcintro::quest_block_of`), and
//! the host drops the clients' `EV_TALK_FLAG` (npcintro.rs `TalkOrder`).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::game::{self, Chr, module_base, read, read_ptr};
use crate::log;
use crate::net::Net;
use crate::peers::{PeerId, PeerSet};

/// Event kind: a = lock key (entity id), b = 1 started / refreshed, 0 ended. Free: the kinds
/// in use are 1-8, 40, 41 (world.rs, npcintro.rs, resync.rs).
pub const EV_TALK_LOCK: u8 = 42;

/// `[GAMEPLAY] quest_choices_host_only`.
pub static HOST_ONLY: AtomicBool = AtomicBool::new(false);

pub fn host_only() -> bool {
    HOST_ONLY.load(Ordering::Relaxed)
}

const TALK_MAN_RVA: usize = 0x3EA0CE0;
const TM_PRESS: usize = 0x20E0;
const TM_PRESS_ID: usize = 0x20E4;
const TM_BLOCKS_N: usize = 0x28;
const TM_BLOCKS: usize = 0x30;
const BLOCK_SIZE: usize = 0xB0;
const BLOCK_LIST: usize = 0x80;
const BLOCK_LIST_N: usize = 0x88;
const TALK_ID: usize = 0x98;
const TALK_CHR_RVA: usize = 0x11C9D80;
const WORLD_CHR_MAN_RVA: usize = 0x3D7A1E0;
const MAX_BLOCKS: u32 = 256;
const MAX_INSTANCES: u32 = 1024;

const PRESS_RVA: usize = 0xA775B0;
/// The first 31 bytes of `0x140A775B0` (nine instructions, none RIP-relative: `mov r11,rsp`,
/// `mov [r11+20h],rbx`, five pushes, `lea rbp,[r11-5Fh]`, `sub rsp,0A0h`, `movaps [r11-38h],xmm6`).
const PRESS_PROLOGUE: [u8; 31] = [
    0x4C, 0x8B, 0xDC, 0x49, 0x89, 0x5B, 0x20, 0x55, 0x57, 0x41, 0x54, 0x41, 0x56, 0x41, 0x57, 0x49, 0x8D, 0x6B, 0xA1, 0x48, 0x81, 0xEC,
    0xA0, 0x00, 0x00, 0x00, 0x41, 0x0F, 0x29, 0x73, 0xC8,
];

/// A lock not refreshed for this long is gone.
pub const LOCK_TTL: Duration = Duration::from_secs(20);
/// The owner sends its lock again this often while the conversation runs.
const REFRESH_EVERY: Duration = Duration::from_secs(5);
/// The talk menu must be idle this long before the conversation counts as over (the menu
/// is down between some lines).
const END_GRACE: Duration = Duration::from_millis(1500);
/// A press older than this no longer names the conversation that is up.
const PRESS_KEEP: Duration = Duration::from_secs(10);
/// One notice per this long.
const NOTICE_EVERY: Duration = Duration::from_secs(2);
/// Lock key of an NPC whose chr could not be found: `FALLBACK | talk id`.
pub const FALLBACK: i32 = 0x4000_0000;

// --- The lock table (pure) --------------------------------------------------------

/// Other players' locks: key -> (owner, last refresh).
#[derive(Default, Debug)]
pub struct Locks {
    by_key: HashMap<i32, (PeerId, Instant)>,
}

impl Locks {
    /// `peer` started or refreshed its conversation with `key`. The first holder keeps it
    /// until it ends or expires; false = another peer holds it (its lock is not replaced).
    pub fn set(&mut self, key: i32, peer: PeerId, now: Instant) -> bool {
        match self.by_key.get(&key) {
            Some(&(owner, t)) if owner != peer && now.saturating_duration_since(t) < LOCK_TTL => false,
            _ => {
                self.by_key.insert(key, (peer, now));
                true
            }
        }
    }

    /// `peer`'s conversation ended: only the owner can clear a lock.
    pub fn clear(&mut self, key: i32, peer: PeerId) {
        if self.by_key.get(&key).is_some_and(|&(owner, _)| owner == peer) {
            self.by_key.remove(&key);
        }
    }

    /// Locks of peers not in `present` (they left) are dropped; returns how many.
    pub fn retain_peers(&mut self, present: &[PeerId]) -> usize {
        let before = self.by_key.len();
        self.by_key.retain(|_, (owner, _)| present.contains(owner));
        before - self.by_key.len()
    }

    /// Drops what expired; returns how many.
    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.by_key.len();
        self.by_key.retain(|_, (_, t)| now.saturating_duration_since(*t) < LOCK_TTL);
        before - self.by_key.len()
    }

    /// Who holds `key` now (expired locks do not count even before `expire` ran).
    pub fn owner(&self, key: i32, now: Instant) -> Option<PeerId> {
        self.by_key.get(&key).filter(|(_, t)| now.saturating_duration_since(*t) < LOCK_TTL).map(|&(o, _)| o)
    }

    pub fn len(&self) -> usize {
        self.by_key.len()
    }
}

/// What the press handler's veto does with a press on `key`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Another peer is talking to this NPC.
    LockedBy(PeerId),
    /// `quest_choices_host_only`: a client may not talk to a quest NPC.
    HostOnly,
}

/// Pure decision. `client` = we are a client in the host's world; `quest` = the NPC has a
/// talk-flag block (`npcintro::quest_block_of`). Our own lock never blocks us (the table holds
/// other players' locks only).
pub fn verdict(locks: &Locks, key: i32, now: Instant, client: bool, host_only: bool, quest: bool) -> Verdict {
    if host_only && client && quest {
        return Verdict::HostOnly;
    }
    match locks.owner(key, now) {
        Some(p) => Verdict::LockedBy(p),
        None => Verdict::Allow,
    }
}

// --- Our own conversation (pure state machine) ------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mine {
    pub key: i32,
    sent_at: Instant,
    idle_since: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    Start(i32),
    Refresh(i32),
    End(i32),
}

/// One frame of our conversation: `idle` = no talk menu is up (`game::talk_idle`), `key` = the
/// NPC of the conversation, if it can be named now.
pub fn step_mine(mine: &mut Option<Mine>, idle: bool, key: Option<i32>, now: Instant) -> Act {
    match (mine.as_mut(), idle) {
        (None, false) => match key {
            Some(key) => {
                *mine = Some(Mine { key, sent_at: now, idle_since: None });
                Act::Start(key)
            }
            None => Act::None,
        },
        (None, true) => Act::None,
        (Some(m), false) => {
            m.idle_since = None;
            if now.saturating_duration_since(m.sent_at) >= REFRESH_EVERY {
                m.sent_at = now;
                Act::Refresh(m.key)
            } else {
                Act::None
            }
        }
        (Some(m), true) => {
            let since = *m.idle_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= END_GRACE {
                let key = m.key;
                *mine = None;
                Act::End(key)
            } else {
                Act::None
            }
        }
    }
}

// --- Shared state ------------------------------------------------------------------

#[derive(Default)]
struct State {
    locks: Locks,
    names: Vec<(PeerId, String)>,
    mine: Option<Mine>,
    /// The talk id of the last press that was let through, and when.
    press: Option<(i32, Instant)>,
    client: bool,
    last_notice: Option<Instant>,
    vetoes: u32,
    late: u32,
    logged_forms: u8,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut g = STATE.try_lock().ok()?;
    Some(f(g.get_or_insert_with(State::default)))
}

/// Our conversation's lock key, if one runs (npcintro.rs learns NPC -> talk-flag block).
pub fn my_key() -> Option<i32> {
    with(|s| s.mine.map(|m| m.key)).flatten()
}

fn name_of(names: &[(PeerId, String)], p: PeerId) -> String {
    names.iter().find(|(id, _)| *id == p).map_or_else(|| "Someone".to_owned(), |(_, n)| n.clone())
}

// --- Game reads --------------------------------------------------------------------

fn talk_man() -> Option<usize> {
    read_ptr(module_base() + TALK_MAN_RVA)
}

/// The `SprjNpcTalkIns` with `+0x98 == talk_id`.
fn find_instance(tm: usize, talk_id: i32) -> Option<usize> {
    let n = read::<u32>(tm + TM_BLOCKS_N)?.min(MAX_BLOCKS) as usize;
    let blocks = read_ptr(tm + TM_BLOCKS)?;
    for b in 0..n {
        let blk = blocks + b * BLOCK_SIZE;
        let (Some(count), Some(list)) = (read::<u32>(blk + BLOCK_LIST_N), read_ptr(blk + BLOCK_LIST)) else { continue };
        for i in 0..count.min(MAX_INSTANCES) as usize {
            if let Some(ins) = read_ptr(list + 8 * i)
                && read::<i32>(ins + TALK_ID) == Some(talk_id)
            {
                return Some(ins);
            }
        }
    }
    None
}

/// `0x1411C9D80(handle)` -> ChrIns. The function reads `*handle`, `*(*handle + 0x20)` and the
/// world chr manager, and range-checks everything else: it is called only after those reads
/// were checked here.
fn chr_from_handle(handle: usize) -> Option<Chr> {
    let obj = read_ptr(handle)?;
    read::<i32>(obj + 0x20)?;
    read_ptr(module_base() + WORLD_CHR_MAN_RVA)?;
    let f: extern "C" fn(usize) -> usize = unsafe { std::mem::transmute(module_base() + TALK_CHR_RVA) };
    let p = f(handle);
    (p >= 0x10000 && read::<usize>(p + 0x1FF8).is_some()).then_some(Chr(p))
}

/// The NPC's entity id of a talk instance. The spec passes the instance itself; the class's
/// own methods pass `*(this + 8)`: both are tried (the first that yields a chr with an entity
/// id wins, logged once).
fn entity_of(ins: usize) -> Option<i32> {
    let forms = [Some(ins), read_ptr(ins + 8)];
    for (n, h) in forms.into_iter().enumerate() {
        if let Some(e) = h.and_then(chr_from_handle).and_then(Chr::entity_id).filter(|&e| e > 0) {
            with(|s| {
                if s.logged_forms & (1 << n) == 0 {
                    s.logged_forms |= 1 << n;
                    log::line(&format!("talklock: NPC chr found with 0x1411C9D80({}) (entity {e})", if n == 0 { "talk ins" } else { "*(talk ins + 8)" }));
                }
            });
            return Some(e);
        }
    }
    None
}

/// The lock key of the NPC with talk id `talk_id`.
fn key_of(tm: usize, talk_id: i32) -> i32 {
    find_instance(tm, talk_id).and_then(entity_of).unwrap_or(FALLBACK | (talk_id & 0x3FFF_FFFF))
}

// --- The veto ----------------------------------------------------------------------

/// After the press handler: a press on a locked NPC is cleared. Game thread.
fn after_press() {
    let Some(tm) = talk_man() else { return };
    if read::<u8>(tm + TM_PRESS).unwrap_or(0) == 0 {
        return;
    }
    let Some(talk_id) = read::<i32>(tm + TM_PRESS_ID).filter(|&i| i >= 0) else { return };
    let key = key_of(tm, talk_id);
    let now = Instant::now();
    let quest = crate::npcintro::quest_block_of(key).is_some();
    let out = with(|s| {
        let v = verdict(&s.locks, key, now, s.client, host_only(), quest);
        if v == Verdict::Allow {
            s.press = Some((talk_id, now));
            return None;
        }
        s.vetoes += 1;
        let text = match v {
            Verdict::LockedBy(p) => format!("{} is talking to this NPC", name_of(&s.names, p)),
            _ => "Only the host can talk to quest NPCs.".to_owned(),
        };
        let say = s.last_notice.is_none_or(|t| now.saturating_duration_since(t) >= NOTICE_EVERY);
        if say {
            s.last_notice = Some(now);
        }
        Some((v, text, say, s.vetoes))
    })
    .flatten();
    let Some((v, text, say, n)) = out else { return };
    game::write(tm + TM_PRESS, 0u8);
    game::write(tm + TM_PRESS_ID, -1i32);
    if say {
        crate::bossrules::notice(text, 3);
    }
    if n <= 20 {
        log::line(&format!("talklock: talk press on NPC {key:#x} (talk id {talk_id}) vetoed: {v:?} (#{n})"));
    }
}

static ORIG: AtomicUsize = AtomicUsize::new(0);

extern "C" fn press_detour(a1: usize, dt: f32) -> usize {
    let orig = ORIG.load(Ordering::Acquire);
    if orig == 0 {
        return 0;
    }
    let f: extern "C" fn(usize, f32) -> usize = unsafe { std::mem::transmute(orig) };
    let ret = f(a1, dt);
    if crate::mod_alive() {
        let _ = std::panic::catch_unwind(after_press);
    }
    ret
}

/// Detours the end of the press handler (lib.rs, at the title screen with the other hooks).
pub fn install() {
    let detour: extern "C" fn(usize, f32) -> usize = press_detour;
    match crate::hook::install_detour(PRESS_RVA, &PRESS_PROLOGUE, detour as usize, "talk press handler (0xA775B0)") {
        Some(t) => {
            ORIG.store(t, Ordering::Release);
            log::line("talklock: press hook installed: a locked NPC's talk press is cleared in the same frame");
        }
        None => log::line("talklock: press hook NOT installed: the veto runs in the frame task and may come too late"),
    }
}

fn hooked() -> bool {
    ORIG.load(Ordering::Acquire) != 0
}

// --- Frame task ---------------------------------------------------------------------

/// A loading screen: nothing of the last world stays.
pub fn on_loading() {
    with(|s| {
        s.mine = None;
        s.press = None;
        s.locks = Locks::default();
    });
}

/// `EV_TALK_LOCK` from `from` (world.rs): a = key, b = 1 started / refreshed, 0 ended.
pub fn on_event(from: PeerId, a: i32, b: i32, now: Instant) {
    with(|s| {
        if b != 0 {
            if s.locks.set(a, from, now) {
                if s.locks.owner(a, now) == Some(from) && s.mine.is_some_and(|m| m.key == a) {
                    log::line(&format!("talklock: peer {} started talking to NPC {a:#x} at the same time as we did", from.slot()));
                }
            } else if let Some(o) = s.locks.owner(a, now) {
                log::line(&format!("talklock: peer {}'s lock on NPC {a:#x} ignored: peer {} holds it", from.slot(), o.slot()));
            }
        } else {
            s.locks.clear(a, from);
        }
    });
}

/// A peer's talk flag arrived (world.rs): if it holds a lock, that NPC's conversation wrote
/// this block, so the NPC is a quest NPC (learned for `quest_choices_host_only`).
pub fn note_peer_talk_flag(from: PeerId, flag: u32, now: Instant) {
    let key = with(|s| s.locks.by_key.iter().find(|(_, (o, t))| *o == from && now.saturating_duration_since(*t) < LOCK_TTL).map(|(k, _)| *k)).flatten();
    if let Some(key) = key {
        crate::npcintro::learn_quest_npc(key, flag);
    }
}

/// Once per frame in the world (world.rs): follows our conversation, sends / refreshes / ends
/// our lock, drops what expired or belongs to a peer that left. Also the fallback veto when
/// the press hook could not be installed.
pub fn tick(net: &mut Net, peers: &PeerSet, now: Instant) {
    let ids: Vec<PeerId> = peers.iter().map(|p| p.id).collect();
    let connected = net.connected();
    let client = connected && !net.is_host();
    let idle = game::talk_idle();
    if !hooked() {
        after_press();
    }
    let tm = talk_man();
    // The NPC of a conversation that is up: the press that started it, else (no press seen)
    // the talk id the manager still shows. Resolved before the lock is taken (it logs).
    let (have_mine, press) = with(|s| (s.mine.is_some(), s.press)).unwrap_or((false, None));
    let key = if idle || have_mine {
        None
    } else {
        press
            .filter(|(_, t)| now.saturating_duration_since(*t) < PRESS_KEEP)
            .map(|(id, _)| id)
            .or_else(|| tm.and_then(|t| read::<i32>(t + TM_PRESS_ID)).filter(|&i| i >= 0))
            .zip(tm)
            .map(|(id, tm)| key_of(tm, id))
    };
    let act = with(|s| {
        s.client = client;
        s.names = peers.iter().map(|p| (p.id, p.name.clone())).collect();
        let gone = if connected { s.locks.retain_peers(&ids) } else { std::mem::take(&mut s.locks).len() };
        let expired = s.locks.expire(now);
        if gone + expired > 0 {
            log::line(&format!("talklock: {gone} lock(s) of peers that left and {expired} expired ones dropped"));
        }
        step_mine(&mut s.mine, idle, key, now)
    });
    let Some(act) = act else { return };
    let (key, on) = match act {
        Act::None => return,
        Act::Start(k) => (k, 1),
        Act::Refresh(k) => (k, 1),
        Act::End(k) => (k, 0),
    };
    if matches!(act, Act::Start(_) | Act::End(_)) {
        log::line(&format!("talklock: our conversation with NPC {key:#x} {}", if on == 1 { "started" } else { "ended" }));
    }
    if connected {
        net.send_event(EV_TALK_LOCK, key, on);
    }
    // Ordering check without the hook: a conversation that opened on a locked NPC.
    if !hooked() && on == 1 && matches!(act, Act::Start(_)) {
        let locked = with(|s| s.locks.owner(key, now)).flatten();
        if let Some(p) = locked {
            with(|s| s.late += 1);
            log::line(&format!(
                "talklock: ORDERING FAILED (no hook): our conversation with NPC {key:#x} opened while peer {} holds it",
                p.slot()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn a_lock_expires_without_a_refresh_and_a_refresh_keeps_it() {
        let t0 = Instant::now();
        let mut l = Locks::default();
        assert!(l.set(1100500, PeerId(1), t0));
        assert_eq!(l.owner(1100500, at(t0, 19)), Some(PeerId(1)));
        assert_eq!(l.owner(1100500, at(t0, 20)), None, "20 s without a refresh");
        // A refresh extends it.
        assert!(l.set(1100500, PeerId(1), at(t0, 15)));
        assert_eq!(l.owner(1100500, at(t0, 30)), Some(PeerId(1)));
        assert_eq!(l.owner(1100500, at(t0, 35)), None);
        assert_eq!(l.expire(at(t0, 40)), 1);
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn first_holder_keeps_the_lock_and_only_the_owner_clears_it() {
        let t0 = Instant::now();
        let mut l = Locks::default();
        assert!(l.set(7, PeerId(1), t0));
        assert!(!l.set(7, PeerId(2), at(t0, 1)), "second talker does not take it");
        assert_eq!(l.owner(7, at(t0, 1)), Some(PeerId(1)));
        l.clear(7, PeerId(2));
        assert_eq!(l.owner(7, at(t0, 2)), Some(PeerId(1)), "not the owner");
        // After the owner's TTL the next one takes it.
        assert!(l.set(7, PeerId(2), at(t0, 21)));
        assert_eq!(l.owner(7, at(t0, 22)), Some(PeerId(2)));
        l.clear(7, PeerId(2));
        assert_eq!(l.owner(7, at(t0, 22)), None);
    }

    #[test]
    fn a_leaving_peers_locks_are_dropped_for_any_number_of_peers() {
        let t0 = Instant::now();
        let mut l = Locks::default();
        for p in 1..=5u8 {
            assert!(l.set(100 + p as i32, PeerId(p), t0));
        }
        assert_eq!(l.retain_peers(&[PeerId(1), PeerId(2), PeerId(4), PeerId(5)]), 1);
        assert_eq!(l.owner(103, t0), None);
        assert_eq!(l.owner(104, t0), Some(PeerId(4)));
        assert_eq!(l.retain_peers(&[]), 4);
        assert_eq!(l.len(), 0);
    }

    #[test]
    fn veto_decision() {
        let t0 = Instant::now();
        let mut l = Locks::default();
        l.set(11, PeerId(3), t0);
        let v = |key, client, host_only, quest| verdict(&l, key, t0, client, host_only, quest);
        // Locked by another peer: vetoed; an unlocked NPC and our own conversation: allowed
        // (our own lock is never in the table).
        assert_eq!(v(11, false, false, false), Verdict::LockedBy(PeerId(3)));
        assert_eq!(v(12, false, false, false), Verdict::Allow);
        // Expired locks do not veto.
        assert_eq!(verdict(&l, 11, at(t0, 25), false, false, false), Verdict::Allow);
        // Host-only: a client with a quest NPC, whoever is talking; not the host, not an
        // ordinary NPC, not with the setting off.
        assert_eq!(v(12, true, true, true), Verdict::HostOnly);
        assert_eq!(v(12, false, true, true), Verdict::Allow, "the host");
        assert_eq!(v(12, true, true, false), Verdict::Allow, "ordinary NPC");
        assert_eq!(v(12, true, false, true), Verdict::Allow, "setting off");
        assert_eq!(v(11, true, true, false), Verdict::LockedBy(PeerId(3)), "ordinary NPC, still locked");
    }

    #[test]
    fn our_conversation_starts_refreshes_and_ends_after_the_grace() {
        let t0 = Instant::now();
        let mut m = None;
        assert_eq!(step_mine(&mut m, true, Some(5), t0), Act::None, "idle: nothing");
        assert_eq!(step_mine(&mut m, false, None, t0), Act::None, "menu up but the NPC unknown yet");
        assert_eq!(step_mine(&mut m, false, Some(5), at(t0, 1)), Act::Start(5));
        assert_eq!(step_mine(&mut m, false, None, at(t0, 3)), Act::None);
        assert_eq!(step_mine(&mut m, false, None, at(t0, 6)), Act::Refresh(5));
        // The menu is down for a moment between lines: not the end.
        assert_eq!(step_mine(&mut m, true, None, at(t0, 7)), Act::None);
        assert_eq!(step_mine(&mut m, false, None, at(t0, 8)), Act::None);
        assert_eq!(step_mine(&mut m, true, None, at(t0, 9)), Act::None);
        assert_eq!(step_mine(&mut m, true, None, at(t0, 11)), Act::End(5));
        assert_eq!(step_mine(&mut m, true, None, at(t0, 12)), Act::None);
    }

    #[test]
    fn fallback_key_is_distinct_from_entity_ids() {
        let k = FALLBACK | 100050;
        assert!(k > 0x3FFF_FFFF);
        assert_ne!(k, 100050);
    }
}
