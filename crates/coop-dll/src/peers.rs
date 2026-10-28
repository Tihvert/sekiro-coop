//! The other players of this session, one frame at a time (PLAN-MULTIPLAYER §5.3, frozen API).
//!
//! The puppet hub (puppet.rs) builds one `PeerSet` per frame after `Net::on_frame` and the
//! native remotes, and hands it to the modules. With two players it holds at most one peer:
//! `first()` is the old "friend". No game memory is read here; everything is plain data so
//! the rules on top of it can be unit tested.

// Most of the API is for the P1c module switch; only the hub uses it in P1b.
#![allow(dead_code)]

use std::time::Instant;

use crate::game::Chr;

pub const MAX_PLAYERS: usize = 6;
pub const MAX_PEERS: usize = MAX_PLAYERS - 1;

/// Session slot: host 0, clients 1..=5, assigned by the host for the life of a link.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PeerId(pub u8);

impl PeerId {
    pub const HOST: PeerId = PeerId(0);

    /// Some for 0..=5.
    pub fn new(slot: u8) -> Option<PeerId> {
        ((slot as usize) < MAX_PLAYERS).then_some(PeerId(slot))
    }

    pub fn slot(self) -> u8 {
        self.0
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }

    pub fn is_host(self) -> bool {
        self == Self::HOST
    }
}

/// One remote peer this frame (built by the puppet hub from net.rs + native.rs).
#[derive(Clone, Debug)]
pub struct PeerFrame {
    pub id: PeerId,
    pub name: String,
    /// Link epoch (client: the host link's).
    pub epoch: u64,
    pub rtt_ms: Option<u32>,
    /// Latest visible snapshot, as `Net::remote()` today (None while hidden or stale).
    pub snapshot: Option<coop_net::Snapshot>,
    /// `remote_smoothed(ghost_delay_ms)`.
    pub smoothed: Option<([f32; 3], f32)>,
    /// native.rs `remote_of(id)`.
    pub native: Option<Chr>,
    /// Native pos/yaw, else smoothed.
    pub view: Option<([f32; 3], f32)>,
    /// Last hidden snapshot (a loading screen on that side).
    pub loaded_at: Option<Instant>,
    /// Snapshot hp <= 0 since.
    pub dead_since: Option<Instant>,
    /// world.rs committed EV_DEATH (or a boss wipe); the hub refreshes it right after
    /// `World::on_frame`, so the modules after it see this frame's.
    pub death_at: Option<Instant>,
    /// Its latest fresh snapshot says it is in a cutscene (it also reads hidden, so `snapshot`
    /// is None meanwhile). v8.3.
    pub cutscene: bool,
}

impl PeerFrame {
    /// A peer with nothing known yet (tests and the hub fill in the rest).
    pub fn bare(id: PeerId) -> Self {
        Self {
            id,
            name: String::new(),
            epoch: 0,
            rtt_ms: None,
            snapshot: None,
            smoothed: None,
            native: None,
            view: None,
            loaded_at: None,
            dead_since: None,
            death_at: None,
            cutscene: false,
        }
    }

    pub fn hp(&self) -> Option<i32> {
        self.snapshot.map(|s| s.hp)
    }

    /// Visible and hp > 0.
    pub fn alive(&self) -> bool {
        self.snapshot.is_some_and(|s| !s.hidden && s.hp > 0)
    }

    pub fn block(&self) -> Option<i32> {
        self.snapshot.map(|s| s.block)
    }

    /// In our map block `here` (false when either is unknown), as `here == Some(r.block)`.
    pub fn in_block(&self, here: Option<i32>) -> bool {
        self.block().is_some_and(|b| here == Some(b))
    }

    /// A loading screen on that side within `secs` seconds (as `Net::friend_loaded_recently`).
    pub fn loaded_recently(&self, secs: u64) -> bool {
        self.loaded_at.is_some_and(|t| t.elapsed().as_secs() < secs)
    }

    /// Where the peer stands for distance checks: the drawn position, else the snapshot's.
    fn pos(&self) -> Option<[f32; 3]> {
        self.view.map(|(p, _)| p).or(self.snapshot.map(|s| s.pos))
    }
}

/// Every remote peer this frame, sorted by id (never contains `me`).
#[derive(Clone, Debug)]
pub struct PeerSet {
    me: PeerId,
    is_host: bool,
    peers: Vec<PeerFrame>,
}

impl Default for PeerSet {
    fn default() -> Self {
        Self::solo()
    }
}

impl PeerSet {
    pub fn new(me: PeerId, is_host: bool) -> Self {
        Self { me, is_host, peers: Vec::with_capacity(MAX_PEERS) }
    }

    /// No link: empty (me = slot 0, not a host).
    pub fn solo() -> Self {
        Self::new(PeerId::HOST, false)
    }

    /// Keeps id order, replaces the same id. A frame with our own id is dropped.
    pub fn push(&mut self, p: PeerFrame) {
        if p.id == self.me {
            return;
        }
        match self.peers.binary_search_by_key(&p.id, |q| q.id) {
            Ok(i) => self.peers[i] = p,
            Err(i) => self.peers.insert(i, p),
        }
    }

    pub fn me(&self) -> PeerId {
        self.me
    }

    pub fn is_host(&self) -> bool {
        self.is_host
    }

    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// 1 + len.
    pub fn players(&self) -> u32 {
        1 + self.peers.len() as u32
    }

    pub fn iter(&self) -> impl Iterator<Item = &PeerFrame> + '_ {
        self.peers.iter()
    }

    /// Lowest id: the 2-player "friend".
    pub fn first(&self) -> Option<&PeerFrame> {
        self.peers.first()
    }

    pub fn by_id(&self, id: PeerId) -> Option<&PeerFrame> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// Client: the host's frame (None on the host).
    pub fn host(&self) -> Option<&PeerFrame> {
        self.by_id(PeerId::HOST)
    }

    pub fn by_native(&self, chr: Chr) -> Option<&PeerFrame> {
        self.peers.iter().find(|p| p.native == Some(chr))
    }

    /// Peers in our map block `here` (none when `here` is unknown).
    pub fn in_block(&self, here: Option<i32>) -> impl Iterator<Item = &PeerFrame> + '_ {
        self.peers.iter().filter(move |p| p.in_block(here))
    }

    /// The nearest living peer in our block; a tie goes to the lowest id. Peers without a
    /// usable position are skipped.
    pub fn nearest_alive(&self, pos: [f32; 3], here: Option<i32>) -> Option<&PeerFrame> {
        let mut best: Option<(f32, &PeerFrame)> = None;
        for p in self.in_block(here).filter(|p| p.alive()) {
            let Some(q) = p.pos() else { continue };
            let d = (0..3).map(|i| (q[i] - pos[i]) * (q[i] - pos[i])).sum::<f32>();
            if !d.is_finite() {
                continue;
            }
            // Strictly closer only: the earlier (lower) id keeps a tie.
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, p));
            }
        }
        best.map(|(_, p)| p)
    }

    /// Every peer has been dead since `t` or later (false if empty).
    pub fn all_dead_since(&self, t: Instant) -> bool {
        !self.peers.is_empty() && self.peers.iter().all(|p| p.dead_since.is_some_and(|d| d >= t))
    }

    /// (id, link epoch) of every peer, for an `EpochTracker`.
    pub fn epochs(&self) -> impl Iterator<Item = (PeerId, u64)> + '_ {
        self.peers.iter().map(|p| (p.id, p.epoch))
    }

    /// The links a module talks to for a host <-> each exchange (doctor, key items, join
    /// snapshot, scaling): the host every peer; a client only the host (the other clients'
    /// frames share the host link's epoch).
    pub fn link_epochs(&self) -> Vec<(PeerId, u64)> {
        if self.is_host { self.epochs().collect() } else { self.host().map(|h| (h.id, h.epoch)).into_iter().collect() }
    }
}

/// Players in the session this frame (1 = no peer), published by the hub for lock-free
/// readers outside the frame task (items.rs texts).
pub static SESSION_PLAYERS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Peers that joined since the last `EpochTracker::update`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Joins {
    /// New id, or the same id on a new link (its epoch changed), in id order.
    pub joined: Vec<PeerId>,
    /// Nobody who was here at the last update is still here on the same link: a new
    /// session (with two players: every (re)connection, exactly the old `Net::epoch()` rule).
    pub fresh: bool,
}

/// Link epochs a module has handled (PLAN-MULTIPLAYER §5.11 "Epochs"). On the host
/// `Net::epoch()` changes with every client's join; keyed per peer, a second client
/// joining is one join, not a reconnect of everyone.
#[derive(Clone, Debug, Default)]
pub struct EpochTracker {
    seen: std::collections::BTreeMap<PeerId, u64>,
}

impl EpochTracker {
    /// `now`: (id, epoch) of every peer this frame. Peers that are gone are forgotten.
    pub fn update(&mut self, now: impl IntoIterator<Item = (PeerId, u64)>) -> Joins {
        let now: std::collections::BTreeMap<PeerId, u64> = now.into_iter().collect();
        let joined: Vec<PeerId> = now.iter().filter(|(id, e)| self.seen.get(id) != Some(e)).map(|(id, _)| *id).collect();
        let stayed = now.iter().any(|(id, e)| self.seen.get(id) == Some(e));
        self.seen = now;
        Joins { fresh: !joined.is_empty() && !stayed, joined }
    }

    pub fn epoch_of(&self, id: PeerId) -> Option<u64> {
        self.seen.get(&id).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn snap(pos: [f32; 3], hp: i32, block: i32) -> coop_net::Snapshot {
        coop_net::Snapshot { pos, hp, block, ..Default::default() }
    }

    fn peer(slot: u8, s: Option<coop_net::Snapshot>) -> PeerFrame {
        PeerFrame { snapshot: s, ..PeerFrame::bare(PeerId(slot)) }
    }

    #[test]
    fn peer_id_range_and_flags() {
        assert_eq!(PeerId::new(0), Some(PeerId::HOST));
        assert_eq!(PeerId::new(5), Some(PeerId(5)));
        assert_eq!(PeerId::new(6), None);
        assert_eq!(PeerId::new(0xFF), None);
        assert!(PeerId::HOST.is_host());
        assert!(!PeerId(3).is_host());
        assert_eq!(PeerId(4).slot(), 4);
        assert_eq!(PeerId(4).index(), 4);
        assert_eq!(MAX_PEERS, 5);
    }

    #[test]
    fn solo_and_default_are_empty() {
        for s in [PeerSet::solo(), PeerSet::default()] {
            assert!(s.is_empty());
            assert_eq!(s.len(), 0);
            assert_eq!(s.players(), 1);
            assert!(s.first().is_none());
            assert!(s.host().is_none());
            assert_eq!(s.me(), PeerId::HOST);
            assert!(!s.is_host());
            assert!(!s.all_dead_since(Instant::now() - Duration::from_secs(1)));
        }
    }

    #[test]
    fn push_keeps_id_order_replaces_and_drops_me() {
        let mut s = PeerSet::new(PeerId(2), false);
        s.push(peer(4, None));
        s.push(peer(0, None));
        s.push(peer(1, None));
        s.push(peer(2, None)); // ourselves: dropped
        let mut again = peer(1, None);
        again.name = "second".into();
        s.push(again);
        let ids: Vec<u8> = s.iter().map(|p| p.id.0).collect();
        assert_eq!(ids, [0, 1, 4]);
        assert_eq!(s.len(), 3);
        assert_eq!(s.players(), 4);
        assert_eq!(s.first().map(|p| p.id), Some(PeerId::HOST));
        assert_eq!(s.by_id(PeerId(1)).map(|p| p.name.as_str()), Some("second"));
        assert!(s.by_id(PeerId(3)).is_none());
        assert_eq!(s.host().map(|p| p.id), Some(PeerId::HOST));
        assert_eq!(s.me(), PeerId(2));
    }

    /// Two players, seen from each side: the one peer is `first()`; only the client has a host.
    #[test]
    fn two_players_first_is_the_friend() {
        let mut host = PeerSet::new(PeerId::HOST, true);
        host.push(peer(1, Some(snap([1.0, 0.0, 0.0], 100, 7))));
        assert!(host.is_host());
        assert_eq!(host.first().map(|p| p.id), Some(PeerId(1)));
        assert!(host.host().is_none());
        assert_eq!(host.players(), 2);

        let mut client = PeerSet::new(PeerId(1), false);
        client.push(peer(0, Some(snap([1.0, 0.0, 0.0], 100, 7))));
        assert_eq!(client.first().map(|p| p.id), Some(PeerId::HOST));
        assert_eq!(client.host().map(|p| p.id), Some(PeerId::HOST));
    }

    #[test]
    fn frame_hp_alive_block() {
        let p = peer(1, Some(snap([0.0; 3], 50, 3)));
        assert_eq!(p.hp(), Some(50));
        assert!(p.alive());
        assert_eq!(p.block(), Some(3));
        assert!(p.in_block(Some(3)));
        assert!(!p.in_block(Some(4)));
        assert!(!p.in_block(None));

        let dead = peer(1, Some(snap([0.0; 3], 0, 3)));
        assert!(!dead.alive());
        let hidden = peer(1, Some(coop_net::Snapshot { hidden: true, ..snap([0.0; 3], 50, 3) }));
        assert!(!hidden.alive());
        let gone = peer(1, None);
        assert!(!gone.alive());
        assert_eq!(gone.hp(), None);
        assert!(!gone.in_block(Some(3)));
    }

    #[test]
    fn loaded_recently_follows_loaded_at() {
        let mut p = peer(1, None);
        assert!(!p.loaded_recently(10));
        p.loaded_at = Some(Instant::now());
        assert!(p.loaded_recently(10));
        assert!(!p.loaded_recently(0));
        p.loaded_at = Instant::now().checked_sub(Duration::from_secs(20));
        assert!(!p.loaded_recently(10));
        assert!(p.loaded_recently(30));
    }

    #[test]
    fn by_native_finds_the_chr() {
        let mut s = PeerSet::new(PeerId::HOST, true);
        s.push(PeerFrame { native: Some(Chr(0x1000)), ..peer(1, None) });
        s.push(PeerFrame { native: Some(Chr(0x2000)), ..peer(2, None) });
        s.push(peer(3, None));
        assert_eq!(s.by_native(Chr(0x2000)).map(|p| p.id), Some(PeerId(2)));
        assert!(s.by_native(Chr(0x3000)).is_none());
    }

    #[test]
    fn in_block_filters() {
        let mut s = PeerSet::new(PeerId::HOST, true);
        s.push(peer(1, Some(snap([0.0; 3], 1, 5))));
        s.push(peer(2, Some(snap([0.0; 3], 1, 6))));
        s.push(peer(3, Some(snap([0.0; 3], 0, 5))));
        s.push(peer(4, None));
        let ids: Vec<u8> = s.in_block(Some(5)).map(|p| p.id.0).collect();
        assert_eq!(ids, [1, 3]);
        assert_eq!(s.in_block(None).count(), 0);
    }

    #[test]
    fn nearest_alive_skips_dead_other_blocks_and_breaks_ties_by_id() {
        let mut s = PeerSet::new(PeerId::HOST, true);
        s.push(peer(1, Some(snap([10.0, 0.0, 0.0], 100, 1))));
        s.push(peer(2, Some(snap([1.0, 0.0, 0.0], 0, 1)))); // dead
        s.push(peer(3, Some(snap([2.0, 0.0, 0.0], 100, 2)))); // other block
        s.push(peer(4, Some(snap([-10.0, 0.0, 0.0], 100, 1)))); // tie with 1
        s.push(peer(5, Some(snap([f32::NAN, 0.0, 0.0], 100, 1)))); // unusable
        assert_eq!(s.nearest_alive([0.0; 3], Some(1)).map(|p| p.id), Some(PeerId(1)));
        assert_eq!(s.nearest_alive([-6.0, 0.0, 0.0], Some(1)).map(|p| p.id), Some(PeerId(4)));
        assert_eq!(s.nearest_alive([0.0; 3], Some(2)).map(|p| p.id), Some(PeerId(3)));
        assert!(s.nearest_alive([0.0; 3], Some(9)).is_none());
        assert!(s.nearest_alive([0.0; 3], None).is_none());
    }

    /// The drawn position (`view`) wins over the snapshot position.
    #[test]
    fn nearest_alive_uses_the_view() {
        let mut s = PeerSet::new(PeerId::HOST, true);
        s.push(PeerFrame { view: Some(([100.0, 0.0, 0.0], 0.0)), ..peer(1, Some(snap([1.0, 0.0, 0.0], 100, 1))) });
        s.push(peer(2, Some(snap([5.0, 0.0, 0.0], 100, 1))));
        assert_eq!(s.nearest_alive([0.0; 3], Some(1)).map(|p| p.id), Some(PeerId(2)));
    }

    #[test]
    fn all_dead_since_needs_every_peer() {
        let t0 = Instant::now();
        let before = t0 - Duration::from_secs(5);
        let after = t0 + Duration::from_secs(1);
        let mut s = PeerSet::new(PeerId::HOST, true);
        assert!(!s.all_dead_since(t0));
        s.push(PeerFrame { dead_since: Some(after), ..peer(1, None) });
        assert!(s.all_dead_since(t0));
        assert!(s.all_dead_since(after));
        s.push(PeerFrame { dead_since: Some(before), ..peer(2, None) });
        assert!(!s.all_dead_since(t0));
        assert!(s.all_dead_since(before));
        s.push(peer(3, None)); // alive
        assert!(!s.all_dead_since(before));
    }

    /// P4 "epoch per peer": with 3-6 players a later client's join is one join, not a new
    /// session; with two players every (re)connection is a new session, as `Net::epoch()` was.
    #[test]
    fn epoch_tracker_per_peer() {
        let mut t = EpochTracker::default();
        assert_eq!(t.update([]), Joins::default());
        // Two players: connect, steady, reconnect (new epoch) = fresh each time.
        assert_eq!(t.update([(PeerId(1), 10)]), Joins { joined: vec![PeerId(1)], fresh: true });
        assert_eq!(t.update([(PeerId(1), 10)]), Joins::default());
        assert_eq!(t.update([(PeerId(1), 11)]), Joins { joined: vec![PeerId(1)], fresh: true });
        // A second and a third client join: per-peer joins, the session goes on.
        assert_eq!(t.update([(PeerId(1), 11), (PeerId(2), 12)]), Joins { joined: vec![PeerId(2)], fresh: false });
        let j = t.update([(PeerId(1), 11), (PeerId(2), 12), (PeerId(3), 13), (PeerId(5), 14)]);
        assert_eq!(j, Joins { joined: vec![PeerId(3), PeerId(5)], fresh: false });
        // One leaves, one rejoins on a new link while the others stay.
        assert_eq!(t.update([(PeerId(1), 11), (PeerId(3), 15), (PeerId(5), 14)]), Joins { joined: vec![PeerId(3)], fresh: false });
        assert_eq!(t.epoch_of(PeerId(2)), None);
        assert_eq!(t.epoch_of(PeerId(3)), Some(15));
        // Everyone gone (modules are not called while unlinked), then a new one: fresh.
        assert_eq!(t.update([(PeerId(4), 20)]), Joins { joined: vec![PeerId(4)], fresh: true });
    }

    #[test]
    fn link_epochs_host_each_client_host_only() {
        let mut host = PeerSet::new(PeerId::HOST, true);
        host.push(PeerFrame { epoch: 7, ..peer(1, None) });
        host.push(PeerFrame { epoch: 9, ..peer(3, None) });
        assert_eq!(host.link_epochs(), vec![(PeerId(1), 7), (PeerId(3), 9)]);
        let mut client = PeerSet::new(PeerId(2), false);
        client.push(PeerFrame { epoch: 4, ..peer(0, None) });
        client.push(PeerFrame { epoch: 4, ..peer(1, None) });
        assert_eq!(client.link_epochs(), vec![(PeerId::HOST, 4)]);
    }
}
