//! The seamless session: one datagram socket, never torn down by game events. Deaths,
//! idol warps and loading screens only change what the snapshots say; the links live
//! until the game closes.
//!
//! Host binds a port and waits; a client sends Hello until welcomed. If a peer goes silent
//! for `LOST_AFTER`, its link drops to Lost and (client) keeps re-sending Hello, so a
//! crashed/restarted peer reconnects with no invite.
//!
//! Up to 6 players (PLAN-MULTIPLAYER §1, wire v10): a star through the host. A client has
//! one `Link`, to the host (`links[0]`); the host has one per client (index = slot - 1)
//! and relays between them.
//! - Slots: host 0, clients 1..=`MAX_PEERS`, given in Welcome. A Hello from a new address
//!   whose uid a link already has takes that link's slot (the client restarted); otherwise
//!   the lowest free slot under `set_max_peers` (then a lost link's), else Reject(Full).
//! - Routed packets carry `src, dst` (protocol.rs). The host takes a client's packet as
//!   coming from that client's slot, delivers it if `dst` is 0 or all, and relays it to
//!   `dst` (all: every other client): unreliable kinds cut-through (snapshots at most
//!   `set_relay_snapshot_hz` per source), events acked on each hop and queued again on every
//!   target link, bulk transfers stored until complete and then sent on as new transfers.
//! - Receivers keep order per source (snapshots, raw packets), so a client's one link carries
//!   every player's stream; events say which slot they came `from` (the original sender).
//! - Roster: the host sends a `ROSTER_KIND` bulk with every member when it changes; a client
//!   turns the differences into PeerJoined / PeerLeft for the other clients, and they all
//!   leave when the host link ends. With one client the roster names only the host and that
//!   client, so it is not sent at all: two players see the v9 traffic plus the route bytes.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use crate::protocol::{
    BULK_CHUNK, DST_ALL, DamageReport, EnemyState, K_HELLO, MAX_BULK_BYTES, MAX_BULK_CHUNKS, MAX_DATAGRAM,
    MAX_ENEMIES_PER_PACKET, MAX_NETAI_PER_PACKET, MAX_PEERS, MAX_PLAYER_RAW, MAX_POSTURE_PER_PACKET,
    NETAI_PACKET_HEADER, NetAiEntry, Packet, PostureState, ROSTER_KIND, RosterEntry, Snapshot, VERSION, decode_roster,
    decode_routed, encode, encode_roster, encode_routed, password_hash, peek_header, version_reject,
};
use crate::transport::{Datagram, steam_id_of};

/// Most distinct raw packet types remembered for the per-type order check
/// (the game uses about six per source; a peer cannot grow the map without bound).
const MAX_RAW_TYPES: usize = 64;

pub const HELLO_EVERY: Duration = Duration::from_millis(500);
pub const HEARTBEAT_EVERY: Duration = Duration::from_millis(250);
pub const LOST_AFTER: Duration = Duration::from_secs(5);
pub const PING_EVERY: Duration = Duration::from_secs(1);
pub const EVENT_RESEND_EVERY: Duration = Duration::from_millis(200);
/// Unacked events kept for re-sending (per link); the oldest is dropped beyond this.
pub const MAX_PENDING_EVENTS: usize = 64;
/// How many received event seqs are remembered for de-duplication.
const SEEN_EVENTS: usize = 256;
/// Host: a Hello from a linked peer this long after our last Welcome to it is
/// a client that dropped the link and is rejoining (see `on_hello`). Shorter
/// gaps are the same join (queued or re-sent Hellos).
const REJOIN_HELLO_AFTER: Duration = Duration::from_secs(2);
/// Bulk chunks sent but not acked at once (16 * 1.1 KB per round trip is
/// ~100 KB in well under a second on a normal link, without a burst that
/// a home router would drop).
pub const BULK_WINDOW: usize = 16;
/// Completed incoming transfers remembered, so late duplicate chunks are
/// acked but not delivered again.
const BULK_DONE_KEPT: usize = 16;
/// Session slots, the host's 0 included.
const SLOTS: usize = MAX_PEERS + 1;
/// `me()` of a client before its Welcome.
const NO_SLOT: u8 = 0xFF;
/// Host: relayed snapshots per second per source until `set_relay_snapshot_hz`.
pub const RELAY_SNAPSHOT_HZ: u32 = 30;
/// Host: refused Hellos kept for `take_refusals` (a client repeats its Hello every 500 ms).
const MAX_REFUSALS: usize = 16;

#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    /// Waits for clients on the bound port.
    Host,
    /// Connects to the host's address.
    Client { host: SocketAddr },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Waiting,
    Connected,
    /// Peer silent too long; client keeps retrying, host keeps listening.
    Lost,
}

/// Why the host refused a Hello (on the wire as this number).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RejectReason {
    Password = 1,
    Full = 2,
    Version = 3,
}

impl RejectReason {
    fn from_wire(v: u8) -> Self {
        match v {
            1 => RejectReason::Password,
            2 => RejectReason::Full,
            _ => RejectReason::Version,
        }
    }
}

/// Why a slot left the roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveWhy {
    /// Said Bye.
    Quit,
    /// Silent for `LOST_AFTER` (a client keeps trying to come back). Client: also another
    /// client the host's roster no longer lists (the roster carries no reason).
    Lost,
    /// Another connection took the slot (same uid at a new address, or a rejoin).
    Replaced,
    /// Refused after it had joined (not sent yet).
    Rejected,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A link came up. Host: `slot` is the new client's; client: `slot` is ours.
    Connected { peer: SocketAddr, name: String, slot: u8 },
    /// Host: with `Connected`. Client: the host (with `Connected`) and every other client
    /// the host's roster lists.
    PeerJoined { slot: u8, uid: u64, name: String },
    PeerLeft { slot: u8, why: LeaveWhy },
    /// Client: the host's whole roster (host slot 0 and ourselves included), after the
    /// PeerJoined / PeerLeft it caused. Only sent while it names another client.
    Roster(Vec<(u8, u64, String)>),
    Rejected { reason: RejectReason },
    /// The link of `slot` went silent (with `PeerLeft { why: Lost }`).
    Lost { slot: u8 },
    /// `slot` said Bye (with `PeerLeft { why: Quit }`).
    PeerQuit { slot: u8 },
    Snapshot { from: u8, snap: Snapshot },
    Enemies { from: u8, list: Vec<EnemyState> },
    Hkb { from: u8, list: Vec<crate::protocol::HkbEvent> },
    Damage { from: u8, report: DamageReport },
    Posture { from: u8, list: Vec<PostureState> },
    /// A reliable game event, delivered exactly once.
    Game { from: u8, kind: u8, a: i32, b: i32 },
    NetAi { from: u8, list: Vec<NetAiEntry> },
    /// A complete bulk transfer (all chunks, in order), once.
    Bulk { from: u8, kind: u8, data: Vec<u8> },
    /// A raw game player packet of `ptype`, never older than the last one
    /// of the same type from the same source.
    PlayerRaw { from: u8, ptype: u32, data: Vec<u8> },
}

/// Outgoing bulk transfer: chunks re-sent until each is acked.
struct BulkOut {
    xfer: u16,
    src: u8,
    dst: u8,
    kind: u8,
    chunks: Vec<Vec<u8>>,
    acked: Vec<bool>,
    sent_at: Vec<Option<Instant>>,
}

/// Incoming bulk transfer being assembled.
struct BulkIn {
    xfer: u16,
    kind: u8,
    chunks: Vec<Option<Vec<u8>>>,
    got: usize,
}

struct PendingEvent {
    seq: u32,
    src: u8,
    dst: u8,
    kind: u8,
    a: i32,
    b: i32,
    last_sent: Instant,
}

/// Everything that belongs to one peer. Pure bookkeeping: the `Session` does the socket I/O.
struct Link {
    addr: SocketAddr,
    slot: u8,
    /// The peer's uid: from its Hello (host), else its SteamID64 from a Steam address, else 0.
    uid: u64,
    name: String,
    state: State,
    last_heard: Instant,
    last_sent: Instant,
    /// Host: when we last answered this peer's Hello.
    last_welcome: Option<Instant>,
    last_ping: Option<Instant>,
    /// Smoothed round-trip time in microseconds.
    rtt_us: Option<f64>,
    /// Newest snapshot seq delivered, per source slot (a client hears everyone on this link).
    last_seq: [Option<u32>; SLOTS],
    pending: VecDeque<PendingEvent>,
    seen: VecDeque<u32>,
    /// Queued outgoing bulk transfers; the front one is being sent.
    bulk_out: VecDeque<BulkOut>,
    next_xfer: u16,
    bulk_in: Option<BulkIn>,
    bulk_done: VecDeque<u16>,
    /// Newest raw seq delivered per (source slot, packet type).
    raw_last: HashMap<(u8, u32), u32>,
    /// Host: links this slot has had (roster `inc`).
    inc: u8,
    /// Host: the roster generation this client is up to date with (None after a reset).
    roster_gen: Option<u32>,
    /// Host: the last roster sent to this client named another client.
    roster_told_others: bool,
}

/// Wrapping-safe "a is newer than b".
fn newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// A bulk payload in chunks (an empty transfer is one empty chunk, so it still arrives).
fn bulk_chunks(data: &[u8]) -> Vec<Vec<u8>> {
    if data.is_empty() { vec![Vec::new()] } else { data.chunks(BULK_CHUNK).map(<[u8]>::to_vec).collect() }
}

impl Link {
    fn new(addr: SocketAddr, slot: u8, now: Instant) -> Self {
        Self {
            addr,
            slot,
            uid: steam_id_of(&addr).unwrap_or(0),
            name: String::new(),
            state: State::Waiting,
            last_heard: now,
            last_sent: now,
            last_welcome: None,
            last_ping: None,
            rtt_us: None,
            last_seq: [None; SLOTS],
            pending: VecDeque::new(),
            seen: VecDeque::new(),
            bulk_out: VecDeque::new(),
            next_xfer: 0,
            bulk_in: None,
            bulk_done: VecDeque::new(),
            raw_last: HashMap::new(),
            inc: 0,
            roster_gen: None,
            roster_told_others: false,
        }
    }

    fn connected(&self) -> bool {
        self.state == State::Connected
    }

    /// Forgets all per-connection state (called whenever the link restarts or dies).
    fn reset(&mut self) {
        self.last_seq = [None; SLOTS];
        self.last_ping = None;
        self.rtt_us = None;
        self.pending.clear();
        self.seen.clear();
        // A transfer belongs to one link: the new one starts afresh (the
        // world dump is re-sent for every new connection anyway).
        self.bulk_out.clear();
        self.next_xfer = 0;
        self.bulk_in = None;
        self.bulk_done.clear();
        // The peer may have restarted and count from 0 again.
        self.raw_last.clear();
        self.roster_gen = None;
        self.roster_told_others = false;
    }

    /// Forgets the order state of one source (it left; its next link counts afresh).
    fn forget_source(&mut self, src: u8) {
        if let Some(s) = self.last_seq.get_mut(src as usize) {
            *s = None;
        }
        self.raw_last.retain(|k, _| k.0 != src);
    }

    fn rtt_ms(&self) -> Option<u32> {
        self.rtt_us.map(|us| (us / 1000.0).round() as u32)
    }

    fn on_pong(&mut self, sample_us: f64) {
        self.rtt_us = Some(self.rtt_us.map_or(sample_us, |old| old * 0.8 + sample_us * 0.2));
    }

    /// Drops reordered/duplicate snapshots of `src`; true if `seq` is to be delivered.
    fn accept_snapshot(&mut self, src: u8, seq: u32) -> bool {
        let Some(last) = self.last_seq.get_mut(src as usize) else { return false };
        let fresh = last.is_none_or(|l| newer(seq, l));
        if fresh {
            *last = Some(seq);
        }
        fresh
    }

    /// Per-(source, type) order, like snapshots: an older full-state frame would move the
    /// remote back. True if the packet is to be delivered.
    fn accept_raw(&mut self, src: u8, ptype: u32, seq: u32) -> bool {
        let key = (src, ptype);
        let fresh = self.raw_last.get(&key).is_none_or(|&last| newer(seq, last));
        let room = self.raw_last.len() < MAX_RAW_TYPES || self.raw_last.contains_key(&key);
        if fresh && room {
            self.raw_last.insert(key, seq);
        }
        fresh && room
    }

    /// True the first time `seq` arrives (within the last `SEEN_EVENTS`).
    fn accept_event(&mut self, seq: u32) -> bool {
        if self.seen.contains(&seq) {
            return false;
        }
        if self.seen.len() >= SEEN_EVENTS {
            self.seen.pop_front();
        }
        self.seen.push_back(seq);
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_event(&mut self, seq: u32, src: u8, dst: u8, kind: u8, a: i32, b: i32, now: Instant) {
        if self.pending.len() >= MAX_PENDING_EVENTS {
            self.pending.pop_front();
        }
        self.pending.push_back(PendingEvent { seq, src, dst, kind, a, b, last_sent: now });
    }

    fn on_ack(&mut self, seq: u32) {
        self.pending.retain(|e| e.seq != seq);
    }

    /// Unacked events due for a re-send (marked as sent now), encoded.
    fn due_events(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let mut due = Vec::new();
        for e in self.pending.iter_mut() {
            if now.saturating_duration_since(e.last_sent) >= EVENT_RESEND_EVERY {
                e.last_sent = now;
                due.push(encode_routed(&Packet::Event { seq: e.seq, kind: e.kind, a: e.a, b: e.b }, e.src, e.dst));
            }
        }
        due
    }

    fn queue_bulk(&mut self, src: u8, dst: u8, kind: u8, chunks: Vec<Vec<u8>>) {
        let b = self.new_bulk(src, dst, kind, chunks);
        self.bulk_out.push_back(b);
    }

    fn new_bulk(&mut self, src: u8, dst: u8, kind: u8, chunks: Vec<Vec<u8>>) -> BulkOut {
        let n = chunks.len();
        let xfer = self.next_xfer;
        self.next_xfer = self.next_xfer.wrapping_add(1);
        BulkOut { xfer, src, dst, kind, chunks, acked: vec![false; n], sent_at: vec![None; n] }
    }

    /// Like `queue_bulk`, but ahead of every transfer that has not started (the roster is a few
    /// bytes and must not wait behind a world dump). A transfer already under way keeps the
    /// front: the receiver assembles one at a time and a newer transfer replaces an unfinished
    /// one, so interrupting it would lose the chunks it already got.
    fn queue_bulk_first(&mut self, src: u8, dst: u8, kind: u8, chunks: Vec<Vec<u8>>) {
        let b = self.new_bulk(src, dst, kind, chunks);
        let started = self.bulk_out.front().is_some_and(|f| f.sent_at.iter().any(Option::is_some));
        self.bulk_out.insert(usize::from(started), b);
    }

    /// New chunks of the current bulk transfer while the window has room, and
    /// unacked ones every EVENT_RESEND_EVERY, encoded.
    fn due_bulk(&mut self, now: Instant) -> Vec<Vec<u8>> {
        let Some(b) = self.bulk_out.front_mut() else { return Vec::new() };
        let count = b.chunks.len() as u16;
        let mut in_flight = (0..b.chunks.len()).filter(|&i| !b.acked[i] && b.sent_at[i].is_some()).count();
        let mut due = Vec::new();
        for i in 0..b.chunks.len() {
            if b.acked[i] {
                continue;
            }
            let send = match b.sent_at[i] {
                None if in_flight < BULK_WINDOW => {
                    in_flight += 1;
                    true
                }
                None => false,
                Some(t) => now.saturating_duration_since(t) >= EVENT_RESEND_EVERY,
            };
            if send {
                b.sent_at[i] = Some(now);
                let p = Packet::Bulk { xfer: b.xfer, kind: b.kind, idx: i as u16, count, data: b.chunks[i].clone() };
                due.push(encode_routed(&p, b.src, b.dst));
            }
        }
        due
    }

    /// One received chunk (the caller acks it); the whole transfer once it is complete.
    fn on_bulk_chunk(&mut self, xfer: u16, kind: u8, idx: u16, count: u16, data: Vec<u8>) -> Option<(u8, Vec<u8>)> {
        if self.bulk_done.contains(&xfer) {
            return None;
        }
        let fresh = self.bulk_in.as_ref().is_none_or(|b| b.xfer != xfer || b.chunks.len() != count as usize);
        if fresh {
            // A newer transfer replaces an unfinished one (the sender only
            // starts the next after the previous one was fully acked).
            self.bulk_in = Some(BulkIn { xfer, kind, chunks: vec![None; count as usize], got: 0 });
        }
        let b = self.bulk_in.as_mut()?;
        let slot = b.chunks.get_mut(idx as usize)?;
        if slot.is_none() {
            *slot = Some(data);
            b.got += 1;
        }
        if b.got < b.chunks.len() {
            return None;
        }
        let b = self.bulk_in.take()?;
        let data: Vec<u8> = b.chunks.into_iter().flatten().flatten().collect();
        if self.bulk_done.len() >= BULK_DONE_KEPT {
            self.bulk_done.pop_front();
        }
        self.bulk_done.push_back(b.xfer);
        Some((b.kind, data))
    }

    fn on_bulk_ack(&mut self, xfer: u16, idx: u16) {
        let Some(b) = self.bulk_out.front_mut() else { return };
        if b.xfer != xfer || idx as usize >= b.acked.len() {
            return;
        }
        b.acked[idx as usize] = true;
        if b.acked.iter().all(|&a| a) {
            self.bulk_out.pop_front();
        }
    }

    /// Bulk chunks not acknowledged yet (all queued transfers).
    fn pending_bulk_chunks(&self) -> usize {
        self.bulk_out.iter().map(|b| b.acked.iter().filter(|&&a| !a).count()).sum()
    }

    /// Queues a bulk transfer and sends what the window allows now.
    fn start_bulk(&mut self, sock: &dyn Datagram, src: u8, dst: u8, kind: u8, chunks: Vec<Vec<u8>>, now: Instant) {
        self.queue_bulk(src, dst, kind, chunks);
        for b in self.due_bulk(now) {
            send_on(sock, self, &b, now);
        }
    }

    /// `start_bulk` with the transfer queued ahead of those not started (`queue_bulk_first`).
    fn start_bulk_first(&mut self, sock: &dyn Datagram, src: u8, dst: u8, kind: u8, chunks: Vec<Vec<u8>>, now: Instant) {
        self.queue_bulk_first(src, dst, kind, chunks);
        for b in self.due_bulk(now) {
            send_on(sock, self, &b, now);
        }
    }
}

/// Sends one datagram on a link (its heartbeat timer counts it).
fn send_on(sock: &dyn Datagram, link: &mut Link, bytes: &[u8], now: Instant) {
    let _ = sock.send_to(bytes, link.addr);
    link.last_sent = now;
}

/// Which links a send goes to.
type Mask = [bool; MAX_PEERS];

pub struct Session {
    sock: Box<dyn Datagram + Send>,
    role: Role,
    password_hash: u64,
    name: String,
    /// Our uid (Steam: SteamID64; UDP: whatever the caller sets), sent in Hello.
    uid: u64,
    /// Our slot: host 0, client the one from Welcome (`NO_SLOT` before).
    me: u8,
    /// Host: clients allowed (1..=MAX_PEERS).
    max_peers: u8,
    /// Host: index = slot - 1. Client: [0] = the host link, the rest unused.
    links: [Option<Link>; MAX_PEERS],
    /// Client: when we last sent Hello.
    last_hello: Option<Instant>,
    created: Instant,
    // Outgoing sequence counters, shared by every link and never reset: the receiver only
    // needs them to grow (order checks, event de-duplication across a one-sided reset).
    next_seq: u32,
    next_event_seq: u32,
    next_raw_seq: u32,
    /// Host: one source's snapshots go on to the other clients at most this often.
    relay_every: Duration,
    /// Host: when each source's next relayed snapshot is due.
    relay_next: [Option<Instant>; SLOTS],
    /// Host: datagrams and bytes relayed (stats).
    relayed: (u64, u64),
    /// Host: the members (slot 0 = us), as last computed. Client: the host's last roster.
    roster: Vec<RosterEntry>,
    /// Host: generation of `roster`, bumped on every change.
    roster_gen: u32,
    /// Client: generation of `roster` (None until one arrives on this link).
    heard_gen: Option<u32>,
    /// Host: links every slot has had (roster `inc`).
    incarnations: [u8; SLOTS],
    /// Host: refused Hellos (reason, name) until `take_refusals`.
    refusals: Vec<(RejectReason, String)>,
    /// Client: when a host speaking another version was last reported.
    version_seen: Option<Instant>,
}

impl Session {
    pub fn new(bind: SocketAddr, role: Role, password: &str, name: &str) -> std::io::Result<Self> {
        let sock = UdpSocket::bind(bind)?;
        sock.set_nonblocking(true)?;
        Ok(Self::with_transport(Box::new(sock), role, password, name))
    }

    /// A session over any datagram transport (Steam relay, tests). The transport must be non-blocking.
    pub fn with_transport(sock: Box<dyn Datagram + Send>, role: Role, password: &str, name: &str) -> Self {
        let now = Instant::now();
        let mut s = Self {
            sock,
            role: Role::Host,
            password_hash: password_hash(password),
            name: name.to_owned(),
            uid: 0,
            me: 0,
            max_peers: 1,
            links: Default::default(),
            last_hello: None,
            created: now,
            next_seq: 0,
            next_event_seq: 0,
            next_raw_seq: 0,
            relay_every: Duration::from_micros(1_000_000 / RELAY_SNAPSHOT_HZ as u64),
            relay_next: [None; SLOTS],
            relayed: (0, 0),
            roster: Vec::new(),
            roster_gen: 0,
            heard_gen: None,
            incarnations: [0; SLOTS],
            refusals: Vec::new(),
            version_seen: None,
        };
        s.start_role(role, now);
        s
    }

    /// Fresh links for `role`: a client has its (Waiting) host link at once.
    fn start_role(&mut self, role: Role, now: Instant) {
        self.links = Default::default();
        self.me = match &role {
            Role::Client { host } => {
                self.links[0] = Some(Link::new(*host, 0, now));
                NO_SLOT
            }
            Role::Host => 0,
        };
        self.role = role;
        self.last_hello = None;
        self.relay_next = [None; SLOTS];
        self.roster.clear();
        self.heard_gen = None;
    }

    /// Switches the role of a session that has not met its peer yet (Steam: the lobby search
    /// decides after the transport exists). Forgets every link, like a restart.
    pub fn set_role(&mut self, role: Role) {
        self.start_role(role, Instant::now());
    }

    pub fn set_uid(&mut self, uid: u64) {
        self.uid = uid;
    }

    /// Host: how many clients may join (`max_players - 1`, clamped to 1..=MAX_PEERS). Lowering
    /// it keeps the clients already linked; only new ones are refused.
    pub fn set_max_peers(&mut self, n: u8) {
        self.max_peers = n.clamp(1, MAX_PEERS as u8);
    }

    /// Host: snapshots relayed per second per source (settings `relay_snapshot_hz`).
    pub fn set_relay_snapshot_hz(&mut self, hz: u32) {
        self.relay_every = Duration::from_micros(1_000_000 / hz.clamp(1, 1000) as u64);
    }

    /// Host: datagrams and bytes relayed between clients so far.
    pub fn relay_stats(&self) -> (u64, u64) {
        self.relayed
    }

    /// Host: the Hellos refused since the last call (reason, the name it gave; empty for a
    /// build of another version). A client repeats its Hello, so the same one comes again.
    pub fn take_refusals(&mut self) -> Vec<(RejectReason, String)> {
        std::mem::take(&mut self.refusals)
    }

    /// Our slot: host 0; client the one from Welcome (0xFF before the first Welcome).
    pub fn me(&self) -> u8 {
        self.me
    }

    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.sock.local_addr()
    }

    fn is_host(&self) -> bool {
        self.role == Role::Host
    }

    /// Client: the host link's state. Host: Connected if any link is, else Lost if a
    /// link was lost (still listening for it), else Waiting.
    pub fn state(&self) -> State {
        let mut lost = false;
        for l in self.links.iter().flatten() {
            match l.state {
                State::Connected => return State::Connected,
                State::Lost => lost = true,
                State::Waiting => {}
            }
        }
        if lost { State::Lost } else { State::Waiting }
    }

    /// The link state of a slot. Client: another client in the host's roster is reached
    /// through the host link, so it has that link's state.
    pub fn link_state(&self, slot: u8) -> Option<State> {
        if let Some(l) = self.link(slot) {
            return Some(l.state);
        }
        if self.relayed_peer(slot) { self.links[0].as_ref().map(|l| l.state) } else { None }
    }

    /// Connected slots with uid and name, in slot order. Client: the host, then the other
    /// clients of the host's roster.
    pub fn peers(&self) -> Vec<(u8, u64, String)> {
        let mut v: Vec<_> =
            self.links.iter().flatten().filter(|l| l.connected()).map(|l| (l.slot, l.uid, l.name.clone())).collect();
        if !self.is_host() && !v.is_empty() {
            v.extend(self.roster.iter().filter(|e| self.relayed_peer(e.slot)).map(|e| (e.slot, e.uid, e.name.clone())));
        }
        v
    }

    /// Smoothed round-trip time of the first link that is not lost; None until its first Pong (compat).
    pub fn rtt_ms(&self) -> Option<u32> {
        self.links.iter().flatten().find(|l| l.state != State::Lost).and_then(Link::rtt_ms)
    }

    /// The round-trip time of a slot's own link (None for a client's relayed peers).
    pub fn peer_rtt_ms(&self, slot: u8) -> Option<u32> {
        self.link(slot).and_then(Link::rtt_ms)
    }

    /// Client: `slot` is another client in the host's current roster.
    fn relayed_peer(&self, slot: u8) -> bool {
        !self.is_host() && slot != 0 && slot != self.me && self.roster.iter().any(|e| e.slot == slot)
    }

    /// Index into `links` of a slot's own link.
    fn index_of_slot(&self, slot: u8) -> Option<usize> {
        let i = match self.role {
            Role::Host => (slot as usize).checked_sub(1)?,
            Role::Client { .. } => (slot == 0).then_some(0)?,
        };
        (i < MAX_PEERS).then_some(i)
    }

    fn link(&self, slot: u8) -> Option<&Link> {
        self.links[self.index_of_slot(slot)?].as_ref()
    }

    /// The link that carries traffic for `slot` (client: the host link for every peer).
    fn route_link(&self, slot: u8) -> Option<&Link> {
        if self.relayed_peer(slot) { self.links[0].as_ref() } else { self.link(slot) }
    }

    /// Connected links a send to `dst` goes out on. Host: that client, or every client.
    /// Client: the host link, for the host, all, or a client of the roster.
    fn targets(&self, dst: u8) -> Mask {
        let mut m = [false; MAX_PEERS];
        let up = |i: usize| self.links[i].as_ref().is_some_and(Link::connected);
        if self.is_host() {
            if dst == DST_ALL {
                for (i, on) in m.iter_mut().enumerate() {
                    *on = up(i);
                }
            } else if let Some(i) = self.index_of_slot(dst) {
                m[i] = up(i);
            }
        } else if dst == DST_ALL || dst == 0 || self.relayed_peer(dst) {
            m[0] = up(0);
        }
        m
    }

    /// Host: the other links a packet from link `from` to `dst` goes on to.
    fn relay_targets(&self, from: usize, dst: u8) -> Mask {
        let mut m = self.targets(dst);
        m[from] = false;
        m
    }

    /// Sends the same datagram on every masked link; how many it went out on.
    fn send_bytes(&mut self, mask: &Mask, bytes: &[u8], now: Instant) -> usize {
        let mut n = 0;
        for (l, _) in self.links.iter_mut().zip(mask).filter(|(_, on)| **on) {
            if let Some(l) = l {
                let _ = self.sock.send_to(bytes, l.addr);
                l.last_sent = now;
                n += 1;
            }
        }
        n
    }

    /// Our own packet to `dst` on the masked links.
    fn send_mask(&mut self, mask: &Mask, p: &Packet, dst: u8, now: Instant) {
        let bytes = encode_routed(p, self.me, dst);
        self.send_bytes(mask, &bytes, now);
    }

    /// Host: someone else's packet on to the masked links (route kept).
    fn relay(&mut self, mask: &Mask, p: &Packet, src: u8, dst: u8, now: Instant) {
        let bytes = encode_routed(p, src, dst);
        let n = self.send_bytes(mask, &bytes, now) as u64;
        self.relayed.0 += n;
        self.relayed.1 += n * bytes.len() as u64;
    }

    /// Host: a relayed snapshot of `src` is due (`relay_every` apart on a steady stream).
    fn relay_due(&mut self, src: u8, now: Instant) -> bool {
        let every = self.relay_every;
        let Some(next) = self.relay_next.get_mut(src as usize) else { return false };
        if next.is_some_and(|t| now < t) {
            return false;
        }
        *next = Some(match *next {
            Some(t) if now.duration_since(t) < every => t + every,
            _ => now + every,
        });
        true
    }

    fn micros_since_created(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.created).as_micros() as u64
    }

    /// Call once per frame: sends Hello/heartbeats as due, drains the socket.
    pub fn poll(&mut self, now: Instant) -> Vec<Event> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => match decode_routed(&buf[..n]) {
                    Some((p, route)) => self.handle(p, route, from, now, &mut events),
                    None => self.on_foreign(&buf[..n], from, now, &mut events),
                },
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                // Windows reports ICMP "port unreachable" as ConnectionReset
                // when the peer isn't up yet; that's normal, keep going.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(_) => break,
            }
        }

        let mut lost_host = false;
        for l in self.links.iter_mut().flatten() {
            if l.connected() && now.duration_since(l.last_heard) > LOST_AFTER {
                l.state = State::Lost;
                l.reset();
                events.push(Event::Lost { slot: l.slot });
                events.push(Event::PeerLeft { slot: l.slot, why: LeaveWhy::Lost });
                lost_host |= l.slot == 0;
            }
        }
        if lost_host {
            self.forget_roster(LeaveWhy::Lost, &mut events);
        }

        if let Role::Client { host } = self.role
            && self.links[0].as_ref().is_some_and(|l| !l.connected())
            && self.last_hello.is_none_or(|t| now.duration_since(t) >= HELLO_EVERY)
        {
            self.last_hello = Some(now);
            let hello = encode(&Packet::Hello { password_hash: self.password_hash, uid: self.uid, name: self.name.clone() });
            if let Some(l) = self.links[0].as_mut() {
                l.addr = host;
                send_on(&*self.sock, l, &hello, now);
            }
        }

        let t = self.micros_since_created(now);
        let sock = &*self.sock;
        for l in self.links.iter_mut().flatten().filter(|l| l.connected()) {
            if now.duration_since(l.last_sent) >= HEARTBEAT_EVERY {
                send_on(sock, l, &encode(&Packet::Heartbeat), now);
            }
            if l.last_ping.is_none_or(|p| now.duration_since(p) >= PING_EVERY) {
                l.last_ping = Some(now);
                send_on(sock, l, &encode(&Packet::Ping { t }), now);
            }
            for p in l.due_events(now) {
                send_on(sock, l, &p, now);
            }
            for p in l.due_bulk(now) {
                send_on(sock, l, &p, now);
            }
        }
        self.sync_roster(now);
        events
    }

    fn handle(&mut self, p: Packet, route: Option<(u8, u8)>, from: SocketAddr, now: Instant, events: &mut Vec<Event>) {
        match (&self.role, p) {
            (Role::Host, Packet::Hello { password_hash, uid, name }) => {
                self.on_hello(password_hash, uid, name, from, now, events)
            }
            (Role::Client { host }, Packet::Welcome { name, slot }) if *host == from => {
                self.on_welcome(name, slot, from, now, events)
            }
            (Role::Client { host }, Packet::Reject { reason }) if *host == from => {
                events.push(Event::Rejected { reason: RejectReason::from_wire(reason) })
            }
            (_, p) => {
                let Some(i) = self.links.iter().position(|l| l.as_ref().is_some_and(|l| l.addr == from && l.connected()))
                else {
                    return;
                };
                self.on_link_packet(i, p, route, now, events);
            }
        }
    }

    /// A datagram we cannot decode. One of another co-op version gets an answer it can read
    /// (host: Reject(Version) to its Hello) or is reported (client: our host speaks another).
    fn on_foreign(&mut self, buf: &[u8], from: SocketAddr, now: Instant, events: &mut Vec<Event>) {
        let Some((version, kind)) = peek_header(buf) else { return };
        if version == VERSION {
            return; // ours, but malformed
        }
        match &self.role {
            Role::Host if kind == K_HELLO => {
                let _ = self.sock.send_to(&version_reject(version), from);
                self.note_refusal(RejectReason::Version, String::new());
            }
            Role::Client { host } if *host == from => {
                if self.version_seen.is_none_or(|t| now.duration_since(t) >= HELLO_EVERY) {
                    self.version_seen = Some(now);
                    events.push(Event::Rejected { reason: RejectReason::Version });
                }
            }
            _ => {}
        }
    }

    fn note_refusal(&mut self, reason: RejectReason, name: String) {
        if self.refusals.len() < MAX_REFUSALS {
            self.refusals.push((reason, name));
        }
    }

    fn reject(&mut self, reason: RejectReason, name: String, to: SocketAddr) {
        let _ = self.sock.send_to(&encode(&Packet::Reject { reason: reason as u8 }), to);
        self.note_refusal(reason, name);
    }

    /// Host: a slot for a Hello from an address and uid no link has. The lowest free slot
    /// under the cap, then a link that is not connected. With one client and no uid there is
    /// no telling a restart from a stranger: the newcomer replaces the link, as in v9.
    /// None = full.
    fn slot_for_newcomer(&self, uid: u64) -> Option<usize> {
        let cap = (self.max_peers as usize).clamp(1, MAX_PEERS);
        (0..cap)
            .find(|&i| self.links[i].is_none())
            .or_else(|| (0..cap).find(|&i| self.links[i].as_ref().is_some_and(|l| !l.connected())))
            .or((cap == 1 && uid == 0).then_some(0))
    }

    #[allow(clippy::too_many_arguments)]
    fn on_hello(&mut self, hash: u64, uid: u64, name: String, from: SocketAddr, now: Instant, events: &mut Vec<Event>) {
        if hash != self.password_hash {
            self.reject(RejectReason::Password, name, from);
            return;
        }
        // A client only says Hello while it is NOT connected, so a Hello from a linked peer
        // means it dropped the link while we did not (e.g. our game froze > LOST_AFTER on a
        // loading screen while its packets queued up). Restart the link here too: otherwise
        // only the client started a new epoch, and the two sides' damage totals, join state
        // and event counters no longer matched.
        let known = self.links.iter().position(|l| l.as_ref().is_some_and(|l| l.addr == from));
        // The same player at a new address (its game restarted): its old slot.
        let by_uid = (uid != 0).then(|| self.links.iter().position(|l| l.as_ref().is_some_and(|l| l.uid == uid))).flatten();
        let Some(i) = known.or(by_uid).or_else(|| self.slot_for_newcomer(uid)) else {
            self.reject(RejectReason::Full, name, from);
            return;
        };
        let slot = i as u8 + 1;
        if known.is_none() {
            if self.links[i].take().is_some_and(|old| old.connected()) {
                events.push(Event::PeerLeft { slot, why: LeaveWhy::Replaced });
            }
            self.links[i] = Some(Link::new(from, slot, now));
        }
        let welcome = encode(&Packet::Welcome { name: self.name.clone(), slot });
        let Some(l) = self.links[i].as_mut() else { return };
        let rejoin = l.last_welcome.is_none_or(|t| now.saturating_duration_since(t) > REJOIN_HELLO_AFTER);
        let fresh = !l.connected() || rejoin;
        l.last_welcome = Some(now);
        l.last_heard = now;
        send_on(&*self.sock, l, &welcome, now);
        if fresh {
            l.state = State::Connected;
            l.reset();
            l.name = name.clone();
            if uid != 0 {
                l.uid = uid;
            }
            self.incarnations[slot as usize] = self.incarnations[slot as usize].wrapping_add(1);
            l.inc = self.incarnations[slot as usize];
            self.relay_next[slot as usize] = None;
            events.push(Event::Connected { peer: from, name: name.clone(), slot });
            events.push(Event::PeerJoined { slot, uid: l.uid, name });
        }
    }

    fn on_welcome(&mut self, name: String, slot: u8, from: SocketAddr, now: Instant, events: &mut Vec<Event>) {
        if !(1..=MAX_PEERS as u8).contains(&slot) {
            return;
        }
        let Some(l) = self.links[0].as_mut() else { return };
        l.last_heard = now;
        if !l.connected() {
            l.state = State::Connected;
            l.reset();
            l.name = name.clone();
            self.me = slot;
            self.roster.clear();
            self.heard_gen = None;
            events.push(Event::Connected { peer: from, name: name.clone(), slot });
            events.push(Event::PeerJoined { slot: 0, uid: l.uid, name });
        }
    }

    /// A packet from the connected link `i`.
    fn on_link_packet(&mut self, i: usize, p: Packet, route: Option<(u8, u8)>, now: Instant, events: &mut Vec<Event>) {
        let host = self.is_host();
        let me = self.me;
        let t_now = self.micros_since_created(now);
        let Some(l) = self.links[i].as_mut() else { return };
        l.last_heard = now;
        let link_slot = l.slot;
        // Who made it and who it is for. The host takes a client's word only for itself.
        let (src, dst) = match route {
            None => (link_slot, me),
            Some((_, dst)) if host => (link_slot, dst),
            Some(r) => r,
        };
        if route.is_some() && (src as usize >= SLOTS || src == me) {
            return;
        }
        let local = route.is_none() || dst == DST_ALL || dst == me;
        let relay = if host && route.is_some() { self.relay_targets(i, dst) } else { [false; MAX_PEERS] };
        let relaying = relay.contains(&true);
        let sock = &*self.sock;
        let Some(l) = self.links[i].as_mut() else { return };
        match p {
            Packet::Snapshot(s) => {
                if !self.links[i].as_mut().is_some_and(|l| l.accept_snapshot(src, s.seq)) {
                    return;
                }
                if relaying && self.relay_due(src, now) {
                    self.relay(&relay, &Packet::Snapshot(s), src, dst, now);
                }
                if local {
                    events.push(Event::Snapshot { from: src, snap: s });
                }
            }
            p @ (Packet::Enemies(_)
            | Packet::Hkb(_)
            | Packet::Damage(_)
            | Packet::Posture(_)
            | Packet::NetAi(_)
            | Packet::PlayerRaw { .. }) => {
                if relaying {
                    self.relay(&relay, &p, src, dst, now);
                }
                if !local {
                    return;
                }
                match p {
                    Packet::Enemies(list) => events.push(Event::Enemies { from: src, list }),
                    Packet::Hkb(list) => events.push(Event::Hkb { from: src, list }),
                    Packet::Damage(report) => events.push(Event::Damage { from: src, report }),
                    Packet::Posture(list) => events.push(Event::Posture { from: src, list }),
                    Packet::NetAi(list) => events.push(Event::NetAi { from: src, list }),
                    Packet::PlayerRaw { seq, ptype, data } => {
                        if self.links[i].as_mut().is_some_and(|l| l.accept_raw(src, ptype, seq)) {
                            events.push(Event::PlayerRaw { from: src, ptype, data });
                        }
                    }
                    _ => {}
                }
            }
            Packet::Event { seq, kind, a, b } => {
                // Acked on this hop (always: our earlier ack may have been lost).
                send_on(sock, l, &encode(&Packet::Ack { seq }), now);
                if !l.accept_event(seq) {
                    return;
                }
                if local {
                    events.push(Event::Game { from: src, kind, a, b });
                }
                if relaying {
                    // A new event on every target link, under our own seq; reliable per hop.
                    let seq = self.next_event_seq;
                    self.next_event_seq = self.next_event_seq.wrapping_add(1);
                    for (l, _) in self.links.iter_mut().zip(&relay).filter(|(_, on)| **on) {
                        if let Some(l) = l {
                            l.queue_event(seq, src, dst, kind, a, b, now);
                        }
                    }
                    self.relay(&relay, &Packet::Event { seq, kind, a, b }, src, dst, now);
                }
            }
            Packet::Bulk { xfer, kind, idx, count, data } => {
                // Always ack: our earlier ack may have been lost.
                send_on(sock, l, &encode(&Packet::BulkAck { xfer, idx }), now);
                let Some((kind, data)) = l.on_bulk_chunk(xfer, kind, idx, count, data) else { return };
                if kind == ROSTER_KIND {
                    // Only the host's own roster means anything: never relayed or delivered.
                    if !host && src == 0 {
                        self.on_roster(&data, events);
                    }
                    return;
                }
                if relaying {
                    // Stored until complete, then a new transfer on every target link.
                    let chunks = bulk_chunks(&data);
                    let sock = &*self.sock;
                    for (l, _) in self.links.iter_mut().zip(&relay).filter(|(_, on)| **on) {
                        if let Some(l) = l {
                            self.relayed.0 += chunks.len() as u64;
                            self.relayed.1 += data.len() as u64;
                            l.start_bulk(sock, src, dst, kind, chunks.clone(), now);
                        }
                    }
                }
                if local {
                    events.push(Event::Bulk { from: src, kind, data });
                }
            }
            Packet::Ping { t } => send_on(sock, l, &encode(&Packet::Pong { t }), now),
            Packet::Pong { t } => l.on_pong(t_now.saturating_sub(t) as f64),
            Packet::Ack { seq } => l.on_ack(seq),
            Packet::BulkAck { xfer, idx } => l.on_bulk_ack(xfer, idx),
            Packet::Bye => {
                l.state = State::Lost;
                l.reset();
                events.push(Event::PeerQuit { slot: link_slot });
                events.push(Event::PeerLeft { slot: link_slot, why: LeaveWhy::Quit });
                if !host {
                    self.forget_roster(LeaveWhy::Quit, events);
                }
            }
            _ => {}
        }
    }

    /// Host: the members now; a roster bulk to every client that does not have it yet and
    /// either is told about another client now or was told about one before.
    fn sync_roster(&mut self, now: Instant) {
        if !self.is_host() {
            return;
        }
        let mut list = vec![RosterEntry { slot: 0, uid: self.uid, inc: 0, name: self.name.clone() }];
        list.extend(
            self.links
                .iter()
                .flatten()
                .filter(|l| l.connected())
                .map(|l| RosterEntry { slot: l.slot, uid: l.uid, inc: l.inc, name: l.name.clone() }),
        );
        if list != self.roster {
            self.roster = list;
            self.roster_gen = self.roster_gen.wrapping_add(1);
        }
        let generation = self.roster_gen;
        let others = self.roster.len() > 2;
        let roster = &self.roster;
        let mut chunks: Option<Vec<Vec<u8>>> = None;
        let sock = &*self.sock;
        for l in self.links.iter_mut().flatten().filter(|l| l.connected()) {
            if l.roster_gen == Some(generation) {
                continue;
            }
            l.roster_gen = Some(generation);
            // Host and this client only: nothing it could learn (the 2-player case).
            if !others && !l.roster_told_others {
                continue;
            }
            l.roster_told_others = others;
            let c = chunks.get_or_insert_with(|| bulk_chunks(&encode_roster(generation, roster))).clone();
            l.start_bulk_first(sock, 0, l.slot, ROSTER_KIND, c, now);
        }
    }

    /// Client: a roster from the host. The other clients it adds or drops become events; a
    /// changed entry (another uid, name or link) is the old one leaving and a new one joining.
    fn on_roster(&mut self, data: &[u8], events: &mut Vec<Event>) {
        let Some((generation, list)) = decode_roster(data) else { return };
        if self.heard_gen.is_some_and(|g| !newer(generation, g)) {
            return;
        }
        self.heard_gen = Some(generation);
        let me = self.me;
        let others = |l: &[RosterEntry]| -> Vec<RosterEntry> {
            l.iter().filter(|e| e.slot != 0 && e.slot != me).cloned().collect()
        };
        let (old, new) = (others(&self.roster), others(&list));
        for o in &old {
            let why = match new.iter().find(|n| n.slot == o.slot) {
                None => LeaveWhy::Lost,
                Some(n) if n != o => LeaveWhy::Replaced,
                Some(_) => continue,
            };
            events.push(Event::PeerLeft { slot: o.slot, why });
            if let Some(l) = self.links[0].as_mut() {
                l.forget_source(o.slot);
            }
        }
        for n in new.iter().filter(|n| !old.contains(n)) {
            events.push(Event::PeerJoined { slot: n.slot, uid: n.uid, name: n.name.clone() });
        }
        // A UDP host's uid is only known from here.
        if let (Some(h), Some(l)) = (list.iter().find(|e| e.slot == 0), self.links[0].as_mut())
            && l.uid == 0
        {
            l.uid = h.uid;
        }
        events.push(Event::Roster(list.iter().map(|e| (e.slot, e.uid, e.name.clone())).collect()));
        self.roster = list;
    }

    /// Client: the host link ended, so every other client is gone too.
    fn forget_roster(&mut self, why: LeaveWhy, events: &mut Vec<Event>) {
        let me = self.me;
        for e in self.roster.drain(..) {
            if e.slot != 0 && e.slot != me {
                events.push(Event::PeerLeft { slot: e.slot, why });
            }
        }
        self.heard_gen = None;
    }

    /// Sends our snapshot to every peer (sequence number is filled in here).
    pub fn send_snapshot(&mut self, mut s: Snapshot, now: Instant) {
        let mask = self.targets(DST_ALL);
        if !mask.contains(&true) {
            return;
        }
        s.seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.send_mask(&mask, &Packet::Snapshot(s), DST_ALL, now);
    }

    /// Sends enemy states, split into packets of at most `MAX_ENEMIES_PER_PACKET`.
    pub fn send_enemies_to(&mut self, dst: u8, states: &[EnemyState], now: Instant) {
        let mask = self.targets(dst);
        if !mask.contains(&true) {
            return;
        }
        for chunk in states.chunks(MAX_ENEMIES_PER_PACKET) {
            self.send_mask(&mask, &Packet::Enemies(chunk.to_vec()), dst, now);
        }
    }

    /// Sends recent behavior events (one packet; the caller repeats them).
    pub fn send_hkb_to(&mut self, dst: u8, list: &[crate::protocol::HkbEvent], now: Instant) {
        let mask = self.targets(dst);
        if !mask.contains(&true) || list.is_empty() {
            return;
        }
        self.send_mask(&mask, &Packet::Hkb(list.to_vec()), dst, now);
    }

    /// Sends the cumulative damage dealt to one enemy (host applies the increase).
    pub fn send_damage_to(&mut self, dst: u8, report: DamageReport, now: Instant) {
        let mask = self.targets(dst);
        if mask.contains(&true) {
            self.send_mask(&mask, &Packet::Damage(report), dst, now);
        }
    }

    /// Sends host posture states, split into packets of at most `MAX_POSTURE_PER_PACKET`.
    pub fn send_posture_to(&mut self, dst: u8, states: &[PostureState], now: Instant) {
        let mask = self.targets(dst);
        if !mask.contains(&true) {
            return;
        }
        for chunk in states.chunks(MAX_POSTURE_PER_PACKET) {
            self.send_mask(&mask, &Packet::Posture(chunk.to_vec()), dst, now);
        }
    }

    /// Queues a reliable game event on each target link and sends it now; it is re-sent
    /// from `poll` until acked. Dropped silently when no target is connected.
    pub fn send_event_to(&mut self, dst: u8, kind: u8, a: i32, b: i32, now: Instant) {
        let mask = self.targets(dst);
        if !mask.contains(&true) {
            return;
        }
        let seq = self.next_event_seq;
        self.next_event_seq = self.next_event_seq.wrapping_add(1);
        let me = self.me;
        for (l, _) in self.links.iter_mut().zip(&mask).filter(|(_, on)| **on) {
            if let Some(l) = l {
                l.queue_event(seq, me, dst, kind, a, b, now);
            }
        }
        self.send_mask(&mask, &Packet::Event { seq, kind, a, b }, dst, now);
    }

    /// Sends NetAI entries in as few datagrams as fit `MAX_DATAGRAM`.
    /// Returns the bytes handed to the socket (for bandwidth stats).
    pub fn send_netai_to(&mut self, dst: u8, list: &[NetAiEntry], now: Instant) -> usize {
        let mask = self.targets(dst);
        let links = mask.iter().filter(|&&on| on).count();
        if links == 0 || list.is_empty() {
            return 0;
        }
        let mut sent = 0;
        let mut start = 0;
        while start < list.len() {
            let mut size = NETAI_PACKET_HEADER;
            let mut end = start;
            while end < list.len() && end - start < MAX_NETAI_PER_PACKET {
                let len = list[end].wire_len();
                // Always at least one entry (the largest one fits, see tests).
                if end > start && size + len > MAX_DATAGRAM {
                    break;
                }
                size += len;
                end += 1;
            }
            self.send_mask(&mask, &Packet::NetAi(list[start..end].to_vec()), dst, now);
            sent += size * links;
            start = end;
        }
        sent
    }

    /// Queues a reliable bulk transfer of `kind` on each target link (delivered complete
    /// and once, after any transfer queued before it on that link). False if too large, no
    /// target, or the session's own roster kind.
    pub fn send_bulk_to(&mut self, dst: u8, kind: u8, data: &[u8], now: Instant) -> bool {
        let mask = self.targets(dst);
        if !mask.contains(&true) || data.len() > MAX_BULK_BYTES || kind == ROSTER_KIND {
            return false;
        }
        let chunks = bulk_chunks(data);
        debug_assert!(chunks.len() <= MAX_BULK_CHUNKS);
        let (sock, me) = (&*self.sock, self.me);
        for (l, _) in self.links.iter_mut().zip(&mask).filter(|(_, on)| **on) {
            if let Some(l) = l {
                l.start_bulk(sock, me, dst, kind, chunks.clone(), now);
            }
        }
        true
    }

    /// Sends one raw game player packet (unreliable). False when no target is
    /// connected or larger than `MAX_PLAYER_RAW`.
    pub fn send_player_raw_to(&mut self, dst: u8, ptype: u32, data: &[u8], now: Instant) -> bool {
        let mask = self.targets(dst);
        if !mask.contains(&true) || data.len() > MAX_PLAYER_RAW {
            return false;
        }
        let seq = self.next_raw_seq;
        self.next_raw_seq = self.next_raw_seq.wrapping_add(1);
        self.send_mask(&mask, &Packet::PlayerRaw { seq, ptype, data: data.to_vec() }, dst, now);
        true
    }

    // Every peer (the frozen API's `send_X` = `send_X_to(DST_ALL)`).
    pub fn send_enemies(&mut self, states: &[EnemyState], now: Instant) {
        self.send_enemies_to(DST_ALL, states, now)
    }
    pub fn send_hkb(&mut self, list: &[crate::protocol::HkbEvent], now: Instant) {
        self.send_hkb_to(DST_ALL, list, now)
    }
    pub fn send_damage(&mut self, report: DamageReport, now: Instant) {
        self.send_damage_to(DST_ALL, report, now)
    }
    pub fn send_posture(&mut self, states: &[PostureState], now: Instant) {
        self.send_posture_to(DST_ALL, states, now)
    }
    pub fn send_event(&mut self, kind: u8, a: i32, b: i32, now: Instant) {
        self.send_event_to(DST_ALL, kind, a, b, now)
    }
    pub fn send_netai(&mut self, list: &[NetAiEntry], now: Instant) -> usize {
        self.send_netai_to(DST_ALL, list, now)
    }
    pub fn send_bulk(&mut self, kind: u8, data: &[u8], now: Instant) -> bool {
        self.send_bulk_to(DST_ALL, kind, data, now)
    }
    pub fn send_player_raw(&mut self, ptype: u32, data: &[u8], now: Instant) -> bool {
        self.send_player_raw_to(DST_ALL, ptype, data, now)
    }

    /// Reliable events on the link to `slot` not acknowledged yet (client: the host link,
    /// which carries the events to every peer).
    pub fn pending_events_to(&self, slot: u8) -> usize {
        self.route_link(slot).map_or(0, |l| l.pending.len())
    }

    /// Bulk chunks on the link to `slot` not acknowledged yet (all queued transfers; client:
    /// the host link's).
    pub fn pending_bulk_chunks_to(&self, slot: u8) -> usize {
        self.route_link(slot).map_or(0, Link::pending_bulk_chunks)
    }

    /// Reliable events not acknowledged yet, the most of any link (compat).
    pub fn pending_events(&self) -> usize {
        self.links.iter().flatten().map(|l| l.pending.len()).max().unwrap_or(0)
    }

    /// Bulk chunks not acknowledged yet, the most of any link (compat).
    pub fn pending_bulk_chunks(&self) -> usize {
        self.links.iter().flatten().map(Link::pending_bulk_chunks).max().unwrap_or(0)
    }

    /// Bye on every connected link (host: every client; client: the host).
    pub fn quit(&mut self, now: Instant) {
        let mask = self.targets(DST_ALL);
        self.send_bytes(&mask, &encode(&Packet::Bye), now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::decode;

    fn pair(pw_host: &str, pw_client: &str) -> (Session, Session) {
        let host = Session::new("127.0.0.1:0".parse().unwrap(), Role::Host, pw_host, "host").unwrap();
        let addr = host.local_addr().unwrap();
        let client =
            Session::new("127.0.0.1:0".parse().unwrap(), Role::Client { host: addr }, pw_client, "client").unwrap();
        (host, client)
    }

    /// Polls both sides a few times with a simulated clock; collects events.
    fn pump(h: &mut Session, c: &mut Session, now: &mut Instant, rounds: u32) -> (Vec<Event>, Vec<Event>) {
        let (mut he, mut ce) = (Vec::new(), Vec::new());
        for _ in 0..rounds {
            ce.extend(c.poll(*now));
            std::thread::sleep(Duration::from_millis(2));
            he.extend(h.poll(*now));
            std::thread::sleep(Duration::from_millis(2));
            ce.extend(c.poll(*now));
            *now += Duration::from_millis(100);
        }
        (he, ce)
    }

    fn snap(x: f32) -> Snapshot {
        Snapshot { pos: [x, 0.0, 0.0], ..Default::default() }
    }

    /// Unacked events of the first link.
    fn pending(s: &Session) -> &VecDeque<PendingEvent> {
        static EMPTY: VecDeque<PendingEvent> = VecDeque::new();
        s.links.iter().flatten().next().map_or(&EMPTY, |l| &l.pending)
    }

    #[test]
    fn connects_and_exchanges_snapshots_both_ways() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        let (he, ce) = pump(&mut h, &mut c, &mut now, 3);
        assert!(he.iter().any(|e| matches!(e, Event::Connected { name, slot: 1, .. } if name == "client")));
        assert!(ce.iter().any(|e| matches!(e, Event::Connected { name, slot: 1, .. } if name == "host")));

        h.send_snapshot(snap(1.0), now);
        c.send_snapshot(snap(2.0), now);
        let (he, ce) = pump(&mut h, &mut c, &mut now, 2);
        assert!(he.contains(&Event::Snapshot { from: 1, snap: Snapshot { seq: 0, ..snap(2.0) } }));
        assert!(ce.contains(&Event::Snapshot { from: 0, snap: Snapshot { seq: 0, ..snap(1.0) } }));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let (mut h, mut c) = pair("susi", "kettu");
        let mut now = Instant::now();
        let (he, ce) = pump(&mut h, &mut c, &mut now, 3);
        assert!(he.is_empty());
        assert!(ce.contains(&Event::Rejected { reason: RejectReason::Password }));
        assert_eq!(c.state(), State::Waiting);
        assert_eq!(c.me(), NO_SLOT);
    }

    #[test]
    fn heartbeats_keep_session_alive_through_a_long_loading_screen() {
        // No snapshots for 20 s (loading, death screen): still connected.
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let (he, ce) = pump(&mut h, &mut c, &mut now, 200);
        let lost = |ev: &[Event]| ev.iter().any(|e| matches!(e, Event::Lost { .. }));
        assert!(!lost(&he) && !lost(&ce));
        assert_eq!((h.state(), c.state()), (State::Connected, State::Connected));
    }

    #[test]
    fn silent_peer_is_lost_then_client_reconnects_without_invite() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);

        // Host freezes (e.g. game crash) for longer than LOST_AFTER.
        let mut ce = Vec::new();
        for _ in 0..60 {
            ce.extend(c.poll(now));
            now += Duration::from_millis(100);
        }
        assert!(ce.contains(&Event::Lost { slot: 0 }));
        assert!(ce.contains(&Event::PeerLeft { slot: 0, why: LeaveWhy::Lost }));
        assert_eq!(c.state(), State::Lost);

        // Host comes back: client's periodic Hello re-establishes the session.
        let (_, ce) = pump(&mut h, &mut c, &mut now, 10);
        assert!(ce.iter().any(|e| matches!(e, Event::Connected { .. })));
        assert_eq!(c.state(), State::Connected);
    }

    #[test]
    fn client_side_loss_restarts_the_link_on_both_sides() {
        // The host's game freezes (long load): the client hears nothing for
        // > LOST_AFTER and rejoins, while the host still has all the client's
        // packets queued and never noticed a gap. Both must see Connected
        // (new epoch), exactly once.
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let mut ce = Vec::new();
        for _ in 0..60 {
            ce.extend(c.poll(now));
            now += Duration::from_millis(100);
        }
        assert!(ce.contains(&Event::Lost { slot: 0 }));
        std::thread::sleep(Duration::from_millis(5));
        let (he, ce) = pump(&mut h, &mut c, &mut now, 5);
        let connected = |ev: &[Event]| ev.iter().filter(|e| matches!(e, Event::Connected { .. })).count();
        assert_eq!(connected(&he), 1, "{he:?}");
        assert_eq!(connected(&ce), 1, "{ce:?}");
        // The same address rejoining is the same slot, not a replacement.
        assert!(!he.iter().any(|e| matches!(e, Event::PeerLeft { .. })), "{he:?}");
        assert_eq!((h.state(), c.state()), (State::Connected, State::Connected));
    }

    #[test]
    fn repeated_hello_during_one_join_is_one_connection() {
        let (mut h, c) = pair("susi", "susi");
        let now = Instant::now();
        let hello = encode(&Packet::Hello { password_hash: password_hash("susi"), uid: 0, name: "client".into() });
        let addr = h.local_addr().unwrap();
        c.sock.send_to(&hello, addr).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let first = h.poll(now);
        // Welcome lost: the client re-sends Hello 500 ms later.
        c.sock.send_to(&hello, addr).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let second = h.poll(now + HELLO_EVERY);
        assert!(first.iter().any(|e| matches!(e, Event::Connected { .. })));
        assert!(!second.iter().any(|e| matches!(e, Event::Connected { .. })), "{second:?}");
    }

    #[test]
    fn host_restart_on_new_port_is_reaccepted() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        // Client restarts its game: new socket, new port, same host.
        let addr = h.local_addr().unwrap();
        let mut c2 = Session::new("127.0.0.1:0".parse().unwrap(), Role::Client { host: addr }, "susi", "client").unwrap();
        let (he, ce) = pump(&mut h, &mut c2, &mut now, 3);
        assert!(he.iter().any(|e| matches!(e, Event::Connected { slot: 1, .. })));
        assert!(he.contains(&Event::PeerLeft { slot: 1, why: LeaveWhy::Replaced }));
        assert!(ce.iter().any(|e| matches!(e, Event::Connected { .. })));
        assert_eq!(h.peers().len(), 1);
    }

    #[test]
    fn old_snapshots_are_dropped() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let addr = c.local_addr().unwrap();
        // Hand-craft out-of-order seqs 5, 3, 6 from host to client.
        for seq in [5u32, 3, 6] {
            let p = Packet::Snapshot(Snapshot { seq, ..snap(seq as f32) });
            h.sock.send_to(&encode(&p), addr).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let seqs: Vec<u32> = c
            .poll(now)
            .into_iter()
            .filter_map(|e| if let Event::Snapshot { snap, .. } = e { Some(snap.seq) } else { None })
            .collect();
        assert_eq!(seqs, vec![5, 6]);
    }

    #[test]
    fn bye_ends_session_immediately() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        h.quit(now);
        std::thread::sleep(Duration::from_millis(5));
        let ev = c.poll(now);
        assert!(ev.contains(&Event::PeerQuit { slot: 0 }));
        assert!(ev.contains(&Event::PeerLeft { slot: 0, why: LeaveWhy::Quit }));
        assert_eq!(c.state(), State::Lost);
    }

    #[test]
    fn enemies_are_split_across_packets_and_all_arrive() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let states: Vec<EnemyState> = (0..100).map(|i| EnemyState { id: i, hp: 10 * i, ..Default::default() }).collect();
        h.send_enemies(&states, now);
        let (_, ce) = pump(&mut h, &mut c, &mut now, 2);
        let got: Vec<EnemyState> = ce
            .into_iter()
            .filter_map(|e| if let Event::Enemies { list, .. } = e { Some(list) } else { None })
            .flatten()
            .collect();
        assert_eq!(got, states);
    }

    #[test]
    fn damage_reaches_host_and_is_not_sent_when_not_connected() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        let r = |id, hp| DamageReport { id, hp, posture: 3 * hp, deathblows: 1 };
        c.send_damage(r(1, 5), now); // not connected yet: dropped
        pump(&mut h, &mut c, &mut now, 3);
        c.send_damage(r(9, 250), now);
        let (he, _) = pump(&mut h, &mut c, &mut now, 2);
        let dmg: Vec<_> = he.iter().filter(|e| matches!(e, Event::Damage { .. })).collect();
        assert_eq!(dmg, vec![&Event::Damage { from: 1, report: r(9, 250) }]);
    }

    #[test]
    fn posture_states_are_split_across_packets_and_all_arrive() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        h.send_posture(&[PostureState { id: 1, posture: 1, max: 2, lives: 0 }], now); // not connected: dropped
        pump(&mut h, &mut c, &mut now, 3);
        let states: Vec<PostureState> =
            (0..150).map(|i| PostureState { id: i, posture: 7 * i, max: 1000, lives: (i % 3) as i8 }).collect();
        h.send_posture(&states, now);
        let (_, ce) = pump(&mut h, &mut c, &mut now, 2);
        let got: Vec<PostureState> = ce
            .into_iter()
            .filter_map(|e| if let Event::Posture { list, .. } = e { Some(list) } else { None })
            .flatten()
            .collect();
        assert_eq!(got, states);
    }

    #[test]
    fn enemy_and_damage_packets_from_strangers_are_ignored() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        stranger.send_to(&encode(&Packet::Damage(DamageReport { id: 1, hp: 1, ..Default::default() })), h.local_addr().unwrap()).unwrap();
        stranger.send_to(&encode(&Packet::Posture(vec![PostureState::default()])), c.local_addr().unwrap()).unwrap();
        stranger.send_to(&encode(&Packet::Enemies(vec![EnemyState::default()])), c.local_addr().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(h.poll(now).is_empty() && c.poll(now).is_empty());
    }

    #[test]
    fn old_protocol_version_datagram_is_ignored() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let mut old = encode(&Packet::Damage(DamageReport { id: 1, hp: 1, ..Default::default() }));
        old[4..6].copy_from_slice(&1u16.to_le_bytes());
        c.sock.send_to(&old, h.local_addr().unwrap()).unwrap();
        // The previous protocol (8) is refused the same way.
        old[4..6].copy_from_slice(&8u16.to_le_bytes());
        c.sock.send_to(&old, h.local_addr().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(h.poll(now).is_empty());
    }

    fn drain(s: &Session) {
        let mut buf = [0u8; 2048];
        while s.sock.recv_from(&mut buf).is_ok() {}
    }

    fn game_events(ev: &[Event]) -> Vec<(u8, i32, i32)> {
        ev.iter().filter_map(|e| if let Event::Game { kind, a, b, .. } = e { Some((*kind, *a, *b)) } else { None }).collect()
    }

    #[test]
    fn rtt_becomes_known_after_ping_exchange() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        assert_eq!(c.rtt_ms(), None);
        pump(&mut h, &mut c, &mut now, 3);
        pump(&mut h, &mut c, &mut now, 3);
        assert!(c.rtt_ms().is_some());
        assert!(h.rtt_ms().is_some());
        assert_eq!(h.peer_rtt_ms(1), h.rtt_ms());
        assert_eq!(c.peer_rtt_ms(0), c.rtt_ms());
        assert_eq!(h.peer_rtt_ms(2), None);
    }

    #[test]
    fn ping_gets_pong_with_same_t() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        drain(&c);
        c.sock.send_to(&encode(&Packet::Ping { t: 777 }), h.local_addr().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        h.poll(now);
        std::thread::sleep(Duration::from_millis(5));
        let mut buf = [0u8; 2048];
        let mut got = false;
        while let Ok((n, _)) = c.sock.recv_from(&mut buf) {
            got |= decode(&buf[..n]) == Some(Packet::Pong { t: 777 });
        }
        assert!(got);
    }

    #[test]
    fn event_is_delivered_once_and_acked() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        h.send_event(4, -1, 99, now);
        let (_, ce) = pump(&mut h, &mut c, &mut now, 10);
        assert_eq!(game_events(&ce), vec![(4, -1, 99)]);
        assert!(ce.contains(&Event::Game { from: 0, kind: 4, a: -1, b: 99 }));
        assert!(pending(&h).is_empty());
    }

    #[test]
    fn event_survives_three_dropped_transmissions() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        drain(&c);
        h.send_event(2, 10, 20, now); // transmission 1
        std::thread::sleep(Duration::from_millis(5));
        drain(&c);
        for _ in 0..2 {
            now += EVENT_RESEND_EVERY;
            h.poll(now); // transmissions 2 and 3
            std::thread::sleep(Duration::from_millis(5));
            drain(&c);
        }
        assert_eq!(pending(&h).len(), 1);
        assert_eq!(h.pending_events_to(1), 1);
        now += EVENT_RESEND_EVERY;
        let (_, ce) = pump(&mut h, &mut c, &mut now, 10);
        assert_eq!(game_events(&ce), vec![(2, 10, 20)]);
        assert!(pending(&h).is_empty());
    }

    #[test]
    fn duplicate_event_packets_are_delivered_once() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let addr = c.local_addr().unwrap();
        for seq in [5u32, 5, 5, u32::MAX, 0, u32::MAX, 0] {
            h.sock.send_to(&encode(&Packet::Event { seq, kind: 1, a: seq as i32, b: 0 }), addr).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let ev = c.poll(now);
        assert_eq!(game_events(&ev).len(), 3);
    }

    #[test]
    fn events_not_sent_when_disconnected_and_pending_is_capped() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        c.send_event(1, 0, 0, now);
        assert!(pending(&c).is_empty());
        pump(&mut h, &mut c, &mut now, 3);
        for i in 0..70 {
            c.send_event(1, i, 0, now);
        }
        assert_eq!(pending(&c).len(), MAX_PENDING_EVENTS);
        assert_eq!(pending(&c).front().unwrap().a, 6);
        assert_eq!(c.pending_events(), MAX_PENDING_EVENTS);
    }

    fn bulks(ev: &[Event]) -> Vec<(u8, Vec<u8>)> {
        ev.iter().filter_map(|e| if let Event::Bulk { kind, data, .. } = e { Some((*kind, data.clone())) } else { None }).collect()
    }

    #[test]
    fn bulk_transfer_arrives_complete_once_and_in_order() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 7 % 251) as u8).collect();
        assert!(h.send_bulk(1, &big, now));
        assert!(h.send_bulk(2, &[], now));
        assert!(h.send_bulk(3, b"done", now));
        let (_, ce) = pump(&mut h, &mut c, &mut now, 60);
        assert_eq!(bulks(&ce), vec![(1, big), (2, vec![]), (3, b"done".to_vec())]);
        assert_eq!(h.pending_bulk_chunks(), 0);
    }

    #[test]
    fn bulk_survives_dropped_chunks() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        drain(&c);
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        assert!(h.send_bulk(7, &data, now)); // first window sent ...
        std::thread::sleep(Duration::from_millis(5));
        drain(&c); // ... and lost
        assert!(h.pending_bulk_chunks() > 0);
        assert_eq!(h.pending_bulk_chunks_to(1), h.pending_bulk_chunks());
        now += EVENT_RESEND_EVERY;
        let (_, ce) = pump(&mut h, &mut c, &mut now, 20);
        assert_eq!(bulks(&ce), vec![(7, data)]);
        assert_eq!(h.pending_bulk_chunks(), 0);
    }

    #[test]
    fn bulk_too_large_or_disconnected_is_refused() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        assert!(!h.send_bulk(1, b"x", now));
        pump(&mut h, &mut c, &mut now, 3);
        assert!(!h.send_bulk(1, &vec![0; MAX_BULK_BYTES + 1], now));
        assert!(h.send_bulk(1, &vec![0; MAX_BULK_BYTES], now));
    }

    #[test]
    fn netai_is_split_into_datagrams_under_the_limit_and_all_arrive() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        drain(&c);
        let list: Vec<NetAiEntry> =
            (0..40).map(|i| crate::protocol::tests::netai(i, true, i % 3 == 0, i % 5 == 0)).collect();
        let bytes = h.send_netai(&list, now);
        std::thread::sleep(Duration::from_millis(5));
        let mut buf = [0u8; 2048];
        let (mut got, mut total, mut packets) = (Vec::new(), 0, 0);
        while let Ok((n, _)) = c.sock.recv_from(&mut buf) {
            assert!(n <= MAX_DATAGRAM, "{n}");
            total += n;
            packets += 1;
            if let Some(Packet::NetAi(l)) = decode(&buf[..n]) {
                got.extend(l);
            }
        }
        assert_eq!(got, list);
        assert_eq!(total, bytes);
        assert!(packets > 1);
    }

    fn raws(ev: &[Event]) -> Vec<(u32, Vec<u8>)> {
        ev.iter().filter_map(|e| if let Event::PlayerRaw { ptype, data, .. } = e { Some((*ptype, data.clone())) } else { None }).collect()
    }

    #[test]
    fn player_raw_arrives_in_order_per_type_and_old_ones_are_dropped() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        assert!(!h.send_player_raw(1, b"early", now), "not connected yet");
        pump(&mut h, &mut c, &mut now, 3);
        drain(&c);
        assert!(h.send_player_raw(1, &[1; 1024], now));
        assert!(h.send_player_raw(37, &[2; 4], now));
        assert!(!h.send_player_raw(1, &vec![0; MAX_PLAYER_RAW + 1], now));
        std::thread::sleep(Duration::from_millis(5));
        let got = raws(&c.poll(now));
        assert_eq!(got, vec![(1, vec![1; 1024]), (37, vec![2; 4])]);

        // Replayed / reordered datagrams: an older seq of type 1 is dropped,
        // another type with its own counter is not affected.
        let addr = c.local_addr().unwrap();
        for (seq, ptype) in [(0u32, 1u32), (5, 1), (3, 1), (2, 38)] {
            h.sock.send_to(&encode(&Packet::PlayerRaw { seq, ptype, data: vec![seq as u8] }), addr).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        let got = raws(&c.poll(now));
        assert_eq!(got, vec![(1, vec![5]), (38, vec![2])]);
    }

    #[test]
    fn pending_events_and_rtt_are_cleared_after_loss_and_reconnect() {
        let (mut h, mut c) = pair("susi", "susi");
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 6);
        assert!(h.rtt_ms().is_some());
        drain(&c);
        h.send_event(1, 2, 3, now);
        assert_eq!(pending(&h).len(), 1);
        // Client goes silent; host drops to Lost and forgets everything.
        for _ in 0..60 {
            h.poll(now);
            now += Duration::from_millis(100);
        }
        assert_eq!(h.state(), State::Lost);
        assert_eq!(h.link_state(1), Some(State::Lost));
        assert!(pending(&h).is_empty());
        assert_eq!(h.rtt_ms(), None);
        assert!(h.peers().is_empty());
        drain(&c);
        pump(&mut h, &mut c, &mut now, 10);
        assert_eq!(h.state(), State::Connected);
        assert!(pending(&h).is_empty());
    }

    /// A transport that is not a socket: two in-memory queues, peers shown as Steam addresses.
    struct Wire {
        me: u64,
        inbox: std::sync::Arc<std::sync::Mutex<VecDeque<(Vec<u8>, u64)>>>,
        peer_inbox: std::sync::Arc<std::sync::Mutex<VecDeque<(Vec<u8>, u64)>>>,
    }

    impl Datagram for Wire {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> std::io::Result<usize> {
            assert!(crate::transport::steam_id_of(&to).is_some());
            self.peer_inbox.lock().unwrap().push_back((buf.to_vec(), self.me));
            Ok(buf.len())
        }
        fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            let (data, from) = self.inbox.lock().unwrap().pop_front().ok_or(ErrorKind::WouldBlock)?;
            buf[..data.len()].copy_from_slice(&data);
            Ok((data.len(), crate::transport::steam_addr(from)))
        }
        fn local_addr(&self) -> std::io::Result<SocketAddr> {
            Ok(crate::transport::steam_addr(self.me))
        }
    }

    #[test]
    fn session_runs_over_a_custom_transport_with_steam_style_addresses() {
        use std::sync::{Arc, Mutex};
        let (a, b) = (Arc::new(Mutex::new(VecDeque::new())), Arc::new(Mutex::new(VecDeque::new())));
        let wire = |me, inbox: &Arc<Mutex<VecDeque<(Vec<u8>, u64)>>>, peer: &Arc<Mutex<VecDeque<(Vec<u8>, u64)>>>| {
            Box::new(Wire { me, inbox: inbox.clone(), peer_inbox: peer.clone() })
        };
        let mut h = Session::with_transport(wire(7, &a, &b), Role::Host, "susi", "host");
        let host_addr = crate::transport::steam_addr(7);
        let mut c = Session::with_transport(wire(9, &b, &a), Role::Client { host: host_addr }, "susi", "client");
        let mut now = Instant::now();
        let (he, ce) = pump(&mut h, &mut c, &mut now, 3);
        assert!(he.iter().any(|e| matches!(e, Event::Connected { peer, .. } if crate::transport::steam_id_of(peer) == Some(9))));
        assert!(ce.iter().any(|e| matches!(e, Event::Connected { name, .. } if name == "host")));
        assert_eq!(h.state(), State::Connected);
        assert_eq!(h.local_addr().unwrap(), host_addr);
        // Steam peers' uids come from their addresses.
        assert!(he.contains(&Event::PeerJoined { slot: 1, uid: 9, name: "client".into() }));
        assert!(ce.contains(&Event::PeerJoined { slot: 0, uid: 7, name: "host".into() }));
        assert_eq!(h.peers(), vec![(1, 9, "client".to_string())]);
        assert_eq!(c.peers(), vec![(0, 7, "host".to_string())]);
    }

    // --- Link and slot bookkeeping (multi-player shape, P1a) ---

    fn link() -> Link {
        Link::new("127.0.0.1:1".parse().unwrap(), 1, Instant::now())
    }

    #[test]
    fn link_orders_snapshots_wrapping_safe() {
        let mut l = link();
        assert!(l.accept_snapshot(0, u32::MAX - 1));
        assert!(l.accept_snapshot(0, u32::MAX));
        assert!(l.accept_snapshot(0, 0), "wraps");
        assert!(!l.accept_snapshot(0, u32::MAX), "older after the wrap");
        assert!(!l.accept_snapshot(0, 0), "duplicate");
        assert!(l.accept_snapshot(0, 1));
        assert!(l.accept_snapshot(3, 0), "another source has its own counter");
        assert!(!l.accept_snapshot(3, 0) && l.accept_snapshot(0, 2));
        assert!(!l.accept_snapshot(SLOTS as u8, 5), "no such slot");
    }

    #[test]
    fn link_orders_raw_per_source_and_type_and_caps_the_types() {
        let mut l = link();
        assert!(l.accept_raw(1, 1, 5));
        assert!(!l.accept_raw(1, 1, 4));
        assert!(l.accept_raw(2, 1, 4), "another source has its own counter");
        assert!(l.accept_raw(1, 2, 0), "another type has its own counter");
        for t in 3..(MAX_RAW_TYPES as u32 + 10) {
            l.accept_raw(1, t, 0);
        }
        assert_eq!(l.raw_last.len(), MAX_RAW_TYPES);
        assert!(!l.accept_raw(1, 9999, 0), "no room for a new type");
        assert!(l.accept_raw(1, 1, 6), "known types still pass");
    }

    #[test]
    fn link_dedups_events_and_forgets_acked_ones() {
        let mut l = link();
        let t0 = Instant::now();
        assert!(l.accept_event(3));
        assert!(!l.accept_event(3));
        for s in 10..(10 + SEEN_EVENTS as u32) {
            l.accept_event(s);
        }
        assert!(l.accept_event(3), "forgotten after SEEN_EVENTS newer ones");

        l.queue_event(1, 0, DST_ALL, 7, 1, 2, t0);
        l.queue_event(2, 3, 1, 7, 3, 4, t0);
        assert!(l.due_events(t0).is_empty(), "just sent");
        let due = l.due_events(t0 + EVENT_RESEND_EVERY);
        assert_eq!(due.len(), 2);
        l.on_ack(1);
        assert_eq!(l.pending.len(), 1);
        assert_eq!(l.pending[0].seq, 2);
    }

    #[test]
    fn link_assembles_bulk_once_in_any_chunk_order() {
        let mut l = link();
        assert_eq!(l.on_bulk_chunk(4, 9, 1, 3, b"b".to_vec()), None);
        assert_eq!(l.on_bulk_chunk(4, 9, 1, 3, b"b".to_vec()), None, "duplicate chunk");
        assert_eq!(l.on_bulk_chunk(4, 9, 2, 3, b"c".to_vec()), None);
        assert_eq!(l.on_bulk_chunk(4, 9, 0, 3, b"a".to_vec()), Some((9, b"abc".to_vec())));
        assert_eq!(l.on_bulk_chunk(4, 9, 0, 3, b"a".to_vec()), None, "done transfers are not delivered again");
        assert_eq!(l.on_bulk_chunk(4, 9, 7, 3, b"x".to_vec()), None, "out of range idx is ignored");

        let t0 = Instant::now();
        l.queue_bulk(0, DST_ALL, 1, vec![vec![1]; BULK_WINDOW + 2]);
        assert_eq!(l.due_bulk(t0).len(), BULK_WINDOW, "window");
        assert_eq!(l.pending_bulk_chunks(), BULK_WINDOW + 2);
        for i in 0..BULK_WINDOW as u16 {
            l.on_bulk_ack(0, i);
        }
        assert_eq!(l.due_bulk(t0).len(), 2);
        l.on_bulk_ack(0, BULK_WINDOW as u16);
        l.on_bulk_ack(0, BULK_WINDOW as u16 + 1);
        assert_eq!(l.pending_bulk_chunks(), 0);
        assert!(l.bulk_out.is_empty());
    }

    #[test]
    fn the_roster_bulk_goes_ahead_of_transfers_not_started_but_never_interrupts_one_under_way() {
        let mut l = link();
        let t0 = Instant::now();
        let kinds = |l: &Link| l.bulk_out.iter().map(|b| b.kind).collect::<Vec<_>>();
        // Empty queue, or only transfers that have not started: first in line.
        l.queue_bulk_first(0, 1, ROSTER_KIND, vec![vec![1]]);
        l.queue_bulk(0, 1, 7, vec![vec![2]; 3]);
        l.queue_bulk(0, 1, 8, vec![vec![3]]);
        assert_eq!(kinds(&l), vec![ROSTER_KIND, 7, 8]);
        // The front is under way (a chunk went out): the roster waits for it, not for the rest.
        let _ = l.due_bulk(t0);
        l.queue_bulk_first(0, 1, ROSTER_KIND, vec![vec![4]]);
        assert_eq!(kinds(&l), vec![ROSTER_KIND, ROSTER_KIND, 7, 8]);
        let mut m = link();
        m.queue_bulk(0, 1, 7, vec![vec![2]; 3]);
        let _ = m.due_bulk(t0);
        m.queue_bulk(0, 1, 8, vec![vec![3]]);
        m.queue_bulk_first(0, 1, ROSTER_KIND, vec![vec![4]]);
        assert_eq!(kinds(&m), vec![7, ROSTER_KIND, 8]);
    }

    #[test]
    fn link_reset_forgets_the_connection_but_not_who_it_is() {
        let mut l = link();
        let t0 = Instant::now();
        l.name = "Wolf".into();
        l.accept_snapshot(2, 5);
        l.accept_raw(1, 1, 5);
        l.accept_event(1);
        l.queue_event(1, 0, DST_ALL, 1, 1, 1, t0);
        l.queue_bulk(0, DST_ALL, 1, vec![vec![1]]);
        l.on_pong(2000.0);
        assert_eq!(l.rtt_ms(), Some(2));
        l.reset();
        assert!(l.accept_snapshot(2, 0) && l.accept_raw(1, 1, 0) && l.accept_event(1));
        assert!(l.pending.is_empty() && l.bulk_out.is_empty() && l.next_xfer == 0);
        assert_eq!(l.rtt_ms(), None);
        assert_eq!((l.slot, l.name.as_str()), (1, "Wolf"));
    }

    #[test]
    fn slots_me_and_routing_with_one_link() {
        let (mut h, mut c) = pair("susi", "susi");
        assert_eq!((h.me(), c.me()), (0, NO_SLOT));
        assert_eq!(h.link_state(1), None);
        assert_eq!(c.link_state(0), Some(State::Waiting));
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        assert_eq!(c.me(), 1);
        assert_eq!(h.link_state(1), Some(State::Connected));
        assert_eq!(h.link_state(0), None, "the host has no link to itself");
        assert_eq!(c.link_state(1), None, "a client links only to the host");

        // To a slot nobody has: dropped.
        assert!(!h.send_player_raw_to(2, 1, b"x", now));
        assert!(!h.send_bulk_to(3, 1, b"x", now));
        h.send_event_to(4, 1, 1, 1, now);
        assert_eq!(h.pending_events(), 0);
        // To the right slot: arrives, with `from`.
        assert!(h.send_player_raw_to(1, 1, b"x", now));
        h.send_event_to(1, 5, 6, 7, now);
        assert!(c.send_bulk_to(0, 2, b"up", now));
        let (he, ce) = pump(&mut h, &mut c, &mut now, 5);
        assert!(ce.contains(&Event::PlayerRaw { from: 0, ptype: 1, data: b"x".to_vec() }));
        assert!(ce.contains(&Event::Game { from: 0, kind: 5, a: 6, b: 7 }));
        assert!(he.contains(&Event::Bulk { from: 1, kind: 2, data: b"up".to_vec() }));
    }

    #[test]
    fn udp_host_links_two_clients_when_max_peers_allows() {
        let (mut h, mut c) = pair("susi", "susi");
        h.set_max_peers(5);
        h.set_uid(42);
        c.set_uid(7);
        let mut now = Instant::now();
        pump(&mut h, &mut c, &mut now, 3);
        let addr = h.local_addr().unwrap();
        let mut c2 = Session::new("127.0.0.1:0".parse().unwrap(), Role::Client { host: addr }, "susi", "second").unwrap();
        c2.set_uid(8);
        let (he, _) = pump(&mut h, &mut c2, &mut now, 3);
        assert!(!he.iter().any(|e| matches!(e, Event::PeerLeft { .. })), "{he:?}");
        assert_eq!(h.peers(), vec![(1, 7, "client".to_string()), (2, 8, "second".to_string())]);
        assert_eq!((c.me(), c2.me()), (1, 2));
    }

    // --- Star through the host: 3..6 sessions over an in-memory mesh (wire v10, P2) ---

    type Filter = Box<dyn FnMut(SocketAddr, SocketAddr, &[u8]) -> bool + Send>;

    #[derive(Default)]
    struct MeshInner {
        queues: HashMap<SocketAddr, VecDeque<(Vec<u8>, SocketAddr)>>,
        /// false = the datagram is lost.
        filter: Option<Filter>,
        /// Every datagram sent (from, to, bytes), lost ones included.
        log: Vec<(SocketAddr, SocketAddr, Vec<u8>)>,
    }

    #[derive(Clone, Default)]
    struct Mesh(std::sync::Arc<std::sync::Mutex<MeshInner>>);

    impl Mesh {
        /// A datagram as if `from` had sent it to `to`.
        fn inject(&self, from: SocketAddr, to: SocketAddr, bytes: Vec<u8>) {
            self.0.lock().unwrap().queues.entry(to).or_default().push_back((bytes, from));
        }
    }

    struct Port {
        me: SocketAddr,
        mesh: Mesh,
    }

    impl Datagram for Port {
        fn send_to(&self, buf: &[u8], to: SocketAddr) -> std::io::Result<usize> {
            let mut m = self.mesh.0.lock().unwrap();
            m.log.push((self.me, to, buf.to_vec()));
            if m.filter.as_mut().is_none_or(|f| f(self.me, to, buf)) {
                m.queues.entry(to).or_default().push_back((buf.to_vec(), self.me));
            }
            Ok(buf.len())
        }
        fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
            let mut m = self.mesh.0.lock().unwrap();
            let (data, from) = m.queues.get_mut(&self.me).and_then(VecDeque::pop_front).ok_or(ErrorKind::WouldBlock)?;
            buf[..data.len()].copy_from_slice(&data);
            Ok((data.len(), from))
        }
        fn local_addr(&self) -> std::io::Result<SocketAddr> {
            Ok(self.me)
        }
    }

    fn at(i: u8) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, i], 4000))
    }

    /// s[0] the host (uid 100, "host"), s[i] client i (uid 100 + i, "c{i}") at `at(i)`.
    struct Star {
        mesh: Mesh,
        s: Vec<Session>,
        ev: Vec<Vec<Event>>,
        now: Instant,
    }

    impl Star {
        fn new(clients: u8, max_peers: u8) -> Self {
            let mesh = Mesh::default();
            let mut host = Session::with_transport(Box::new(Port { me: at(0), mesh: mesh.clone() }), Role::Host, "susi", "host");
            host.set_uid(100);
            host.set_max_peers(max_peers);
            let mut s = vec![host];
            for i in 1..=clients {
                s.push(Self::client(&mesh, at(i), 100 + i as u64, &format!("c{i}"), "susi"));
            }
            let n = s.len();
            Star { mesh, s, ev: vec![Vec::new(); n], now: Instant::now() }
        }

        fn client(mesh: &Mesh, addr: SocketAddr, uid: u64, name: &str, pw: &str) -> Session {
            let port = Box::new(Port { me: addr, mesh: mesh.clone() });
            let mut c = Session::with_transport(port, Role::Client { host: at(0) }, pw, name);
            c.set_uid(uid);
            c
        }

        /// Adds (or replaces) session `i`.
        fn put(&mut self, i: usize, s: Session) {
            if i < self.s.len() {
                self.s[i] = s;
            } else {
                self.s.push(s);
                self.ev.push(Vec::new());
            }
        }

        /// Polls the sessions `rounds` times 20 ms apart (clients, host, clients), all but `skip`.
        fn pump_without(&mut self, rounds: u32, skip: &[usize]) {
            let n = self.s.len();
            for _ in 0..rounds {
                for i in (1..n).chain([0]).chain(1..n) {
                    if !skip.contains(&i) {
                        let e = self.s[i].poll(self.now);
                        self.ev[i].extend(e);
                    }
                }
                self.now += Duration::from_millis(20);
            }
        }

        fn pump(&mut self, rounds: u32) {
            self.pump_without(rounds, &[]);
        }

        fn take(&mut self, i: usize) -> Vec<Event> {
            std::mem::take(&mut self.ev[i])
        }

        fn clear(&mut self) {
            self.ev.iter_mut().for_each(Vec::clear);
            self.mesh.0.lock().unwrap().log.clear();
        }

        fn filter(&self, f: impl FnMut(SocketAddr, SocketAddr, &[u8]) -> bool + Send + 'static) {
            self.mesh.0.lock().unwrap().filter = Some(Box::new(f));
        }

        /// Datagrams sent since the last `clear`, decoded (undecodable ones left out).
        fn sent(&self) -> Vec<(SocketAddr, SocketAddr, Packet)> {
            let m = self.mesh.0.lock().unwrap();
            m.log.iter().filter_map(|(f, t, b)| Some((*f, *t, decode(b)?))).collect()
        }
    }

    /// The data events as (from, what).
    fn tags(ev: &[Event]) -> Vec<(u8, String)> {
        ev.iter()
            .filter_map(|e| {
                Some(match e {
                    Event::Snapshot { from, .. } => (*from, "snap".to_string()),
                    Event::Enemies { from, .. } => (*from, "enemies".into()),
                    Event::Hkb { from, .. } => (*from, "hkb".into()),
                    Event::Damage { from, .. } => (*from, "damage".into()),
                    Event::Posture { from, .. } => (*from, "posture".into()),
                    Event::Game { from, kind, .. } => (*from, format!("game{kind}")),
                    Event::NetAi { from, .. } => (*from, "netai".into()),
                    Event::Bulk { from, kind, .. } => (*from, format!("bulk{kind}")),
                    Event::PlayerRaw { from, ptype, .. } => (*from, format!("raw{ptype}")),
                    _ => return None,
                })
            })
            .collect()
    }

    fn sorted(mut v: Vec<(u8, String)>) -> Vec<(u8, String)> {
        v.sort();
        v
    }

    fn peer(slot: u8, uid: u64, name: &str) -> (u8, u64, String) {
        (slot, uid, name.to_string())
    }

    #[test]
    fn six_players_join_get_slots_and_see_each_other_through_the_host() {
        let mut w = Star::new(5, 5);
        w.pump(10);
        for i in 1..=5 {
            assert_eq!(w.s[i].me(), i as u8);
            assert_eq!(w.s[i].state(), State::Connected);
        }
        let all: Vec<_> = (1..=5u8).map(|i| peer(i, 100 + i as u64, &format!("c{i}"))).collect();
        assert_eq!(w.s[0].peers(), all);
        // Client 3: the host (its uid learnt from the roster), then the other four.
        let mut want = vec![peer(0, 100, "host")];
        want.extend(all.iter().filter(|p| p.0 != 3).cloned());
        assert_eq!(w.s[3].peers(), want);
        assert_eq!(w.s[3].link_state(5), Some(State::Connected));
        assert_eq!(w.s[3].link_state(3), None, "not a peer of itself");
        let ev = w.take(3);
        assert!(ev.contains(&Event::Connected { peer: at(0), name: "host".into(), slot: 3 }));
        assert!(ev.contains(&Event::PeerJoined { slot: 0, uid: 0, name: "host".into() }));
        for i in [1u8, 2, 4, 5] {
            assert!(ev.contains(&Event::PeerJoined { slot: i, uid: 100 + i as u64, name: format!("c{i}") }), "{ev:?}");
        }
        let rosters: Vec<_> = ev.iter().filter(|e| matches!(e, Event::Roster(_))).collect();
        assert_eq!(rosters.len(), 1, "every join in one host poll: one roster {ev:?}");
        // A seventh player: the world is full.
        let c6 = Star::client(&w.mesh, at(6), 106, "c6", "susi");
        w.put(6, c6);
        w.pump(5);
        assert!(w.take(6).contains(&Event::Rejected { reason: RejectReason::Full }));
        assert_eq!(w.s[6].me(), NO_SLOT);
        assert!(w.s[0].take_refusals().contains(&(RejectReason::Full, "c6".into())));
        assert_eq!(w.s[0].peers().len(), 5);
    }

    #[test]
    fn leave_by_bye_or_silence_reaches_the_other_clients_and_frees_the_slot() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        let now = w.now;
        w.s[2].quit(now);
        w.pump(5);
        let he = w.take(0);
        assert!(he.contains(&Event::PeerQuit { slot: 2 }) && he.contains(&Event::PeerLeft { slot: 2, why: LeaveWhy::Quit }));
        for i in [1, 3] {
            let ev = w.take(i);
            assert!(ev.contains(&Event::PeerLeft { slot: 2, why: LeaveWhy::Lost }), "{ev:?}");
            assert!(!w.s[i].peers().iter().any(|p| p.0 == 2));
            assert_eq!(w.s[i].link_state(2), None);
        }
        // Client 3 goes silent: lost on the host after LOST_AFTER, gone for client 1.
        w.pump_without(LOST_AFTER.as_millis() as u32 / 20 + 10, &[2, 3]);
        assert!(w.take(0).contains(&Event::PeerLeft { slot: 3, why: LeaveWhy::Lost }));
        assert!(w.take(1).contains(&Event::PeerLeft { slot: 3, why: LeaveWhy::Lost }));
        assert_eq!(w.s[1].peers(), vec![peer(0, 100, "host")]);
        // A newcomer gets the lowest slot nobody had (lost slots wait for their uid first).
        let c4 = Star::client(&w.mesh, at(4), 104, "c4", "susi");
        w.put(4, c4);
        w.pump_without(5, &[2, 3]);
        assert_eq!(w.s[4].me(), 4);
        assert!(w.take(1).contains(&Event::PeerJoined { slot: 4, uid: 104, name: "c4".into() }));
    }

    #[test]
    fn rtt_comes_from_a_link_that_is_not_lost() {
        let mut w = Star::new(2, 5);
        w.pump(10);
        assert!(w.s[0].rtt_ms().is_some());
        // Client 1 (the first link) goes silent and is lost; client 2 keeps answering.
        w.pump_without(LOST_AFTER.as_millis() as u32 / 20 + 10, &[1]);
        assert_eq!(w.s[0].link_state(1), Some(State::Lost));
        assert_eq!(w.s[0].link_state(2), Some(State::Connected));
        assert!(w.s[0].rtt_ms().is_some(), "the lost first link does not hide the live one");
    }

    #[test]
    fn a_known_uid_at_a_new_address_replaces_its_slot_and_the_others_see_a_new_link() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        // Client 3 streams a while, then its game restarts (new socket, same uid).
        for _ in 0..5 {
            let now = w.now;
            w.s[3].send_snapshot(snap(3.0), now);
            w.pump(2);
        }
        w.clear();
        let c3 = Star::client(&w.mesh, at(13), 103, "c3", "susi");
        w.put(3, c3);
        w.pump(5);
        let he = w.take(0);
        assert!(he.contains(&Event::PeerLeft { slot: 3, why: LeaveWhy::Replaced }), "{he:?}");
        assert!(he.iter().any(|e| matches!(e, Event::Connected { slot: 3, peer, .. } if *peer == at(13))));
        assert_eq!(w.s[3].me(), 3);
        for i in [1, 2] {
            let ev = w.take(i);
            let left = ev.iter().position(|e| *e == Event::PeerLeft { slot: 3, why: LeaveWhy::Replaced });
            let joined = ev.iter().position(|e| *e == Event::PeerJoined { slot: 3, uid: 103, name: "c3".into() });
            assert!(left.is_some() && joined.is_some() && left < joined, "{ev:?}");
        }
        // The new link counts its snapshots from 0 again: the others take them.
        let now = w.now;
        w.s[3].send_snapshot(snap(33.0), now);
        w.pump(2);
        for i in [0, 1, 2] {
            let got: Vec<_> = w.take(i).into_iter().filter(|e| matches!(e, Event::Snapshot { from: 3, .. })).collect();
            assert_eq!(got, vec![Event::Snapshot { from: 3, snap: snap(33.0) }], "session {i}");
        }
    }

    #[test]
    fn every_routed_kind_is_relayed_to_all_or_to_one_client_with_the_original_src() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        let now = w.now;
        let c = &mut w.s[1];
        let e = [EnemyState { id: 1, ..Default::default() }];
        let h = [crate::protocol::HkbEvent { n: 1, entity: 0, idx: 2 }];
        let r = DamageReport { id: 1, hp: 2, posture: 3, deathblows: 0 };
        let p = [PostureState { id: 1, posture: 2, max: 3, lives: 1 }];
        let n = [crate::protocol::tests::netai(1, true, false, false)];
        // To everyone.
        c.send_snapshot(snap(1.0), now);
        c.send_enemies(&e, now);
        c.send_hkb(&h, now);
        c.send_damage(r, now);
        c.send_posture(&p, now);
        c.send_event(1, 2, 3, now);
        c.send_netai(&n, now);
        assert!(c.send_bulk(9, b"all", now));
        assert!(c.send_player_raw(5, b"raw", now));
        // To client 2 only.
        c.send_enemies_to(2, &e, now);
        c.send_hkb_to(2, &h, now);
        c.send_damage_to(2, r, now);
        c.send_posture_to(2, &p, now);
        c.send_event_to(2, 7, 2, 3, now);
        c.send_netai_to(2, &n, now);
        assert!(c.send_bulk_to(2, 10, b"two", now));
        assert!(c.send_player_raw_to(2, 6, b"r2", now));
        // To the host only.
        c.send_event_to(0, 8, 0, 0, now);
        // Nobody: not in the roster, or ourselves.
        assert!(!c.send_player_raw_to(4, 6, b"x", now));
        assert!(!c.send_bulk_to(1, 10, b"x", now));
        w.pump(10);
        let all = ["snap", "enemies", "hkb", "damage", "posture", "game1", "netai", "bulk9", "raw5"];
        let direct = ["enemies", "hkb", "damage", "posture", "game7", "netai", "bulk10", "raw6"];
        let want = |names: &[&[&str]]| sorted(names.concat().iter().map(|s| (1u8, s.to_string())).collect());
        assert_eq!(sorted(tags(&w.take(2))), want(&[&all, &direct]));
        assert_eq!(sorted(tags(&w.take(3))), want(&[&all]));
        assert_eq!(sorted(tags(&w.take(0))), want(&[&all, &["game8"]]));
        assert!(tags(&w.take(1)).is_empty(), "never back to the source");
        let (packets, bytes) = w.s[0].relay_stats();
        assert!(packets >= 9 + 9 && bytes > 0, "{packets} {bytes}");
        // The host's own sends to one client.
        w.clear();
        let now = w.now;
        w.s[0].send_event_to(3, 4, 0, 0, now);
        assert!(w.s[0].send_player_raw_to(2, 1, b"h", now));
        w.pump(5);
        assert_eq!(tags(&w.take(3)), vec![(0, "game4".to_string())]);
        assert_eq!(tags(&w.take(2)), vec![(0, "raw1".to_string())]);
        assert!(tags(&w.take(1)).is_empty());
    }

    #[test]
    fn a_client_cannot_speak_for_another_slot_and_bad_sources_are_dropped() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        // Client 1 claims to be 3: the host takes it as 1's.
        let fake = encode_routed(&Packet::Damage(DamageReport { id: 9, ..Default::default() }), 3, DST_ALL);
        w.mesh.inject(at(1), at(0), fake);
        // From the host to client 2: its own slot or no slot as the source.
        for src in [2u8, SLOTS as u8, NO_SLOT] {
            w.mesh.inject(at(0), at(2), encode_routed(&Packet::Damage(DamageReport::default()), src, DST_ALL));
        }
        // To someone else: not for client 2.
        w.mesh.inject(at(0), at(2), encode_routed(&Packet::Damage(DamageReport::default()), 0, 3));
        w.pump(3);
        assert_eq!(tags(&w.take(0)), vec![(1, "damage".to_string())]);
        assert_eq!(tags(&w.take(2)), vec![(1, "damage".to_string())]);
        assert_eq!(tags(&w.take(3)), vec![(1, "damage".to_string())]);
    }

    /// A filter step that loses the first `drop` datagrams it is asked about.
    fn first_n(drop: u32) -> impl FnMut() -> bool {
        let mut seen = 0;
        move || {
            seen += 1;
            seen > drop
        }
    }

    #[test]
    fn events_stay_reliable_across_the_relay_with_loss_on_both_hops() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        let (mut up, mut down, mut acks) = (first_n(3), first_n(2), first_n(2));
        w.filter(move |from, to, b| match decode(b) {
            Some(Packet::Event { .. }) if from == at(1) && to == at(0) => up(),
            Some(Packet::Event { .. }) if from == at(0) && to == at(3) => down(),
            // Lost acks make client 1 send again: the host relays it once anyway.
            Some(Packet::Ack { .. }) if from == at(0) && to == at(1) => acks(),
            _ => true,
        });
        let now = w.now;
        w.s[1].send_event(4, 5, 6, now);
        w.pump(100);
        for i in [0, 2, 3] {
            let got: Vec<_> = w.take(i).into_iter().filter(|e| matches!(e, Event::Game { .. })).collect();
            assert_eq!(got, vec![Event::Game { from: 1, kind: 4, a: 5, b: 6 }], "session {i}");
        }
        assert_eq!(w.s[1].pending_events(), 0);
        assert_eq!(w.s[0].pending_events(), 0);
        assert_eq!(w.s[0].pending_events_to(3), 0);
        // Client 1 sent it more than 3 times; the host relayed it once to each (2 lost hops
        // to client 3 are re-sends of that one relay, not new relays).
        let events_out = w.sent().iter().filter(|(f, t, p)| *f == at(1) && *t == at(0) && matches!(p, Packet::Event { .. })).count();
        assert!(events_out >= 4, "{events_out}");
    }

    #[test]
    fn bulk_is_stored_and_forwarded_complete_once_with_lost_chunks() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        let mut n = 0u32;
        w.filter(move |from, to, b| {
            if !matches!(decode(b), Some(Packet::Bulk { .. })) {
                return true;
            }
            n += 1;
            // Every 3rd chunk host -> 3 and every 4th 1 -> host is lost.
            !((from == at(0) && to == at(3) && n % 3 == 0) || (from == at(1) && to == at(0) && n % 4 == 0))
        });
        let big: Vec<u8> = (0..30_000u32).map(|i| (i * 13 % 251) as u8).collect();
        let now = w.now;
        assert!(w.s[1].send_bulk_to(3, 7, &big, now));
        assert!(w.s[1].send_bulk(8, b"everyone", now));
        w.pump(150);
        assert_eq!(bulks(&w.take(3)), vec![(7, big.clone()), (8, b"everyone".to_vec())]);
        assert_eq!(bulks(&w.take(2)), vec![(8, b"everyone".to_vec())]);
        assert_eq!(bulks(&w.take(0)), vec![(8, b"everyone".to_vec())]);
        assert!(w.take(1).iter().all(|e| !matches!(e, Event::Bulk { .. })));
        assert_eq!(w.s[1].pending_bulk_chunks(), 0);
        assert_eq!(w.s[0].pending_bulk_chunks(), 0);
        // The session's roster kind is not for callers.
        let now = w.now;
        assert!(!w.s[1].send_bulk(ROSTER_KIND, b"x", now));
    }

    #[test]
    fn the_roster_is_only_sent_while_it_names_another_client_and_old_ones_are_ignored() {
        let mut w = Star::new(1, 5);
        w.pump(20);
        // One client: no roster on the wire, no Roster event.
        assert!(!w.sent().iter().any(|(_, _, p)| matches!(p, Packet::Bulk { .. })));
        assert!(!w.take(1).iter().any(|e| matches!(e, Event::Roster(_))));
        let g1 = w.s[0].roster_gen;
        let c2 = Star::client(&w.mesh, at(2), 102, "c2", "susi");
        w.put(2, c2);
        w.pump(5);
        let g2 = w.s[0].roster_gen;
        assert!(newer(g2, g1));
        let want = vec![peer(0, 100, "host"), peer(1, 101, "c1"), peer(2, 102, "c2")];
        for i in [1, 2] {
            let ev = w.take(i);
            assert!(ev.contains(&Event::Roster(want.clone())), "{ev:?}");
            assert_eq!(w.s[i].heard_gen, Some(g2));
        }
        // An older roster (a late duplicate) changes nothing; a newer one applies.
        let host_only = [RosterEntry { slot: 0, uid: 100, inc: 0, name: "host".into() }];
        let mut ev = Vec::new();
        w.s[1].on_roster(&encode_roster(g1, &host_only), &mut ev);
        assert!(ev.is_empty());
        assert_eq!(w.s[1].peers().len(), 2);
        w.s[1].on_roster(&encode_roster(g2.wrapping_add(1), &host_only), &mut ev);
        assert_eq!(ev, vec![Event::PeerLeft { slot: 2, why: LeaveWhy::Lost }, Event::Roster(vec![peer(0, 100, "host")])]);
        // Client 2 leaves: client 1 is told (the roster shrank), then nothing more is sent.
        w.clear();
        let now = w.now;
        w.s[2].quit(now);
        w.pump_without(5, &[2]);
        assert!(w.sent().iter().any(|(_, t, p)| *t == at(1) && matches!(p, Packet::Bulk { .. })));
        w.clear();
        w.pump_without(20, &[2]);
        assert!(!w.sent().iter().any(|(_, _, p)| matches!(p, Packet::Bulk { .. })));
    }

    #[test]
    fn rejects_for_full_password_and_version() {
        // Full: two clients allowed, the third is refused until a slot frees up.
        let mut w = Star::new(3, 2);
        w.pump(10);
        assert!(w.take(3).contains(&Event::Rejected { reason: RejectReason::Full }));
        assert_eq!((w.s[3].me(), w.s[3].state()), (NO_SLOT, State::Waiting));
        assert!(w.s[0].take_refusals().contains(&(RejectReason::Full, "c3".into())));
        let now = w.now;
        w.s[1].quit(now);
        w.pump_without(30, &[1]); // its next Hello comes within HELLO_EVERY
        assert_eq!(w.s[3].me(), 1, "the freed slot");
        assert_eq!(w.s[0].peers().iter().map(|p| p.0).collect::<Vec<_>>(), vec![1, 2]);

        // Password.
        let wrong = Star::client(&w.mesh, at(4), 104, "c4", "kettu");
        w.put(4, wrong);
        w.pump_without(3, &[1]);
        assert!(w.take(4).contains(&Event::Rejected { reason: RejectReason::Password }));
        assert!(w.s[0].take_refusals().contains(&(RejectReason::Password, "c4".into())));

        // Version: a v9 client's Hello gets a Reject it can read.
        w.clear();
        let mut v9 = encode(&Packet::Hello { password_hash: password_hash("susi"), uid: 0, name: String::new() });
        v9[4..6].copy_from_slice(&9u16.to_le_bytes());
        w.mesh.inject(at(9), at(0), v9);
        w.pump_without(1, &[1]);
        let log = w.mesh.0.lock().unwrap().log.clone();
        assert!(log.iter().any(|(f, t, b)| *f == at(0) && *t == at(9) && *b == version_reject(9)));
        assert!(w.s[0].take_refusals().contains(&(RejectReason::Version, String::new())));
        // A client whose host answers in another version reports it (not every datagram).
        let c5 = Star::client(&w.mesh, at(5), 105, "c5", "susi");
        w.put(5, c5);
        for _ in 0..3 {
            w.mesh.inject(at(0), at(5), version_reject(9));
        }
        let now = w.now;
        let ev = w.s[5].poll(now);
        assert_eq!(ev, vec![Event::Rejected { reason: RejectReason::Version }]);
    }

    #[test]
    fn receivers_order_snapshots_and_raw_packets_per_source() {
        let mut w = Star::new(2, 5);
        w.s[0].set_relay_snapshot_hz(1000);
        w.pump(10);
        for k in 0..3 {
            let now = w.now;
            w.s[0].send_snapshot(snap(k as f32), now);
            w.s[1].send_snapshot(snap(10.0 + k as f32), now);
            w.s[1].send_player_raw(1, &[k], now);
            w.pump(1);
        }
        w.clear();
        // Client 1's stream is at seq 2, the host's at seq 2: an old one of either is dropped,
        // a newer one of either passes, each on its own counter.
        let s = |seq: u32| Packet::Snapshot(Snapshot { seq, ..snap(seq as f32) });
        for (src, seq) in [(1u8, 1u32), (0, 0), (1, 3), (0, 9)] {
            w.mesh.inject(at(0), at(2), encode_routed(&s(seq), src, DST_ALL));
        }
        let raw = |seq: u32| Packet::PlayerRaw { seq, ptype: 1, data: vec![seq as u8] };
        for (src, seq) in [(1u8, 0u32), (0, 0), (1, 5)] {
            w.mesh.inject(at(0), at(2), encode_routed(&raw(seq), src, DST_ALL));
        }
        let now = w.now;
        let ev = w.s[2].poll(now);
        let snaps: Vec<_> =
            ev.iter().filter_map(|e| if let Event::Snapshot { from, snap } = e { Some((*from, snap.seq)) } else { None }).collect();
        assert_eq!(snaps, vec![(1, 3), (0, 9)]);
        let raws: Vec<_> =
            ev.iter().filter_map(|e| if let Event::PlayerRaw { from, data, .. } = e { Some((*from, data[0])) } else { None }).collect();
        assert_eq!(raws, vec![(0, 0), (1, 5)], "src 1 was at 2: 0 is old; src 0 had none yet");
    }

    #[test]
    fn relayed_snapshots_are_rate_limited_per_source() {
        let mut w = Star::new(2, 5);
        w.s[0].set_relay_snapshot_hz(30);
        w.pump(10);
        w.clear();
        for _ in 0..200 {
            let now = w.now;
            w.s[1].send_snapshot(snap(1.0), now);
            for i in [1, 0, 2] {
                let e = w.s[i].poll(now);
                w.ev[i].extend(e);
            }
            w.now += Duration::from_millis(5);
        }
        let count = |ev: &[Event]| ev.iter().filter(|e| matches!(e, Event::Snapshot { from: 1, .. })).count();
        assert_eq!(count(&w.take(0)), 200, "the host gets every one");
        let relayed = count(&w.take(2));
        assert!((29..=31).contains(&relayed), "{relayed} in 1 s at 30 Hz");
    }

    #[test]
    fn host_quit_or_loss_ends_every_peer_for_the_clients() {
        let mut w = Star::new(3, 5);
        w.pump(10);
        w.clear();
        let now = w.now;
        w.s[0].quit(now);
        w.pump_without(2, &[0]);
        for i in 1..=3u8 {
            let ev = w.take(i as usize);
            assert!(ev.contains(&Event::PeerQuit { slot: 0 }));
            for j in (1..=3u8).filter(|&j| j != i) {
                assert!(ev.contains(&Event::PeerLeft { slot: j, why: LeaveWhy::Quit }), "{i}: {ev:?}");
            }
            assert!(w.s[i as usize].peers().is_empty());
            assert_eq!(w.s[i as usize].link_state(j_of(i)), None);
        }
    }

    fn j_of(i: u8) -> u8 {
        if i == 1 { 2 } else { 1 }
    }

    #[test]
    fn two_players_behave_like_v9() {
        // max_players = 2: one client allowed.
        let mut w = Star::new(1, 1);
        w.pump(150);
        let he = w.take(0);
        let ce = w.take(1);
        assert_eq!(he, vec![
            Event::Connected { peer: at(1), name: "c1".into(), slot: 1 },
            Event::PeerJoined { slot: 1, uid: 101, name: "c1".into() },
        ]);
        assert_eq!(ce, vec![
            Event::Connected { peer: at(0), name: "host".into(), slot: 1 },
            Event::PeerJoined { slot: 0, uid: 0, name: "host".into() },
        ]);
        // Only the v9 link traffic while idle: no roster, nothing relayed.
        let kinds: std::collections::BTreeSet<&str> = w
            .sent()
            .iter()
            .map(|(_, _, p)| match p {
                Packet::Hello { .. } => "hello",
                Packet::Welcome { .. } => "welcome",
                Packet::Heartbeat => "heartbeat",
                Packet::Ping { .. } => "ping",
                Packet::Pong { .. } => "pong",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds.into_iter().collect::<Vec<_>>(), vec!["heartbeat", "hello", "ping", "pong", "welcome"]);
        assert_eq!(w.s[0].relay_stats(), (0, 0));
        assert_eq!(w.s[1].peers(), vec![peer(0, 0, "host")]);
        // Data both ways, with `from` as before.
        let now = w.now;
        w.s[0].send_snapshot(snap(1.0), now);
        w.s[1].send_snapshot(snap(2.0), now);
        w.s[0].send_event(3, 1, 1, now);
        w.s[1].send_bulk(4, b"up", now);
        w.pump(5);
        assert_eq!(sorted(tags(&w.take(0))), vec![(1, "bulk4".into()), (1, "snap".into())]);
        assert_eq!(sorted(tags(&w.take(1))), vec![(0, "game3".into()), (0, "snap".into())]);
        // The friend restarting (same uid, new address) takes the slot back, as in v9 ...
        let again = Star::client(&w.mesh, at(11), 101, "c1", "susi");
        w.put(1, again);
        w.pump(5);
        assert!(w.take(0).contains(&Event::PeerLeft { slot: 1, why: LeaveWhy::Replaced }));
        assert_eq!(w.s[1].me(), 1);
        // ... a stranger is refused instead of replacing the friend (v9 replaced).
        let stranger = Star::client(&w.mesh, at(12), 999, "x", "susi");
        w.put(2, stranger);
        w.pump(5);
        assert!(w.take(2).contains(&Event::Rejected { reason: RejectReason::Full }));
        assert!(!w.take(0).iter().any(|e| matches!(e, Event::PeerLeft { .. } | Event::Connected { .. })));
        // No uid at all and one client allowed: the v9 rule (the newcomer replaces the link).
        let anon = Star::client(&w.mesh, at(13), 0, "anon", "susi");
        w.put(3, anon);
        w.pump_without(5, &[2]);
        assert!(w.take(0).contains(&Event::PeerLeft { slot: 1, why: LeaveWhy::Replaced }));
        assert_eq!(w.s[3].me(), 1);
    }
}
