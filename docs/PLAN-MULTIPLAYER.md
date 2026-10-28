# Plan: 3–6 players (branch coop-multi, from v7.5 c1d75ef)

Owner request 2026-10-24: remove the 2-player cap. Model: ERSC / DS3SC (host + up to 5).
Status: P0 done 2026-10-24 (IDA answers, API freeze, N0 code; N0 NOT verified live), see
"P0 results + API freeze" at the end. Default `max_players = 2` until each phase is verified live.
P1b done 2026-10-24 (peers.rs, per-frame PeerSet in puppet.rs, multi settings keys; unit tests
only, NOT verified live; net.rs read through a marked P1a shim until P1a lands), see 5.5.
P1a done 2026-10-24 (coop-net `Link`/slots/`from`/`send_*_to`, net.rs `PeerState` + per-peer
accessors, steam.rs member set; wire v9, one link; unit tests only, NOT verified live), see 5.6.
P1c + P1d done 2026-10-24 (every module takes the PeerSet; dead compat removed; 2-player
regression review of c1d75ef..HEAD with 2 fixes; build + unit tests only, the P1 live check
"full 2-player regression" is still open), see 5.7-5.10.
P2 done 2026-10-24 (wire v10: uid/slot/reject reasons, host relay, roster, Steam lobby size =
max_players, `loopback_peers`; unit tests with 2-6 in-memory sessions only, NOT verified live;
7.5 is locked out), see 5.11.
P3 done 2026-10-24 (native remote per peer: per-peer recv object + inbox, staggered spawns,
removal per peer, pins moved into native.rs; artfx owner per sender; ui presence per name;
spectate target API; unit tests only, NOT verified live), see 5.12.
P4 done 2026-10-24 (epochs per peer, host flag sequencer, per-joiner snapshot, bossgate per
fight, N-player spectate / wipe / death sync / scaling, v7.6 9100 hotfix; unit tests only, NOT
verified live), see 5.13.
P5 done 2026-10-24 (nsession PROTO 5: slot netIds, hop-by-hop frames with origin, per-link
engagements, type-23 recipient wraps, Engaged set, ownfill for N, `native_session_max_players`
and a never-half-native block rule; unit tests only, NOT verified live), see 5.14.
P6 done 2026-10-24 (mode 4 for N: host-only authority, damage to the host, totals per
(peer, enemy), NetAI player targets `0x10 + slot`, hkb per source, per-peer epochs in enemies /
hkbsync; unit tests only, NOT verified live), see 5.15.
v7.7 merged 2026-10-24 (auditor, reward reconcile, deathblow contests, boss record fixes, adapted
to per-peer state; unit tests only, NOT verified live), see 5.17.
Auditor + coop-audit for every peer 2026-10-24 (host <-> each client, per-peer reward counters,
multi-log analyzer; unit tests only, NOT verified live), see 5.19.

## Facts
- Engine capacity 6 players (log: `native: player chr set capacity 6 (WCM+0x40)`); native.rs
  `PGD_SLOTS = 5`, remote playerNo 1..5, PGD slots 0..4 → host + 5.
- The game's ownership code is multi-member (SPEC-NATIVE-SESSION §3/§4, IMPLEMENTATION-NOTES
  §27.2): candidate records per player handle (`T+0x368`, `0xA1D7E0`/`0xA1C6C0`), pairs settled
  by `0xA1D550`→`0xA1C590`, one type-23 per non-host record via `0xA1DD00`→`0xA29DD0`→`0x8E6FA0`.
- Above N=2 (spec §3): inline count constants (`B9 <56·N>`), fake member vector length
  (`FAKE_MEMBERS_N` 4 → 5), type-23 recipient (wrap the call at `0xA1DFC1`, capture rcx =
  recipient chr, `LAST23_CHR`; fallback hook `0xA1D240`). Count sites `0xA4DC38`, `0xA4A6CB`,
  `0xA1CF13`, `0xA1DD5E`, `0xA4E5BE` look like thresholds only — verify in P0.
- Single-friend coupling: coop-net `Session` one peer; net.rs one remote/history/name/epoch;
  nsession single PEER/LANE/ACCEPT/OWN_PNET/REMOTE_PNET, WIRE_SENDER/RECEIVER, one Engine;
  native.rs one remote/RECV_OBJ/INBOX; World friend_* fields; netai WIRE_HOST/CLIENT_PLAYER;
  activation FRIEND_POS; ui one FriendTag; puppet passes `remote: Option<Snapshot>`;
  steam MAX_MEMBERS = 2 and host accepts one member; scaling `players = 2|1`.

## 1. Topology: star through the host (logical mesh, physical star)
Clients link only to the host; the host relays. Packets carry `src`/`dst` (slot or ALL).
Reasons: UDP fallback/NAT (only the host has UPnP/forwarding), one link state per client,
host already the authority/sequencer. Host leaving ends the session for all (as ERSC).
Host upload worst case 6 players ≈ 3–4 Mbit/s; mitigations: relay snapshots ≤30 Hz, relay
PlayerRaw only to same-block peers that want raw, own snapshot 60 Hz when N>2.

Wire (coop-net VERSION 9 → 10; lobby proto follows):
- PeerId u8: host 0, clients 1..5 assigned by the host. `Hello{password_hash, uid, name}`,
  `Welcome{name, slot}`, `Reject{reason: 1 pw, 2 full, 3 version}`. Known uid at a new address
  replaces that slot's link.
- Routed kinds carry `src, dst` (0xFF = all but src): Snapshot, Enemies, Damage, Posture,
  Event, NetAi, Bulk, PlayerRaw, Hkb. Link-local: Hello/Welcome/Reject/Heartbeat/Bye/Ping/Pong/
  Ack/BulkAck.
- Host relay: unreliable cut-through with src kept; Event acked hop-by-hop and re-queued per
  target link; Bulk store-and-forward. Receiver orders snapshots/raw per src.
- Roster: reliable host→all (bulk 0x52 `{gen, n, (slot, uid, name)}`) on every join/leave.
- Events: Connected{peer,name,slot}, PeerJoined, PeerLeft{why}, Roster; data events gain `from`.
- API `send_*_to(Dest::{All, Peer(id), Host})`; current `send_*` = All wrappers.
- Epochs per link; host `peer_epoch(id)`; client `epoch()` = host link.
- nsession PROTO 4 → 5: player netIds on the wire `0xFFF0|slot`; lane messages gain origin;
  per-peer generations; `MSG_ENGAGED = 7` (host→clients engaged set).
- Bulk: scale config gains `players`; persist bulk 50 merged from all.

## 2. Rules for N (per subsystem)
- coop-net: `struct Link`; `Session { links: [Option<Link>;5], max_peers, me }`; full → Reject(2).
- steam.rs: lobby size = `max_players`; host accepts every member; client unchanged; full notice.
- net.rs: `peers: [Option<PeerState>;6]`, inboxes carry `from`, damage/posture keyed (from,id);
  accessors `me()`, `peers()`, `peer(id)`, `friend()` (compat = first).
- native.rs: `remotes[5]` per peer, recv object + INBOX per peer, staggered spawns (≤1 per 3 s),
  `remote_of(peer)`, `remotes()`.
- puppet.rs hub builds `PeerSet` (new peers.rs); per-peer pin; tags Vec; ghost fallback for at
  most one peer.
- artfx: k fake members, owner rewritten to that peer's remote.
- ui: tags/arrows per peer, status lists names+pings, joined/left from Roster diffs.
- world flags: host is sequencer; clients send to host; host applies and re-sends to others
  with its own version; clients accept flags only from host. Events carry `from`, go to All.
  Join snapshot per joiner on its epoch.
- bossgate per fight; npcintro origin-aware (first talker in host order decides); doors
  unchanged; resync all rest; join follows the host; ownworld per joiner, progress windows
  accept any peer.
- bossrules/spectate: hold the death reload while ≥1 living peer is engaged in the same fight;
  camera = nearest living engaged peer, switch on its death; wipe only when no engaged peer
  alive; expel on any rest; boss reward to All.
- persist: wipe = every connected peer dead within 90 s; dead set union, boss records minimum.
- restsync: any rest resets all. livesync: minimum over N. keyitems: to all, doctor host↔each.
- death_sync P3: host death → every client in block sends dead list, host takes union.
- scaling: host decides players = 1 + connected peers, sent in ScaleCfg; mult per extra player.
- items/leave/dialog: Charm refused "This world is full (N/N)"; client Thread leaves alone,
  host closes for all; "A companion entered your world" with name per join.
- activation positions for all peers; mode-4: records broadcast, wire target `0x10 + slot`,
  damage/posture totals per (peer, enemy).
- nsession: fake members compact array of engaged peers; recv per peer inbox; player netIds
  rewritten at push time per src; send to_member via fake member or LAST23 recipient; host 22
  per src; lanes hop-by-hop with origin, host re-queues; per-peer engagements on the host, one
  on clients + MSG_ENGAGED; a failing peer leaves alone, host block change/rest disengages all;
  ownfill nearest player (tie lowest slot); counts recomputed if P0 says they matter.

## 3. Strategy (2-player works at every step)
N=1 base case first. New `peers.rs` (PeerId, MAX_PLAYERS 6, PeerFrame, PeerSet with first/iter/
in_block/nearest_alive/all_dead_since/by_id); net.rs accessors; native.rs `remote_of` stubs;
settings `max_players` (2..6, default 2), `relay_snapshot_hz` 30, `snapshot_hz_multi` 60,
`scaling_count` session.
Order: P0 RE + API freeze → P1a NET (Link, from, Dest) → P1b integrator peers.rs + PeerSet
passed alongside old args → P1c modules switch internals using `first()` → P1d remove old args,
2-player regression → P2 wire v10 with N (unit tests with 3–6 in-memory sessions, hub Wire)
→ P3/P4/P5/P6 in parallel → P7 default 4, docs, perf.
File ownership: NET (coop-net/*, net.rs, steam.rs); NATIVE/VIEW (native, artfx, ui,
spectate_cam, ghost); WORLD (world, bossgate, npcintro, doors, resync, join, ownworld, doctor,
keyitems); RULES (bossrules, persist, restsync, scaling, debuff, items, leave, dialog);
NSESSION (nsession, livesync, activation); MODE4 (enemies, netai, posture, hkbsync, motion);
INTEGRATOR (puppet, lib, settings, tuning, peers, testsync, docs).
One-machine tools early: N0 multi-clone (`native::spawn_test(k)`, k=1..5) and
`loopback_peers = k` (k echo sessions with different delays).

## 4. Phases
| Phase | Work | Effort | Live check |
|---|---|---|---|
| P0 | IDA: LAST23 recipient / 0xA1D240; count sites; 0xA1D550 ≥3 records; player netId uniqueness playerNo 1..5; any-member vector + artfx with >1 fake member; API list | 4–6 h | N0 with 5 clones: capacity 6, playerNo 1..5, PGD 0..4, FPS, 10 min no crash, clean delete on load |
| P1 | Peers abstraction, N=1 | 12–16 h | full 2-player regression |
| P2 | Transport N + loopback_peers | 14–18 h | 2 PCs + loopback peers, then 3 PCs (Steam + UDP) |
| P3 | Native remotes per peer | 10–14 h | 3 PCs: two remotes each, leaving removes one, bullets per peer |
| P4 | World + rules | 18–26 h | 3 PCs: flags via host, mid-session join, restores, rest resets all, spectate switch, wipe all dead, scaling at 3 |
| P5 | nsession N | 18–26 h | 3 PCs one block: both clients engaged and owning, hand-offs, client↔client hits once, lives sync |
| P6 | Mode-4 fallback N | 10–14 h | native off, 3 PCs |
| P7 | Polish, default 4, perf | 6–10 h | 4 PCs, soak |
| P8 (opt.) | Steam direct paths for unreliable streams | 8–12 h | latency |
Total ≈ 92–130 agent-hours + 5–7 live sessions.

Risks: native 23 recipient mapping (fallback: native only for N=2, mode 4 for N>2); host
bandwidth/CPU; more PlayerIns vs memory/driver crash (N0 test); flag write races (host order
wins); clients in a block without the host have no enemy sync; no host migration; partly
engaged native degraded; machine-local handles in player packets; needs 3 copies/accounts;
mixed versions locked out by version bump.

## 5. P0 results + API freeze (2026-10-24)

IDA on a private copy `F:\Temp\ida_multi\db.i64` (from ida_v75), jobs `o1`-`o7` (decompiles
and disassembly in those folders). Addresses are RVAs (image base 0x140000000).

### 5.1 IDA answers
**(a) Type-23 recipient.** Yes, per record. `0xA1DD00` walks the record list `T+0x368`
(`a1+872`); for each record whose handle `*(u32*)rec` is not our own (`*(WCM+0x88)+8`) it does
`chr = 0xA4A050(WCM, handle)` (`0xA1DF98`, result in rbx), `chr->vtbl[53]()` (`call [rdx+1A8h]`
@`0xA1DFAF`; PlayerIns slot 53 = `0xA2B590` = `return 1`), then `mov rcx, rbx` @`0xA1DFBE` and
`call 0xA29DD0` @`0xA1DFC1` (bytes `E8 0A BE 00 00`, re-read). So rcx at `0xA1DFC1` is the
recipient chr. The record stays in r14: `0xA1D240(rcx = r14 = record, &size)` @`0xA1DFDD`
(`E8 5E F2 FF FF`; its only caller is `0xA1DD00`), then `0x8E6FA0(mgr, &member, 23, data,
size)` @`0xA1E005`. Nothing else runs between the two calls.
- Mapping: `LAST23_CHR = rcx` -> the peer whose native remote chr == LAST23_CHR (equivalently
  `u32 [chr+8]` == record handle, the key our host feed passes to `0xA1D7E0`). Stub for the
  call at `0xA1DFC1`: `48 89 0D <rel32 LAST23_CHR> E9 <rel32 0xA29DD0>`. The to-member hook
  uses LAST23 only for type 23 with an empty member, clears it, and drops + counts a 23 it
  cannot map (never broadcasts it).
- `0xA29DD0(chr, &member)`: local player -> own member (`0x8E3FD0`); otherwise
  `(*(chr+0x2038))->vtbl[11](obj, &tmp)`, copied out. `0xA4A340` (the vanilla host 22 loop)
  matches member -> chr the same way (`0xA2BF80` = `chr+0x2038`, slot 11). The game's own
  member<->chr link is the recv session's slot 11, so a per-peer recv object could answer it
  (alternative B; only needed if another whitelisted to-member send shows up).
- The other fixed-type `0x8E6FA0` senders (`0x8D8EE0`..`0x8DB800`, `0x916B20/60`,
  `0x91D700/860`, `0x89CB60`, `0xB1BC10`, `0x1061C50`) send types 7, 8, 10-13, 16, 17, 19, 20,
  24, 30, 31, 37-39, 44, 50, 58, 60, 61, 64, 0x49: none whitelisted. Exception: `0x1061D00`
  sends **type 23** to `a1+8` (a real PlayerSession's member, not reached offline); with the
  rule above it passes through.

**(b) Count sites: thresholds only.** `0xA4DC38` `cmp eax,1; setle forceLocal`; `0xA4A6CB`
`test eax,eax; jle` (members > 0 -> owner lookup `0xA1BB10`); `0xA1CF13`, `0xA1DD5E`,
`0xA4E5BE` compute `(end-begin)/56` and only `cmp edx,1; jle/jg` (edx dead afterwards). No
loop bound, no array size. `CNT2` (`mov eax,2`) and `ECX_2M` (`mov ecx,0x70`) are right for
every N and stay. The loop in `0xA4E520` bounded by `WCM+0x1C8 - 2` is the chr-set count,
not members. The real member loops (`0xA1CED0` host 22 loop, `0x8E7400`, `0xA1E140`,
`0xA1E9F0`) iterate the real, empty `S+0x158` vector and stay inert.

**(c) `0xA1D550` / `0xA1C590` with 3+ records: N-generic.** Pass 1 drops records whose handle
no longer resolves (`0xA4A050`). Pass 2 runs `0xA1C590(rec_i, rec_j)` for every pair i<j of
the list. `0xA1C590(a, b)`: for each netId in a that b also has with b.prio != 0, a keeps it
if `a.prio > b.prio` or (equal and `a.count < b.count`), and `0xA1B700(b, netId)` removes it
from b; otherwise it is removed from a. Over all pairs each netId ends in exactly one record.
Existing quirk (same at N=2): b.prio == 0 -> both keep it.

**(d) Player netId** = `(*(*(WCM+0x10)+0x18) << 11) | (PGD+0x10 & 0x7FF)` for chrs in the
player set `WCM+0x40` (`0xA4CE60`, `0xA2A110` = `*(chr+0x2000)+0x10`). The block part is the
same for every player (live: 0xA000), so unique netId = unique playerNo. Local = 0; native.rs
`create` writes PGD+0x10 itself (lowest free 1..5, never the local one or another slot's)
before `0x7F86F0` / `0xA49460`, so 0xA001..0xA005 are unique on one machine. The game does
not assign it for our remotes. The numbers are machine-local (creation order), so the wire
carries `0xFFF0 | slot`. `0xA4F940` (21/28 apply) finds a player victim by searching the
player set for that playerNo, so the receiver maps slot -> its local remote's playerNo.

**(e) Any-member vector `0x8E4010` (= `S+0x158`).** About 40 consumers. The ones we serve
(21 `0x8D77B0`, 28 `0x8D7320`, 52 `0x8D7290`, 18 `0x8D7D70`, 42 `0x8D8260`, 45 `0x8D8750`)
loop the vector, give each member pointer only to `0x8E6B50` and return 1 on the first packet:
no index, no field read, no sender returned. Unrouted consumers (`0x8D7120` calls
`0x1BD9480(m)` etc.) read the real, empty vector (only the call @`0xA1D936` is redirected).
The NetAI receivers 4 `0xA1BDE0`, 70 `0xA1AF40`, 71 `0xA1D8D0` also use the member only as the
recv handle; their data is keyed by netId. artfx `0xB1D748`: per member
`while (0x8E6B50(m, 63) > 0)` deserialises into one vector, then creates all; the member is
not used again, the owner comes from record +0. So k fake members would work, and **one fake
member with a merged inbox works the same**.

**(f) Other one-member assumptions.** In the game: none beyond (a)-(e). In our code:
`to_member_detour` "empty member = the friend"; the OWN/REMOTE_PNET pair (0xFFF0/0xFFF1);
one ACCEPT generation, lane and engagement; `fake_member_index` tied to one peer queue.
Routing facts for N: `0xA4F940` applies every 21 it gets with no owner check, so a broadcast
21 (a hit on an enemy owned elsewhere) damages each receiver's copy once. That is right for
N if the host applies a relayed 21 once and forwards it unchanged. A 21 sent to the host (enemy
owned by the sender) must not be relayed. 22 is to-host only, 5 is a host broadcast, 4/70/71
go from the owner to all (client-owned ones relayed by the host).

### 5.2 Plan changes from P0
1. The count constants stay ("counts recomputed" dropped from §2 nsession).
2. The fake member vector stays at one member (no `FAKE_MEMBERS_N` growth). The recv hooks
   serve a merged per-type inbox whose entries carry `from`; player netIds are rewritten when
   an entry is queued (per src). artfx keeps one fake member as well.
3. GameNet packets keep their send mode on the wire: `Bcast` (the host relays to all but src),
   `ToHost` (the host consumes, never relays), `ToPeer(slot)` (23: host -> that client).
4. Type 23 recipient: the `0xA1DFC1` wrap is the main path (alternative B in 5.1a).
5. P5 check (unverified, also at N=2): whether 4/70/71 carry player netIds (AI targets); dump
   the 70 target fields as evidence before relying on them.

### 5.3 Frozen API (Rust). Owners per §3; signatures change only by agreement.

**peers.rs (INTEGRATOR, new)**
```rust
pub const MAX_PLAYERS: usize = 6;
pub const MAX_PEERS: usize = MAX_PLAYERS - 1;
/// Session slot: host 0, clients 1..=5, assigned by the host for the life of a link.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct PeerId(pub u8);
impl PeerId {
    pub const HOST: PeerId = PeerId(0);
    pub fn new(slot: u8) -> Option<PeerId>;          // Some for 0..=5
    pub fn slot(self) -> u8;
    pub fn index(self) -> usize;
    pub fn is_host(self) -> bool;
}
/// One remote peer this frame (built by the puppet hub from net.rs + native.rs).
#[derive(Clone, Debug)]
pub struct PeerFrame {
    pub id: PeerId,
    pub name: String,
    pub epoch: u64,                                   // link epoch (client: the host link's)
    pub rtt_ms: Option<u32>,
    pub snapshot: Option<coop_net::Snapshot>,         // latest visible, as Net::remote() today
    pub smoothed: Option<([f32; 3], f32)>,            // remote_smoothed(ghost_delay_ms)
    pub native: Option<Chr>,                          // native.rs remote_of(id)
    pub view: Option<([f32; 3], f32)>,                // native pos/yaw, else smoothed
    pub loaded_at: Option<Instant>,                   // last hidden snapshot
    pub dead_since: Option<Instant>,                  // snapshot hp <= 0 since
    pub death_at: Option<Instant>,                    // world.rs committed EV_DEATH (last frame)
}
impl PeerFrame {
    pub fn hp(&self) -> Option<i32>;
    pub fn alive(&self) -> bool;                      // visible and hp > 0
    pub fn block(&self) -> Option<i32>;
    pub fn in_block(&self, here: Option<i32>) -> bool;
    pub fn loaded_recently(&self, secs: u64) -> bool;
}
#[derive(Clone, Debug, Default)]
pub struct PeerSet { /* me, is_host, peers sorted by id */ }
impl PeerSet {
    pub fn new(me: PeerId, is_host: bool) -> Self;
    pub fn solo() -> Self;                            // no link: empty
    pub fn push(&mut self, p: PeerFrame);             // keeps id order, replaces the same id
    pub fn me(&self) -> PeerId;
    pub fn is_host(&self) -> bool;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn players(&self) -> u32;                     // 1 + len
    pub fn iter(&self) -> impl Iterator<Item = &PeerFrame> + '_;
    pub fn first(&self) -> Option<&PeerFrame>;        // lowest id: the 2-player "friend"
    pub fn by_id(&self, id: PeerId) -> Option<&PeerFrame>;
    pub fn host(&self) -> Option<&PeerFrame>;         // client: by_id(HOST)
    pub fn by_native(&self, chr: Chr) -> Option<&PeerFrame>;
    pub fn in_block(&self, here: Option<i32>) -> impl Iterator<Item = &PeerFrame> + '_;
    pub fn nearest_alive(&self, pos: [f32; 3], here: Option<i32>) -> Option<&PeerFrame>; // tie: lowest id
    pub fn all_dead_since(&self, t: Instant) -> bool; // every peer dead_since >= t (false if empty)
}
```

**coop-net (NET).** `VERSION = 10`. Slots are plain `u8` there (no dependency on the dll):
`pub const DST_ALL: u8 = 0xFF; pub const MAX_PEERS: usize = 5;`
```rust
pub enum RejectReason { Password = 1, Full = 2, Version = 3 }
pub enum LeaveWhy { Quit, Lost, Replaced, Rejected }
struct Link {                                         // private
    addr: SocketAddr, slot: u8, uid: u64, name: String, state: State,
    /* last_heard, last_sent, last_seq, rtt_us, events pending/seen, bulk out/in/done,
       raw_last: HashMap<(u8 /*src*/, u32 /*ptype*/), u32> */
}
pub struct Session { /* sock, role, password_hash, name, uid: u64, me: u8, max_peers: u8,
                        links: [Option<Link>; MAX_PEERS] (host: slot - 1; client: [0] = host) */ }
impl Session {
    pub fn new(bind: SocketAddr, role: Role, password: &str, name: &str) -> io::Result<Self>; // unchanged
    pub fn with_transport(sock: Box<dyn Datagram + Send>, role: Role, password: &str, name: &str) -> Self; // unchanged
    pub fn set_uid(&mut self, uid: u64);
    pub fn set_max_peers(&mut self, n: u8);           // host: 1..=5 (max_players - 1)
    pub fn me(&self) -> u8;                           // host 0; client: slot from Welcome (0xFF before)
    pub fn state(&self) -> State;                     // client: host link; host: Connected if any link is
    pub fn link_state(&self, slot: u8) -> Option<State>;
    pub fn peers(&self) -> Vec<(u8, u64, String)>;    // connected slots (the roster on clients)
    pub fn rtt_ms(&self) -> Option<u32>;              // first link (compat)
    pub fn peer_rtt_ms(&self, slot: u8) -> Option<u32>;
    pub fn poll(&mut self, now: Instant) -> Vec<Event>;
    pub fn send_snapshot(&mut self, s: Snapshot, now: Instant);                 // own, to all
    pub fn send_enemies_to(&mut self, dst: u8, states: &[EnemyState], now: Instant);
    pub fn send_hkb_to(&mut self, dst: u8, list: &[HkbEvent], now: Instant);
    pub fn send_damage_to(&mut self, dst: u8, r: DamageReport, now: Instant);
    pub fn send_posture_to(&mut self, dst: u8, s: &[PostureState], now: Instant);
    pub fn send_event_to(&mut self, dst: u8, kind: u8, a: i32, b: i32, now: Instant);
    pub fn send_netai_to(&mut self, dst: u8, list: &[NetAiEntry], now: Instant) -> usize;
    pub fn send_bulk_to(&mut self, dst: u8, kind: u8, data: &[u8], now: Instant) -> bool;
    pub fn send_player_raw_to(&mut self, dst: u8, ptype: u32, data: &[u8], now: Instant) -> bool;
    // every current send_X(..) stays as send_X_to(DST_ALL, ..) until P1d
    pub fn pending_events_to(&self, slot: u8) -> usize;
    pub fn pending_bulk_chunks_to(&self, slot: u8) -> usize;
    pub fn pending_events(&self) -> usize;            // max over links (compat)
    pub fn pending_bulk_chunks(&self) -> usize;       // max over links (compat)
    pub fn quit(&mut self, now: Instant);             // Bye on every link
}
pub enum Event {
    Connected { peer: SocketAddr, name: String, slot: u8 }, // host: the new client's slot; client: ours
    PeerJoined { slot: u8, uid: u64, name: String },        // host: with Connected; client: roster diff
    PeerLeft { slot: u8, why: LeaveWhy },
    Roster(Vec<(u8, u64, String)>),
    Rejected { reason: RejectReason },
    Lost { slot: u8 },
    PeerQuit { slot: u8 },
    Snapshot { from: u8, snap: Snapshot },
    Enemies { from: u8, list: Vec<EnemyState> },
    Hkb { from: u8, list: Vec<HkbEvent> },
    Damage { from: u8, report: DamageReport },
    Posture { from: u8, list: Vec<PostureState> },
    Game { from: u8, kind: u8, a: i32, b: i32 },
    NetAi { from: u8, list: Vec<NetAiEntry> },
    Bulk { from: u8, kind: u8, data: Vec<u8> },
    PlayerRaw { from: u8, ptype: u32, data: Vec<u8> },
}
```

**net.rs (NET)**
```rust
pub use crate::peers::PeerId;
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dest { All, Peer(PeerId), Host }
pub struct PeerState { /* per peer: name, uid, epoch, remote + history, loading_at, dead_since */ }
impl PeerState {
    pub fn id(&self) -> PeerId;
    pub fn name(&self) -> &str;
    pub fn epoch(&self) -> u64;
    pub fn rtt_ms(&self) -> Option<u32>;
    pub fn remote(&self) -> Option<Snapshot>;
    pub fn remote_smoothed(&self, delay_ms: u32) -> Option<([f32; 3], f32)>;
    pub fn remote_delayed(&self, delay_ms: u32) -> Option<Snapshot>;
    pub fn loaded_recently(&self, secs: u64) -> bool;
    pub fn loaded_since(&self, t: Instant) -> bool;
    pub fn dead_since(&self) -> Option<Instant>;
    pub fn wants_raw(&self) -> bool;
}
impl Net {
    pub fn me(&self) -> PeerId;
    pub fn peers(&self) -> impl Iterator<Item = &PeerState> + '_;   // connected, id order
    pub fn peer(&self, id: PeerId) -> Option<&PeerState>;
    pub fn friend(&self) -> Option<&PeerState>;                     // first (compat)
    pub fn peer_count(&self) -> usize;
    pub fn peer_epoch(&self, id: PeerId) -> Option<u64>;
    pub fn send_event_to(&mut self, d: Dest, kind: u8, a: i32, b: i32);
    pub fn send_bulk_to(&mut self, d: Dest, kind: u8, data: &[u8]) -> bool;
    pub fn send_enemies_to(&mut self, d: Dest, s: &[EnemyState]);
    pub fn send_damage_to(&mut self, d: Dest, r: DamageReport);
    pub fn send_posture_to(&mut self, d: Dest, s: &[PostureState]);
    pub fn send_netai_to(&mut self, d: Dest, l: &[NetAiEntry]);
    pub fn send_hkb_to(&mut self, d: Dest, l: &[HkbEvent]);
    pub fn send_player_raw_to(&mut self, d: Dest, ptype: u32, data: &[u8]) -> bool;
    pub fn take_game_events_from(&mut self) -> Vec<(PeerId, u8, i32, i32)>;
    pub fn take_bulk_from(&mut self, kind: u8) -> Option<(PeerId, Vec<u8>)>;
    pub fn take_enemy_states_from(&mut self) -> Vec<(PeerId, EnemyState)>;
    pub fn take_damage_from(&mut self) -> Vec<(PeerId, DamageReport)>;
    pub fn take_posture_from(&mut self) -> Vec<(PeerId, PostureState)>;
    pub fn take_netai_from(&mut self) -> Vec<(PeerId, NetAiEntry)>;
    pub fn take_hkb_from(&mut self) -> Vec<(PeerId, HkbEvent)>;
    pub fn pending_events_to(&self, id: PeerId) -> usize;
    pub fn pending_bulk_chunks_to(&self, id: PeerId) -> usize;
}
```
Every current accessor stays as the friend() / All wrapper until P1d: `remote`,
`remote_smoothed`, `remote_delayed`, `peer_name`, `rtt_ms`, `epoch`, `friend_loaded_recently`,
`friend_loaded_since`, `send_X` (= All), `take_X` (from dropped).
Inbound fan-out gains `from`:
- `nsession::on_raw(from: PeerId, ptype: u32, data: &[u8], epoch: u64) -> bool`
- `nsession::on_bulk(from: PeerId, data: &[u8], epoch: u64)`
- `artfx::on_raw(from: PeerId, ptype: u32, data: &[u8]) -> bool`
- `native::push_inbound(from: PeerId, ptype: u32, data: Vec<u8>)`
`nsession::pump(&mut Session, now)` keeps its signature and uses `send_player_raw_to`.
`native::set_forward(on: bool)` -> `native::set_forward(mask: u8)` (bit = slot that wants our
raw packets); `native::take_outbound()` and `artfx::take_outbound(forward: bool)` unchanged
(net.rs sends to the slots in the mask).

**native.rs (NATIVE/VIEW)**
```rust
pub struct NetWant { pub peer: PeerId, pub want: bool, pub hp: Option<i32> }
impl Native {
    pub fn remote_of(&self, peer: PeerId) -> Option<Chr>;
    pub fn remotes(&self) -> impl Iterator<Item = (PeerId, Chr)> + '_;
    pub fn net_remote(&self) -> Option<Chr>;          // compat: the lowest-id remote
    pub fn ensure_peers(&mut self, player: Chr, wants: &[NetWant]); // at most 1 creation per 3 s
    pub fn remove_peer(&mut self, peer: PeerId, why: &str);
    pub fn failing_for(&self, peer: PeerId) -> bool;
    pub fn spawn_test(&mut self, k: u32);             // N0 (done, 5.4)
    pub fn remove_tests(&mut self, why: &str);        // done
    pub fn has_test(&self) -> bool;                   // done
}
/// Lock-free, hook-safe (game threads): chr pointer -> peer, for nsession's LAST23 mapping.
pub fn peer_of_chr(chr: usize) -> Option<PeerId>;
```

**Entry points that take `&PeerSet` (current -> new).** The hub builds one `PeerSet` per frame
after `net.on_frame` and native (P1b passes it alongside the old args; P1d drops those).

| Module | Current | New |
|---|---|---|
| enemies | `Enemies::on_frame(&mut self, net: &mut Net, player: Chr, friend_pos: Option<[f32;3]>, ghost: Option<Chr>, probe: &Probe)` | `on_frame(&mut self, net: &mut Net, peers: &PeerSet, player: Chr, ghost: Option<Chr>, probe: &Probe)` |
| world | `World::on_frame(&mut self, net: &mut Net, in_world_for: Duration)` | `on_frame(&mut self, net: &mut Net, peers: &PeerSet, in_world_for: Duration)`; new `pub fn peer_death_at(&self, id: PeerId) -> Option<Instant>` (replaces `friend_death_at`) |
| restsync | `tick(enemies: &mut Enemies, is_host: bool)` | `tick(enemies: &mut Enemies, peers: &PeerSet)` |
| livesync | `tick(enemies: &Enemies)` | `tick(enemies: &Enemies, peers: &PeerSet)` |
| persist | `Ctx { net, enemies, player, friend_hp, friend_death_at, in_world_for }` | `Ctx { net, peers: &PeerSet, enemies, player, in_world_for }` (hp / death_at from PeerFrame) |
| puppet `death_sync` | `(world, enemies, n, player, remote: Option<Snapshot>, in_world_since)` | `(world, enemies, n, peers: &PeerSet, player, in_world_since)` |
| bossrules | `Frame { net, world, player, remote: Option<Snapshot>, here, in_world_for, wake, friend_view }` | `Frame { net, world, peers: &PeerSet, player, here, in_world_for, wake }` (view per PeerFrame) |
| ownworld | `on_frame(player, anim, friend_anim: Option<i32>, in_world_for) -> bool` | `on_frame(player, anim, peers: &PeerSet, in_world_for) -> bool` (host anim = `peers.host()`) |
| join | `Join::on_frame(player, epoch, host: &Snapshot, join_on_connect, friend_loaded, in_world_for, hold)` | unchanged; the caller passes `peers.host()` values |
| artfx | `tick(player: Option<Chr>, remote: Option<Chr>)` | `tick(player: Option<Chr>, peers: &PeerSet)` |
| nsession | `Frame { player, remote: Option<Chr>, connected, is_host, epoch, in_world_for, here, friend_block }` | `Frame<'a> { player, peers: &'a PeerSet, connected, is_host, in_world_for, here }` |
| activation | `set_friend(pos: Option<[f32;3]>)`, `log_stats(remote: Option<usize>)` | `set_peers(pos: &[[f32;3]])`, `log_stats(remotes: &[usize])` |
| hkbsync | `HkbSync::on_frame(&mut self, net, player, ghost, enemies)` | `on_frame(&mut self, net, peers: &PeerSet, player, ghost, enemies)` |
| ui | `Status { friend_tag: Option<FriendTag> }`, `publish_tag(Option<FriendTag>)`, `FriendTag { pos, name, ping_ms }` | `Status { friend_tags: Vec<FriendTag> }`, `publish_tags(Vec<FriendTag>)`, `FriendTag { id: PeerId, pos, name, ping_ms }` |
| scaling / debuff | `Scaling::apply(enemies, players: u32, cfg, bosses)`, `Debuff::on_frame(enabled, connected, own_rest, pct)` | unchanged; the host passes `peers.players()`, a client the host's `ScaleCfg.players` |
| steam | `MAX_MEMBERS = 2` const | lobby size `tuning::max_players()`; the host accepts every member up to it |

Modules that read `Net` directly today (bossrules, doctor, enemies, hkbsync, join, keyitems,
persist, resync, testsync, world) switch to `take_*_from` / `send_*_to` inside their own files.

nsession internals (NSESSION; reference, not cross-module): `LAST23_CHR: AtomicUsize` and the
`0xA1DFC1` stub; `PNET[slot]: [AtomicU16; 6]` (both forms) replacing OWN/REMOTE_PNET; per-peer
ACCEPT, lane and engagement keyed by `PeerId`; one fake member; inbox entries
`(from: PeerId, type, data)`.

### 5.4 N0 multi-clone (implemented, NOT verified live)
`[DEBUG] n0_clones = 0` (0..5, tuning `N0_CLONES`). With k > 0, after every load and
`native_spawn_delay_s`, puppet.rs calls `Native::spawn_test(k)`; `tick` creates one loopback
clone every 3 s (`create`: the game's free PGD slot 0..4, the lowest free playerNo 1..5), and
only after the previous world's stale remotes are deleted. Clones live in `Native::tests`;
`remote` holds only the friend's Net remote, and while clones exist the friend stays a ghost.
A load forgets them (deleted 2 s into the next world, like the friend's remote) and the next
world creates them again; `n0_clones = 0` deletes them at once.
Log: `native: N0 test requested: k clones ...`, `native: N0 clone i playerNo p PGD slot s
created (chr, t s in the world)`, the `native[N0 spawn]` / `native[N0 clone i after 3 s]`
checks, `native: N0 done: n of k clones [...]; player chr set u of cap used`, every 10 s
`native: N0 n clones, F fps` and the stats line `; N0 clones n [playerNo .. PGD .. hp .. m],
player chr set u of cap`; at the load `N0 clones forgotten`, then `remote ... survived the
load, deleting it` / `did not survive the load`.
Open: all clones read the game's one PlayerDebugSession loopback queue (`loopback queue ...
subscribers` in the log); whether 5 readers work is the live check.

### 5.5 P1b integrator (done 2026-10-24, unit tests only)
- `peers.rs` as frozen in 5.3 (11 unit tests). Additions, no changes: `PeerFrame::bare(id)`
  (everything empty); `Default` for `PeerSet` is `solo()` (me 0, not host, empty);
  `push` drops a frame with our own id; `nearest_alive` uses `view`, else the snapshot
  position, skips non-finite distances, needs the peer `in_block(here)` and `alive()`.
- `Puppet::build_peers` runs once per frame right after `Net::on_frame` and
  `native.net_remote()` (before `follow_native` and every module), and on loading screens
  (no native remote). Stored as `Puppet::peers`, getter `Puppet::peers() -> &PeerSet`.
  One line `peers: me s (host|not host), n player(s): [(slot, name)..]` when the ids change.
- Per peer today (2 players): `id`/`me` host 0, client 1; `name` `peer_name()` (else
  "Friend"); `epoch`, `rtt_ms`, `snapshot` = `remote()`, `smoothed` =
  `remote_smoothed(ghost_delay_ms)`; `native` = `net_remote()`; `view` = native pos/yaw
  (sane) else smoothed (= bossrules' `friend_view`); `loaded_at` recovered from
  `friend_loaded_since(t)` by bisection (puppet `loading_instant`, <= 1 ms, last hour:
  the frozen PeerState has no instant accessor; an optional `PeerState::loading_at()` would
  replace it); `dead_since` from visible snapshots (hp <= 0 first seen, cleared alive, kept
  hidden); `death_at` = `world.friend_death_at` (last frame's).
- Shims, all marked in puppet.rs: `// P1a shim` (`p1a_ids`, the single-friend reads,
  `shim_dead_since` / `track_dead_since`) -> `Net::me()`, `n.peers()`, `PeerState::*`;
  `// P3 shim` native = `net_remote()` -> `remote_of(id)`; `// P4 (WORLD)` death_at ->
  `world.peer_death_at(id)`.
- Passed alongside the old args: puppet `death_sync(.., peers, player, remote, ..)` (unused
  until P1c). No other module signature changed; `n0_clones` path untouched.
- Settings: `[GAMEPLAY] scaling_count = session` (session|block, 0|1),
  `[ADVANCED] max_players = 2` (2..6), `[DEBUG] relay_snapshot_hz = 30` (10..120),
  `snapshot_hz_multi = 60` (20..120); tuning `MAX_PLAYERS`/`max_players()` (clamped 2..=6),
  `RELAY_SNAPSHOT_HZ`, `SNAPSHOT_HZ_MULTI`, `SCALING_COUNT` (+ `SCALING_COUNT_SESSION/BLOCK`,
  `parse_scaling_count`), all in `tuning::describe()`. Nobody reads them yet (steam.rs /
  coop-net `set_max_peers`: NET; scaling: RULES).

### 5.6 P1a NET (done 2026-10-24, unit tests only)
- Wire unchanged: `VERSION` stays 9 (the 5.3 `VERSION = 10` is the P2 target), so v7.5 and
  this build still meet. v9 has no uid in Hello, no slot in Welcome and no Reject reason: the
  client is slot 1 (`me()` 0xFF before the first Welcome), `Rejected { reason: Password }`,
  link uid = SteamID64 from a Steam address, else 0.
- coop-net: `struct Link` (addr, slot, uid, name, state, timers, rtt, pending/seen events,
  bulk out/in/done, `raw_last` keyed (src, ptype)); `Session { links: [Option<Link>; 5] }`
  (host: slot - 1, client: [0] = host). `set_max_peers` is stored, but P1 links one client
  (`P1_LINK_CAP`): a Hello from a new address replaces it (`PeerLeft { Replaced }`), as
  before. Outgoing seq counters stay session-wide and never reset (as before). Events as
  frozen; `Lost`/`PeerQuit` come with `PeerLeft { Lost | Quit }`; the host sends `PeerJoined`
  with `Connected`, the client one `PeerJoined { slot: 0 }` for the host; `Roster` is not
  sent yet. A client's `send_*_to(slot != 0)` is dropped (P2 relays it). New: `DST_ALL`,
  `MAX_PEERS`, `RejectReason`, `LeaveWhy`, `peers()`, `link_state`, `peer_rtt_ms`,
  `pending_*_to`, every `send_X` = `send_X_to(DST_ALL)`. 66 coop-net tests (7 new).
- net.rs: `PeerState` per slot (`slots: [Option<PeerState>; 6]`), kept after its link drops
  so the old single-friend accessors (`remote*`, `peer_name`, `friend_loaded_*`) read the
  friend, else the peer that connected last, exactly as before. `peers()`/`peer()`/
  `friend()`/`peer_count()`/`peer_epoch()` list connected peers only. Addition:
  `PeerState::loading_at()` (integrator request). `dead_since` = puppet's shim rule.
  Inboxes store `from`; `take_X` drops it. Raw forwarding builds a slot mask from each
  peer's `wants_raw()` and sends to those slots; `native::set_forward(mask != 0)`,
  `nsession::on_raw/on_bulk(.., epoch_of(from))`, `artfx::on_raw`, `native::push_inbound`
  keep their old signatures until NATIVE/NSESSION change them (one-line switch in
  `Net::on_event`). Host calls `set_max_peers(max_players - 1)` every frame; Steam sets uid.
  Loopback: me 0, the echo peer 1 (puppet's shim says the reverse for loopback).
- steam.rs: `MAX_MEMBERS` stays 2; the host keeps `members: Vec<u64>` (accept + nudge each
  member without a live link, fast poll while any is unlinked); `Lobby::tick(connected,
  linked: &[u64])`. Full notice: P2.

### 5.7 P1c MODE4 + shim removal done (2026-10-24, build + unit tests only)
- `Enemies::on_frame(net, peers: &PeerSet, player, ghost, probe)` and
  `HkbSync::on_frame(net, peers: &PeerSet, player, ghost, enemies)` as frozen. Enemies derives
  `friend_pos` once at the top from `peers.first()` (snapshot position, exactly what
  `n.remote()` gave); internals unchanged (2-player behaviour identical). hkbsync only
  receives `peers` (the ghost is still the one friend's); nothing reads it yet.
- Inboxes: enemies/hkbsync/posture-receive use `take_damage_from`, `take_enemy_states_from`,
  `take_netai_from`, `take_posture_from`, `take_hkb_from`. Keyed by sender where it matters:
  `Enemies::applied_damage: HashMap<(PeerId, i32), i32>` (host damage totals per reporting
  peer + enemy); `HkbSync::applied_n: HashMap<PeerId, u32>` (event counter is per sender).
  Client-side state/netai/posture take the entries from whoever sent them (only the host).
  netai.rs, posture.rs, motion.rs needed no change (no Net inbox use; posture send stays
  `send_posture` = All until P1d).
- puppet.rs `build_peers` now uses `Net::me()` (fixes the loopback id mismatch: loopback host
  0, echo peer 1) and `for p in n.peers()` with `PeerState::id/name/epoch/rtt_ms/remote/
  remote_smoothed/loading_at/dead_since`. Removed: `shim_dead_since`, `track_dead_since`,
  `loading_instant`, `p1a_ids` and their 2 unit tests. Empty peer name still shows "Friend".
  Left: `// P3 shim` (native = `net_remote()`, attached to the lowest-id peer) and
  `// P4 (WORLD)` (death_at = `world.friend_death_at`, same for every peer).
- Open for NET: the old `Net::take_hkb/take_enemy_states/take_damage/take_posture/take_netai`
  wrappers are now unused (dead-code warnings in net.rs; delete or keep until P1d).

### 5.8 P1c NATIVE/VIEW + NSESSION done (2026-10-24, unit tests only)
Signatures as frozen in 5.3; internals still serve ONE peer (`peers.first()`, the lowest id =
the 2-player friend), so 2 players behave as before. Not run in the game.
- native.rs: `NetWant { peer, want, hp }`, `ensure_peers(player, &[NetWant])` replaces
  `ensure_net` (removes a remote whose peer no longer wants one or left the list, then keeps
  one for the lowest-id wanting peer; creations for DIFFERENT peers >= 3 s apart, so one
  peer's respawn timing is unchanged). `Remote.peer`; `remote_of(peer)`, `remotes()`,
  `net_remote()` (compat = first of `remotes()`), `remove_peer`, `failing_for(peer)` (one
  shared failure count for now; `failing()` kept, puppet's ghost fallback uses it).
  `set_forward(mask: u8)` (tee forwards while mask != 0; stats print the mask).
  `push_inbound(from, ..)`: one inbox, `from` not filtered yet (P3: per-peer inboxes).
  `peer_of_chr(chr)`: lock-free `PEER_CHR[6]` (chr by peer index), rewritten by
  `publish_peers` on every create / remove / forget / vanish (cleared before the delete).
- artfx.rs: `tick(player, &PeerSet)` (remote = first peer's native), `on_raw(from, ..)`
  (`from` unused: one inbox, one fake member).
- ui.rs: `FriendTag { id, pos, name, ping_ms }` (no Default), `Status.friend_tags: Vec`,
  `publish_tags(Vec)`; the overlay draws every tag (camera read once per frame).
- nsession.rs: `Frame<'a> { player, peers, connected, is_host, in_world_for, here }`;
  private `remote()` / `epoch()` / `friend_block()` read `peers.first()`. Without a peer,
  `epoch()` returns the last peer epoch seen (`FRAME_EPOCH`, refreshed at the top of
  `tick`) instead of `Net::epoch()`: only differs while unlinked after a Net rebuild, where
  `tick` returns early anyway. `on_raw(from, ptype, data, epoch)`, `on_bulk(from, data,
  epoch)`; `from` unused (one lane, one PEER record: P3).
- livesync.rs `tick(enemies, &PeerSet)` (peers unused). activation.rs `set_peers(&[[f32;3]])`
  (up to 5 positions, lock-free; the setter forces every-frame AI near ANY of them),
  `log_stats(&[usize])` (one remote block per native remote). 2 new unit tests (activation,
  peer_of_chr).
- net.rs: `set_forward(mask)` / `set_forward(0)` on drop; Bulk(nsession) and PlayerRaw drop a
  sender slot without a PeerId (never happens: coop-net only yields 0..=5), then pass `from`.
- puppet.rs: `friend_tags`; activation gets the snapshot positions of every peer (hp > 0, our
  block, sane) while hosting; the tag list is built per peer (name now PeerFrame's, i.e.
  "Friend" instead of "?" for an empty name); `ensure_peers` with one `NetWant` for the
  friend, `native_chr = remote_of(friend)`; the `// P3 shim` in `build_peers` is gone
  (`build_peers(in_world: bool)`, native = `remote_of(p.id())` in the world). New
  `refresh_peer_natives()` right after `native.tick`: the set's `native` follows a remote
  created / removed this frame (artfx, nsession and later modules never see a deleted chr;
  `view` keeps the frame-start value). Left: puppet's `native_friend` (= `net_remote()`) for
  follow_native, the enemies' ghost target and bossrules' `friend_view`; spectate_cam.rs and
  ghost.rs needed no change.

### 5.9 P1c WORLD+RULES done (2026-10-24, build + unit tests only)
Signatures as frozen in 5.3; the rules still follow ONE peer (`peers.first()`, lowest id = the
2-player friend). Not run in the game.
- world.rs: `World::on_frame(net, peers, in_world_for)` (`peers` unused until P4's host
  sequencer). Events via `take_game_events_from`; the old `friend_death_at / friend_back /
  friend_boss / friend_down` fields became `peer_world: BTreeMap<PeerId, PeerWorld { death_at,
  back, boss, down }>` keyed by the sender (BTreeMap instead of HashMap: "first" = lowest id).
  EV_DEATH/BOSS/DOWN/BACK/REST write the sender's entry; a new epoch clears every entry's
  `boss`/`down` (death/back kept, as before); `resync_rest` clears every `boss`.
  `boss_announce: Option<PeerId>` (`take_boss_announce`). `note_wipe(&PeerSet)` marks every
  known peer and every peer in the set dead now. Accessors: `peer`, `peer_death_at`,
  `peer_boss`, `peer_down`, `peer_bosses()`, `any_peer_boss`, `any_peer_down`, `end_peer_boss`;
  compat for puppet `death_sync` (lowest-id entry): `friend_death_at()`, `friend_down()`,
  `take_friend_back()`. Join dump via `take_bulk_from` (any sender; P4: host only). 2 new tests.
- bossrules.rs: `Frame { net, world, peers, player, here, in_world_for, wake }`; remote /
  view / name from `peers.first()` (name falls back to `Net::peer_name()`, then "Friend");
  "loaded since" = `PeerFrame::loaded_at > t` (replaces `Net::friend_loaded_since`, now
  unused); the friend's fight / down / death read by its id; reward bulk `take_bulk_from`.
- bossgate.rs: `Ctx { connected, epoch, here, pos, peers: Vec<PeerCtx { peer, at: Option<(block,
  pos)>, fight }> }`; the gate follows the first entry.
- persist.rs: `Ctx { net, peers, enemies, player, in_world_for }`; hp / death_at of
  `peers.first()`. Deviation: `PeerFrame::death_at` is the world state of the frame START
  (build_peers runs before `World::on_frame`), so an EV_DEATH reaches persist one frame later
  than before (90 s windows: harmless; INTEGRATOR could refresh death_at after world.on_frame).
- restsync.rs `tick(enemies, &PeerSet)` (is_host from the set). ownworld.rs `on_frame(player,
  anim, &PeerSet, in_world_for)`: friend anim = `peers.host()`, else `first()` (the host side
  kept its client's anim). resync.rs busy check: `any_peer_boss` / `any_peer_down`.
  doctor.rs / keyitems.rs: `take_bulk_from`. join, doors, npcintro, scaling, debuff, items,
  leave, dialog: no change needed (join's caller is puppet, unchanged).
- puppet.rs call sites: `world.on_frame(n, &self.peers, ..)`, `restsync::tick(.., &self.peers)`,
  persist / bossrules `peers: &self.peers`, `ownworld::on_frame(.., &self.peers, ..)`, scaling
  bosses `world.peer_bosses()`, build_peers `death_at: world.peer_death_at(p.id())` (P4 shim
  gone), `death_sync` reads the compat methods.
- Behaviour differences only while the link is down (PeerSet empty, the old compat accessors
  kept the last peer for REMOTE_STALE_AFTER): every rule that used them is gated by
  `connected` or ends on "link lost", so 2-player behaviour is unchanged.
- Open for NET: `Net::friend_loaded_since` and `take_game_events` are now unused (warnings).

### 5.10 P1d done (2026-10-24, build + unit tests only, NOT run in the game)
Removed (unused after P1c): net.rs `take_hkb`, `take_enemy_states`, `take_damage`,
`take_posture`, `take_game_events`, `take_netai`, `rtt_ms`, `friend_loaded_since`; world.rs
compat `friend_world` / `friend_death_at` / `friend_down` / `take_friend_back`; puppet
`death_sync`'s `remote` argument (now `peers.first()`'s snapshot: death_sync only runs while
connected, where that is exactly what `remote` was). Kept on purpose: `Net::remote*`,
`peer_name`, `friend_loaded_recently` (puppet's ghost / join / notices still need the v7.5
link-down window, see below), the `send_*` = All wrappers and coop-net's compat `send_*`
(frozen API, loopback, tests). Build warning-free; coop-net 66 tests, dll lib 122 tests.
- `PeerFrame::death_at` is refreshed right after `World::on_frame` (puppet
  `refresh_peer_deaths`): persist sees an EV_DEATH the same frame again, as in v7.5.

2-player regression review of `git diff c1d75ef..HEAD` (v7.5 -> P1c), one peer connected:
- FIXED, death sync after a role switch in one game: world.rs keeps per-sender entries after
  the link drops, and the compat reads took the LOWEST entry. Joined a host (peer 0), later
  hosted (friend = peer 1): the host's wipe check read the old host's death/down instead of
  the client's (v7.5 had one field, overwritten by the current friend), so the client's death
  was missed: a real wipe went down as a single host death (EV_BACK sent, the client's dead
  list asked for and applied) unless its snapshot still read hp <= 0 (hidden while dead).
  (A client's friend is the host, id 0 = always the lowest entry: that side was right.)
  death_sync now reads world.rs by
  `peers.first()`'s id (`peer_death_at`, `peer_down`, `take_peer_back(id)`); test
  `a_stale_lower_entry_does_not_stand_for_the_current_friend`. bossrules / persist already
  read by id.
- FIXED, stale bulk transfers: v7.5 dropped every queued bulk on `Connected`; P1a only the
  joining slot's. A bulk from a slot no longer linked (Steam lobby role switch inside one
  `Net`: was client, now host) survived and `take_bulk_from(kind)` (any sender) could apply
  the old host's reward / key items / persist / doctor data. PeerJoined now drops the
  joining slot's bulks and every bulk from a slot whose link is not Connected (one link:
  all of them, = v7.5; N peers: the others' live transfers stay).
- Checked, identical: coop-net with one link (Hello/rejoin/replace = old `fresh` rule, Bye,
  Lost after 5 s, per-link reset = old `reset_link` incl. `next_xfer`, snapshot / raw order,
  event de-dup, bulk window, send gating = old "Connected" checks, `rtt_ms` = first link,
  pending counts = max of one); ids (host me 0 / friend 1, client me 1 (0xFF before Welcome
  reads 1) / host 0, loopback 0 / echo 1, `PeerSet::push` never drops the friend); event
  and inbox `from` (one sender per side, so `applied_damage` per (peer, enemy) and hkb
  `applied_n` per peer equal the old single maps); epochs (`PeerFrame::epoch` = `Net::epoch`
  while linked); native `ensure_peers` with one `NetWant` = old `ensure_net` (the stagger only
  applies across different peers); raw forwarding mask (v7.5 was already gated by
  `connected`); Steam host member list with one member, uid = SteamID64 of the address;
  `max_players = 2` -> `set_max_peers(1)`, `P1_LINK_CAP` 1, lobby `MAX_MEMBERS` 2; the
  PeerSet natives after `ensure_peers` / `tick` (`refresh_peer_natives`; nothing deletes a
  remote between the build and that point except the load / warp paths, which return).
- Accepted (link-down window only): the old compat accessors kept the last peer's snapshot
  for REMOTE_STALE_AFTER (500 ms) after a Bye; the PeerSet empties at once. Every consumer
  checked: enemies / world / restsync / livesync / persist / death_sync / hkb / scaling /
  native wants / nsession run only while connected; bossgate clears on link down; bossrules'
  spectate (dead 1 s -> wipe, friend-gone and link-lost timers, camera "position lost") reaches
  the same "link lost" release; ownworld's friend ObjAct anim cannot change in 500 ms after a
  Bye. Puppet's own ghost / join / native-spawn checks still read `Net::remote()` (compat), so
  they keep the v7.5 window exactly. Not fixed (no visible effect found): a peer's boss fight
  is no longer ended by a hidden snapshot that arrives in the same poll as its Bye (v7.5:
  ended; now: until the next epoch clears it, nothing reads it while unlinked); an
  `EV_BOSS(1)` announcement held over a warp and shown after a drop has no distance line; a
  spectate wait that starts in the very frame the link drops records no friend boss id.
- Accepted (cosmetic): a peer with an EMPTY name shows "Friend" on the nameplate and in
  bossrules' texts (v7.5: an empty string; `display_name` never sends one). nsession's epoch
  while unlinked is the last peer's (`FRAME_EPOCH`) instead of `Net::epoch` (differs only
  after a connection that nsession never saw; the engine is disengaged while unlinked).
Live check still open: the P1 row "full 2-player regression" (both roles, Steam and UDP).

### 5.11 P2 NET done (2026-10-24, build + unit tests only, NOT run in the game)
Built (coop-net protocol.rs / session.rs, net.rs, steam.rs; settings.rs one key):
- **Wire v10** (`VERSION = 10`, Steam lobby `proto` "10"): `Hello{password_hash, uid, name}`,
  `Welcome{name, slot}`, `Reject{reason}` (1 password, 2 full, 3 version). The routed kinds
  (Snapshot, Enemies, Damage, Posture, Event, Hkb, NetAi, Bulk, PlayerRaw; `Packet::is_routed`)
  carry `src, dst` right after the kind byte (`encode_routed` / `decode_routed`); the link-local
  kinds are unchanged. +2 bytes per routed datagram (largest: PlayerRaw 1119 B, Bulk chunk
  1118 B, under `MAX_DATAGRAM` 1200).
- **Slots** (host, `on_hello`): same address = same link (the v9 rejoin rule); else a link with
  the Hello's uid (connected or lost) keeps its slot and the old link goes (`PeerLeft{Replaced}`);
  else the lowest slot no link has, then a lost link's slot, all under `set_max_peers` (net.rs:
  `max_players - 1` every frame); else `Reject(Full)`. Kept from v9: cap 1 and uid 0 = the
  newcomer replaces the link. uids: Steam SteamID64; UDP FNV(COMPUTERNAME|USERNAME) (stable
  across restarts); echo peers `0xEC40_0000_0000_0000 | n`.
- **Relay** (host): a client's packet counts as from its own slot (a claimed `src` is ignored),
  is delivered if `dst` is 0 or all, and goes on to `dst` (all = every other client) with `src`
  kept. Unreliable kinds cut-through; snapshots at most `relay_snapshot_hz` per source
  (`set_relay_snapshot_hz`, steady-rate limiter; host itself gets every one). Event: acked on
  each hop, de-duplicated per hop, then queued on every target link under the host's own seq.
  Bulk: stored until complete, then a new transfer per target link (after what is queued there).
  `relay_stats()`; net.rs logs the totals every 60 s when something was relayed.
- **Receivers** order snapshots (`last_seq` per source) and raw packets ((src, ptype)) per
  source; a client drops routed packets whose `src` is itself / out of range or whose `dst` is
  neither it nor all.
- **Roster** (deviation: one more byte per entry): bulk `ROSTER_KIND` 0x52 `{gen u32, n u8,
  (slot u8, uid u64, inc u8, name)}`, slot 0 = host. `inc` is the host's link count for that slot,
  so a rejoin / replace with the same uid and name still reaches the others as
  `PeerLeft{Replaced}` + `PeerJoined` and resets their per-source order (a restarted game counts
  its seqs from 0). Sent to a client whenever it changes, but only while it names another client
  or to a client that was told about one before: **two players never see a roster**. Client:
  stale gens ignored; diff -> `PeerJoined` / `PeerLeft{Lost}` (removed: the roster has no reason)
  / `{Replaced}`, then `Event::Roster(full list incl. host and itself)`; the host link's Lost /
  Bye -> `PeerLeft` with that why for every other client. A UDP host's uid comes from the roster.
- **Session accessors on a client**: `peers()` = host + the roster's other clients;
  `link_state(slot)` of a roster member = the host link's state; `peer_rtt_ms` = None for
  relayed peers; `pending_*_to(slot)` = the host link's (it carries everything). `PeerQuit` /
  `Lost` only for links (client: slot 0). New: `set_relay_snapshot_hz`, `relay_stats`,
  `take_refusals` (host: refused Hellos (reason, name)).
- **Other versions**: a v9 Hello gets a v9-format Reject (`SKCO`, 9, kind 3, no body); 7.5 reads
  it as its own "host rejected the password" (7.5 only logs that). Our host logs it and shows
  "A player with another co-op version tried to join your world." A v10 client hearing another
  version from its host address gets `Rejected{Version}` (at most every 500 ms). A 7.5 host never
  answers a v10 Hello (UDP: we stay "searching"); Steam: the lobby proto differs, so the existing
  "another co-op version" status, now also as a notice.
- **net.rs**: clients learn the other clients from the roster (`PeerJoined` / `PeerLeft` drive
  `PeerState`; `connected` follows `link_state`), so `peers()` / `PeerSet` list them with their
  slots, the host link's epoch, `rtt_ms` None; their data arrives through the host with the
  original `from`. `send_*_to(Dest::Peer(id))` from a client goes via the host; the raw-forward
  mask sends one copy per wanting slot (each through the host). Own snapshot every frame with at
  most one peer (v7.5), `snapshot_hz_multi` with more. Notices (dialog::game_notice, Important):
  `Rejected` once per reason per 60 s (password / "The host's world is full. Trying again..." /
  version; the log line comes with it, no longer every 500 ms); host: refusals logged, notice for
  Full ("NAME tried to join, but your world is full (n/n).") and Version, wrong passwords log only.
  Status line with 2+ peers: "N players: you, A 40 ms, B (Steam)".
- **steam.rs**: lobby size = `tuning::max_players()` when the lobby is created; the host accepts
  every member's message session (the session caps and answers Reject(Full)); joiner:
  EChatRoomEnterResponse 4 -> `take_full()` notice, status "the host's world is full (Steam),
  trying again", retry in 5 s; `Want::Join`: a slot-filtered search without a hit is followed at
  once by one without the filter (so a full lobby is found and reported, not "not found");
  `other_version()` -> notice. `Want::Auto` searches unchanged.
- **`loopback_peers = k`** ([DEBUG], 0..5, `NetSettings`, a change reconnects): k echo clients
  inside the game join OUR host session over in-memory queues (`Hub`: the host transport is
  wrapped, so it works under UDP and Steam, also before Steam is up), each echoing our snapshots
  (and raw packets, back to the host only) 1 s, 1.25 s, 1.5 s ... later. They run only while we
  host (not as a joiner). With `loopback = 1`, k = the number of echoes (the first is the old UDP
  one). Room: `max_players >= 1 + friends + k`, else the extra ones log `refused: Full`. Log:
  `net: N loopback peer(s) will join while we host`, host side `net: connected to
  "loopback-echo-n" at 127.77.0.m:1`, `net: loopback peer loopback-echo-n (echo d ms) joined as
  slot s`, then the puppet's `peers: me 0 (host), N player(s): [...]`.
- Tests: coop-net 82 (17 new: protocol route/v9-reject/roster; session over an in-memory mesh
  with 2-6 sessions: six players join + Full, leave by Bye / silence + slot reuse, uid replace +
  order reset, every routed kind to all / to one / to host, src spoofing, events across the relay
  with loss on both hops and lost acks, bulk store-and-forward with lost chunks, roster sending
  rule + stale gens, Reject full / password / version both ways, per-source order, relay rate
  limit, host quit, and `two_players_behave_like_v9`); dll lib 123 (hub echo peers, settings).
  `build-dist` and `--features testsync,dev-native` build warning-free.

NOT verified live: everything above. In particular: Steam lobbies with 3+ members and the
host accepting them; that RequestLobbyList without the slot filter returns full lobbies and
JoinLobby answers 4 on a full one; UDP with 2+ clients behind NATs (only the host is reachable);
host upload with 5 clients; the 7.5 client's view of our v9 Reject; `loopback_peers` in game.

2-player parity (max_players = 2): the same datagrams flow (bytes differ: uid, slot, reason,
route), no roster, nothing relayed, same events and slots. Differences: a newcomer with ANOTHER
uid while the friend is connected now gets Reject(Full) instead of replacing the friend (a
friend whose uid changed waits up to LOST_AFTER for his old link to drop); Reject notices in
game (7.5: log only); a Steam joiner without a lobby makes one extra unfiltered search per miss.

For P3-P5 (and P4's epoch work):
- **Relayed data**: every event / inbox `from` is the ORIGINAL sender's slot; nothing shows the
  hop. On a client, `from != 0` means it came through the host. The host sees what a client
  sends to `Dest::Host` or `All`, but NOT what it sends to `Dest::Peer(k)` (relayed only). The
  session already relays a client's `All` traffic to the other clients: a host module must not
  re-broadcast it, and when the host must sequence something (world flags, §2), the client sends
  to `Dest::Host` and the host re-sends with its own `src` 0.
- No ordering between sources; events relayed one hop later than the host's own. Relayed
  snapshots arrive at `relay_snapshot_hz` (30) per source; PlayerRaw / NetAi / Hkb / Enemies /
  Damage / Posture are relayed at full rate. A relayed bulk starts only once the host has all of
  it and queues behind other bulks on that link.
- **Roster**: a client's peers come only from the roster bulk; unreliable data of a new client
  can arrive before its `PeerJoined` (net.rs keeps the `PeerState`, `peers()` lists it once it is
  in the roster). `Event::Roster` comes after the diff events. `PeerLeft{Lost}` for a relayed
  peer can mean quit or lost.
- **Epochs (must fix before max_players > 2 live)**: `Net::epoch()` (compat, newest link) changes
  on the host with EVERY client's join, so modules still keyed on it on the host (enemies,
  hkbsync, world, doctor, persist, resync, keyitems, testsync, puppet `scale_sent` /
  `join_hold`) take a second client's join as a reconnect of everyone. Switch them to
  `peer_epoch(id)` / `PeerFrame::epoch` (P4/P5/P6). A client's relayed peers share the host
  link's epoch.
- `pending_events()` / `pending_bulk_chunks()` are the max over links (world.rs flow control);
  per peer: `pending_*_to(id)`. `PeerState::rtt_ms` is None for a client's relayed peers.
- Raw packets: `native::set_forward(mask)` bit = slot; a client's raw packets to k other clients
  cost it k uploads (one per dst). With 2+ echo peers or clients, native.rs still has ONE inbox
  (P3: per-peer inboxes, `push_inbound(from, ..)` already carries the slot).
- `loopback_peers` echoes reply only to src 0 (our own stream) and send raw only to the host; each
  is a real client to everyone (its own slot, name `loopback-echo-n`), so the other clients see
  relayed copies of the host replayed late: a one-PC way to give P3 two or more remotes.

### 5.12 P3 NATIVE/VIEW done (2026-10-24, build + unit tests only, NOT run in the game)
Built (native.rs, artfx.rs, ui.rs, spectate_cam.rs; small edits in puppet.rs and enemies.rs):
- **native.rs, one Net remote per peer.** `remotes: Vec<Remote>` (sorted by peer id, one per
  peer, at most 5 = the 5 PGD slots; playerNo `pick_player_no` 1..5, never the local one or a
  taken one). Per peer: its own 8-byte recv session object (`RECV_OBJS[peer index]`, built on
  its first spawn, reused; one shared vtable copy `RECV_VT`); `recv_slot` maps `this` -> peer
  (an unknown `this` is served by the single active peer if exactly one is active, else
  nothing; counted in the stats line as "recv on an unknown session object"); its own raw
  inbox `INBOXES[peer index]` and active bit `RECV_ACTIVE` (bit = index): `push_inbound(from)`
  queues only into `from`'s inbox and drops packets of a peer without a remote. Per-peer
  bookkeeping `PeerNative { fails, retry_at, revive_seen }` (kept over "not wanted" spells,
  cleared when `native_remote = 0`, as v7.5's single counters).
- **`ensure_peers(player, &wants)`**: removes exactly the remotes whose peer is unlisted (left)
  or no longer wants one (`unwanted`), then the per-peer revive check (dead remote, peer alive
  for 2 s -> replaced), then at most ONE creation (`pick_spawn`: lowest wanting id without a
  remote and not backing off; no other peer's remote created in the last 3 s; a peer's own
  respawn is not delayed). Never a creation in a frame that deleted a remote (as v7.5). Failure
  back-off per peer (10 s x failures, max 60 s); a vanished remote counts as a failure for its
  peer. `wants_for(&PeerSet, here, in_world_for, connected)` = v7.5's friend rule per peer
  (`want_rule`): option on, linked, not in another block, and (has a remote, or alive here after
  `native_spawn_delay_s`).
- **Removal** `remove_peer(peer, why)`: its active bit off, its inbox cleared, `peer_of_chr`
  map republished, its position pin released (hook slot cleared at once), then the chr
  deleted; the other peers' remotes / inboxes / pins are untouched. `forget` (load) drops all.
- **Position pin moved from puppet.rs into native.rs** (`follow_peers(Option<&Net>)`, called
  where `follow_native` was): one pin per peer (`Pin`, shared `PinStats` 10 s line), target =
  that peer's `PeerState::remote_smoothed(130 ms)`. Hook slots `pin_slot(peer)` =
  `PIN_SLOT_BASE (58) + peer index` (58..63); enemies.rs now allocates `1..PIN_SLOT_BASE` (57
  enemy pins instead of 62).
- **artfx.rs**: one fake member (P0 5.1e/5.2.2, not k members: the type-63 loop takes each
  bullet's owner from its record, so per-message owners on one member are equivalent). Inbox
  entries carry `from`; `REMOTE_HANDLE`/`REMOTE_AT_MS`/`PASS_OWNER` per peer index;
  `members_hook` resolves the net-form owner of every sender with queued messages, drops the
  messages of senders without a fresh remote ("no remote"), serves the rest; `serve` rewrites
  each message's records to ITS sender's remote (`rewrite_records`, target cleared). A peer's
  remote going away drops only its queued messages.
- **ui.rs**: tags/arrows were already per peer (P1c); off-screen markers that would overlap
  are stacked along the edge (`edge_stack`). `presence(&[(PeerId, name)], is_host)`: per-name
  "NAME joined" / "NAME left" overlay lines (diff of the connected peers, `presence_diff`; the
  same slot under a new name = "NEW joined" only, as v7.5), replacing puppet's single
  `last_peer` notice. As host, a companion joining while another is already here also gets the
  game notice "NAME has entered your world." (the FIRST companion keeps leave.rs' unchanged
  "A companion has entered your world.", so 2 players see exactly v7.5's notices). The status
  line with names + pings is net.rs' (P2, unchanged).
- **spectate_cam.rs**: API for P4 (bossrules calls it already; the single-friend `learn` is
  gone): `update_peers(want, &PeerSet, from, engaged: &dyn Fn(&PeerFrame) -> bool)`,
  `learn_peers(player, alive, &PeerSet)` (per-peer previous positions), `target()`, and the pure
  `choose_target(peers, current, from, engaged)`: keep a living, engaged, drawn target; else the
  nearest living engaged drawn peer (to the camera, else `from`; tie = lowest id; logged
  "spectate: camera switches from A to B"); else keep the current target while it is drawn
  (dead too: wipe/release rules decide, as with 2 players).
- **Ghost fallback (ghost.rs unchanged)**: at most one peer, the friend (`peers.first()`, whose
  data `Net::remote()` reads). puppet.rs `ghost_allowed` now uses `failing_for(friend)`
  (`Native::failing()` removed), so another peer's failure or stagger never brings up the
  friend's ghost. A non-first peer whose native spawn fails is NOT drawn (logged by native.rs).
- puppet.rs edits (P3, kept small): the joined/left block -> `ui::presence` (field `last_peer`
  gone); `follow_native` + `NativePin` + their constants/fields removed (now native.rs); the
  `NetWant` block -> `native.wants_for(..)`; `ghost_allowed` as above.
- Tests (dll lib 135 in a HEAD + P3 tree; 157 in the shared tree with P4/P5 work): native `unwanted_maps_removal_to_exactly_that_peer`,
  `spawns_are_staggered_across_peers_and_lowest_id_goes_first`,
  `five_peers_get_one_spawn_per_three_seconds`, `player_numbers_skip_the_local_one_and_taken_ones`,
  `want_rule_is_v75s_friend_rule`, `pin_slots_are_one_per_peer_above_the_enemy_range`,
  `inbound_packets_reach_only_their_peers_inbox_and_recv_object`; artfx
  `records_get_their_senders_owner_and_lose_the_target`; ui `presence_diff_names_who_joined_and_left`,
  `edge_markers_stack_instead_of_overlapping`; spectate_cam
  `camera_keeps_its_target_and_switches_to_the_nearest_on_death`,
  `with_nobody_left_the_dead_target_stays_and_ties_go_to_the_lowest_id`. Release build warning-free
  in these files (default features and `--no-default-features`).

2-player parity (one peer): same spawn rule, timing, revive, back-off, vanish and ghost
fallback; one remote, one inbox, the same recv path (the `this` fallback covers an unexpected
`this`); artfx owner = the friend's remote as before. Differences: the friend's pin uses hook
slot 58/59 instead of 63 and enemies get 57 pins instead of 62; the pin is cleared the frame the
remote is removed (v7.5: next frame) and follows `Net::peer(id)` (connected only: no pin in the
500 ms compat window after a Bye, the remote is removed that frame anyway); a revived friend's
remote is never re-created in the frame it was deleted (v7.5 neither); an empty peer name shows
"Friend joined" (v7.5: " joined"); log lines name the peer slot.

NOT verified live: everything above. Risks: (1) that `recv` is really called with `this` = the
object at `chr+0x2038` (else, with 2+ remotes, nothing is served: watch "recv on an unknown
session object" in the stats line); (2) 2-5 Net remotes at once (memory, the driver crash after
creations, FPS) beyond N0's clones; (3) the 3 s stagger with 5 peers = 12 s until the last
remote shows after a load; (4) artfx with bullets from 2+ peers in one pass; (5) the owner
form `0xE8F040` of the 2nd..5th remote (playerNo 2..5) is assumed to work like the first's.
Live check (P3 row): `loopback_peers = 2` on one PC (two echo remotes), then 3 PCs.

What P4 / P5 can call:
- `Native::remote_of(peer)`, `remotes()` (id order), `net_remote()` (lowest id),
  `failing_for(peer)`, `remove_peer(peer, why)` (e.g. on `PeerLeft` for an immediate removal;
  `ensure_peers` also removes it the frame the peer leaves the `PeerSet`), `wants_for(..)`.
- `native::peer_of_chr(chr)` (lock-free, any thread; every remote, republished on each create /
  remove / forget / vanish) for nsession's LAST23 mapping.
- `native::PIN_SLOT_BASE` / `pin_slot(peer)`: hook slots >= 58 are taken.
- `ui::presence(..)` (already wired in puppet), `ui::presence_diff`.
- `SpectateCam::update_peers` / `learn_peers` / `target`, `spectate_cam::choose_target` (P4:
  `engaged` = living peer in the same fight, e.g. `world.peer_boss(id)` or bossgate's fight).

### 5.13 P4 WORLD+RULES done (2026-10-24, build + unit tests only, NOT run in the game)
Also ported the v7.6 hotfix (main repo coop-v7.1 `45d0c92`): flags 9100 / 9110 per player
(`PER_PLAYER_STATE`, never shared, kept across the join dump), test `player_state_flags_stay_local`.
- **Epochs per peer** (peers.rs `EpochTracker`, `Joins { joined, fresh }`, `PeerSet::epochs()` /
  `link_epochs()`: host = every client, client = the host only). `fresh` = a join with nobody
  from before still on the same link (two players: every (re)connection = old `Net::epoch()`).
  Switched: world (session = the link that started it; a later join gets its own snapshot,
  versions, boss state), resync + doctor cross-check (world's session epoch), doctor and key
  items (exchange per link), persist (full dead set / boss records to the joiner's link only;
  all links new = one send to all, as before), puppet scale config (per link), testsync (offer
  / DLL / log per client, `friend-log-<slot>.txt` from slot 2), bossgate (per peer gate),
  leave (join notice per link). Still on the global `Net::epoch()` (P6, MODE4): **enemies.rs
  :328** (`Enemies::epoch`: damage totals / per-session state) and **hkbsync.rs :112**
  (`HkbSync::epoch`); puppet's `join_hold(n.epoch())` / `join.on_frame(n.epoch())` are
  client-side = the host link (on the host join never warps), fine.
- **World flags, host = sequencer**: a client sends its flag changes to `Dest::Host` and takes
  `EV_FLAG` / `EV_TALK_FLAG` only from the host; the host applies a client's change and, with
  2+ clients, queues it (`relay_q`, same pacing / room as its own) and re-sends it with its own
  version to every OTHER peer (`Dest::Peer`), back to the origin too when the host sent a
  different value of that flag in the last 5 s (write race: host order wins in every world;
  test `write_race_ends_in_host_order`). Payload `b` now: bit 0 value, bits 1-3 origin slot,
  bits 4-30 version (27-bit wrap); versions kept per (sender, flag). Two players: nothing is
  re-sent (identical traffic). The join snapshot (+ `EV_HOST_IDOL`) goes to each joiner only
  (`Dest::Peer`; a client already in the session never gets one); clients accept a dump and
  the host idol from the host only. Doors unchanged.
- **bossgate per fight**: one `PeerGate` per peer (own epoch, away = far from THAT peer, its
  `EV_BOSS` start/fall/gauge-gone, its held / waiting flags); a flag is gated by its origin (the
  payload tag on a client). Learned start flags shared.
- **npcintro origin-aware** (`TalkOrder`, host): the first client whose talk flags of an NPC
  block arrive owns it while it talks (20 s after its last); another client's talk flags of
  that block meanwhile are dropped (not applied, not re-sent). Host's own talk and one client:
  never limited.
- **bossrules / spectate**: `engaged_ids` per peer (= v7.5 `in_fight && friend_here &&
  !friend_down` each); the reload is held while >= 1 is engaged. The wait watches the peers
  engaged at its start (+ any that engage later); each one leaves by EV_DEATH / EV_DOWN(1)
  ("wipe"), EV_BOSS(3) ("boss gauge gone") via `bossrules::peer_out` (world.rs no longer
  releases directly), 0 HP 1 s or out of the block 5 s (`watch_step`). Release when none is
  left (`wait_verdict`): "wipe" only if each one died; `note_wipe` / persist wipe only when the
  watched set is every connected peer (a living companion elsewhere: no session wipe). Camera =
  P3's `SpectateCam::update_peers` / `learn_peers` with "watched and not out" (nearest living,
  sticky, switches on its death). Boss announcements: a list (several at once). Expel on any
  rest and rewards to All unchanged (the session relays).
- **persist**: wipe = we + every connected peer dead, all within 90 s of each other
  (`is_wipe`); dead sets merged (union, unchanged), boss records minimum (unchanged).
- **death_sync**: host back after its death = a wipe only if every connected peer is dead
  (`peer_dead_for_wipe` / `all_dead`), else `EV_BACK` to all; each client sends its dead list to
  `Dest::Host`; the host kills every list it gets (union). (Every client sends, not only those
  in the host's block: the host's `kill_ids` skips what is not loaded, as v7.5.)
- **scaling**: `ScaleCfg.players` (9th byte; an 8-byte body = 2): the host sends 1 + connected
  peers (`scaling_count = session`) or 1 + peers in its block (`block`), per link and on change;
  clients use the host's count. `scaled(base, pct, players)` per extra player; defaults
  60/150/60/80 give the same numbers at two players.
- **restsync / resync / keyitems**: any peer's rest / resync request reaches everyone (session
  relay); key item grants and boss drops to All; the key-item doctor host <-> each client.
- **ownworld**: ObjAct progress windows from every peer's animation (index 1 + slot).
- **items / leave**: the Charm while connected to a full world (`max_players` > 2) says "This
  world is full (N/N)."; a client's Thread leaves alone, the host's closes for all ("Your
  companions return to their own worlds." with 3+); the host's join notice per link: the old
  text while the joiner is the only one, "NAME has entered your world." with others there.
  `peers::SESSION_PLAYERS` (published in `build_peers`) for those texts.
- Other files touched: net.rs one `#[allow(dead_code)]` on `take_bulk` (no caller left).
- Tests (+17): peers epoch tracker / link epochs, world payload / sequencing targets / write
  race / 9100, npcintro talk order, bossgate per-peer gate, bossrules engaged / wipe verdict
  1-5 peers / watch timers / camera target, persist wipe 2-6 players, scaling count + multiplier
  1-6, items full text, puppet death_sync wipe 2-6. dll lib 158, coop-net 82; build-dist and
  `--features testsync,dev-native` warning-free.

2-player parity (max_players = 2): same flags, events, bulks and timings; differences: the flag
payload layout (origin bits, 27-bit version; both sides of this build), join notice / key-item
texts read the peer's name ("Friend" fallback), and `scaling_count = block` now really counts
the block (default session = the old 2). Live check (P4 row) still open: 3 PCs: flags via host,
mid-session join, restores, rest resets all, spectate switch, wipe all dead, scaling at 3.

### 5.14 P5 NSESSION done (2026-10-24, build + unit tests only, NOT run in the game)
nsession PROTO 4 -> 5 (both sides need this build; a PROTO 4 peer gets "different builds" in
the handshake and stays on our own sync). Files: nsession.rs (engine split per link),
livesync.rs (docs, one log line, 1 test), activation.rs (doc only: it already took every
peer's position in P1c), settings.rs (+6 lines: the key below, outside NSESSION's files).
- **Wire.** Player netIds `0xFFF0 | slot` (§5.1d), rewritten on send (both forms, `chr+0x78`
  and `0xA4CE60`) and on serve (`0xFFF0|s` -> our player at our slot, else our native remote
  of slot s, 0xFFFF without one). The form names slots, not "sender / receiver", so the
  mapping is the same for every origin and the host relays the bytes unchanged. Deviation
  from §5.2.2: the rewrite happens when the game takes the packet (as in v7.5), not when it is
  queued; same result. Game frames (unreliable PlayerRaw and lane messages) =
  `u32 generation, u8 origin, u8 mode, bytes`, mode 0 all / 1 host only / 2 this peer only
  (§5.2.3). Owned / Lives gain `origin`; `MSG_ENGAGED = 7` `{generation, n, slots}` host ->
  each engaged client (on change and every 10 s).
- **Topology: hop by hop.** One lane (ARQ) per link: a client's only link is the host, the
  host has one per client (`LANES[slot]`). A client sends everything to the host (`dst 0`,
  never coop-net's relay); the host applies a client's mode-0 packet AND re-queues it for
  every other engaged client (origin kept, our generation for that link; `relay` counter in
  the 10 s types line), consumes mode 1, sends 23 with mode 2 to one client. Owned / Lives of
  an engaged client go on the same way (re-tagged). Unreliable 4 / 5: one PlayerRaw per target
  link (`send_player_raw_to`). Nothing is relayed with one client.
- **Generations per link.** `GEN_OUT[slot]`, `ACCEPT[slot]` (`1 << 32 | peer gen` while
  engaged with it), `ENGAGED_LINKS` bitmask; a frame counts only with its link's current
  generation and a believable origin (host: origin == the sending client; client: from the
  host, not our own slot). Generations come from one counter (as before). Epochs per link =
  `PeerFrame::epoch` (net.rs `peer_epoch`): a client's join never looks like a reconnect of
  the others. `FRAME_EPOCH` is gone.
- **Engine.** `Link` per partner (host: every client in the PeerSet; client: the host):
  handshake, engage, per-link drop rules (its leave / restart / other block for 3 s / no
  remote / the client's 23 rules), per-link latch + fallback world, cooldown, stage-0 hello.
  Shared: patches (applied with the first engaged link, restored when the last one leaves:
  `teardown`, the old disengage order), the client's late chr-loop patch, Owned sending, the
  Engaged set, ownerless report, ownfill, the host's 22 feed (per origin into
  `remote_of(origin)`'s record, stealth strip, first-22 23 force per link), stats (one lane line
  per link). Our block change / load / rest / stage change / setting off / role change = every
  link (each sees it). A client leaving alone (others stay engaged): its queued inbox entries
  go, its ownership record gets an empty candidate list (`0xA1C6C0(rec, 0)`) and the 23
  distribution is forced, so its enemies move to the others instead of staying NetAI without
  data.
- **Merged inbox** (one fake member, §5.1e): entries `(origin, bytes)`, queue depth times the
  number of origins. Count patches unchanged (§5.2.1).
- **Type 23 recipient** (host): two wraps, both from §5.1a, bytes checked once at start:
  `call 0xA29DD0` @`0xA1DFC1` (rcx = recipient chr -> `native::peer_of_chr`) and
  `call 0xA1D240` @`0xA1DFDD` (rcx = its record, `*(u32*)rec` = handle -> the peer whose remote
  has `u32 chr+8` == handle; this is the "fallback 0xA1D240" done as a call wrap, no prologue
  needed). Stubs `mov [rip+LAST23_x], rcx; jmp target` on the fake page; the to-member hook
  reads and clears them. Rule (`resolve_member`): a known recipient must be engaged (else
  dropped: `unmapped` in the types line); unknown = the one engaged peer if there is exactly
  one (the v7.5 "empty member = the friend"), else dropped, never broadcast. The wraps are in
  the host's patch set only when `max_players > 2` (with the default 2 the host's patches are
  byte for byte v7.5's). A send to our fake member = every peer.
- **Gate (automatic + setting).** `[ADVANCED] native_session_max_players = 6` (2..6, the
  host's value decides; clients follow the host's handshakes). The host keeps our own sync
  for the whole block (`Reason::OwnSync = 10`, a non-latched Leave to engaged clients, log
  `nsession: host keeps our own enemy sync: ...` once per change) when: more players than
  that; 3+ players and no usable recipient wrap (`wrap_mask` = 0: the native session stays a
  2-player feature, logged at start); or a peer in our block cannot join (a latched failure
  or fallback world with it, its native session unavailable, or our last handshake with it
  saw no hello = `native_session = 0` there). So a block is never half native: an unengaged
  peer in it would get no enemy sync at all (the host's mode 4 is off while anyone is
  engaged). A peer still handshaking (just joined, remote not spawned yet) does not block; it
  has no enemy sync for those seconds.
- **ownfill (§31) for N.** Competitors = the engaged players with their native remote
  positions (host: its engaged clients; client: the host + the clients of the Engaged set);
  key = distance, + 5 m for clients (the host's old tie band); the minimum (key, slot) takes
  it, so exactly one machine (a pairwise 2-player rule has a 3-player cycle where nobody
  would); `keep` = 5 m more hysteresis; a client never takes one without the host's position;
  an unknown client position does not compete. A conflict (another player's Owned list has
  it): drop at once if a lower slot owns it, else after 3 s. Needs a fresh Owned list from
  every competitor. Two players: exactly the 7.4 numbers (test).
- **livesync**: `take_lives` returns the states of every origin whose link generation is
  accepted; merged by minimum (lives only go down, a death anywhere is final) = "minimum over
  N". `engaged_key` = the one ACCEPT with one peer (unchanged), else a hash that changes
  whenever a client engages or leaves (the state starts over; the 2 s resends rebuild it).
- Tests: nsession 23 (+7: frame origin / mode, hop-by-hop relay targets / origins / modes, 23
  recipient resolution, engaged key + set, own-sync rule, slot netIds across the relay with
  different playerNo orders, ownfill nearest / tie / 3-player cycle / grid + conflicts),
  livesync +1 (minimum over 3 origins in any order). dll lib 159, coop-net 82, `build-dist`
  warning-free.

Two-player parity (one peer): the same handshake, generations, patches (with `max_players =
2`), drop rules, timers, latch / fallback, Owned / Lives / ownfill numbers; extra: one Engaged
message per engagement (ignored), log lines name the peer. Not identical: the bytes on the
wire (PROTO 5); with `max_players >= 3` and two players the host's 23 calls go through the
wrap stubs (same result).

NOT verified live: everything above. Live check (P5 row): first 2 PCs for parity (`ENGAGED
... with peer 1`, ownership both ways, hits once, lives sync, rest / load disengage), then
`native_session_max_players = 2` with 3 players (`host keeps our own enemy sync`), then 3 PCs
in one block: the start line `type-23 recipient wraps: ... ok`; host `ENGAGED ... with peer 1`
and `... with peer 2`, `engaged [1, 2]`; both clients `the host's engaged clients: [1, 2]`;
`host 22 from peer 1` / `2`; `owned-here` > 0 on each client; `relay` counts in the host's
types line; no `unmapped` for 23 (report it if there is); a client's hit on an enemy the other
client owns applied once; one client walks to another block: only it disengages (`leaves the
native session alone ... record emptied`) and its enemies move to the others.
Open: whether 4 / 70 / 71 carry player netIds (§5.2.5, also at N = 2). `loopback_peers` echo
clients echo our own handshake back, so by the code they cannot engage ("both sides think
they are the host", a non-latched timeout) and block nothing (not run): a one-PC N>2 native
test needs real clients.

### 5.15 P6 MODE4 done (2026-10-24, build + unit tests only, NOT run in the game)
Files: enemies.rs, hkbsync.rs, posture.rs, netai.rs, new mode4.rs (pure helpers + tests);
outside MODE4: lib.rs `mod mode4;`, net.rs two `#[allow(dead_code)]` on the now unused All
wrappers `send_hkb` / `send_damage`. motion.rs needed no change (one ghost, the friend's).
- **Authority / routing.** The host alone sends enemy states (mode 2/3), NetAI records (mode 4)
  and posture to All, once; the session delivers to every client and nothing is relayed or
  re-sent by a module. Clients take states / records / posture only `from` the host (others
  dropped, counted in the 20 s client line, should stay 0). A client's DamageReport (HP total +
  posture / deathblow totals, incl. the 250 ms re-sends) goes to `Dest::Host` (an All report
  would be relayed to every other client). Sync radius / NetAI near-far class = the distance to
  the nearest of us and every peer's snapshot position (as v7.5 with the one friend: no block
  filter).
- **Totals per (peer, enemy).** Host: `mode4::DamageTotals` (HP) and posture.rs `applied`
  keyed `(PeerId, entity)`; each client counts its own hits from 0 per link, so two clients
  hitting one enemy are two totals, each applied once (repeats / reordered totals add 0).
  Client-side totals (`dealt_total`, posture `client`) are unchanged (one host).
- **Epochs per peer** (replaces `Net::epoch()` at enemies.rs `Enemies::epoch` and hkbsync.rs
  `HkbSync::epoch`): `mode4::SessionEpochs` (like `peers::EpochTracker`, but a peer missing from
  one frame's set is not forgotten: forgetting it would apply its re-sent cumulative totals a
  second time; a new link always has a new epoch). enemies: `peers.link_epochs()` (host: each
  client; client: the host link); `fresh` (nobody stayed) = the old full reset (applied, dealt,
  posture session); another join = only that peer's applied HP / posture totals restart (log
  `enemies: peer s on a new link ...`). hkbsync: `peers.epochs()` (every sender, relayed ones
  too); fresh = the old reset (outbox, queues, ring, graphs, all counters); a join clears only
  that sender's counter. Extra: a sender's counter more than 2^16 behind what we applied is a
  restarted game (a relayed peer rejoining on the host link's epoch), not a repeat.
- **NetAI wire targets.** Player = `0x10 + slot` (`netai::WIRE_PLAYER_BASE`; two players: host
  0x10, client 0x11 = the old HOST/CLIENT codes); enemy = `WIRE_ENEMY` moved 0x12 -> 0x20 (0x12
  is slot 2 now; both sides need this build, v10 already locks 7.5 out). Host map
  (`mode4::player_targets`, netIds looked up once per tick when a record needs it): our player
  -> our slot, every peer's native remote (`PeerFrame::native`) -> its slot, then puppet's
  `ghost` (ReplayGhostIns, else the lowest-id native remote) -> the peer whose remote it is, else
  the first peer. Client (`target_from_wire`, `Follow` -> `follow: Option<PeerId>`): own slot ->
  our position, another slot -> that peer's snapshot position (the host's own; other clients'
  relayed at `relay_snapshot_hz`), refreshed every frame; unknown -> no target.
- **hkb per source.** Player events (entity 0) are raised only for the peer the ghost stands for
  (`peers.first()`: on a client the host, on the host the lowest client; at most one ghost, P3);
  enemy events (mode 3) only from the host; the rest counted "unused" in the 30 s line. The host
  sends to All (every client's ghost is the host); a client sends to `Dest::Host` only (no other
  client has a ghost of it).
- **Native session gate (integration point).** enemies.rs `native_owns_enemies()` = the single
  read of `nsession::step_aside::enemy_sync_off()` (posture.rs `enabled` reads it too). P5's
  host rule (§5.14: > `native_session_max_players`, 3+ without a recipient wrap, a same-block
  peer that cannot join) never engages, so it stays false and mode 4 runs for everyone with no
  further switch. If nsession ever engages only some peers, make that function per peer there.
- Activation positions for all peers: already done (P1c puppet `set_peers`, P5 activation.rs).
- Tests (+11): mode4 `damage_totals_are_per_peer_and_enemy`,
  `one_peer_totals_match_the_old_single_map`, `wire_targets_name_the_slot`,
  `host_maps_each_remote_to_its_peer`, `two_players_map_like_v75`, `client_resolves_every_slot`,
  `session_epochs_per_peer`; posture `host_totals_are_per_peer_and_enemy`; hkbsync
  `counters_are_per_sender_with_repeats_dropped`,
  `player_events_only_from_the_ghosts_peer_enemy_events_only_from_the_host`; enemies
  `netai_targets_go_on_the_wire_as_enemy_or_player_slot`. dll lib 170, coop-net 82; `build-dist`
  and `--features testsync,dev-native` warning-free.

Two-player parity (one peer): same states / records / posture / damage datagrams to the same
one link, same totals, same resets (every (re)connection is `fresh`), same targets (0x10 /
0x11). Differences: an enemy target's wire code 0x20 instead of 0x12; log lines name the peer
slot ("friend 1 hit ..."); a host that has both a spawned ReplayGhost and the friend's native
remote now maps an AI target on the native remote to the client too (v7.5: "unmapped", no
target); the client's player position is read every frame while an enemy follows any player.

NOT verified live: everything above. Live check (P6 row, `native_session = 0` or
`native_session_max_players = 2` with 3 PCs, `enemy_mode = 4`): both clients' `netai client: N
host-driven`; an enemy aggroed on client 2 turns to client 2 on both clients (not to client 1 /
the host); client 1 and client 2 hit the same enemy: the host log shows `friend 1 hit` and
`friend 2 hit` and its HP drops by the sum, once; posture / deathblow by client 2 consumes one
life (`posture: friend 2 deathblow`); client 2 rejoins: `enemies: peer 2 on a new link`, client
1's hits keep landing (no double damage, no ignored hits); no `entries from a non-host peer
dropped` on clients.

### 5.16 Review fixes (2026-10-24, build + unit tests only, NOT run in the game)
Fixes for the review findings of §5.11-§5.15. With `max_players = 2` (one peer) behaviour is
as before: the blocker rule only looks at OTHER peers, and the new drops need `origins() > 1`.
dll lib 180 (+10 over §5.15's 170), coop-net 84 (+2), `build-dist` and `--features
testsync,dev-native` warning-free.
- **1. Half-native block (nsession.rs).** `Link::blocks_block` (pure) decides per same-block
  peer whether it keeps the whole block on our own sync (mode 4): the old `cannot_join`
  reasons (latched failure, fallback world, unavailable, no hello) plus: the host has no
  native remote of it, a hello with another stage, still handshaking after more than one
  attempt, and idle after a FAILED attempt whose hello was seen (timeout, never ready, no
  23 / soft fails, no remote, patch sites busy). Load / rest / own-sync holds / setting off
  are not failures. A missing hello does not count for 15 s after the peer's loading screen
  (`NO_HELLO_LOADED_SECS`). Every blocker must stand `BLOCKER_GRACE` = 3 s before it counts
  (`mature_blockers`, `Engine::update_blockers` once per tick), so a join or a flicker does
  not drop every engaged peer (finding 5). Two traps handled: a link idle only because the
  gate holds it (`Link::held`, last tick's result) is not a blocker for the attempt rules,
  and a blocker is held back only by blockers of LOWER slots (`own_sync_rule`), so blockers
  never hold each other out. Readers are unchanged: `OWNS_ENEMIES` (= `any_engaged`) is read by
  enemies.rs `native_owns_enemies()`, posture.rs and puppet activation, and a matured blocker
  now disengages every engaged link (OwnSync) in the same tick, before `set_owns`. Known gap:
  during the 3 s grace an engaged block still has the not-yet-engaged peer without enemy sync
  (the §5.14 "just joined" case, now bounded). 3 s also applies to "no native remote": a
  remote that takes longer than that to spawn after a join drops the block to mode 4 until it
  is up (tuning point if it flaps live; look for `host keeps our own enemy sync: peer N ...`).
- **2. EV_BOSS(2) (world.rs, bossrules.rs).** The global `spectate_release("boss defeated")`
  is gone from `EV_BOSS`; world.rs calls `bossrules::boss_defeated_by(from, entity)` (entity =
  the event's, else the one the peer announced). The frame task releases only when `from` is in
  the wait's `watch` set or the entity is the wait's boss (`defeat_ends_wait`); otherwise it
  logs `peer N's boss ... fell, not the fight we wait for`. `ownworld::note_defeat` still runs
  for any sender. `spectate_release` stays for "rest".
- **3. All links at once (nsession.rs).** `disengage_many` / `disengage_all`: when the links
  going are every engaged link (`goes_all_at_once`), no link runs `drop_inbound_from` /
  `clear_record` (the game table calls 0xA1D7E0 / 0xA1C6C0 / make_due), and `teardown` runs
  once at the end (`Engine::all_going`). Used by load (`on_world_reset`), rest, role change,
  link down, `broke`, a stage change, and a new pre-pass in `tick` for our own block change
  (before, each link saw it separately in `engaged_link`). Setting off (`switched_off`) still
  goes link by link (not a load; left alone).
- **4. SYNC_IN (nsession.rs).** `counts_as_sync_in`: on a client only `origin == 0` packets
  count for the host-liveness and first-23 rule.
- **6. Duplicate join notice.** ui.rs `presence` no longer posts "... has entered your world."
  (its `is_host` parameter is gone; puppet.rs call updated); leave.rs `entered_text` is the
  only one.
- **7. Steam lobby size (steam.rs, net.rs).** `Lobby::set_max_members` is called every Steam
  tick with `tuning::max_players()`; while hosting it calls `ISteamMatchmaking::
  SetLobbyMemberLimit` (vtable slot 31 of SteamMatchMaking009, from the public header, NOT
  verified live; log `steam: lobby ... size set to N players (accepted: bool)`). Before the
  lobby exists the value is used at creation. Settings comment updated.
- **8. PeerJoined (net.rs).** `PeerState::uid`; `adopt_uid` clears `remote`, `history`,
  `loading_at`, `dead_since` when a known uid is replaced by a different known one (an unknown
  uid = UDP cannot tell, nothing is dropped). A client gives every relayed peer
  (`slot != 0`) a fresh `EPOCHS` value on each PeerJoined.
- **9. coop-net session.rs.** The roster bulk is queued ahead of transfers that have not
  started (`queue_bulk_first`, used by `sync_roster`). A transfer already under way keeps the
  front on purpose: the receiver assembles one bulk at a time and a newer one replaces an
  unfinished one, so putting the roster before a half-sent dump would lose that dump's acked
  chunks. `Session::rtt_ms()` takes the first link that is not Lost.
- **10. nsession.** (a) `serve` drops and counts (`player-not-here` in the types line) a
  packet of type 18 / 21 / 28 / 45 whose player netId field names a slot with no player here
  (`pnet_unmapped`), only when more than one origin feeds the inbox. (b) `resolve_member`
  drops a 23 when the chr wrap and the record wrap name different players. (c) The 10 s
  "type-23 sends without a known recipient ..." line only prints when the recipient wraps are
  patched (`wraps_applied`).

NOT verified live. Live checks: 3 PCs, one client with `native_session = 0` (the others must
not stay native: `host keeps our own enemy sync: peer N ... cannot join`, `enemies are ours`);
a load with 3 engaged (one `DISENGAGED` per link, no `leaves the native session alone`, no
`ownership record emptied` during the load); two boss groups in one block (one wipes /
wins, the other group's spectator wait is not released by the first's `EV_BOSS(2)`).

### 5.17 v7.7 merged (2026-10-24, build + unit tests only, NOT run in the game)

`git merge coop-v7.1` (v7.6 45d0c92 + v7.7 271ba60: IMPLEMENTATION-NOTES §32-§35). Every v7.7
fix is in; where it assumed one friend it now runs on the per-peer structures. Two players:
the same messages, timings and decisions as v7.7 (except the wire notes below).

**Wire.** Multi stays wire v10 / nsession PROTO 5 (a v7.x peer cannot link, so v7.7's "an
older peer leaves it unread" notes do not apply here). New bulk kinds 44 (grant ack), 45 (lot
ledger), 60 (auditor) fit beside 1/5/6/7/20-23/30/41-43/50, `0x4E` and the roster `0x52`.
nsession: v7.7's MaxHp was message 7, which multi already uses for Engaged, so here it is
**message 8** (`MSG_MAX_HP`), per link, never relayed (`Peer::max_hp` per slot).

**Adapted for N.**
- persist (§35): the wipe latch is gone for N too: each death is stamped on alive -> 0 (us and
  each connected peer, `peer_alive`), and `wipe_deaths(ours, peers, cleared)` clears once per new
  set of deaths (every player dead within 90 s, `is_wipe`); `hold_load` / `frozen()` as v7.7.
- keyitems (§33), per link (host <-> each client, a client with the host; `KeyLink` per link
  epoch, `update_links` at the top of `on_frame`): the queued grants go out on every new link
  (host: `Dest::Peer(client)`; a client: `Dest::All`, the host relays). Acks go to the grant's
  sender (`grant_from`, `Dest::Peer`). A queued grant leaves `pending_grants.txt` once every
  peer present in the session acked it (`fully_acked`; two players = the friend's ack). The lot
  ledger is sent per link (connect, and 5 s after every rest on every link) and each link's
  ledger is applied on its own (one per frame, `apply_ledger`); a ledger from a peer that is not
  a link (another client) is ignored.
- nsession (§34): the throw ring notes the machine of each throw (`ThrowNote::slot`); "our
  player" = `WIRE_SLOT | ME`, a received throw's player = that origin's player netIds here
  (`slot_pnets`); the collide rule drops a received throw whose attacker is its origin's player
  while ours runs on that enemy. Host contests: every engaged client's last 22 is kept per link
  (`Link::last22`); a netId claimed by two or more machines (the host's `claims_now()` and / or
  clients' prio-0xFFFF entries) is settled by the host (`contest_winner`: the candidates meet
  one by one through `host_wins_contest`, deathblow first, then the nearer player with 1.5 m for
  the last winner). Each client's 22 loses its entries for our claims it did not win and for
  contests another machine won (`Link::blocked`, `strip_22`); a change re-feeds that client's
  last 22 and forces 23. Two players: the v7.7 log line (`both claim ... the host | the friend
  runs it`); 3+: `N machines claim ... peer S runs it (...)`. The engage HP check takes that
  link's MaxHp.
- livesync (§34 rest baseline): the lives come merged from every engaged peer, so the baseline
  per netId is the minimum of any peer's first states after a rest (simple, correct for N).
- scaling (§35): every loaded enemy on both roles, with multi's `players` count.
- world: `audit::flag_write` on our sends and on every received flag (any origin).

**2-player only for now.**
- The auditor (§32, audit.rs): with 3+ players in the session it says once `audit: 3+ players
  in the session: comparisons off until two remain` and sends / reads nothing (no digest, HP
  sample or reward ack, its inbox drained, no findings); back to two = a fresh start as on a
  new connection. Its reward counters and HP samples speak of "the friend" (one peer).
- Contest logs and stats keep the friend's wording; the pending-grant log lines too.
- A client's queued grant reaches clients that join later only through the host's ledger
  (the client resends its queue on a new host link only).

Tests: persist `every_wipe_pair_clears_once` now covers 3 players; new: keyitems
`queued_grant_done_when_every_peer_acked`, nsession `contests_between_any_machines_are_
settled_by_the_host`; v7.7's tests adapted (throw ring slot, `throw_collides` netId list, MaxHp
in the message round trip). dll lib 207, coop-net 84, coop-audit 6; build-dist and
`--features testsync,dev-native` warning-free.

Live checks (both from v7.7 §32-§35 and for N): 2 PCs as v7.7's lists; 3 PCs: a grant made
while one client is away comes over on its next link; two clients behind one miniboss: one
`3 machines claim` / `both claim` line per change, one deathblow per orb.

### 5.18 v7.8-v7.13 merged (2026-10-24, build + unit tests only, NOT run in the game)

`git merge coop-v7.1` (3b68596 v7.8 .. 6b63da2 v7.13 + handoff aef6fb2; IMPLEMENTATION-NOTES
§36-§38, SPEC-BOSS-PHASES, GAP-ANALYSIS). Every v7.8-v7.13 change is in; where it assumed one
friend it now runs per peer.

**Wire.** Both branches had bumped to v10 (multi: routing / roster; v7.10: `Snapshot.max_hp`),
so the merge is **VERSION 11** (routing + `max_hp`). `version_reject` answers v10 without a
body (the released 2-player v10 has a bodiless Reject), from v11 with the reason byte.

**Adapted for N.**
- Party HP bars (v7.10): one row per connected peer, id order (`PeerSet`), each peer's last
  visible HP / max HP kept through its loads (`party_last_hp` per `PeerId`), state from
  `party_state` (away = no visible snapshot or another block, dead = 0 HP).
- Remote throw pin release (v7.11): `REMOTE_THROW_UNTIL` per slot; `throw_remote_slots` finds
  every other slot's player in a type 18 (wire `WIRE_SLOT | s` when sent, `slot_pnets` when
  received); native.rs `follow_peers` releases that peer's pin for 4 s and eases back
  (`Pin::in_throw` / `resume`, `moved_eased`, `ease_toward`, log budget reset every 10 s).
- Doors (v7.11): `doors::tick` takes every peer's anim (an ObjAct animation of any peer marks
  the ON flags that arrive with it); guard 3 (a peer's OFF for an open door ignored) also
  drops the host's queued relay of that OFF, so the other clients keep it ON.
- Host closing its world (v7.10, leave.rs): reloads when the host's intent goes Solo with any
  companion connected; per-companion join notices kept.
- Minibosses (v7.10) / boss phases (v7.12): `mini_scan` takes every peer's position ("the
  friend at it" = the nearest peer); `our_fight` includes the miniboss; the scripted-loss
  check takes our gauge, else an engaged peer's boss, else our recent gauge.
- v7.8 ini switches (`ns_force_distribution`, `ns_throw_claims`, `ns_throw_collide_drop`):
  the host's per-link re-feed of a client's last 22 and the forced 23 obey them like v7.8.
- Reconnect resume / `session_gen` (v7.9/v7.10, join.rs): unchanged; join is client -> host
  and `session_gen` counts our own deliberate session changes (global is right).
- Cross-cycle gate (v7.13, doctor.rs): already per link (each client compares with the host
  and leaves on a refusal); the REFUSED line names the peer slot.
- NG+ block / quit block (v7.12-v7.13): `net::link()` in a session = any link.

**Still 2-player only.** The auditor / coop-audit (§5.17); statelog's "friend" wording; doors
log wording ("the friend"); world.rs idol notice wording; the full-record snapshot may still
carry flags 20-30 (as on coop-v7.1).

Tests: new nsession `throw_window_and_remote_involvement` (per slot), puppet
`party_row_state_per_peer`, native `moved_is_eased_between_hitch_floor_and_warp` /
`ease_closes_a_quarter_of_the_gap_from_cur`, protocol v10 bodiless reject; all v7.8-v7.13
tests. dll lib 242, coop-net 84, coop-audit 6; warning-free (default features).

### 5.19 Auditor and log analyzer for every peer (2026-10-24, build + unit tests only, NOT run in the game)

Replaces the "2-player only" notes of 5.17 / 5.18 for the auditor and coop-audit.

**In game (audit.rs, §32).** Star like the rest of multi: one `Link` per audit partner
(`PeerSet::link_epochs`: the host one per client, a client one for the host). Each link has
v7.7's per-friend state: hello / periodic / rest / boss / follow-up schedule, the 45 s grace
trackers (lots, items, skills, rewards; shared-flag detail), "runs the auditor", HP samples and
the HP / lives / posture timers per boss, its ack and detail outbox. Every message goes to the
partner only (`Dest::Peer`); clients never exchange auditor messages. A new peer or a new link
epoch restarts only that link (`sync_links`); a peer that leaves is dropped and its pending acks
forgotten. Per-player data (items, skills, map-pickup lots) is compared with the 2-player rules
(`compare` unchanged); shared records must match host <-> each client, so across all players.
- Reward counters are per peer (`Rewards::counters(peer)`: sent to it, received / resolved /
  not given from it); a digest carries the counters towards its recipient, so `compare`'s
  sent-vs-received check is per pair. The hooks gained the sender: `reward_received(.., from)`,
  `reward_resolved(.., from)` (keyitems.rs: `grant_from` / `pending_drops`; bossrules.rs:
  `pending_rewards` keep it). A grant of ours expects acks from every partner that ran the
  auditor when it went out (`Want` per peer, overdue per peer); a client's grant is checked by
  the host only. A grant from another client (relayed by the host) is still checked locally
  ("not given" SUSPECT) but not acked (that client is not our partner).
- Traffic: hello rounds of several links are staggered by `STAGGER_MS` (10 s) per partner, rest
  and boss-death rounds too (`schedule_all`); the host starts at most one round per
  `ROUND_GAP_MS` (5 s); HP samples go per link at 2 Hz only to partners in our block with an idle
  bulk queue to that peer (`pending_bulk_chunks_to`).
- Output: with 3+ players a finding about one peer is `audit: SUSPECT [peer 2 'Friend2'] (sev 3)
  ...` / `audit: info: [peer 2 'Friend2'] ...` (also in audit.txt, which then lists every partner
  and its latest comparison); dedupe keys are per peer (`p<slot>:`). Two players: the same
  lines, timings and audit.txt as v7.7 (one link; `STAGGER_MS` / `ROUND_GAP_MS` never apply).
  Local checks (flag ping-pong, boss seen damaged) stay untagged.

**Offline (coop-audit, serve.ps1).** `coop-audit <host log> <friend log> [<friend log> ...]`;
two files = the old report byte for byte. More: `analyze_session` takes the first friend log
as the reference, keeps the others whose host-clock span overlaps it (one per player name,
`friend_label` from `log-<name>-<time>.txt`, the rest listed as skipped), analyses each
host <-> friend pair (texts name the friend: `sent to the friend 'Friend2'`; host lines counted
once), and checks every client's grants (script lot, boss drop, boss reward) in every other
client's log (`rewards` with both clients' clocks). `[peer ..]`-tagged SUSPECT lines are read.
serve.ps1 passes the other players' newest uploads of the last 6 hours (up to 5) after the
new one; none = the old call.

**Still 2-player shaped.** No client <-> client comparison in game (HP between two clients
fighting without the host in the block, client-to-client grant acks): the offline analyzer and
the receiving client's local "not given" check cover grants. coop-audit's host-side key-item
and flag lines do not say which peer they are about. "the friend" wording inside texts (the
tag says which peer).

Tests: audit `rewards_are_acked_and_counted_per_peer`, `each_peer_is_compared_with_the_host_on_its_own`,
`links_follow_the_peers_and_restart_one_at_a_time` (+ v7.7's reward test on the per-peer API);
coop-audit `friend_labels_and_tagged_audit_lines`, `one_friend_log_is_the_two_player_report`,
`a_session_covers_each_pair_and_client_to_client_grants`. dll lib 245, coop-net 84, coop-audit 9;
warning-free (the `--no-default-features` `patch_jmp_pub` warning is older).

### 5.20 v7.14-v7.15 merged (2026-10-24, build + unit tests only, NOT run in the game)

`git merge coop-v7.1` (1b001ef v7.14 .. a4d84f8 handoff v7.15; IMPLEMENTATION-NOTES §39-§42,
JATKA-TASTA-MULTIPLAYER, NEXUS-DRAFT, LICENSE, README). Every v7.14-v7.15 change is in, run per
peer. Conflicts: nsession.rs (dblock vs routing), bossrules.rs (spectate triggers vs per-peer
watch), persist.rs (committed deaths vs per-peer wipe), spectate_cam.rs (orbit / lock-on vs
target choice), ui.rs (head bar vs stacked edge markers), puppet.rs / livesync.rs (one line each).

**Wire.** No new message, but **VERSION 12**: the deathblow arbitration and the spectate / boss
rules have to run on every machine of a session, so an 11 build is refused (Reject with the
reason byte, as for 11; the 2-player v10 still gets its bodiless Reject).

**Deathblow lock (§39) for N.** One lock per enemy netId, `Holder::Us | Peer(slot)`; the
0xBE3810 gate vetoes our player's attempt while ANY other player holds it (bitset = locks of
any peer). The decision now runs in `on_game` on the wire form (attacker `WIRE_SLOT | origin`),
before the host relays, so a dropped 18 reaches neither our game nor the other clients.
Collisions:
- host vs a client: v7.14's pairwise rule on both machines (earlier start wins, start =
  arrival - RTT/2 of that link (`PeerFrame::rtt_ms` per slot), tie within 120 ms -> host);
  the pairwise agreement test is kept (`both_machines_agree_on_one_winner`);
- client vs client: the host decides by **arrival at the host** (the one clock everybody
  shares) and relays only the winner's 18 (`OtherKeeps` = dropped, not relayed). Deviation
  from "earliest start": two clients' starts are compared only through the host, so within
  the latency difference the first to reach the host wins;
- a client takes a relayed client 18 as the host's verdict (`Verdict`: applied, our own throw
  gives way) and drops the host's own 18 while the host has given way to another client
  (`LEFTOVER_MS` = 2 s after that client's latest 18; later it is a new deathblow).
`every_machine_agrees_on_one_winner_through_the_host` simulates host + 3 clients (each its own
table, exact RTTs, the host relaying what it applied) over press times / latencies: exactly one
deathblow survives and every screen shows it last.

**Party HP bars (v7.14 ini).** `party_hp_position/content/scale` as on coop-v7.1. Head mode:
each nameplate (`FriendTag.id`) gets its own peer's row (`PartyMember.id`, `member_of`; no row
= no bar, never another peer's); off-screen markers return their stacked plate, the bar hangs
under it. Left / top: one row per peer in id order (unchanged from §5.18).

**Statelog** (1000 menu states + throw node): no peer assumptions, merged as is.

**Spectate release safety (§40).** Any peer's rest releases (`World::apply_rest` ->
`rest_while_dead`, our own rest too). Per watched peer HP 0 for `DEAD_AFTER` (now 5 s, a
resurrection resets it) or its committed EV_DEATH = dead; the wait ends when every watched
peer is out, a wipe only when each of them died (`wait_verdict`), and that verdict is now a hard
trigger like v7.15's EV_DEATH (cancels a pending "boss defeated" check), as are link lost, the
5 min limit and rests. A watched peer whose link drops: the camera switches at once to another
living one (`choose_target` needs a drawn peer), the peer is out after `GONE_AFTER`. Dead
watchdogs (15 / 45 s, forced reload -> warp) and the phase-B held-load replay are unchanged.

**Spectator camera (§41).** Orbit + lock-on run on the watched peer (`update_peers` passes
`boss_hint`; lock candidates within 40 m of that peer). New switch input: `spectate_switch_key`
(ini, virtual-key, default 5 = mouse side button X1, 0 = off) or D-pad right = next living
engaged peer by id, D-pad left = previous (`next_peer`, wraps; 250 ms debounce; logged
`spectate: watching <name> (switch input; was ...)`). Not the flick / stick edge: those steer
the orbit while unlocked. Input is sampled once per frame (`update_peers`), `step_orbit` reuses it.

**Boss get-up / lives / persist (§42).** livesync's lives-drop-before-dead, 6 s get-up hold,
monotonic `set_lives` and single-life boss tracking are per enemy copy (no peer assumption).
persist: boss records restored only after OUR committed death reload; the wipe needs our
committed death (`DEATH_COMMITTED` edge) plus every connected peer's EV_DEATH within 90 s
(snapshot HP 0 no longer counts for peers either).

**Still 2-player shaped.** Client-vs-client deathblow collisions use host arrival order, not
estimated starts. A client's `LEFTOVER_MS` heuristic for the host's cut deathblow. Log /
comment wording "the friend" in livesync and spectate overlay texts.

Tests: nsession `the_host_keeps_the_first_client_and_clients_follow_its_verdict`,
`every_machine_agrees_on_one_winner_through_the_host` (+ v7.14's dblock tests on the per-slot
API), spectate_cam `the_switch_input_walks_the_living_peers_by_id`, ui
`head_mode_finds_each_peers_own_row`, persist `a_resurrection_is_not_a_death_for_the_wipe`; all
v7.14-v7.15 tests. dll lib 290, coop-net 84, coop-audit 9; warning-free (the
`--no-default-features` `patch_jmp_pub` warning is older).
