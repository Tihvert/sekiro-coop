//! Each player hears an NPC's introduction (2026-10-24, NOT verified live).
//!
//! Owner, after the 7.4 test: when the friend talked to an NPC first, the host no longer
//! got that NPC's first conversation; decisions must stay shared, but each player should
//! hear the introduction. Setting `[GAMEPLAY] shared_npc_intros` (default 0; 1 = the old
//! behaviour, every talk flag shared at once).
//!
//! Why there is no clean per-flag split: Sekiro's talk scripts (ESD) choose a line with
//! the same group-7 event flags (`7AABBkxxx`, one block of 100 per NPC, IMPLEMENTATION-NOTES
//! §19) that hold the quest decisions, and the game files are encrypted here, so the ESD
//! that would say "this flag only selects a line" cannot be read. The randomizer data
//! (`events.txt`, `annotations.txt`) covers EMEVD and shops, not talk scripts. So the
//! split is made per NPC and per conversation instead of per flag:
//! - the sender marks the group-7 flags its game wrote during a conversation (talk menu up,
//!   `game::talk_idle()` false, plus 1.5 s after it closes for the flag the script sets
//!   last) and sends them as `EV_TALK_FLAG` instead of `EV_FLAG`;
//! - the receiver applies them at once, unless the NPC's block of 100 flags is still all
//!   off in its own world (= it never talked to that NPC): then the whole block's talk
//!   flags from the friend wait ("held");
//! - when we talk to that NPC ourselves (our conversation writes into the held block),
//!   we hear its introduction from our own (all-off) state; our own writes into that
//!   block are not sent; when our conversation ends, our writes are undone and the
//!   friend's held flags are applied: the NPC continues from the friend's decisions,
//!   both worlds agree again;
//! - a held block that we never talk to stays held until the session ends (the friend's
//!   quest progress with that NPC then never reaches our world; for the joiner nothing of
//!   it is merged into its own world either). NPC state machines (group 0, 1000-1999),
//!   map flags and every flag written outside a conversation stay shared as before.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::log;

/// Flag event of the sender's own conversation (same payload as world.rs `EV_FLAG`).
pub const EV_TALK_FLAG: u8 = 8;
/// After the talk menu closes, writes still count as the conversation's for this long.
const TALK_GRACE: Duration = Duration::from_millis(1500);
const BLOCK: u32 = 100;
const MAX_HELD: usize = 400;
const MAX_LOG: u32 = 60;

/// Settings file `shared_npc_intros`: 0 = per player (this module), 1 = shared at once.
pub static SHARED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn per_player() -> bool {
    !SHARED.load(std::sync::atomic::Ordering::Relaxed)
}

fn is_talk_flag(id: u32) -> bool {
    id / 10_000_000 % 10 == 7
}

fn block_of(id: u32) -> u32 {
    id - id % BLOCK
}

/// NPCs known to have talk flags (quest NPCs): lock key (talklock.rs, the entity id) -> the
/// block of 100 talk flags their conversation wrote. Learned from conversations, ours and
/// every peer's (`learn_quest_npc`): the game files that would list them are encrypted.
static QUEST_NPCS: std::sync::Mutex<Option<HashMap<i32, u32>>> = std::sync::Mutex::new(None);

/// Remember that the NPC `key` wrote the talk flag `flag` in a conversation.
pub fn learn_quest_npc(key: i32, flag: u32) {
    if !is_talk_flag(flag) {
        return;
    }
    if let Ok(mut m) = QUEST_NPCS.lock() {
        let m = m.get_or_insert_with(HashMap::new);
        if m.len() < 1024 && m.insert(key, block_of(flag)).is_none() {
            log::line(&format!("npc intro: NPC {key:#x} is a quest NPC (talk flag block {})", block_of(flag)));
        }
    }
}

/// The block of talk flags of the NPC `key`, if it is known to have one (a quest NPC). The
/// first conversation with an NPC is never known as one: nothing wrote a flag yet.
pub fn quest_block_of(key: i32) -> Option<u32> {
    QUEST_NPCS.lock().ok()?.as_ref()?.get(&key).copied()
}

/// What world.rs does with a flag change of ours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Own {
    /// As before (`EV_FLAG`).
    Send,
    /// Written in our conversation: `EV_TALK_FLAG`.
    SendTalk,
    /// Our conversation with an NPC the friend met first: not sent, undone at the end.
    Suppress,
}

#[derive(Default)]
struct Held {
    /// The friend's talk flags of this block, latest value per id, in arrival order.
    flags: Vec<(u32, bool)>,
    since: Option<Instant>,
}

#[derive(Default)]
pub struct Intro {
    talk_until: Option<Instant>,
    talking: bool,
    /// Held blocks (base id) -> the friend's flags.
    held: HashMap<u32, Held>,
    /// Blocks decided as "already met here" (or released): applied at once.
    open: HashSet<u32>,
    /// Our writes into held blocks during this conversation: id -> value before.
    ours: HashMap<u32, bool>,
    logged: u32,
}

impl Intro {
    fn note(&mut self, line: impl FnOnce() -> String) {
        if self.logged < MAX_LOG {
            self.logged += 1;
            log::line(&line());
        }
    }

    /// New session: nothing held any more (the friend's held talk never reaches us).
    pub fn reset(&mut self) {
        let n: usize = self.held.values().map(|h| h.flags.len()).sum();
        if n > 0 {
            log::line(&format!(
                "npc intro: session over: {} NPC blocks with {n} of the friend's talk flags were still held (we never talked to them): dropped",
                self.held.len()
            ));
        }
        self.held.clear();
        self.open.clear();
        self.ours.clear();
    }

    /// Every frame (world.rs): our conversation state. Returns the blocks to release now
    /// (our conversation with them just ended).
    pub fn frame(&mut self, now: Instant) -> Vec<u32> {
        let menu = !crate::game::talk_idle();
        if menu {
            self.talk_until = Some(now + TALK_GRACE);
        }
        let talking = self.talk_until.is_some_and(|t| now < t);
        let ended = self.talking && !talking;
        self.talking = talking;
        if !ended || self.ours.is_empty() {
            return Vec::new();
        }
        let mut blocks: Vec<u32> = self.ours.keys().map(|&id| block_of(id)).filter(|b| self.held.contains_key(b)).collect();
        blocks.sort_unstable();
        blocks.dedup();
        blocks
    }

    /// A change of ours (settled, not the friend's).
    pub fn own_write(&mut self, id: u32, on: bool, before: Option<bool>) -> Own {
        if !per_player() || !is_talk_flag(id) || !self.talking {
            return Own::Send;
        }
        let b = block_of(id);
        if let Some(key) = crate::talklock::my_key() {
            learn_quest_npc(key, id);
        }
        if self.held.contains_key(&b) {
            self.ours.entry(id).or_insert(before.unwrap_or(!on));
            self.note(|| format!("npc intro: we talk to the NPC of flags {b}-{} for the first time: our flag {id} = {on} stays here", b + BLOCK - 1));
            return Own::Suppress;
        }
        Own::SendTalk
    }

    /// A friend's `EV_TALK_FLAG`: true = held (not applied now).
    pub fn friend_talk(&mut self, id: u32, on: bool, fm: usize) -> bool {
        if !per_player() || !is_talk_flag(id) {
            return false;
        }
        let b = block_of(id);
        if let Some(h) = self.held.get_mut(&b) {
            if let Some(e) = h.flags.iter_mut().find(|(f, _)| *f == id) {
                e.1 = on;
            } else if h.flags.len() < MAX_HELD {
                h.flags.push((id, on));
            }
            return true;
        }
        if self.open.contains(&b) {
            return false;
        }
        // Never talked to this NPC here: its whole block is still off.
        let untouched = (b..b + BLOCK).all(|f| !crate::world::get_flag(fm, f));
        if !untouched {
            self.open.insert(b);
            return false;
        }
        self.held.insert(b, Held { flags: vec![(id, on)], since: Some(Instant::now()) });
        self.note(|| {
            format!(
                "npc intro: the friend talked to an NPC we have not met (flags {b}-{}): its talk flags wait until we talk to it ourselves",
                b + BLOCK - 1
            )
        });
        true
    }

    /// Our conversation with the NPC of `b` ended: (our writes to undo, the friend's flags to
    /// apply). The block is applied at once from now on.
    pub fn release(&mut self, b: u32) -> (Vec<(u32, bool)>, Vec<(u32, bool)>) {
        let held = self.held.remove(&b).unwrap_or_default();
        self.open.insert(b);
        let friend: HashSet<u32> = held.flags.iter().map(|(f, _)| *f).collect();
        let undo: Vec<(u32, bool)> =
            self.ours.iter().filter(|(id, _)| block_of(**id) == b && !friend.contains(id)).map(|(&id, &v)| (id, v)).collect();
        self.ours.retain(|id, _| block_of(*id) != b);
        let waited = held.since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        self.note(|| {
            format!(
                "npc intro: we heard the NPC of flags {b}-{} ourselves: {} of our talk flags undone, {} of the friend's applied (held {waited} s); its decisions are shared again",
                b + BLOCK - 1,
                undo.len(),
                held.flags.len()
            )
        });
        (undo, held.flags)
    }
}

/// A client's introduction of an NPC owns its block on the host this long after its last
/// talk flag there.
const TALK_RACE: Duration = Duration::from_secs(20);

/// Host, 3+ players (PLAN-MULTIPLAYER §2 "npcintro origin-aware"): the host re-sends every
/// client's talk flags to the others, so two clients meeting the same NPC for the first time
/// at the same moment would overwrite each other's conversation. The first client whose talk
/// flags of a block reach the host (host order) owns that block while it talks (refreshed
/// by each of its talk flags, `TALK_RACE`); another client's talk flags of the block in that
/// time are dropped on the host (not applied, not re-sent): the others follow the first
/// talker's decisions, the second talker's own world keeps its lines until the first's
/// arrive. The host's own talk and a single client (two players) are never limited.
#[derive(Default)]
pub struct TalkOrder {
    owners: HashMap<u32, (crate::peers::PeerId, Instant)>,
}

impl TalkOrder {
    pub fn reset(&mut self) {
        self.owners.clear();
    }

    /// A client's talk flag `id` arrived from `from`: false = drop it.
    pub fn admit(&mut self, from: crate::peers::PeerId, id: u32, now: Instant) -> bool {
        self.admit_with(from, id, now, crate::talklock::host_only())
    }

    /// `admit` with `[GAMEPLAY] quest_choices_host_only` given: then no client's talk flag
    /// (they are the flags its conversation with a quest NPC wrote) reaches the shared world.
    pub fn admit_with(&mut self, from: crate::peers::PeerId, id: u32, now: Instant, host_only: bool) -> bool {
        if host_only && is_talk_flag(id) && !from.is_host() {
            return false;
        }
        if !per_player() || !is_talk_flag(id) || from.is_host() {
            return true;
        }
        let b = block_of(id);
        if let Some(&(owner, t)) = self.owners.get(&b)
            && owner != from
            && now.duration_since(t) < TALK_RACE
        {
            return false;
        }
        self.owners.insert(b, (from, now));
        if self.owners.len() > 256 {
            self.owners.retain(|_, (_, t)| now.duration_since(*t) < TALK_RACE);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::PeerId;

    #[test]
    fn talk_order_first_client_in_host_order_decides() {
        let mut o = TalkOrder::default();
        let t = Instant::now();
        // Client 1 meets the NPC of 71_100_400 first; client 3's lines there are dropped.
        assert!(o.admit(PeerId(1), 71_100_450, t));
        assert!(!o.admit(PeerId(3), 71_100_451, t + Duration::from_secs(1)));
        assert!(o.admit(PeerId(1), 71_100_452, t + Duration::from_secs(15)), "the owner goes on (refreshes)");
        assert!(!o.admit(PeerId(3), 71_100_453, t + Duration::from_secs(30)));
        // Another NPC, map flags, the host itself: never limited.
        assert!(o.admit(PeerId(3), 71_100_550, t + Duration::from_secs(1)));
        assert!(o.admit(PeerId(3), 11_105_107, t + Duration::from_secs(1)));
        assert!(o.admit(PeerId::HOST, 71_100_455, t + Duration::from_secs(1)));
        // After the owner's quiet time, the next talker owns it.
        assert!(o.admit(PeerId(3), 71_100_451, t + Duration::from_secs(36)));
        assert!(!o.admit(PeerId(1), 71_100_450, t + Duration::from_secs(37)));
        // A single client (two players) is never dropped.
        let mut two = TalkOrder::default();
        for s in 0..50 {
            assert!(two.admit(PeerId(1), 71_100_400 + s, t + Duration::from_secs(s as u64)));
        }
    }


    /// `quest_choices_host_only`: the host takes no client's talk flags; the host's own,
    /// map flags and the setting off are untouched.
    #[test]
    fn host_only_drops_every_clients_talk_flags() {
        let mut o = TalkOrder::default();
        let t = Instant::now();
        assert!(!o.admit_with(PeerId(1), 71_100_450, t, true));
        assert!(!o.admit_with(PeerId(5), 71_110_020, t, true));
        assert!(o.admit_with(PeerId::HOST, 71_100_450, t, true));
        assert!(o.admit_with(PeerId(1), 11_105_107, t, true), "not a talk flag");
        assert!(o.admit_with(PeerId(1), 71_100_450, t, false), "setting off");
    }

    /// An NPC is a quest NPC once a conversation with it wrote a talk flag.
    #[test]
    fn quest_npcs_are_learned_from_talk_flags() {
        assert_eq!(quest_block_of(0x7001_0001), None);
        learn_quest_npc(0x7001_0001, 11_105_107); // a map flag: not a talk flag
        assert_eq!(quest_block_of(0x7001_0001), None);
        learn_quest_npc(0x7001_0001, 71_100_452);
        assert_eq!(quest_block_of(0x7001_0001), Some(71_100_400));
        assert_eq!(quest_block_of(0x7001_0002), None, "an ordinary NPC");
    }
    #[test]
    fn own_writes_outside_talk_or_group_7_are_plain() {
        let mut i = Intro::default();
        assert_eq!(i.own_write(71_100_450, true, Some(false)), Own::Send); // not talking
        i.talking = true;
        assert_eq!(i.own_write(11_105_107, true, Some(false)), Own::Send); // map flag
        assert_eq!(i.own_write(1_441, true, Some(false)), Own::Send); // NPC state machine
        assert_eq!(i.own_write(71_100_450, true, Some(false)), Own::SendTalk);
    }

    #[test]
    fn held_block_suppresses_ours_and_release_undoes_them() {
        let mut i = Intro::default();
        i.held.insert(71_100_400, Held { flags: vec![(71_100_450, true), (71_100_451, false)], since: None });
        i.talking = true;
        assert_eq!(i.own_write(71_100_452, true, Some(false)), Own::Suppress);
        assert_eq!(i.own_write(71_100_450, true, None), Own::Suppress);
        assert_eq!(i.own_write(71_100_550, true, Some(false)), Own::SendTalk); // other block
        let (undo, apply) = i.release(71_100_400);
        assert_eq!(undo, vec![(71_100_452, false)]); // 450 comes from the friend anyway
        assert_eq!(apply, vec![(71_100_450, true), (71_100_451, false)]);
        assert!(i.open.contains(&71_100_400));
        assert!(i.ours.is_empty());
    }

    #[test]
    fn friend_flags_of_a_held_block_keep_the_latest_value() {
        let mut i = Intro::default();
        i.held.insert(71_100_400, Held { flags: vec![(71_100_450, true)], since: None });
        // A held block never reads the game's flags (fm is not used).
        assert!(i.friend_talk(71_100_450, false, 0));
        assert!(i.friend_talk(71_100_451, true, 0));
        assert_eq!(i.held[&71_100_400].flags, vec![(71_100_450, false), (71_100_451, true)]);
        i.open.insert(71_100_500);
        assert!(!i.friend_talk(71_100_550, true, 0)); // already met here: applied
        assert!(!i.friend_talk(11_105_107, true, 0)); // not a talk flag
    }
}
