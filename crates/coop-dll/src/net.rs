//!
//! Settings come from `sekiro_coop_settings.ini` (`settings.rs`): a password (co-op
//! stays off without one) and, under [ADVANCED], `transport` (auto | steam | udp) and
//! `auto_connect`.
//!
//! Who hosts and who joins is decided by the co-op items (`items.rs`, like Elden Ring
//! Seamless Co-op): Kindred Bell hosts, Kindred Charm joins, Severed Thread leaves. The
//! game starts solo; an item use sets the `INTENT` and asks for a reconnect, which drops
//! this `Net` and builds a new one for that intent. `auto_connect = 1` keeps the old
//! start-up behaviour (Steam `auto` role) until an item is used.
//!
//! - Steam (`steam.rs`): a lobby keyed by a hash of the password; the session runs over
//!   ISteamNetworkingMessages. `auto` falls back to UDP when Steam cannot be used or no lobby
//!   exists within `STEAM_FALLBACK_AFTER`.
//! - UDP: the host listens on `udp_port` and opens it with UPnP (the join code goes to
//!   `udp_join_code.txt`), the joiner connects to `udp_host_address`.
//! - Debug `loopback` runs host and client inside one game, with the remote side fed our
//!   own snapshots delayed by `LOOPBACK_DELAY`: the puppet then replays you one second late.
//!   That tests the whole pipeline on one PC. With the native remote option on it also
//!   echoes the game's raw player packets, so the native remote (N1, native.rs) replays you
//!   one second late over our UDP.
//! - Debug `loopback_peers = k` (PLAN-MULTIPLAYER P2): k echo clients inside this game join
//!   our own host session over in-memory queues (`Hub`, works under UDP and Steam), each
//!   replaying our snapshots and raw packets with its own delay (1 s, 1.25 s, ...), so one
//!   machine exercises N peers and the host relay. With `loopback = 1` the first echo is the
//!   old UDP one. Echoes only run while we host. Log: `net: connected to "loopback-echo-i"
//!   at 127.77.0.i:1` (host side) and `net: loopback peer i (echo d ms) joined as slot s`
//!   (or `... refused: Full` when `max_players` has no room for it).
//!
//! 3-6 players (wire v10): clients see the other clients as peers too (slots from the host's
//! roster); their data arrives through the host with the original sender's slot, and a
//! client's `send_*_to(Dest::Peer(id))` goes through the host.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coop_net::upnp::{self, Mapping};
use coop_net::{
    DST_ALL, Datagram, Event, Inert, LeaveWhy, RejectReason, Role, Session, Snapshot, steam_addr, steam_id_of,
};

pub use crate::peers::PeerId;
use crate::peers::MAX_PLAYERS;
use crate::game::Chr;
use crate::settings::{self, NetSettings, TransportSetting};
use crate::steam::{self, Lobby, Out, Want};
use crate::log;

const LOOPBACK_DELAY: Duration = Duration::from_secs(1);
// About 60 Hz on a fixed step (NETCODE-RESEARCH: `last_send = now` made the
// interval alternate 33/50 ms at 60 fps; a fuller stream lets the ghost's
// interpolation delay shrink). About 4 KB/s.
const EXTRAPOLATE_MAX: f32 = 0.12;
// 2026-10-23: 8 ms, i.e. one snapshot every frame up to 120 fps (fps.rs unlocks 120).
const SEND_EVERY: Duration = Duration::from_millis(8);
const REMOTE_STALE_AFTER: Duration = Duration::from_millis(500);
/// Two consecutive snapshots further apart than this are a teleport (idol
/// warp, fall respawn, join): show the new place at once instead of flying
/// the ghost across the map for the length of the interpolation delay.
const TELEPORT_M: f32 = 8.0;
/// Inbox caps: the inboxes are drained only while we are in the world and
/// connected, so a long loading screen or menu (or the enemy sync switched
/// off on one side) must not let them grow without bound.
const MAX_DAMAGE_INBOX: usize = 1024;
const MAX_GAME_INBOX: usize = 8192;
/// transport = auto: if no lobby is settled (hosting or joined) this long after
/// start, UDP is used instead.
const STEAM_FALLBACK_AFTER: Duration = Duration::from_secs(15);
/// How often a missing Steam API is looked up again (it appears once the game has
/// logged in to Steam).
const STEAM_RETRY_EVERY: Duration = Duration::from_secs(2);
const DEFAULT_NAME: &str = "Wolf";
/// The same refusal (ours or a joiner's) is shown at most this often (a client repeats its
/// Hello every 500 ms).
const REJECT_NOTICE_EVERY: Duration = Duration::from_secs(60);
/// Host: how often the relay totals go to the log (only when something was relayed).
const RELAY_LOG_EVERY: Duration = Duration::from_secs(60);
/// `loopback_peers`: extra echo delay per echo peer after the first.
const ECHO_DELAY_STEP: Duration = Duration::from_millis(250);
/// In-memory echo queues hold at most this many datagrams (a stalled side drops the rest).
const ECHO_QUEUE_MAX: usize = 4096;
/// uid of echo peer i: `ECHO_UID | i` (never a SteamID64 or a machine uid).
const ECHO_UID: u64 = 0xEC40_0000_0000_0000;

/// Why there is no `Net` (for the status line).
static OFFLINE: Mutex<String> = Mutex::new(String::new());
/// No `Net` because no co-op item was used: the status line shows the items' hint.
static SOLO: AtomicBool = AtomicBool::new(false);

fn set_offline(why: &str) {
    SOLO.store(false, Ordering::Relaxed);
    if let Ok(mut s) = OFFLINE.lock() {
        if *s != why {
            log::line(&format!("net: off ({why})"));
            *s = why.to_owned();
        }
    }
}

/// Status line text while there is no connection object.
pub fn offline_status() -> String {
    if SOLO.load(Ordering::Relaxed) {
        return crate::items::solo_hint();
    }
    let s = OFFLINE.lock().map(|s| s.clone()).unwrap_or_default();
    if s.is_empty() { "off".into() } else { s }
}

/// What the player asked for with a co-op item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intent {
    /// Severed Thread (or the host closed its world): no session.
    Solo,
    /// Kindred Bell: open our world.
    Host,
    /// Kindred Charm: look for the friend's world.
    Join,
}

const INTENT_UNSET: u8 = 0;
const INTENT_SOLO: u8 = 1;
const INTENT_HOST: u8 = 2;
const INTENT_JOIN: u8 = 3;
/// No item used since the game started: `auto_connect` decides.
static INTENT: AtomicU8 = AtomicU8::new(INTENT_UNSET);

/// Sets the intent and reconnects: the frame task drops the current `Net` (a clean
/// quit, the Steam lobby is left) and builds the next one with `Net::from_config`.
pub fn set_intent(intent: Intent) {
    let v = match intent {
        Intent::Solo => INTENT_SOLO,
        Intent::Host => INTENT_HOST,
        Intent::Join => INTENT_JOIN,
    };
    INTENT.store(v, Ordering::Release);
    SESSION_GEN.fetch_add(1, Ordering::AcqRel);
    log::line(&format!("net: co-op intent {intent:?}, restarting the session"));
    settings::request_reconnect();
}

/// Bumped by every deliberate session change (an item: Severed Thread, Kindred Bell /
/// Charm, the host closing; the title screen). A link drop and its reconnect keep it, so
/// join.rs can tell the two apart (live 2026-10-24: a leave + rejoin 36 s later was taken
/// for a reconnect and got no join warp).
static SESSION_GEN: AtomicU64 = AtomicU64::new(0);

pub fn session_gen() -> u64 {
    SESSION_GEN.load(Ordering::Acquire)
}

/// Back at the title screen / a save loads: no session until the player is in a world
/// again (`release_title_hold`), so a new connection always meets the loaded save.
static TITLE_HOLD: AtomicBool = AtomicBool::new(false);
/// Connection counter shared by every `Net`: a rebuilt `Net` must not reuse an epoch the
/// world sync (world.rs, join.rs) has already seen, or the join snapshot is not resent.
static EPOCHS: AtomicU64 = AtomicU64::new(0);

/// items.rs saw the TitleStep / InGameStep being built: co-op ends for us (a clean quit,
/// the friend gets "left"); the items must be used again after loading. With
/// `auto_connect = 1` the old automatic session comes back once we are in a world.
pub fn on_title() {
    crate::optmenu::on_title();
    SESSION_GEN.fetch_add(1, Ordering::AcqRel);
    let was = INTENT.load(Ordering::Acquire);
    if was == INTENT_HOST || was == INTENT_JOIN {
        INTENT.store(INTENT_SOLO, Ordering::Release);
    }
    TITLE_HOLD.store(true, Ordering::Release);
    log::line("net: title screen / save load, co-op ended until the player is in a world again");
    settings::request_reconnect();
}

/// In a world again after the title: `auto_connect` may connect now.
pub fn release_title_hold() {
    if TITLE_HOLD.swap(false, Ordering::AcqRel) {
        log::line("net: in a world after the title");
        settings::request_reconnect();
    }
}

/// The intent an item set, None if no item was used yet.
pub fn intent() -> Option<Intent> {
    match INTENT.load(Ordering::Acquire) {
        INTENT_SOLO => Some(Intent::Solo),
        INTENT_HOST => Some(Intent::Host),
        INTENT_JOIN => Some(Intent::Join),
        _ => None,
    }
}

/// The session as the items see it (published by the live `Net` every frame).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    Off,
    /// Our world is open, nobody connected yet.
    Hosting,
    /// Looking for a host.
    Searching,
    HostConnected,
    ClientConnected,
}

static LINK: AtomicU8 = AtomicU8::new(0);

pub fn link() -> Link {
    match LINK.load(Ordering::Relaxed) {
        1 => Link::Hosting,
        2 => Link::Searching,
        3 => Link::HostConnected,
        4 => Link::ClientConnected,
        _ => Link::Off,
    }
}

fn publish_link(l: Link) {
    let v = match l {
        Link::Off => 0,
        Link::Hosting => 1,
        Link::Searching => 2,
        Link::HostConnected => 3,
        Link::ClientConnected => 4,
    };
    LINK.store(v, Ordering::Relaxed);
}

/// A peer address for the log: Steam users are shortened, nothing else is hidden.
fn peer_text(peer: &SocketAddr) -> String {
    match steam_id_of(peer) {
        Some(id) => format!("steam user ..{:04}", id % 10000),
        None => peer.to_string(),
    }
}

/// `ip` or `ip:port` (an IPv6 address needs the brackets with a port).
fn parse_udp_address(text: &str, default_port: u16) -> Option<SocketAddr> {
    let t = text.trim();
    t.parse::<SocketAddr>().ok().or_else(|| t.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, default_port)))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RoleState {
    /// Steam: the lobby search has not decided yet.
    Pending,
    Host,
    Client,
    /// Debug loopback: neither host nor client for the rest of the mod.
    Loopback,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transport {
    Udp,
    Steam,
}

/// The Steam bring-up and lobby of a session that runs over Steam.
struct SteamLink {
    lobby: Option<Lobby>,
    since: Instant,
    next_load: Instant,
    /// Why the Steam interfaces could not be found (yet).
    error: Option<String>,
}

/// An opened UDP session.
struct UdpOpen {
    session: Session,
    role: RoleState,
    upnp: Option<Arc<Mutex<UpnpState>>>,
    /// Joiner: the host's address (for the status line).
    peer: Option<SocketAddr>,
}

/// The display name: the setting, else the Steam name, else "Wolf".
fn display_name(cfg: &NetSettings, api: Option<&steam::Api>) -> String {
    let set = cfg.player_name.trim();
    if !set.is_empty() {
        return set.to_owned();
    }
    api.and_then(|a| a.persona_name()).unwrap_or_else(|| DEFAULT_NAME.to_owned())
}

/// Our uid in Hello over UDP: the same for this Windows user on this PC (a restarted game gets
/// its slot back), never 0. A Steam session sends the SteamID64 instead.
fn udp_uid() -> u64 {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let who = format!("{}|{}", var("COMPUTERNAME"), var("USERNAME"));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"sekiro-coop-uid\0".iter().chain(who.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h.max(1)
}

/// A UDP session on `bind`, with the echo peers' queues beside the socket when `hub` is set.
fn udp_session(bind: SocketAddr, role: Role, cfg: &NetSettings, name: &str, hub: Option<&Hub>) -> io::Result<Session> {
    let sock = UdpSocket::bind(bind)?;
    sock.set_nonblocking(true)?;
    let mut s = Session::with_transport(wrap(Box::new(sock), hub), role, &cfg.password, name);
    s.set_uid(udp_uid());
    Ok(s)
}

fn open_udp(cfg: &NetSettings, want: Want, name: &str, hub: Option<&Hub>) -> Result<UdpOpen, String> {
    let host = match want {
        Want::Host => true,
        Want::Join => false,
        Want::Auto => cfg.udp_host_address.trim().is_empty(),
    };
    if host {
        let listen = SocketAddr::from(([0, 0, 0, 0], cfg.udp_port));
        let session = udp_session(listen, Role::Host, cfg, name, hub)
            .map_err(|e| format!("could not open UDP port {}: {e}", cfg.udp_port))?;
        // Test builds: the friend's play.cmd downloads our DLL BEFORE his game
        // starts, so serve it from the moment we host, not once in the world
        // (live 2026-10-23: he launched while we were still loading).
        #[cfg(feature = "testsync")]
        crate::testsync::start_http_server();
        Ok(UdpOpen { session, role: RoleState::Host, upnp: Some(start_upnp(cfg.udp_port)), peer: None })
    } else {
        let host = parse_udp_address(&cfg.udp_host_address, cfg.udp_port)
            .ok_or_else(|| "udp_host_address is missing or not an IP address".to_owned())?;
        let any: SocketAddr = "0.0.0.0:0".parse().unwrap();
        // No echo peers on a joiner (they only run while we host).
        let session = udp_session(any, Role::Client { host }, cfg, name, None)
            .map_err(|e| format!("could not open a UDP socket: {e}"))?;
        Ok(UdpOpen { session, role: RoleState::Client, upnp: None, peer: Some(host) })
    }
}

/// What the loopback echo peer sends back after `LOOPBACK_DELAY`.
enum Echo {
    Snap(Snapshot),
    /// A raw game player packet (native remote over our socket, N1).
    Raw(u32, Vec<u8>),
}

/// Datagrams with the address they come from.
type Queue = Arc<Mutex<VecDeque<(Vec<u8>, SocketAddr)>>>;

/// Where the echo peers see our host session.
fn hub_host() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 77, 0, 0)), 1)
}

/// Echo peer i's own (made-up) address.
fn echo_addr(i: usize) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 77, 0, i as u8 + 1)), 1)
}

fn push(q: &Queue, data: &[u8], from: SocketAddr) {
    if let Ok(mut q) = q.lock()
        && q.len() < ECHO_QUEUE_MAX
    {
        q.push_back((data.to_vec(), from));
    }
}

fn pop(q: &Queue, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
    let (data, from) = q.lock().ok().and_then(|mut q| q.pop_front()).ok_or(io::ErrorKind::WouldBlock)?;
    let n = data.len().min(buf.len());
    buf[..n].copy_from_slice(&data[..n]);
    Ok((n, from))
}

/// `loopback_peers`: the in-memory queues between our session and the echo peers. Shared by
/// every transport our session gets (Steam bring-up, UDP fallback), so the echoes survive it.
#[derive(Clone)]
struct Hub {
    to_host: Queue,
    /// Echo i: its address and its inbox.
    echoes: Vec<(SocketAddr, Queue)>,
}

impl Hub {
    fn new(n: usize) -> Self {
        Self { to_host: Queue::default(), echoes: (0..n).map(|i| (echo_addr(i), Queue::default())).collect() }
    }
}

/// Our session's transport with the echo peers beside the real one.
struct HubTransport {
    inner: Box<dyn Datagram + Send>,
    hub: Hub,
}

impl Datagram for HubTransport {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        match self.hub.echoes.iter().find(|(a, _)| *a == to) {
            Some((_, q)) => {
                push(q, buf, hub_host());
                Ok(buf.len())
            }
            None => self.inner.send_to(buf, to),
        }
    }
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        pop(&self.hub.to_host, buf).or_else(|_| self.inner.recv_from(buf))
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// `sock`, with the hub's echo peers beside it if there is a hub.
fn wrap(sock: Box<dyn Datagram + Send>, hub: Option<&Hub>) -> Box<dyn Datagram + Send> {
    match hub {
        Some(h) => Box::new(HubTransport { inner: sock, hub: h.clone() }),
        None => sock,
    }
}

/// An echo peer's transport: everything goes to the host session.
struct EchoPort {
    me: SocketAddr,
    inbox: Queue,
    to_host: Queue,
}

impl Datagram for EchoPort {
    fn send_to(&self, buf: &[u8], _to: SocketAddr) -> io::Result<usize> {
        push(&self.to_host, buf, self.me);
        Ok(buf.len())
    }
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        pop(&self.inbox, buf)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.me)
    }
}

/// A client inside this game that sends our own snapshots and raw packets back `delay` later.
struct EchoPeer {
    session: Session,
    line: VecDeque<(Instant, Echo)>,
    delay: Duration,
    name: String,
    /// The last refusal logged (a full world refuses every Hello).
    said: Option<RejectReason>,
}

impl EchoPeer {
    /// Echo number `n` (its delay grows with it).
    fn new(session: Session, name: String, n: usize) -> Self {
        Self { session, line: VecDeque::new(), delay: LOOPBACK_DELAY + ECHO_DELAY_STEP * n as u32, name, said: None }
    }

    /// Echo number `n` on queue `j` of `hub`, as a client of the session at `hub_host()`.
    fn on_hub(hub: &Hub, j: usize, n: usize, cfg: &NetSettings) -> Self {
        let (me, inbox) = hub.echoes[j].clone();
        let port = EchoPort { me, inbox, to_host: hub.to_host.clone() };
        let name = format!("loopback-echo-{n}");
        let mut s = Session::with_transport(Box::new(port), Role::Client { host: hub_host() }, &cfg.password, &name);
        s.set_uid(ECHO_UID | n as u64);
        Self::new(s, name, n)
    }

    /// `n` echo peers on a new hub, numbered from `first`.
    fn hub_of(n: usize, first: usize, cfg: &NetSettings) -> (Option<Hub>, Vec<EchoPeer>) {
        if n == 0 {
            return (None, Vec::new());
        }
        let hub = Hub::new(n);
        let echoes = (0..n).map(|j| Self::on_hub(&hub, j, first + j, cfg)).collect();
        (Some(hub), echoes)
    }

    /// Receives our own snapshots (and raw game packets), sends them back `delay` later.
    fn tick(&mut self, now: Instant) {
        for e in self.session.poll(now) {
            match e {
                // Only ours (the host's): the other echoes' streams come relayed too.
                Event::Snapshot { from: 0, snap } => self.line.push_back((now + self.delay, Echo::Snap(snap))),
                Event::PlayerRaw { from: 0, ptype, data } if self.line.len() < 4096 => {
                    self.line.push_back((now + self.delay, Echo::Raw(ptype, data)))
                }
                Event::Connected { slot, .. } => {
                    self.said = None;
                    log::line(&format!("net: loopback peer {} (echo {} ms) joined as slot {slot}", self.name, self.delay.as_millis()));
                }
                Event::Rejected { reason } if self.said != Some(reason) => {
                    self.said = Some(reason);
                    log::line(&format!("net: loopback peer {} refused: {reason:?} (max_players too small?)", self.name));
                }
                _ => {}
            }
        }
        while self.line.front().is_some_and(|(due, _)| *due <= now) {
            match self.line.pop_front() {
                Some((_, Echo::Snap(s))) => self.session.send_snapshot(s, now),
                // To us only: we are the one whose snapshots ask for raw packets.
                Some((_, Echo::Raw(ptype, data))) => {
                    self.session.send_player_raw_to(0, ptype, &data, now);
                }
                None => break,
            }
        }
    }
}

/// Where a `send_*_to` goes (PLAN-MULTIPLAYER §5.3).
// Frozen API for the P1c module switch; parts are unused until then.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dest {
    /// Every connected peer.
    All,
    Peer(PeerId),
    /// The host (nothing on the host itself).
    Host,
}

impl Dest {
    fn slot(self) -> u8 {
        match self {
            Dest::All => DST_ALL,
            Dest::Peer(id) => id.slot(),
            Dest::Host => PeerId::HOST.slot(),
        }
    }
}

/// One remote peer: who it is, its link epoch and what it last showed us. Kept after the
/// link drops (the old single-friend accessors still read it), but `Net::peers()` only
/// lists connected ones.
pub struct PeerState {
    id: PeerId,
    name: String,
    epoch: u64,
    /// The player behind this slot (SteamID64; 0 = unknown, UDP). A different one in the same
    /// slot is a new player: nothing of the old one stays.
    uid: u64,
    rtt_ms: Option<u32>,
    /// Its link is up (synced with the session every frame).
    connected: bool,
    /// Latest snapshot, hidden ones included.
    remote: Option<(Instant, Snapshot)>,
    /// Last second of visible snapshots, for interpolation.
    history: VecDeque<(Instant, Snapshot)>,
    /// Last time the peer was on a loading screen (hidden snapshot): idol
    /// warps always have one, a pit fall-respawn does not.
    loading_at: Option<Instant>,
    /// Its visible snapshots read hp <= 0 since then (kept while nothing is visible).
    dead_since: Option<Instant>,
}

#[allow(dead_code)] // P1c API
impl PeerState {
    fn new(id: PeerId) -> Self {
        Self {
            id,
            name: String::new(),
            epoch: 0,
            uid: 0,
            rtt_ms: None,
            connected: false,
            remote: None,
            history: VecDeque::new(),
            loading_at: None,
            dead_since: None,
        }
    }

    /// A PeerJoined for this slot with the player's `uid`. True when a different player took the
    /// slot: nothing of the old one stays (position, history, loading and death marks), or the
    /// newcomer would be shown where the old player stood. An unknown uid (0, UDP) on either
    /// side cannot tell, so it keeps what is there.
    fn adopt_uid(&mut self, uid: u64) -> bool {
        let new_player = self.uid != 0 && uid != 0 && self.uid != uid;
        if new_player {
            self.remote = None;
            self.history.clear();
            self.loading_at = None;
            self.dead_since = None;
        }
        if uid != 0 {
            self.uid = uid;
        }
        new_player
    }

    /// A sane snapshot from this peer.
    fn on_snapshot(&mut self, now: Instant, s: Snapshot) {
        // A cutscene reads hidden too (the puppet hides), but it is not a loading screen:
        // `loaded_since` ends a peer's boss fight, a cutscene mid-fight must not. The loading
        // and death marks stay as they are meanwhile.
        if s.hidden {
            if !s.cutscene {
                self.loading_at = Some(now);
            }
        } else if s.hp <= 0 {
            self.dead_since.get_or_insert(now);
        } else {
            self.dead_since = None;
        }
        self.remote = Some((now, s));
        if s.hidden {
            self.history.clear();
        } else {
            self.history.push_back((now, s));
            while self.history.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(1)) {
                self.history.pop_front();
            }
        }
    }

    pub fn id(&self) -> PeerId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The link epoch (new on every (re)connection, unique across `Net`s).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn rtt_ms(&self) -> Option<u32> {
        self.rtt_ms
    }

    /// The peer's latest visible snapshot, if fresh.
    pub fn remote(&self) -> Option<Snapshot> {
        let (at, s) = self.remote?;
        (Instant::now().duration_since(at) < REMOTE_STALE_AFTER && !s.hidden).then_some(s)
    }

    /// The peer's latest snapshot (fresh, hidden or not) says it is in a cutscene.
    pub fn in_cutscene(&self) -> bool {
        self.remote.is_some_and(|(at, s)| s.cutscene && Instant::now().duration_since(at) < REMOTE_STALE_AFTER)
    }

    /// The peer's snapshot as of `delay_ms` ago (newest not newer than
    /// that), so motion plays in step with the delayed position.
    pub fn remote_delayed(&self, delay_ms: u32) -> Option<Snapshot> {
        self.remote()?;
        let t = Instant::now().checked_sub(Duration::from_millis(delay_ms as u64))?;
        self.history.iter().rev().find(|(at, _)| *at <= t).or(self.history.front()).map(|(_, s)| *s)
    }

    /// The peer's position and facing `delay_ms` in the past, interpolated
    /// between received snapshots (smooth at the cost of that much lag).
    pub fn remote_smoothed(&self, delay_ms: u32) -> Option<([f32; 3], f32)> {
        let s = self.remote()?;
        if delay_ms == 0 || self.history.len() < 2 {
            return Some((s.pos, s.angle));
        }
        let t = Instant::now().checked_sub(Duration::from_millis(delay_ms as u64))?;
        let mut prev: Option<&(Instant, Snapshot)> = None;
        for e in &self.history {
            if e.0 >= t {
                let Some(a) = prev else { return Some((e.1.pos, e.1.angle)) };
                if crate::game::distance(a.1.pos, e.1.pos) > TELEPORT_M {
                    return Some((e.1.pos, e.1.angle));
                }
                let span = e.0.duration_since(a.0).as_secs_f32().max(1e-4);
                let k = (t.duration_since(a.0).as_secs_f32() / span).clamp(0.0, 1.0);
                let p = [0, 1, 2].map(|i| a.1.pos[i] + (e.1.pos[i] - a.1.pos[i]) * k);
                use std::f32::consts::{PI, TAU};
                // rem_euclid, not a while loop: a huge angle would loop forever.
                let d = (e.1.angle - a.1.angle + PI).rem_euclid(TAU) - PI;
                let angle = (a.1.angle + d * k + PI).rem_euclid(TAU) - PI;
                return Some((p, angle));
            }
            prev = Some(e);
        }
        // Newer than everything we have (late packet): carry on with the
        // last velocity for at most EXTRAPOLATE_MAX, then hold.
        let n = self.history.len();
        let (t1, a) = &self.history[n - 2];
        let (t2, b) = &self.history[n - 1];
        let span = t2.duration_since(*t1).as_secs_f32();
        // No extrapolation out of a teleport: its "velocity" is hundreds of m/s.
        if span > 1e-3 && crate::game::distance(a.pos, b.pos) <= TELEPORT_M {
            let ahead = t.duration_since(*t2).as_secs_f32().min(EXTRAPOLATE_MAX);
            let p = [0, 1, 2].map(|i| b.pos[i] + (b.pos[i] - a.pos[i]) / span * ahead);
            return Some((p, b.angle));
        }
        Some((s.pos, s.angle))
    }

    /// True if the peer had a loading screen within `secs` seconds.
    pub fn loaded_recently(&self, secs: u64) -> bool {
        self.loading_at.is_some_and(|t| t.elapsed().as_secs() < secs)
    }

    /// True if the peer had a loading screen after `t`.
    pub fn loaded_since(&self, t: Instant) -> bool {
        self.loading_at.is_some_and(|l| l > t)
    }

    /// The peer's last loading-screen instant (a hidden snapshot), if any.
    pub fn loading_at(&self) -> Option<Instant> {
        self.loading_at
    }

    pub fn dead_since(&self) -> Option<Instant> {
        self.dead_since
    }

    /// The peer shows us as a native remote: its snapshots ask for our raw player packets.
    pub fn wants_raw(&self) -> bool {
        self.wants_raw_at(Instant::now())
    }

    fn wants_raw_at(&self, now: Instant) -> bool {
        self.remote.is_some_and(|(at, s)| now.duration_since(at) < Duration::from_secs(2) && s.want_raw)
    }
}

pub struct Net {
    session: Session,
    /// Debug loopback / `loopback_peers`: the in-process echo peers (polled while we host).
    echoes: Vec<EchoPeer>,
    /// `loopback_peers`: the queues our session's transport shares with the echo peers.
    hub: Option<Hub>,
    last_send: Instant,
    /// Remote peers by `PeerId::index()` (connected or kept from an earlier link).
    slots: [Option<PeerState>; MAX_PLAYERS],
    /// The peer that connected last: the old single-friend accessors read it while nobody
    /// is connected.
    last_peer: Option<PeerId>,
    hkb_inbox: Vec<(PeerId, coop_net::HkbEvent)>,
    role: RoleState,
    /// What the items (or `auto_connect`) asked for: host, join or the old auto role.
    want: Want,
    transport: Transport,
    cfg: NetSettings,
    /// Steam transport: bring-up and lobby (None for UDP).
    steam: Option<SteamLink>,
    /// UDP joiner: the host's address, for the status line.
    udp_peer: Option<SocketAddr>,
    /// Shown on the status line when the session could not be opened.
    error: Option<String>,
    /// Joiner: Steam found no host in time and there is no `udp_host_address` to fall back
    /// to. Steam keeps searching; the status line says how to join over UDP instead (1.0.3:
    /// before that it said "searching" forever, Nexus report 2026-10-30).
    join_needs_address: bool,
    /// The name we send to the friend.
    own_name: String,
    /// UDP host only: UPnP result, filled by a background thread (can take seconds).
    upnp: Option<Arc<Mutex<UpnpState>>>,
    /// New on every (re)connection (unique across `Net`s, `EPOCHS`); the client re-runs
    /// the join and the host resends the join snapshot then.
    epoch: u64,
    /// This `Net` was connected at least once.
    connected_once: bool,
    // Inboxes: every entry says which peer it came from.
    /// Received this frame, drained by the enemy sync.
    enemy_inbox: Vec<(PeerId, coop_net::EnemyState)>,
    damage_inbox: Vec<(PeerId, coop_net::DamageReport)>,
    /// Host posture states (posture.rs), cleared every frame like the enemy states.
    posture_inbox: Vec<(PeerId, coop_net::PostureState)>,
    /// Reliable game events (rest, flags, boss) received this frame.
    game_inbox: Vec<(PeerId, u8, i32, i32)>,
    /// Host-driven enemy records received this frame (cleared like the
    /// enemy inbox).
    netai_inbox: Vec<(PeerId, coop_net::NetAiEntry)>,
    /// Complete bulk transfers (from, kind, data) of the current links. Kept until
    /// taken (the world dump waits for a frame in the world).
    bulk_inbox: Vec<(PeerId, u8, Vec<u8>)>,
    /// Bytes of NetAI packets sent (host stats).
    netai_bytes: u64,
    /// The last refusal shown to us (client), so a repeated Reject is not shown every 500 ms.
    reject_notice: Option<(RejectReason, Instant)>,
    /// Host: refusals of joiners logged / shown, with when.
    refusals_said: Vec<((RejectReason, String), Instant)>,
    /// Host: when the relay totals were last logged, and the totals then.
    relay_log: (Instant, (u64, u64)),
}

#[derive(Default)]
pub enum UpnpState {
    #[default]
    Working,
    Mapped(Mapping),
    Failed(String),
}

impl Net {
    /// Returns `None` when networking is off or misconfigured (logged, and `offline_status` says why).
    pub fn from_config() -> Option<Self> {
        let cfg = settings::net();
        publish_link(Link::Off);
        // The items decide; with no item used yet `auto_connect = 1` gives the old auto role.
        let want = match INTENT.load(Ordering::Acquire) {
            INTENT_HOST => Some(Want::Host),
            INTENT_JOIN => Some(Want::Join),
            INTENT_UNSET if cfg.auto_connect => Some(Want::Auto),
            _ => None,
        };
        if cfg.password.is_empty() && (want.is_some() || cfg.loopback) {
            set_offline("password not set - edit sekiro_coop_settings.ini");
            return None;
        }
        if cfg.loopback {
            return Self::loopback(cfg);
        }
        let want = want.filter(|_| !TITLE_HOLD.load(Ordering::Acquire));
        let Some(want) = want else {
            set_offline("solo, waiting for a co-op item");
            SOLO.store(true, Ordering::Relaxed);
            return None;
        };
        // `loopback_peers`: echo peers for a world we may host (they run only while we do).
        let (hub, echoes) = match want {
            Want::Join => (None, Vec::new()),
            _ => EchoPeer::hub_of(cfg.loopback_peers as usize, 0, &cfg),
        };
        if !echoes.is_empty() {
            log::line(&format!("net: {} loopback peer(s) will join while we host (loopback_peers)", echoes.len()));
        }
        if cfg.transport == TransportSetting::Udp {
            // The Steam name is only looked up when no name is set.
            let steam_api = if cfg.player_name.trim().is_empty() { steam::Api::load().ok() } else { None };
            let name = display_name(&cfg, steam_api.as_deref());
            return match open_udp(&cfg, want, &name, hub.as_ref()) {
                Ok(u) => {
                    log::line(&format!("net: UDP, role {:?} ({want:?}), name {:?}", u.role, name));
                    let mut net = Self::from_udp(cfg, u, name);
                    net.want = want;
                    net.hub = hub;
                    net.echoes = echoes;
                    Some(net)
                }
                Err(e) => {
                    log::line(&format!("net: {e}"));
                    set_offline(&e);
                    None
                }
            };
        }
        // Steam (or auto): the session is a placeholder until the Steam interfaces are found.
        let now = Instant::now();
        log::line(&format!("net: Steam transport, role {want:?} (transport {:?})", cfg.transport));
        let name = display_name(&cfg, None);
        let placeholder = Session::with_transport(wrap(Box::new(Inert), hub.as_ref()), Role::Host, &cfg.password, &name);
        let mut net = Self::base(placeholder, cfg);
        net.want = want;
        net.own_name = name;
        net.transport = Transport::Steam;
        net.role = RoleState::Pending;
        net.steam = Some(SteamLink { lobby: None, since: now, next_load: now, error: None });
        net.hub = hub;
        net.echoes = echoes;
        Some(net)
    }

    fn base(session: Session, cfg: NetSettings) -> Self {
        let now = Instant::now();
        Self {
            session,
            echoes: Vec::new(),
            hub: None,
            reject_notice: None,
            refusals_said: Vec::new(),
            relay_log: (now, (0, 0)),
            last_send: now,
            slots: Default::default(),
            last_peer: None,
            role: RoleState::Pending,
            want: Want::Auto,
            transport: Transport::Udp,
            cfg,
            steam: None,
            udp_peer: None,
            error: None,
            join_needs_address: false,
            own_name: DEFAULT_NAME.to_owned(),
            upnp: None,
            epoch: EPOCHS.load(Ordering::Acquire),
            connected_once: false,
            enemy_inbox: Vec::new(),
            hkb_inbox: Vec::new(),
            damage_inbox: Vec::new(),
            posture_inbox: Vec::new(),
            game_inbox: Vec::new(),
            netai_inbox: Vec::new(),
            bulk_inbox: Vec::new(),
            netai_bytes: 0,
        }
    }

    fn from_udp(cfg: NetSettings, u: UdpOpen, name: String) -> Self {
        let mut net = Self::base(u.session, cfg);
        net.own_name = name;
        net.role = u.role;
        net.upnp = u.upnp;
        net.udp_peer = u.peer;
        net
    }

    /// Debug: host and an echo client in this game (see the module doc). With
    /// `loopback_peers = k > 1` the other k - 1 echo peers join over the hub.
    fn loopback(cfg: NetSettings) -> Option<Self> {
        let local: SocketAddr = format!("127.0.0.1:{}", cfg.udp_port).parse().unwrap();
        let (hub, extra) = EchoPeer::hub_of((cfg.loopback_peers as usize).saturating_sub(1), 1, &cfg);
        let made = udp_session(local, Role::Host, &cfg, "loopback-host", hub.as_ref()).and_then(|h| {
            let peer = Session::new("127.0.0.1:0".parse().unwrap(), Role::Client { host: local }, &cfg.password, "loopback-echo")?;
            Ok((h, peer))
        });
        match made {
            Ok((host, mut peer)) => {
                log::line(&format!("net: loopback on {local}, {} echo peer(s)", 1 + extra.len()));
                peer.set_uid(ECHO_UID);
                let mut net = Self::base(host, cfg);
                net.role = RoleState::Loopback;
                net.echoes = vec![EchoPeer::new(peer, "loopback-echo".into(), 0)];
                net.echoes.extend(extra);
                net.hub = hub;
                Some(net)
            }
            Err(e) => {
                log::line(&format!("net: could not open socket: {e}"));
                set_offline(&format!("could not open the loopback socket: {e}"));
                None
            }
        }
    }

    /// Steam bring-up, lobby and UDP fallback. Once per frame.
    fn steam_tick(&mut self, now: Instant) {
        let Some(mut link) = self.steam.take() else { return };
        let connected = self.connected();
        // The Steam interfaces appear once the game has logged in; keep looking.
        if link.lobby.is_none() && now >= link.next_load {
            match steam::Api::load() {
                Ok(api) => {
                    let name = display_name(&self.cfg, Some(&api));
                    let sock = wrap(Box::new(steam::SteamTransport::new(api.clone())), self.hub.as_ref());
                    self.session = Session::with_transport(sock, Role::Host, &self.cfg.password, &name);
                    self.session.set_uid(api.me);
                    let want = self.want;
                    log::line(&format!("net: Steam ready, name {name:?}, lobby role {want:?}"));
                    self.own_name = name;
                    let size = crate::tuning::max_players() as i32;
                    link.lobby = Some(Lobby::new(api, &self.cfg.password, want, size));
                    link.error = None;
                }
                Err(e) => {
                    if link.error.as_deref() != Some(e.as_str()) {
                        log::line(&format!("net: Steam interfaces not available yet: {e}"));
                    }
                    link.error = Some(e);
                    link.next_load = now + STEAM_RETRY_EVERY;
                }
            }
        }
        if let Some(lobby) = link.lobby.as_mut() {
            // max_players is live: a lobby we host follows it (Steam fixes the size at creation).
            lobby.set_max_members(crate::tuning::max_players() as i32);
            // The Steam users we have a live link to (uid = SteamID64 from the address).
            let linked: Vec<u64> = self.session.peers().into_iter().map(|(_, uid, _)| uid).filter(|&u| u != 0).collect();
            let out = lobby.tick(connected, &linked);
            if lobby.take_full() {
                self.notify_reject(RejectReason::Full, now);
            }
            if lobby.other_version() && !connected {
                self.notify_reject(RejectReason::Version, now);
            }
            match out {
                Some(Out::Hosting) => {
                    self.role = RoleState::Host;
                    self.session.set_role(Role::Host);
                    log::line("net: hosting through a Steam lobby");
                }
                Some(Out::JoinedHost(id)) => {
                    self.role = RoleState::Client;
                    self.session.set_role(Role::Client { host: steam_addr(id) });
                    log::line("net: joined a Steam lobby, connecting to its host");
                }
                Some(Out::HostLeft) => {
                    self.role = RoleState::Pending;
                    self.session.set_role(Role::Host);
                    log::line("net: left the Steam lobby, searching again");
                    // Joined with the item and the host's lobby is gone: its world is closed.
                    self.host_closed();
                }
                None => {}
            }
        }
        let settled = link.lobby.as_ref().is_some_and(|l| l.ready());
        if self.cfg.transport == TransportSetting::Auto && !settled && now.duration_since(link.since) >= STEAM_FALLBACK_AFTER {
            let why = link.lobby.as_ref().map(|l| l.status()).or(link.error.clone()).unwrap_or_default();
            if self.fall_back_to_udp(&why) {
                return; // `link` (and its lobby search) is dropped here
            }
            // Joining by role without an address: UDP is no way out, Steam keeps trying.
            link.since = now + Duration::from_secs(3600);
        }
        self.steam = Some(link);
    }

    /// Steam did not work out in time: the same role over UDP. False if that is not possible.
    fn fall_back_to_udp(&mut self, why: &str) -> bool {
        let name = self.own_name.clone();
        match open_udp(&self.cfg, self.want, &name, self.hub.as_ref()) {
            Ok(u) => {
                log::line(&format!("net: Steam not usable within {} s ({why}), falling back to UDP as {:?}", STEAM_FALLBACK_AFTER.as_secs(), u.role));
                self.session = u.session;
                self.role = u.role;
                self.upnp = u.upnp;
                self.udp_peer = u.peer;
                self.transport = Transport::Udp;
                true
            }
            Err(e) => {
                if self.want != Want::Join {
                    log::line(&format!("net: UDP fallback failed: {e}"));
                    self.error = Some(format!("Steam not usable and UDP failed: {e}"));
                } else if !self.join_needs_address {
                    log::line(&format!("net: no host found over Steam within {} s ({why}) and no UDP address set ({e}): still searching Steam", STEAM_FALLBACK_AFTER.as_secs()));
                    self.join_needs_address = true;
                }
                false
            }
        }
    }

    /// We joined with Kindred Charm and the host closed its world (Bye, or its Steam
    /// lobby is gone): co-op ends for us too, we stay in our own world. Only after we
    /// were connected once; a lobby that never answered is searched again instead.
    fn host_closed(&mut self) {
        if self.want != Want::Join || !self.connected_once || INTENT.load(Ordering::Acquire) != INTENT_JOIN {
            return;
        }
        let host = self.peer_name().map(str::to_owned).unwrap_or_else(|| "The host".into());
        log::line(&format!("net: {host} closed their world, ending co-op"));
        crate::dialog::game_notice(format!("{host} closed their world. Co-op has ended."), crate::dialog::Importance::Membership, 8);
        set_intent(Intent::Solo);
    }

    /// Call every frame with the local player (None while loading/in menus).
    pub fn on_frame(&mut self, player: Option<Chr>, anim: i32, motion: &mut crate::motion::Motion) {
        let now = Instant::now();
        self.steam_tick(now);

        self.hkb_inbox.clear();
        // Enemy states are consumed in this same frame (enemies.rs runs after
        // us) or are stale anyway; without this the inbox grew ~20 KB/s
        // forever whenever the client had enemy sync off or sat in a menu.
        self.enemy_inbox.clear();
        self.netai_inbox.clear();
        self.posture_inbox.clear();
        // Host: how many clients may join, and how often one client's snapshots go on to the others.
        self.session.set_max_peers(crate::tuning::max_players().saturating_sub(1));
        self.session.set_relay_snapshot_hz(crate::tuning::relay_snapshot_hz());
        for e in self.session.poll(now) {
            self.on_event(e, now);
        }
        self.host_housekeeping(now);
        // Links can also drop without an event (Steam `set_role`): follow the session.
        for p in self.slots.iter_mut().flatten() {
            p.connected = self.session.link_state(p.id.slot()) == Some(coop_net::State::Connected);
            p.rtt_ms = self.session.peer_rtt_ms(p.id.slot());
        }

        // Native remote (N1): our player's own game packets go to each peer
        // only while it asks for them (its snapshots' want_raw).
        let mut mask = 0u8;
        for p in self.peers().filter(|p| p.wants_raw_at(now)) {
            mask |= 1 << p.id.slot();
        }
        let forward = mask != 0;
        crate::native::set_forward(mask);
        for (ptype, data) in crate::native::take_outbound() {
            if forward {
                self.send_raw_masked(mask, ptype, &data, now);
            }
        }
        // Our combat art / prosthetic bullets for the peers' native remotes of us (artfx.rs).
        for data in crate::artfx::take_outbound(forward) {
            self.send_raw_masked(mask, crate::artfx::PTYPE, &data, now);
        }
        // Native session (nsession.rs): sends nothing unless native_session = 1 queued something.
        crate::nsession::pump(&mut self.session, now);

        // One peer: every frame (v7.5). More: `snapshot_hz_multi` (each goes to all of them).
        let every = match self.peer_count() {
            0 | 1 => SEND_EVERY,
            _ => Duration::from_micros(1_000_000 / crate::tuning::snapshot_hz_multi().max(1) as u64),
        };
        if now.duration_since(self.last_send) >= every {
            self.last_send += every;
            if now.duration_since(self.last_send) > 3 * every {
                self.last_send = now; // after a hitch: no burst
            }
            // A half-built player (first frames after a load) can read as
            // garbage: send "hidden" rather than a NaN/huge position the
            // friend would put on the ghost.
            static LOGGED_MAX_HP: AtomicBool = AtomicBool::new(false);
            if let Some(m) = player.and_then(|p| p.max_hp()).filter(|&m| m > 0)
                && !LOGGED_MAX_HP.swap(true, Ordering::Relaxed)
            {
                log::line(&format!("party: our max HP reads {m}"));
            }
            let mut snap = match player
                .and_then(|p| Some((p.position()?, p.angle()?, p.hp()?)))
                .filter(|&(pos, angle, _)| crate::game::sane_pos(pos) && crate::game::sane_angle(angle))
            {
                Some((pos, angle, hp)) => Snapshot {
                    pos,
                    angle,
                    hp,
                    max_hp: player.and_then(|p| p.max_hp()).unwrap_or(0),
                    anim,
                    block: crate::game::current_block().unwrap_or(-1),
                    ..Default::default()
                },
                None => Snapshot { hidden: true, ..Default::default() },
            };
            if let Some(p) = player
                && !snap.hidden
                && crate::tuning::ghost_motion()
            {
                motion.capture(p, &mut snap);
            }
            // A cutscene moves the player by script: the others hide our puppet meanwhile
            // (hidden, which a loading screen reads like), and the party list says why.
            if player.is_some() && !snap.hidden && crate::cutscene::active() {
                snap.hidden = true;
                snap.cutscene = true;
            }
            snap.want_raw = crate::tuning::native_remote();
            self.session.send_snapshot(snap, now);
        }

        // Echo peers: receive our own snapshots (and raw game packets), send them back later.
        // Only while we host (a Steam session decides its role after start).
        if matches!(self.role, RoleState::Host | RoleState::Loopback) {
            for e in &mut self.echoes {
                e.tick(now);
            }
        }
        publish_link(self.link_state());
    }

    /// Host: refused joiners (log, a notice for a full world / another version) and the
    /// relay totals every `RELAY_LOG_EVERY`.
    fn host_housekeeping(&mut self, now: Instant) {
        self.refusals_said.retain(|(_, t)| now.duration_since(*t) < REJECT_NOTICE_EVERY);
        for (reason, name) in self.session.take_refusals() {
            let key = (reason, name);
            if self.refusals_said.iter().any(|(k, _)| *k == key) {
                continue;
            }
            let (reason, name) = &key;
            let who = if name.is_empty() { "a player".to_owned() } else { format!("{name:?}") };
            log::line(&format!("net: refused a join from {who}: {reason:?}"));
            let n = crate::tuning::max_players();
            let shown = if name.is_empty() { "A player" } else { name.as_str() };
            let text = match reason {
                RejectReason::Full => Some(format!("{shown} tried to join, but your world is full ({n}/{n}).")),
                RejectReason::Version => Some("A player with another co-op version tried to join your world.".to_owned()),
                // Wrong passwords stay in the log (anyone can send one).
                RejectReason::Password => None,
            };
            if let Some(text) = text {
                crate::dialog::game_notice(text, crate::dialog::Importance::Membership, 6);
            }
            self.refusals_said.push((key, now));
        }
        if now.duration_since(self.relay_log.0) >= RELAY_LOG_EVERY {
            let (packets, bytes) = self.session.relay_stats();
            let (p0, b0) = self.relay_log.1;
            if packets > p0 {
                log::line(&format!(
                    "net: relayed {} datagrams ({} KB) between clients in the last {} s",
                    packets - p0,
                    (bytes - b0) / 1024,
                    RELAY_LOG_EVERY.as_secs()
                ));
            }
            self.relay_log = (now, (packets, bytes));
        }
    }

    /// We were refused (or the lobby is full): one notice per reason per `REJECT_NOTICE_EVERY`.
    fn notify_reject(&mut self, reason: RejectReason, now: Instant) {
        if self.reject_notice.is_some_and(|(r, t)| r == reason && now.duration_since(t) < REJECT_NOTICE_EVERY) {
            return;
        }
        self.reject_notice = Some((reason, now));
        let (log_text, notice) = match reason {
            RejectReason::Password => (
                "net: host rejected the password",
                "The host refused the password. Both players need the same password in sekiro_coop_settings.ini.",
            ),
            RejectReason::Full => ("net: the host's world is full", "The host's world is full. Trying again..."),
            RejectReason::Version => (
                "net: the host runs another co-op version",
                "The host runs another version of the co-op mod. Both players need the same version.",
            ),
        };
        log::line(log_text);
        crate::dialog::game_notice(notice.to_owned(), crate::dialog::Importance::Membership, 8);
    }

    /// The session for items.rs (`link`).
    fn link_state(&self) -> Link {
        let host = match self.role {
            RoleState::Host | RoleState::Loopback => true,
            RoleState::Client => false,
            RoleState::Pending => self.want == Want::Host,
        };
        match (self.connected(), host) {
            (true, true) => Link::HostConnected,
            (true, false) => Link::ClientConnected,
            (false, true) => Link::Hosting,
            (false, false) => Link::Searching,
        }
    }

    pub fn is_client(&self) -> bool {
        self.role == RoleState::Client
    }

    /// The newest link epoch (client: the host link's; compat for the one friend).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// One event of `Session::poll`.
    fn on_event(&mut self, e: Event, now: Instant) {
        // Every data event names its sender's slot; a slot out of range is not a peer.
        let peer = |slot: u8| PeerId::new(slot);
        match e {
            Event::Snapshot { from, snap: s } => {
                let Some(id) = peer(from) else { return };
                // Garbage from a peer (older build, half-loaded player) never
                // reaches the ghost, the join teleport or the nameplate:
                // everything downstream reads `remote`.
                if !s.hidden && !(crate::game::sane_pos(s.pos) && crate::game::sane_angle(s.angle)) {
                    return;
                }
                self.slot_mut(id).on_snapshot(now, s);
            }
            Event::Connected { peer, name, .. } => {
                self.epoch = EPOCHS.fetch_add(1, Ordering::AcqRel) + 1;
                self.connected_once = true;
                log::line(&format!("net: connected to {name:?} at {}", peer_text(&peer)));
            }
            // Comes right after `Connected` (host: the new client; client: the host).
            Event::PeerJoined { slot, uid, name } => {
                let Some(id) = peer(slot) else { return };
                // A dump from the previous link is not this peer's current world, and one
                // from a slot no longer linked (Steam: the lobby role switched within this
                // Net) belongs to nobody here. One link (P1): every queued bulk goes, as v7.5
                // cleared them all on `Connected`.
                let session = &self.session;
                self.bulk_inbox.retain(|(from, _, _)| {
                    *from != id && session.link_state(from.slot()) == Some(coop_net::State::Connected)
                });
                let relayed = self.role == RoleState::Client && slot != 0;
                // A client's relayed peer has no link of its own to bump the epoch: every PeerJoined gets
                // a fresh one, so the trackers (hkbsync, enemies, peers) see a rejoin of its slot.
                let epoch = if relayed { EPOCHS.fetch_add(1, Ordering::AcqRel) + 1 } else { self.epoch };
                if relayed {
                    log::line(&format!("net: {name:?} (slot {slot}) is in the host's world too"));
                }
                let p = self.slot_mut(id);
                if p.adopt_uid(uid) {
                    log::line(&format!("net: slot {slot} is now a different player ({name:?}): its old position and marks are dropped"));
                }
                p.name = name;
                p.epoch = epoch;
                p.connected = true;
                p.dead_since = None;
                self.last_peer = Some(id);
            }
            Event::PeerLeft { slot, why } => {
                if let Some(p) = peer(slot).and_then(|id| self.slots[id.index()].as_mut()) {
                    p.connected = false;
                    p.dead_since = None;
                }
                if why == LeaveWhy::Replaced {
                    log::line(&format!("net: slot {slot} taken over by a new connection"));
                } else if self.role == RoleState::Client && slot != 0 {
                    log::line(&format!("net: slot {slot} left the host's world ({why:?})"));
                }
            }
            Event::Roster(_) => {}
            Event::Lost { .. } => log::line("net: peer silent, waiting for it to come back"),
            Event::PeerQuit { .. } => {
                log::line("net: peer quit");
                if self.role == RoleState::Client {
                    self.host_closed();
                }
            }
            Event::Rejected { reason } => self.notify_reject(reason, now),
            Event::Enemies { from, list } => {
                let Some(id) = peer(from) else { return };
                self.enemy_inbox.extend(list.into_iter().map(|s| (id, s)));
            }
            Event::Damage { from, report } => {
                let Some(id) = peer(from) else { return };
                // Totals are cumulative: dropping the oldest loses nothing
                // a newer packet does not carry.
                if self.damage_inbox.len() >= MAX_DAMAGE_INBOX {
                    self.damage_inbox.remove(0);
                }
                self.damage_inbox.push((id, report));
            }
            Event::Posture { from, list } => {
                let Some(id) = peer(from) else { return };
                let room = MAX_DAMAGE_INBOX.saturating_sub(self.posture_inbox.len());
                self.posture_inbox.extend(list.into_iter().take(room).map(|s| (id, s)));
            }
            Event::Hkb { from, list } => {
                let Some(id) = peer(from) else { return };
                self.hkb_inbox.extend(list.into_iter().map(|h| (id, h)));
            }
            Event::NetAi { from, list } => {
                let Some(id) = peer(from) else { return };
                self.netai_inbox.extend(list.into_iter().map(|n| (id, n)));
            }
            // Native session messages (nsession.rs): handshake and the game's packets.
            Event::Bulk { from, kind, data } if kind == crate::nsession::BULK_KIND => {
                let Some(id) = peer(from) else { return };
                crate::nsession::on_bulk(id, &data, self.epoch_of(from))
            }
            Event::Bulk { from, kind, data } => {
                let Some(id) = peer(from) else { return };
                // The auditor's boss HP samples come at 2 Hz: audit.rs logs what matters.
                if kind != crate::audit::BULK_AUDIT {
                    log::line(&format!("net: bulk transfer kind {kind} received, {} bytes", data.len()));
                }
                if self.bulk_inbox.len() >= 24 {
                    self.bulk_inbox.remove(0);
                }
                self.bulk_inbox.push((id, kind, data));
            }
            Event::Game { from, kind, a, b } => {
                let Some(id) = peer(from) else { return };
                if self.game_inbox.len() >= MAX_GAME_INBOX {
                    self.game_inbox.remove(0);
                }
                self.game_inbox.push((id, kind, a, b));
            }
            Event::PlayerRaw { from, ptype, data } => {
                let Some(id) = peer(from) else { return };
                let epoch = self.epoch_of(from);
                if !crate::nsession::on_raw(id, ptype, &data, epoch) && !crate::artfx::on_raw(id, ptype, &data) {
                    crate::native::push_inbound(id, ptype, data)
                }
            }
        }
    }

    /// The record of `id`, made on first use.
    fn slot_mut(&mut self, id: PeerId) -> &mut PeerState {
        self.slots[id.index()].get_or_insert_with(|| PeerState::new(id))
    }

    /// The link epoch of a sender slot (the newest epoch if it has no record).
    fn epoch_of(&self, slot: u8) -> u64 {
        PeerId::new(slot).and_then(|id| self.slots[id.index()].as_ref()).map_or(self.epoch, |p| p.epoch)
    }

    /// One raw player packet to every slot whose bit is set in `mask`.
    fn send_raw_masked(&mut self, mask: u8, ptype: u32, data: &[u8], now: Instant) {
        for slot in 0..MAX_PLAYERS as u8 {
            if mask & (1 << slot) != 0 {
                self.session.send_player_raw_to(slot, ptype, data, now);
            }
        }
    }

    /// Our session slot: host 0. A client reports the slot it gets in P1 (1) before the
    /// host's first Welcome.
    #[allow(dead_code)] // P1c API
    pub fn me(&self) -> PeerId {
        PeerId::new(self.session.me()).unwrap_or(PeerId(1))
    }

    /// Connected peers, in id order.
    pub fn peers(&self) -> impl Iterator<Item = &PeerState> + '_ {
        self.slots.iter().flatten().filter(|p| p.connected)
    }

    /// A connected peer.
    #[allow(dead_code)] // P1c API
    pub fn peer(&self, id: PeerId) -> Option<&PeerState> {
        self.slots.get(id.index())?.as_ref().filter(|p| p.connected)
    }

    /// The first connected peer: the 2-player "friend" (compat).
    pub fn friend(&self) -> Option<&PeerState> {
        self.peers().next()
    }

    #[allow(dead_code)] // P1c API
    pub fn peer_count(&self) -> usize {
        self.peers().count()
    }

    #[allow(dead_code)] // P1c API
    pub fn peer_epoch(&self, id: PeerId) -> Option<u64> {
        self.peer(id).map(PeerState::epoch)
    }

    /// What the single-friend accessors read: the friend, else the peer that connected
    /// last (its name and last snapshots stay readable after the link drops, as before).
    fn compat(&self) -> Option<&PeerState> {
        self.friend().or_else(|| self.slots[self.last_peer?.index()].as_ref())
    }

    // Single-friend accessors (compat until P1d).
    pub fn remote(&self) -> Option<Snapshot> {
        self.compat()?.remote()
    }

    pub fn remote_delayed(&self, delay_ms: u32) -> Option<Snapshot> {
        self.compat()?.remote_delayed(delay_ms)
    }

    pub fn remote_smoothed(&self, delay_ms: u32) -> Option<([f32; 3], f32)> {
        self.compat()?.remote_smoothed(delay_ms)
    }

    pub fn take_hkb_from(&mut self) -> Vec<(PeerId, coop_net::HkbEvent)> {
        std::mem::take(&mut self.hkb_inbox)
    }

    #[allow(dead_code)] // All wrapper (frozen API); hkbsync picks All / Host since P6
    pub fn send_hkb(&mut self, list: &[coop_net::HkbEvent]) {
        self.send_hkb_to(Dest::All, list);
    }

    pub fn send_hkb_to(&mut self, d: Dest, list: &[coop_net::HkbEvent]) {
        self.session.send_hkb_to(d.slot(), list, Instant::now());
    }
}

impl Net {
    /// One line for the status overlay (the overlay adds "Co-op: ").
    pub fn status_text(&self) -> String {
        use coop_net::State;
        let age = self.compat().and_then(|p| p.remote).map(|(at, _)| Instant::now().duration_since(at).as_millis());
        let label = match self.transport {
            Transport::Udp => "UDP",
            Transport::Steam => "Steam",
        };
        match self.session.state() {
            // 3+ players: every peer's name (and ping where it has its own link).
            State::Connected if self.peer_count() > 1 => {
                let list: Vec<String> = self
                    .peers()
                    .map(|p| {
                        let name = if p.name().is_empty() { "?" } else { p.name() };
                        p.rtt_ms().map_or_else(|| name.to_owned(), |r| format!("{name} {r} ms"))
                    })
                    .collect();
                format!("{} players: you, {} ({label})", 1 + list.len(), list.join(", "))
            }
            // No per-packet numbers here: they changed every frame and made the
            // auto-sized status window jitter (screenshot 2026-10-22).
            State::Connected => format!(
                "connected to {} ({label}{}){}",
                self.peer_name().unwrap_or("?"),
                self.session.rtt_ms().map_or(String::new(), |r| format!(", {r} ms")),
                if age.is_some_and(|a| a > 1000) { " (no data)" } else { "" }
            ),
            State::Waiting => self.waiting_text(),
            State::Lost => "connection lost, reconnecting...".into(),
        }
    }

    fn waiting_text(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        if let Some(link) = &self.steam {
            let in_lobby = link.lobby.as_ref().is_some_and(|l| l.ready());
            if self.join_needs_address && !in_lobby {
                return "host not found over Steam - to join over UDP, put the host's join code \
                        (udp_join_code.txt) in udp_host_address in sekiro_coop_settings.ini"
                    .into();
            }
            return match (&link.lobby, &link.error) {
                (Some(lobby), _) if lobby.ready() && self.role == RoleState::Host => {
                    "hosting - waiting for a companion (Steam)".into()
                }
                (Some(lobby), _) if self.want == Want::Join && !lobby.ready() && lobby.status().starts_with("searching") => {
                    "searching for host... (Steam)".into()
                }
                (Some(lobby), _) => lobby.status(),
                (None, Some(e)) => format!("waiting for Steam ({e})"),
                (None, None) => "connecting to Steam...".into(),
            };
        }
        match self.role {
            RoleState::Host => self.join_info().unwrap_or_else(|| "hosting - waiting for a companion".into()),
            RoleState::Client => format!("searching for host at {} (UDP)...", self.udp_peer.map_or_else(String::new, |a| a.to_string())),
            RoleState::Loopback => "loopback test, waiting for the echo peer".into(),
            RoleState::Pending => "starting...".into(),
        }
    }

    /// Clean shutdown before settings are re-applied or an item ends the session. The
    /// Bye is unreliable: sent three times so a UDP friend hears it ("left" at once).
    pub fn quit(&mut self) {
        let now = Instant::now();
        for _ in 0..3 {
            self.session.quit(now);
        }
    }
}

/// Opens the host's UDP port on the router (UPnP) without blocking the game.
fn start_upnp(port: u16) -> Arc<Mutex<UpnpState>> {
    let state = Arc::new(Mutex::new(UpnpState::Working));
    let out = state.clone();
    std::thread::spawn(move || {
        // Lease 0 = until removed; we remove it when the session closes.
        let result = upnp::map_udp_port(port, "Sekiro Co-op", 0);
        let new = match result {
            Ok(m) => {
                #[cfg(feature = "testsync")]
                {
                    let r = m.add_extra(crate::testsync::HTTP_PORT, "TCP", "Sekiro Co-op test update");
                    log::line(&format!("upnp: TCP {} for play.cmd updates: {r:?}", crate::testsync::HTTP_PORT));
                }
                log::line(&format!(
                    "upnp: mapped UDP {port}, join code {} (local {}){}",
                    m.join_code(),
                    m.local_ip,
                    if upnp::is_non_public(m.external_ip) {
                        " WARNING: router has no public IP (double NAT / carrier NAT), friends cannot reach it"
                    } else {
                        ""
                    }
                ));
                let path = settings::join_code_path();
                match fs::write(&path, m.join_code()) {
                    Ok(()) => log::line(&format!("upnp: join code written to {}", path.display())),
                    Err(e) => log::line(&format!("upnp: could not write {}: {e}", path.display())),
                }
                UpnpState::Mapped(m)
            }
            Err(e) => {
                log::line(&format!("upnp: failed: {e}"));
                UpnpState::Failed(e)
            }
        };
        if let Ok(mut s) = out.lock() {
            *s = new;
        }
    });
    state
}

impl Net {
    /// UDP host: the join code to give a friend, or why there is none.
    fn join_info(&self) -> Option<String> {
        let state = self.upnp.as_ref()?.lock().ok()?;
        Some(match &*state {
            UpnpState::Working => "UDP host, opening the router port (UPnP)...".into(),
            UpnpState::Mapped(m) if upnp::is_non_public(m.external_ip) => format!(
                "UDP host, join code {} (WARNING: no public IP, the friend probably cannot reach it)",
                m.join_code()
            ),
            UpnpState::Mapped(m) => format!("UDP host, join code {} (saved to udp_join_code.txt)", m.join_code()),
            UpnpState::Failed(e) => format!("UDP host on port {}, UPnP failed: {e}", self.cfg.udp_port),
        })
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        // No session any more: stop teeing our player's packets.
        crate::native::set_forward(0);
        publish_link(Link::Off);
        if let Some(Ok(state)) = self.upnp.as_ref().map(|s| s.lock())
            && let UpnpState::Mapped(m) = &*state
        {
            let _ = m.remove();
            // The code is stale once the port is closed.
            let _ = fs::remove_file(settings::join_code_path());
        }
    }
}

impl Net {
    /// The friend's name (compat: kept after the link drops).
    pub fn peer_name(&self) -> Option<&str> {
        self.compat().map(PeerState::name)
    }

    /// True while a link is up (hidden snapshots included). Client: the host link.
    pub fn connected(&self) -> bool {
        self.session.state() == coop_net::State::Connected
    }
}

impl Net {
    pub fn is_host(&self) -> bool {
        self.role == RoleState::Host
    }

    /// Hosting over UDP: the friend can fetch the test build over HTTP from here.
    #[cfg(feature = "testsync")]
    pub fn serves_updates(&self) -> bool {
        self.role == RoleState::Host && self.transport == Transport::Udp && self.steam.is_none()
    }

    pub fn take_enemy_states_from(&mut self) -> Vec<(PeerId, coop_net::EnemyState)> {
        std::mem::take(&mut self.enemy_inbox)
    }

    pub fn take_damage_from(&mut self) -> Vec<(PeerId, coop_net::DamageReport)> {
        std::mem::take(&mut self.damage_inbox)
    }

    pub fn send_enemies(&mut self, states: &[coop_net::EnemyState]) {
        self.send_enemies_to(Dest::All, states);
    }

    pub fn send_enemies_to(&mut self, d: Dest, states: &[coop_net::EnemyState]) {
        self.session.send_enemies_to(d.slot(), states, Instant::now());
    }

    #[allow(dead_code)] // All wrapper (frozen API); mode 4 sends damage to Dest::Host since P6
    pub fn send_damage(&mut self, report: coop_net::DamageReport) {
        self.send_damage_to(Dest::All, report);
    }

    pub fn send_damage_to(&mut self, d: Dest, report: coop_net::DamageReport) {
        self.session.send_damage_to(d.slot(), report, Instant::now());
    }

    /// Client: host posture states received this frame (posture.rs), with their sender.
    pub fn take_posture_from(&mut self) -> Vec<(PeerId, coop_net::PostureState)> {
        std::mem::take(&mut self.posture_inbox)
    }

    pub fn send_posture(&mut self, states: &[coop_net::PostureState]) {
        self.send_posture_to(Dest::All, states);
    }

    pub fn send_posture_to(&mut self, d: Dest, states: &[coop_net::PostureState]) {
        self.session.send_posture_to(d.slot(), states, Instant::now());
    }
}

impl Net {
    /// Reliable (acked + re-sent) game event to every peer.
    pub fn send_event(&mut self, kind: u8, a: i32, b: i32) {
        self.send_event_to(Dest::All, kind, a, b);
    }

    pub fn send_event_to(&mut self, d: Dest, kind: u8, a: i32, b: i32) {
        self.session.send_event_to(d.slot(), kind, a, b, Instant::now());
    }

    /// Unacked events, the most of any link (compat).
    pub fn pending_events(&self) -> usize {
        self.session.pending_events()
    }

    #[allow(dead_code)] // P1c API
    pub fn pending_events_to(&self, id: PeerId) -> usize {
        self.session.pending_events_to(id.slot())
    }

    pub fn take_game_events_from(&mut self) -> Vec<(PeerId, u8, i32, i32)> {
        std::mem::take(&mut self.game_inbox)
    }

    pub fn take_netai_from(&mut self) -> Vec<(PeerId, coop_net::NetAiEntry)> {
        std::mem::take(&mut self.netai_inbox)
    }

    pub fn send_netai(&mut self, list: &[coop_net::NetAiEntry]) {
        self.send_netai_to(Dest::All, list);
    }

    pub fn send_netai_to(&mut self, d: Dest, list: &[coop_net::NetAiEntry]) {
        self.netai_bytes += self.session.send_netai_to(d.slot(), list, Instant::now()) as u64;
    }

    /// Total NetAI bytes sent so far (for a rate in the stats line).
    pub fn netai_bytes(&self) -> u64 {
        self.netai_bytes
    }

    /// Reliable, chunked, delivered complete and once (world dump).
    pub fn send_bulk(&mut self, kind: u8, data: &[u8]) -> bool {
        self.send_bulk_to(Dest::All, kind, data)
    }

    pub fn send_bulk_to(&mut self, d: Dest, kind: u8, data: &[u8]) -> bool {
        self.session.send_bulk_to(d.slot(), kind, data, Instant::now())
    }

    /// One raw game player packet (unreliable). False when nobody there or too large.
    #[allow(dead_code)] // P1c API
    pub fn send_player_raw_to(&mut self, d: Dest, ptype: u32, data: &[u8]) -> bool {
        self.session.send_player_raw_to(d.slot(), ptype, data, Instant::now())
    }

    /// Unacked bulk chunks, the most of any link (compat).
    pub fn pending_bulk_chunks(&self) -> usize {
        self.session.pending_bulk_chunks()
    }

    #[allow(dead_code)] // P1c API
    pub fn pending_bulk_chunks_to(&self, id: PeerId) -> usize {
        self.session.pending_bulk_chunks_to(id.slot())
    }

    /// The first received bulk transfer of `kind`, if any.
    #[allow(dead_code)] // P4: every module takes the sender now (take_bulk_from)
    pub fn take_bulk(&mut self, kind: u8) -> Option<Vec<u8>> {
        self.take_bulk_from(kind).map(|(_, d)| d)
    }

    /// The first received bulk transfer of `kind` and its sender, if any.
    pub fn take_bulk_from(&mut self, kind: u8) -> Option<(PeerId, Vec<u8>)> {
        let i = self.bulk_inbox.iter().position(|(_, k, _)| *k == kind)?;
        let (from, _, data) = self.bulk_inbox.remove(i);
        Some((from, data))
    }
}

impl Net {
    /// True if the friend had a loading screen within `secs` seconds (compat).
    pub fn friend_loaded_recently(&self, secs: u64) -> bool {
        self.compat().is_some_and(|p| p.loaded_recently(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(hp: i32, hidden: bool) -> Snapshot {
        Snapshot { hp, hidden, pos: [1.0, 2.0, 3.0], ..Default::default() }
    }

    #[test]
    fn dest_maps_to_session_slots() {
        assert_eq!(Dest::All.slot(), DST_ALL);
        assert_eq!(Dest::Host.slot(), 0);
        assert_eq!(Dest::Peer(PeerId(3)).slot(), 3);
    }

    #[test]
    fn a_different_player_in_a_slot_starts_clean() {
        let mut p = PeerState::new(PeerId(2));
        let t0 = Instant::now();
        // First join: nothing to drop, the uid is remembered.
        assert!(!p.adopt_uid(111));
        p.on_snapshot(t0, snap(0, false));
        p.on_snapshot(t0, snap(100, true));
        assert!(p.remote.is_some() && p.loading_at().is_some());
        // The same player again (a reconnect): its last position stays.
        assert!(!p.adopt_uid(111));
        assert!(p.remote.is_some() && p.loading_at().is_some());
        // Another uid in the slot: remote, history, loading and death marks are gone.
        assert!(p.adopt_uid(222));
        assert!(p.remote.is_none() && p.loading_at().is_none() && p.dead_since().is_none() && p.history.is_empty());
        // An unknown uid (UDP) cannot tell and drops nothing; a known one fills in.
        p.on_snapshot(t0, snap(50, false));
        assert!(!p.adopt_uid(0));
        assert!(p.remote.is_some());
        let mut u = PeerState::new(PeerId(1));
        assert!(!u.adopt_uid(0));
        assert!(!u.adopt_uid(333), "a UDP host\'s uid becomes known: the same peer");
    }


    /// A cutscene snapshot reads hidden (the puppet hides) but is no loading screen: the
    /// loading mark (which ends a peer's boss fight) and the death mark stay.
    #[test]
    fn a_cutscene_snapshot_is_hidden_but_not_a_loading_screen() {
        let mut p = PeerState::new(PeerId(1));
        let t0 = Instant::now();
        p.on_snapshot(t0, snap(100, false));
        let cut = Snapshot { cutscene: true, ..snap(100, true) };
        p.on_snapshot(t0, cut);
        assert_eq!(p.remote(), None, "hidden");
        assert!(p.in_cutscene());
        assert_eq!(p.loading_at(), None, "not a loading screen");
        assert!(p.history.is_empty());
        // Out of the cutscene: visible, not in a cutscene any more.
        p.on_snapshot(t0, snap(100, false));
        assert!(!p.in_cutscene() && p.remote().is_some());
        // A plain hidden snapshot (loading) still marks the loading.
        p.on_snapshot(t0, snap(100, true));
        assert_eq!(p.loading_at(), Some(t0));
        assert!(!p.in_cutscene());
    }
    #[test]
    fn peer_state_tracks_death_and_loading_like_the_old_friend_fields() {
        let mut p = PeerState::new(PeerId(1));
        let t0 = Instant::now();
        p.on_snapshot(t0, snap(100, false));
        assert_eq!(p.dead_since(), None);
        assert!(p.remote.is_some());
        let t1 = t0 + Duration::from_millis(10);
        p.on_snapshot(t1, snap(0, false));
        assert_eq!(p.dead_since(), Some(t1));
        // Dead and then loading (death screen): still dead since t1, history cleared.
        let t2 = t1 + Duration::from_millis(10);
        p.on_snapshot(t2, snap(0, true));
        p.on_snapshot(t2, snap(100, true));
        assert_eq!(p.dead_since(), Some(t1));
        assert_eq!(p.loading_at(), Some(t2));
        assert!(p.loaded_since(t1) && !p.loaded_since(t2));
        assert!(p.history.is_empty());
        assert_eq!(p.remote(), None, "hidden");
        // Alive and visible again.
        p.on_snapshot(t2, snap(50, false));
        assert_eq!(p.dead_since(), None);
        assert_eq!(p.remote_smoothed(0), Some(([1.0, 2.0, 3.0], 0.0)));
    }

    /// `loopback_peers`: echo sessions join a host whose real transport is dead (Inert, as a
    /// Steam session before Steam is up) through the hub alone, and data flows both ways and
    /// between them through the host. (EchoPeer::tick is not run: it writes the game log.)
    #[test]
    fn hub_echo_peers_join_our_host_session_beside_its_transport() {
        let cfg = NetSettings { password: "susi".into(), ..Default::default() };
        let (hub, mut echoes) = EchoPeer::hub_of(3, 1, &cfg);
        let hub = hub.unwrap();
        assert_eq!(echoes.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["loopback-echo-1", "loopback-echo-2", "loopback-echo-3"]);
        assert_eq!(echoes.iter().map(|e| e.delay.as_millis()).collect::<Vec<_>>(), [1250, 1500, 1750]);
        let mut host = Session::with_transport(wrap(Box::new(Inert), Some(&hub)), Role::Host, "susi", "host");
        host.set_max_peers(2);
        let mut now = Instant::now();
        let mut rejected = false;
        let mut got: Vec<(usize, u8)> = Vec::new();
        for round in 0..40 {
            if round == 20 {
                host.send_snapshot(snap(5, false), now);
                echoes[1].session.send_snapshot(snap(7, false), now);
            }
            for (i, e) in echoes.iter_mut().enumerate() {
                for ev in e.session.poll(now) {
                    rejected |= i == 2 && ev == Event::Rejected { reason: RejectReason::Full };
                    if let Event::Snapshot { from, .. } = ev {
                        got.push((i, from));
                    }
                }
            }
            for ev in host.poll(now) {
                if let Event::Snapshot { from, .. } = ev {
                    got.push((9, from));
                }
            }
            now += Duration::from_millis(20);
        }
        let uids: Vec<_> = host.peers().into_iter().map(|(slot, uid, _)| (slot, uid)).collect();
        assert_eq!(uids, vec![(1, ECHO_UID | 1), (2, ECHO_UID | 2)]);
        assert!(rejected, "the third echo has no room with max_players = 3");
        // Ours to both echoes, echo 2's to the host and (relayed) to echo 1.
        got.sort();
        assert_eq!(got, vec![(0, 0), (0, 2), (1, 0), (9, 2)]);
    }

    #[test]
    fn peer_state_wants_raw_only_while_fresh() {
        let mut p = PeerState::new(PeerId(0));
        let t0 = Instant::now();
        assert!(!p.wants_raw_at(t0));
        p.on_snapshot(t0, Snapshot { want_raw: true, ..snap(1, false) });
        assert!(p.wants_raw_at(t0 + Duration::from_secs(1)));
        assert!(!p.wants_raw_at(t0 + Duration::from_secs(2)));
    }
}
