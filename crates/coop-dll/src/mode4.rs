//! Mode 4 (our own enemy sync, used while the native session does not own the enemies)
//! for N players: the pure pieces enemies.rs / posture.rs use (PLAN-MULTIPLAYER §2, §5.15).
//!
//! - The host is the authority: it alone sends enemy states, NetAI records and posture,
//!   to All (the session delivers them to every client; clients never relay).
//! - A client's DamageReport goes to `Dest::Host` only (an All report would be relayed to
//!   every other client for nothing). The host keeps the cumulative totals per
//!   (reporting peer, enemy): each client counts its own hits from 0 per link.
//! - NetAI player targets travel as `WIRE_PLAYER_BASE + slot` (host 0, clients 1..=5);
//!   enemy targets as `WIRE_ENEMY` + entity id.
//! - Session state restarts per peer link: `SessionEpochs`.

use std::collections::{BTreeMap, HashMap};

use crate::game::Chr;
use crate::netai;
use crate::peers::{Joins, PeerId, PeerSet};

/// Link epochs a mode-4 module has seen, per peer. Unlike `peers::EpochTracker` a peer
/// missing from one frame's set is NOT forgotten: the host's damage totals of a client
/// must survive a frame without its PeerFrame (forgetting them would apply that client's
/// re-sent cumulative totals a second time). A new link always has a new epoch (net.rs:
/// unique across `Net`s), so a rejoin is still seen.
#[derive(Clone, Debug, Default)]
pub struct SessionEpochs {
    seen: BTreeMap<PeerId, u64>,
}

impl SessionEpochs {
    /// `now`: (id, link epoch) of this frame's peers. `joined`: new ids and ids on a new
    /// link; `fresh`: nobody of this frame is still on a link seen before (with two
    /// players: every (re)connection, the old global `Net::epoch()` rule).
    pub fn update(&mut self, now: &[(PeerId, u64)]) -> Joins {
        let joined: Vec<PeerId> = now.iter().filter(|(id, e)| self.seen.get(id) != Some(e)).map(|(id, _)| *id).collect();
        let stayed = now.iter().any(|(id, e)| self.seen.get(id) == Some(e));
        for &(id, e) in now {
            self.seen.insert(id, e);
        }
        let mut joined = joined;
        joined.sort();
        joined.dedup();
        Joins { fresh: !joined.is_empty() && !stayed, joined }
    }
}

/// Host: cumulative HP damage totals per (reporting peer, enemy). Every client counts its
/// own hits from 0 per link, so two clients hitting one enemy are two totals.
#[derive(Clone, Debug, Default)]
pub struct DamageTotals {
    applied: HashMap<(PeerId, i32), i32>,
}

impl DamageTotals {
    /// The increase of `from`'s total for enemy `id` over what was applied (recorded);
    /// 0 for a repeat or an older (reordered) report.
    pub fn take(&mut self, from: PeerId, id: i32, total: i32) -> i32 {
        let prev = self.applied.get(&(from, id)).copied().unwrap_or(0);
        let delta = total - prev;
        if delta <= 0 {
            return 0;
        }
        self.applied.insert((from, id), total);
        delta
    }

    /// A peer on a new link: its totals restart at 0.
    pub fn forget_peer(&mut self, from: PeerId) {
        self.applied.retain(|(p, _), _| *p != from);
    }

    pub fn clear(&mut self) {
        self.applied.clear();
    }

    #[cfg(test)]
    pub fn get(&self, from: PeerId, id: i32) -> Option<i32> {
        self.applied.get(&(from, id)).copied()
    }
}

/// Wire target code of a player (session slot).
pub fn wire_player(id: PeerId) -> i32 {
    netai::WIRE_PLAYER_BASE + id.slot() as i32
}

/// The player a wire target code names (None: not a player code).
pub fn wire_player_id(code: i32) -> Option<PeerId> {
    let slot = code.checked_sub(netai::WIRE_PLAYER_BASE)?;
    u8::try_from(slot).ok().and_then(PeerId::new)
}

/// Host: the characters an enemy AI may target as a player, with their session slot:
/// our own player (slot `me`), every peer's native remote, then `ghost` (puppet's
/// "friend" chr: the spawned ReplayGhostIns, else the lowest-id native remote) as the
/// peer whose remote it is, else as the first peer (the ghost is the friend's, P3).
/// First match wins when the AI's netId is looked up.
pub fn player_targets(player: Chr, peers: &PeerSet, ghost: Option<Chr>) -> Vec<(Chr, PeerId)> {
    let mut v = vec![(player, peers.me())];
    v.extend(peers.iter().filter_map(|p| p.native.map(|c| (c, p.id))));
    if let Some(g) = ghost
        && !v.iter().any(|(c, _)| *c == g)
        && let Some(id) = peers.by_native(g).or(peers.first()).map(|p| p.id)
    {
        v.push((g, id));
    }
    v
}

/// The player a host netId belongs to (`players`: (netId, slot), first match wins).
pub fn player_of_net_id(nid: u16, players: &[(u16, PeerId)]) -> Option<PeerId> {
    players.iter().find(|(n, _)| *n == nid).map(|(_, id)| *id)
}

/// Client: where player `id` stands (`own` for ourselves, else that peer's last snapshot
/// position: on a client the host's own, other clients' relayed at `relay_snapshot_hz`).
pub fn player_pos(id: PeerId, peers: &PeerSet, own: Option<[f32; 3]>) -> Option<[f32; 3]> {
    if id == peers.me() { own } else { peers.by_id(id).and_then(|p| p.snapshot).map(|s| s.pos) }
}

/// Host: the peers' snapshot positions (the sync radius covers each of them).
pub fn peer_positions(peers: &PeerSet) -> Vec<[f32; 3]> {
    peers.iter().filter_map(|p| p.snapshot).map(|s| s.pos).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::PeerFrame;

    fn frame(slot: u8, native: Option<usize>, pos: Option<[f32; 3]>) -> PeerFrame {
        let mut p = PeerFrame::bare(PeerId(slot));
        p.native = native.map(Chr);
        p.snapshot = pos.map(|pos| coop_net::Snapshot { pos, hp: 100, ..Default::default() });
        p
    }

    #[test]
    fn damage_totals_are_per_peer_and_enemy() {
        let mut t = DamageTotals::default();
        let (a, b) = (PeerId(1), PeerId(2));
        // Two clients hit the same enemy: two independent totals.
        assert_eq!(t.take(a, 7, 100), 100);
        assert_eq!(t.take(b, 7, 40), 40);
        assert_eq!(t.take(a, 7, 150), 50);
        // Repeats and reordered older totals add nothing, for either peer.
        assert_eq!(t.take(a, 7, 150), 0);
        assert_eq!(t.take(a, 7, 120), 0);
        assert_eq!(t.take(b, 7, 40), 0);
        // Another enemy of the same peer is its own total.
        assert_eq!(t.take(b, 8, 30), 30);
        // One peer's new link restarts only its totals.
        t.forget_peer(a);
        assert_eq!(t.get(a, 7), None);
        assert_eq!(t.get(b, 7), Some(40));
        assert_eq!(t.take(a, 7, 20), 20);
        assert_eq!(t.take(b, 7, 60), 20);
        t.clear();
        assert_eq!(t.take(b, 8, 30), 30);
    }

    #[test]
    fn one_peer_totals_match_the_old_single_map() {
        // v7.5: applied[id], delta = total - prev, only positive deltas recorded.
        let mut t = DamageTotals::default();
        let mut old: HashMap<i32, i32> = HashMap::new();
        for (id, total) in [(1, 10), (1, 10), (1, 25), (2, 5), (1, 20), (2, 9), (1, 40)] {
            let prev = old.get(&id).copied().unwrap_or(0);
            let d = total - prev;
            let want = if d > 0 {
                old.insert(id, total);
                d
            } else {
                0
            };
            assert_eq!(t.take(PeerId(1), id, total), want);
        }
    }

    #[test]
    fn wire_targets_name_the_slot() {
        // Two players: the host 0x10, the client 0x11 (the old HOST/CLIENT codes).
        assert_eq!(wire_player(PeerId::HOST), 0x10);
        assert_eq!(wire_player(PeerId(1)), 0x11);
        assert_eq!(wire_player(PeerId(5)), 0x15);
        for s in 0..=5u8 {
            assert_eq!(wire_player_id(wire_player(PeerId(s))), Some(PeerId(s)));
        }
        // Enemy / game codes are not players.
        assert_eq!(wire_player_id(netai::WIRE_ENEMY), None);
        assert_eq!(wire_player_id(netai::TARGET_CHR), None);
        assert_eq!(wire_player_id(netai::TARGET_POS), None);
        assert_eq!(wire_player_id(0x16), None);
        assert!(!(netai::WIRE_PLAYER_BASE..=netai::WIRE_PLAYER_BASE + 5).contains(&netai::WIRE_ENEMY));
    }

    #[test]
    fn host_maps_each_remote_to_its_peer() {
        let mut peers = PeerSet::new(PeerId::HOST, true);
        peers.push(frame(1, Some(0x1100), None));
        peers.push(frame(2, None, None));
        peers.push(frame(3, Some(0x3300), None));
        let me = Chr(0x9000);
        // Natives by peer; the ReplayGhost (no native) stands for the first peer.
        let t = player_targets(me, &peers, Some(Chr(0x7777)));
        assert_eq!(t, vec![(me, PeerId(0)), (Chr(0x1100), PeerId(1)), (Chr(0x3300), PeerId(3)), (Chr(0x7777), PeerId(1))]);
        // puppet's ghost = the lowest-id native remote: no duplicate.
        assert_eq!(player_targets(me, &peers, Some(Chr(0x1100))).len(), 3);
        // The first peer has no remote, the ghost is peer 3's remote: it maps to peer 3.
        let mut p2 = PeerSet::new(PeerId::HOST, true);
        p2.push(frame(1, None, None));
        p2.push(frame(3, Some(0x3300), None));
        assert_eq!(player_targets(me, &p2, Some(Chr(0x3300))), vec![(me, PeerId(0)), (Chr(0x3300), PeerId(3))]);
        // netId lookup: first match wins.
        let ids = [(0xA000, PeerId(0)), (0xA001, PeerId(1)), (0xA003, PeerId(3))];
        assert_eq!(player_of_net_id(0xA003, &ids), Some(PeerId(3)));
        assert_eq!(player_of_net_id(0xA002, &ids), None);
    }

    #[test]
    fn two_players_map_like_v75() {
        // v7.5 host: (own player, ghost) -> (HOST_PLAYER 0x10, CLIENT_PLAYER 0x11).
        let mut peers = PeerSet::new(PeerId::HOST, true);
        peers.push(frame(1, Some(0x1100), None));
        let t = player_targets(Chr(0x9000), &peers, Some(Chr(0x1100)));
        assert_eq!(t.iter().map(|(_, id)| wire_player(*id)).collect::<Vec<_>>(), vec![0x10, 0x11]);
        // v7.5 client: 0x10 -> the host's snapshot position, 0x11 -> our own.
        let mut c = PeerSet::new(PeerId(1), false);
        c.push(frame(0, None, Some([1.0, 2.0, 3.0])));
        let own = Some([9.0, 9.0, 9.0]);
        assert_eq!(player_pos(wire_player_id(0x10).unwrap(), &c, own), Some([1.0, 2.0, 3.0]));
        assert_eq!(player_pos(wire_player_id(0x11).unwrap(), &c, own), own);
    }

    #[test]
    fn client_resolves_every_slot() {
        let mut c = PeerSet::new(PeerId(2), false);
        c.push(frame(0, None, Some([0.0, 0.0, 0.0])));
        c.push(frame(1, None, Some([1.0, 0.0, 0.0])));
        c.push(frame(4, None, None));
        let own = Some([2.0, 0.0, 0.0]);
        assert_eq!(player_pos(PeerId(0), &c, own), Some([0.0, 0.0, 0.0]));
        assert_eq!(player_pos(PeerId(1), &c, own), Some([1.0, 0.0, 0.0]));
        assert_eq!(player_pos(PeerId(2), &c, own), own);
        // Known but no snapshot yet / not in the session: no target.
        assert_eq!(player_pos(PeerId(4), &c, own), None);
        assert_eq!(player_pos(PeerId(5), &c, own), None);
        assert_eq!(peer_positions(&c), vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]]);
    }

    #[test]
    fn session_epochs_per_peer() {
        let mut e = SessionEpochs::default();
        assert_eq!(e.update(&[]), Joins::default());
        // First peer: a new session (two players: every connection).
        assert_eq!(e.update(&[(PeerId(1), 10)]), Joins { joined: vec![PeerId(1)], fresh: true });
        assert_eq!(e.update(&[(PeerId(1), 10)]), Joins::default());
        // A second client joins: one join, not a reconnect of everyone.
        assert_eq!(e.update(&[(PeerId(1), 10), (PeerId(2), 11)]), Joins { joined: vec![PeerId(2)], fresh: false });
        // A frame without peer 1 (same link): it is not forgotten.
        assert_eq!(e.update(&[(PeerId(2), 11)]), Joins::default());
        assert_eq!(e.update(&[(PeerId(1), 10), (PeerId(2), 11)]), Joins::default());
        // Peer 2 reconnects (new epoch) while 1 stays: only 2.
        assert_eq!(e.update(&[(PeerId(1), 10), (PeerId(2), 12)]), Joins { joined: vec![PeerId(2)], fresh: false });
        // Everyone on new links: fresh.
        assert_eq!(e.update(&[(PeerId(1), 13), (PeerId(2), 14)]), Joins { joined: vec![PeerId(1), PeerId(2)], fresh: true });
        // Two players, link down (no frames) and back: fresh, as the old Net::epoch() rule.
        let mut two = SessionEpochs::default();
        assert!(two.update(&[(PeerId(1), 1)]).fresh);
        assert!(two.update(&[(PeerId(1), 2)]).fresh);
        // Client side: the host link only.
        let mut c = SessionEpochs::default();
        assert!(c.update(&[(PeerId::HOST, 5)]).fresh);
        assert!(!c.update(&[(PeerId::HOST, 5)]).fresh);
    }
}
