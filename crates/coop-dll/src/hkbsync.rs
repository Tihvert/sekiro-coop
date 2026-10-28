//! Replay-style animation sync: mirror behavior (hkb) events.
//!
//! The game animates replay ghosts by raising the recorded player's hkb
//! events on the ghost's behavior graph (RE-FINDINGS-2 § Path B). We do the
//! same live: the raise hook (hook.rs) records every event; the ones raised
//! on our player's graph (and, on the host, on synced enemies' graphs) are
//! sent; the friend raises them on the ghost / the same enemy with
//! `raise_on` (0x1001270). Each event is repeated for `REPEAT_FOR` in later
//! packets and applied once (by its counter `n`), so a lost packet loses
//! nothing. Not covered: hkb variables (walk/run blend), see RE-FINDINGS-2.
//!
//! N players (PLAN-MULTIPLAYER §5.15): events are kept per source (counter `n` per
//! sender, epochs per peer). Player events (entity 0) are raised only for the peer the
//! ghost stands for (the first peer, P3: at most one ghost); enemy events only from the
//! host. The host sends to All (every client's ghost is the host); a client sends to the
//! host only (no other client has a ghost of it).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use coop_net::{HkbEvent, MAX_HKB_PER_PACKET};

use crate::game::{Chr, module_base, read_ptr};
use crate::hook::{self, HkbEntry};
use crate::log;
use crate::mode4::SessionEpochs;
use crate::net::{Dest, Net};
use crate::peers::{PeerId, PeerSet};
use crate::tuning;

const CHR_MODULES: usize = 0x1FF8;
const MOD_BEHAVIOR: usize = 0x28;
const RAISE_ON_RVA: usize = 0x1001270;
const SEND_EVERY: Duration = Duration::from_millis(33);
const REPEAT_FOR: Duration = Duration::from_millis(300);
const ENEMY_GRAPHS_EVERY: Duration = Duration::from_secs(1);
const STATS_EVERY: Duration = Duration::from_secs(30);

/// The behavior "hk character" `x` (behavior module +0x10) and its graph.
fn hk(chr: Chr) -> Option<(usize, usize)> {
    let bm = read_ptr(read_ptr(chr.0 + CHR_MODULES)? + MOD_BEHAVIOR).filter(|&p| p >= 0x10000)?;
    let x = read_ptr(bm + 0x10).filter(|&p| p >= 0x10000)?;
    let graph = read_ptr(x + 0x28).filter(|&p| p >= 0x10000)?;
    Some((x, graph))
}

/// A sender's counter this far behind what we applied is not a repeat (those are at
/// most `REPEAT_FOR` old, a few hundred events) but a restarted game counting from 0
/// again on the same link epoch (a client's relayed peer rejoining: relayed peers share
/// the host link's epoch).
const RESTART_GAP: u32 = 1 << 16;

/// Event `n` from a sender whose highest applied counter is `applied`: new (apply it),
/// or a repeat / already applied.
fn is_new(applied: Option<u32>, n: u32) -> bool {
    match applied {
        None => true,
        Some(a) => (n.wrapping_sub(a) as i32) > 0 || a.wrapping_sub(n) > RESTART_GAP,
    }
}

/// Whether an event from `from` is used here: player events (entity 0) only from the
/// peer the ghost stands for (`ghost_owner`; None = no peer known, take all as v7.5),
/// enemy events only from the host (the enemy authority).
fn wanted(from: PeerId, e: &HkbEvent, ghost_owner: Option<PeerId>) -> bool {
    if e.entity == 0 { ghost_owner.is_none_or(|o| o == from) } else { from.is_host() }
}

fn raise_on(chr: Chr, idx: i32) -> bool {
    let Some((x, _)) = hk(chr) else { return false };
    // MSVC bool: only al is set.
    let f: extern "C" fn(usize, i32) -> u8 = unsafe { std::mem::transmute(module_base() + RAISE_ON_RVA) };
    f(x, idx) != 0
}

#[derive(Default)]
pub struct HkbSync {
    /// Link epochs per peer (every sender, relayed clients included).
    epochs: SessionEpochs,
    /// Events from a sender we have no use for (a player that is not our ghost's, an
    /// enemy event from a client).
    unused: u32,
    ring_seen: Option<u32>,
    counter: u32,
    outbox: VecDeque<(Instant, HkbEvent)>,
    last_send: Option<Instant>,
    /// Counter of the newest event already sent once.
    last_sent_n: Option<u32>,
    /// Host: graph -> enemy entity id.
    enemy_graphs: HashMap<usize, i32>,
    enemy_graphs_at: Option<Instant>,
    last_call: Option<Instant>,
    /// Received, waiting until the ghost's position delay has passed, so the
    /// animation starts when the body is there (not 100 ms before).
    delayed: VecDeque<(Instant, HkbEvent)>,
    /// Same for enemy events, on the enemies' own (longer) position delay:
    /// with one queue on the ghost delay, enemy attacks started ~40 ms before
    /// their interpolated bodies arrived.
    delayed_enemies: VecDeque<(Instant, HkbEvent)>,
    /// Highest friend counter applied.
    /// Per sending peer (the event counter `n` is per sender).
    applied_n: HashMap<PeerId, u32>,
    sent: u32,
    recv: u32,
    raised_ok: u32,
    raised_fail: u32,
    lost: u32,
    stats_at: Option<Instant>,
    /// Diagnostics: every recorded event, and per graph (does the hook see the
    /// player's graph at all?).
    seen_total: u32,
    graph_counts: HashMap<usize, u32>,
}

impl HkbSync {
    /// The world was rebuilt (puppet.rs `invalidate_world`): forget every
    /// graph address and queued event; the next call starts like after a gap.
    pub fn on_world_reset(&mut self) {
        self.last_call = None;
        self.enemy_graphs.clear();
        self.enemy_graphs_at = None;
        self.delayed.clear();
        self.delayed_enemies.clear();
    }

    fn raise(&mut self, c: Chr, idx: i32) {
        if raise_on(c, idx) {
            self.raised_ok += 1;
        } else {
            self.raised_fail += 1;
        }
    }

    /// `ghost`: only a validated ReplayGhostIns (raising on a freed pointer
    /// would corrupt the heap).
    /// `peers`: this frame's other players (the ghost is the first peer's).
    pub fn on_frame(&mut self, net: &mut Net, peers: &PeerSet, player: Chr, ghost: Option<Chr>, enemies: &crate::enemies::Enemies) {
        let now = Instant::now();
        // New session, or a gap (loading screen, link down): old recorded
        // events are history, not news. A peer on a new link while others stay
        // only restarts that sender's counter.
        let gap = self.last_call.is_none_or(|t| now.duration_since(t) > REPEAT_FOR);
        self.last_call = Some(now);
        let joins = self.epochs.update(&peers.epochs().collect::<Vec<_>>());
        if !joins.fresh {
            for id in &joins.joined {
                self.applied_n.remove(id);
            }
        }
        if joins.fresh || gap {
            if joins.fresh {
                self.applied_n.clear();
            }
            self.outbox.clear();
            self.delayed.clear();
            self.delayed_enemies.clear();
            self.ring_seen = None;
            // Graph addresses from before a load belong to freed characters;
            // new ones at the same address would be sent under an old id.
            self.enemy_graphs.clear();
            self.enemy_graphs_at = None;
        }
        let mode = tuning::enemy_mode();
        let send_enemies = net.is_host() && mode == tuning::ENEMY_POS_EVENTS;
        if send_enemies && self.enemy_graphs_at.is_none_or(|t| now.duration_since(t) >= ENEMY_GRAPHS_EVERY) {
            self.enemy_graphs_at = Some(now);
            self.enemy_graphs = enemies.synced().iter().filter_map(|&(id, c)| Some((hk(c)?.1, id))).collect();
        }
        let my_graph = hk(player).map(|(_, g)| g);

        // Recorded events -> outbox.
        let count = hook::hkb_ring_count();
        let mut n = *self.ring_seen.get_or_insert(count);
        if count.wrapping_sub(n) > hook::HKB_RING - 64 {
            // Too far behind: the slots are being reused, skip ahead.
            self.lost += count.wrapping_sub(n);
            n = count;
        }
        while n != count {
            match hook::hkb_ring_entry(n) {
                HkbEntry::Pending => break,
                HkbEntry::Lost => self.lost += 1,
                HkbEntry::Ready(graph, idx) => {
                    self.seen_total += 1;
                    *self.graph_counts.entry(graph).or_insert(0) += 1;
                    let entity = if Some(graph) == my_graph {
                        Some(0)
                    } else if send_enemies {
                        self.enemy_graphs.get(&graph).copied()
                    } else {
                        None
                    };
                    if let Some(entity) = entity {
                        self.counter = self.counter.wrapping_add(1);
                        self.outbox.push_back((now, HkbEvent { n: self.counter, entity, idx }));
                    }
                }
            }
            n = n.wrapping_add(1);
        }
        self.ring_seen = Some(n);

        // Outbox -> friend: everything from the last REPEAT_FOR, newest kept.
        while self.outbox.front().is_some_and(|(t, _)| now.duration_since(*t) > REPEAT_FOR) {
            self.outbox.pop_front();
        }
        // New events go out the frame they happen (only the new ones);
        // SEND_EVERY paces the full-window repeats. Waiting for the 33 ms gate
        // started animations up to 33 ms after the (60 Hz) positions they
        // belong to.
        let repeat_due = self.last_send.is_none_or(|t| now.duration_since(t) >= SEND_EVERY);
        let newest = self.outbox.back().map(|(_, e)| e.n);
        let fresh = newest.is_some() && newest != self.last_sent_n;
        if (repeat_due || fresh) && !self.outbox.is_empty() {
            let since = if repeat_due { None } else { self.last_sent_n };
            if repeat_due {
                self.last_send = Some(now);
            }
            self.last_sent_n = newest;
            // Whole window (or just the new events) in chunks, cutting to the
            // newest 4 packets' worth (cutting to the newest 64 lost bursts).
            let list: Vec<HkbEvent> = self
                .outbox
                .iter()
                .map(|(_, e)| *e)
                .filter(|e| since.is_none_or(|s| (e.n.wrapping_sub(s) as i32) > 0))
                .collect();
            let skip = list.len().saturating_sub(4 * MAX_HKB_PER_PACKET);
            // Host: every client's ghost is us. Client: only the host can have a ghost of
            // us (the other clients' ghost is the host), so no relay to them.
            let dest = if net.is_host() { Dest::All } else { Dest::Host };
            for chunk in list[skip..].chunks(MAX_HKB_PER_PACKET) {
                self.sent += 1;
                net.send_hkb_to(dest, chunk);
            }
        }

        // Peers' events -> ghost / enemies, per source.
        let ghost_owner = peers.first().map(|p| p.id);
        let mut incoming = net.take_hkb_from();
        incoming.sort_by_key(|(_, e)| e.n);
        for (from, e) in incoming {
            if !wanted(from, &e, ghost_owner) {
                self.unused += 1;
                continue;
            }
            if !is_new(self.applied_n.get(&from).copied(), e.n) {
                continue; // already applied or queued (repeat)
            }
            self.applied_n.insert(from, e.n);
            self.recv += 1;
            if e.entity == 0 {
                self.delayed.push_back((now, e));
            } else {
                self.delayed_enemies.push_back((now, e));
            }
        }
        let ghost_delay = Duration::from_millis(tuning::ghost_delay_ms() as u64);
        while self.delayed.front().is_some_and(|(t, _)| now.duration_since(*t) >= ghost_delay) {
            let Some((_, e)) = self.delayed.pop_front() else { break };
            if let Some(c) = ghost.filter(|_| tuning::ghost_hkb()) {
                self.raise(c, e.idx);
            }
        }
        // Enemy positions are interpolated `enemy_delay_ms` behind (0 = the
        // old chase, which trails by about one update).
        let enemy_delay = Duration::from_millis(tuning::enemy_delay_ms() as u64);
        while self.delayed_enemies.front().is_some_and(|(t, _)| now.duration_since(*t) >= enemy_delay) {
            let Some((_, e)) = self.delayed_enemies.pop_front() else { break };
            if !net.is_host() && mode == tuning::ENEMY_POS_EVENTS && !enemies.locally_owned(e.entity)
                && let Some(c) = crate::game::chr_by_entity_id(e.entity)
            {
                self.raise(c, e.idx);
            }
        }

        if self.stats_at.is_none_or(|t| now.duration_since(t) >= STATS_EVERY) {
            self.stats_at = Some(now);
            let mut top: Vec<(usize, u32)> = self.graph_counts.drain().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1));
            top.truncate(4);
            let top: Vec<String> = top.iter().map(|(g, c)| format!("{g:#x}:{c}")).collect();
            log::line(&format!("hkb: hook saw {} events in total, busiest graphs {top:?}", self.seen_total));
            log::line(&format!(
                "hkb: sent {} packets, outbox {}, received {} events, raised ok {} failed {}, ring lost {}, my graph {:?}, enemy graphs {}{}",
                self.sent,
                self.outbox.len(),
                self.recv,
                self.raised_ok,
                self.raised_fail,
                self.lost,
                my_graph.map(|g| format!("{g:#x}")),
                self.enemy_graphs.len(),
                if self.unused > 0 { format!(", {} events from other senders unused", self.unused) } else { String::new() }
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(n: u32, entity: i32) -> HkbEvent {
        HkbEvent { n, entity, idx: 1 }
    }

    #[test]
    fn counters_are_per_sender_with_repeats_dropped() {
        // Per sender: each peer's own counter, the same n from two senders both new.
        let mut applied: HashMap<PeerId, u32> = HashMap::new();
        let mut take = |from: u8, n: u32| {
            let id = PeerId(from);
            let new = is_new(applied.get(&id).copied(), n);
            if new {
                applied.insert(id, n);
            }
            new
        };
        assert!(take(0, 5));
        assert!(take(2, 5));
        assert!(!take(0, 5));
        assert!(!take(0, 3));
        assert!(take(0, 6));
        assert!(!take(2, 4));
        // Wrapping counter.
        assert!(is_new(Some(u32::MAX), 0));
        assert!(!is_new(Some(0), u32::MAX));
        // A restarted sender (same link epoch, counting from 1 again) is new.
        assert!(is_new(Some(500_000), 1));
        assert!(!is_new(Some(500_000), 500_000 - 300));
    }

    #[test]
    fn player_events_only_from_the_ghosts_peer_enemy_events_only_from_the_host() {
        let host = PeerId::HOST;
        // Client: the ghost is the host's.
        assert!(wanted(host, &ev(1, 0), Some(host)));
        assert!(wanted(host, &ev(1, 1100800), Some(host)));
        assert!(!wanted(PeerId(2), &ev(1, 0), Some(host)));
        assert!(!wanted(PeerId(2), &ev(1, 1100800), Some(host)));
        // Host with clients 1 and 3: the ghost is client 1's.
        assert!(wanted(PeerId(1), &ev(1, 0), Some(PeerId(1))));
        assert!(!wanted(PeerId(3), &ev(1, 0), Some(PeerId(1))));
        assert!(!wanted(PeerId(1), &ev(1, 1100800), Some(PeerId(1))));
        // No peer known: v7.5 took everything.
        assert!(wanted(PeerId(1), &ev(1, 0), None));
    }
}
