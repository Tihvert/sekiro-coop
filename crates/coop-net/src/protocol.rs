//! Wire format: fixed little-endian encoding, no dependencies.
//!
//! Every datagram: magic "SKCO" (4), protocol version (u16), kind (u8), body.
//! Unknown magic/version/kind is rejected, so stale builds never misread data.
//!
//! Wire v10 (PLAN-MULTIPLAYER §1): the routed kinds (`Packet::is_routed`) carry `src, dst`
//! (two u8 slots) right after the kind byte: `src` is the slot that made the packet, `dst` a
//! slot or `DST_ALL` (every slot but `src`). The host relays them between clients. The
//! link-local kinds (Hello, Welcome, Reject, Heartbeat, Bye, Ping, Pong, Ack, BulkAck) have
//! no route: they only mean something on the link they arrived on.

pub const MAGIC: [u8; 4] = *b"SKCO";
/// 9 (2026-10-23): `Damage` carries posture and deathblow totals, new `Posture`.
/// 10 (2026-10-24): up to 6 players. Hello carries a uid, Welcome the client's slot, Reject a
/// reason; routed kinds carry `src, dst`. (coop-v7.1 also used 10, for `Snapshot.max_hp`.)
/// 11 (2026-10-24): both 10s merged: multi-player routing + `Snapshot.max_hp` (party HP bars).
/// 12 (2026-10-24): v7.14-v7.15 merged (§5.20). Same wire layout as 11; the deathblow lock
/// (host arbitration of type 18 collisions) and the spectate / boss rules must run on every
/// machine of a session, so an 11 build is refused.
/// 13 (2026-10-24): the snapshot's `hidden` byte carries bits: bit 0 hidden, bit 1 in a
/// cutscene (`Snapshot.cutscene`). A 12 build would read a cutscene as plain hidden.
/// 14 (2026-10-31): Kuro's Charm (6911) joins the host-owned phase mask, bit 5. A 13 build would
/// force the bit off on the client or never send it.
pub const VERSION: u16 = 14;

/// Session slots (PLAN-MULTIPLAYER §5.3): the host is 0, clients 1..=`MAX_PEERS`. A slot is a
/// plain `u8` here (the dll wraps it in `peers::PeerId`).
pub const MAX_PEERS: usize = 5;
/// `send_*_to` / wire destination: every slot but the source (the host relays to the others).
pub const DST_ALL: u8 = 0xFF;
/// Bytes of the `src, dst` route after the kind byte of a routed packet.
pub const ROUTE_LEN: usize = 2;
/// Bulk kind of the host's roster (session.rs; never delivered as `Event::Bulk`).
pub const ROSTER_KIND: u8 = 0x52;

/// Most behavior events in one datagram (64 * 12 bytes).
pub const MAX_HKB_PER_PACKET: usize = 64;

/// Most enemy states in one datagram (48 * 28 bytes + header stays under ~1400).
pub const MAX_ENEMIES_PER_PACKET: usize = 48;

/// Largest datagram we build (whole packet, header included). Stays well
/// under a 1280-byte IPv6 / tunnel MTU, so nothing is fragmented.
pub const MAX_DATAGRAM: usize = 1200;

/// NetAI record sizes as sent (RE-FINDINGS-2 § Enemy AI authority). The
/// movement record is 28 B in the game; its last 4 bytes are unused.
pub const NETAI_MOVE_LEN: usize = 24;
pub const NETAI_BEHAVIOR_LEN: usize = 344;
pub const NETAI_ACTION_LEN: usize = 48;

/// Payload bytes per bulk chunk (one datagram each).
pub const BULK_CHUNK: usize = 1100;
/// Largest bulk transfer accepted (the flag dump is at most ~100 KB).
pub const MAX_BULK_BYTES: usize = 256 * 1024;
pub const MAX_BULK_CHUNKS: usize = MAX_BULK_BYTES.div_ceil(BULK_CHUNK);

/// Largest raw game player packet we forward (RE-FINDINGS-2 § Native remote
/// player: the game's own type-1 packet is at most 1024 B; its loopback
/// queue entries hold up to 2048 B, but nothing that big fits a datagram).
pub const MAX_PLAYER_RAW: usize = 1100;
/// magic, version, kind, route, seq, type, length.
pub const PLAYER_RAW_HEADER: usize = 4 + 2 + 1 + ROUTE_LEN + 4 + 4 + 2;

/// One host-driven enemy for the client's NetAIManipulator: the raw game
/// records the host's AI wrote for it, plus HP (death authority). Every
/// record is optional: movement goes out every tick for near enemies only,
/// behavior/action only when they changed (each repeated a couple of times
/// under the same `*_seq`, which the client applies once).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct NetAiEntry {
    /// Entity id (same in both games: same map).
    pub id: i32,
    pub hp: i32,
    pub beh_seq: u8,
    pub act_seq: u8,
    /// `NETAI_MOVE_LEN` bytes: pos xyz, rot xyz (f32).
    pub movement: Option<Vec<u8>>,
    /// `NETAI_BEHAVIOR_LEN` bytes, target field in our wire form (netai.rs).
    pub behavior: Option<Vec<u8>>,
    /// `NETAI_ACTION_LEN` bytes.
    pub action: Option<Vec<u8>>,
}

const NETAI_HAS_MOVE: u8 = 1;
const NETAI_HAS_BEHAVIOR: u8 = 2;
const NETAI_HAS_ACTION: u8 = 4;
/// id, hp, bits, beh_seq, act_seq.
const NETAI_ENTRY_HEADER: usize = 4 + 4 + 3;
/// magic, version, kind, route, count.
pub const NETAI_PACKET_HEADER: usize = 4 + 2 + 1 + ROUTE_LEN + 1;
/// The count byte.
pub const MAX_NETAI_PER_PACKET: usize = 255;

impl NetAiEntry {
    fn field_ok(f: &Option<Vec<u8>>, len: usize) -> bool {
        f.as_ref().is_some_and(|v| v.len() == len)
    }

    /// Encoded size (a field of the wrong length is left out on encode).
    pub fn wire_len(&self) -> usize {
        NETAI_ENTRY_HEADER
            + if Self::field_ok(&self.movement, NETAI_MOVE_LEN) { NETAI_MOVE_LEN } else { 0 }
            + if Self::field_ok(&self.behavior, NETAI_BEHAVIOR_LEN) { NETAI_BEHAVIOR_LEN } else { 0 }
            + if Self::field_ok(&self.action, NETAI_ACTION_LEN) { NETAI_ACTION_LEN } else { 0 }
    }
}

/// Host's shared event flag records, sent once per connection (world.rs).
/// Layout: magic, status, record count, then `{u32 base flag id, u16 len,
/// bytes}` per non-zero record.
pub const FLAG_DUMP_MAGIC: [u8; 4] = *b"FLGD";
/// The dump holds the host's records.
pub const FLAG_DUMP_OK: u8 = 0;
/// The host has the option off: nothing to apply, do not wait.
pub const FLAG_DUMP_DISABLED: u8 = 1;
/// The host could not read its flags (not loaded, Remnant bank): do not wait.
pub const FLAG_DUMP_UNAVAILABLE: u8 = 2;

pub fn encode_flag_dump(status: u8, records: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut v = Vec::with_capacity(16 + records.iter().map(|(_, b)| 6 + b.len()).sum::<usize>());
    v.extend_from_slice(&FLAG_DUMP_MAGIC);
    v.push(status);
    let records = &records[..records.len().min(u16::MAX as usize)];
    v.extend_from_slice(&(records.len() as u16).to_le_bytes());
    for (base, bytes) in records {
        let n = bytes.len().min(u16::MAX as usize);
        v.extend_from_slice(&base.to_le_bytes());
        v.extend_from_slice(&(n as u16).to_le_bytes());
        v.extend_from_slice(&bytes[..n]);
    }
    v
}

pub fn decode_flag_dump(buf: &[u8]) -> Option<(u8, Vec<(u32, Vec<u8>)>)> {
    let mut r = Reader { buf, at: 0 };
    if r.take(4)? != FLAG_DUMP_MAGIC {
        return None;
    }
    let status = r.u8()?;
    let n = r.u16()? as usize;
    // The count comes from the peer: never reserve more than the bytes left
    // can hold (6 header bytes per record).
    let mut out = Vec::with_capacity(n.min(buf.len().saturating_sub(r.at) / 6));
    for _ in 0..n {
        let base = r.u32()?;
        let len = r.u16()? as usize;
        out.push((base, r.take(len)?.to_vec()));
    }
    (r.at == buf.len()).then_some((status, out))
}

/// The local player's state, sent many times per second (unreliable).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Snapshot {
    /// Increases by one per snapshot; receivers drop older ones.
    pub seq: u32,
    pub pos: [f32; 3],
    pub angle: f32,
    /// Current animation id (0 until animation sync exists).
    pub anim: i32,
    pub hp: i32,
    /// Max HP of the sender (0 = unknown), for the party HP bar.
    pub max_hp: i32,
    /// Map/block the player is in, so the other side can hide the puppet
    /// when you are in different areas.
    pub block: i32,
    /// True while loading, dead or in a menu: the puppet is hidden.
    pub hidden: bool,
    /// The sender is in a cutscene (bit 1 of the wire byte). It also sends `hidden`, so the
    /// receivers hide the puppet while the cutscene moves the player.
    pub cutscene: bool,
    /// Replay-style motion (RE-FINDINGS-2 § Ghost animation, part 2): the
    /// movement vector the behavior turns into walk/run (magnitude 0.5 walk,
    /// 1.0 run), and the active behavior sync states `id | seq << 16`.
    pub move_vec: [f32; 4],
    pub dash: bool,
    pub n_states: u8,
    pub states: [u32; MAX_SYNC_STATES],
    /// The sender shows us as a native remote player and wants our game's
    /// own player packets (`Packet::PlayerRaw`). Carried by every snapshot,
    /// so no reliable handshake is needed.
    pub want_raw: bool,
}

/// Most behavior sync states in one snapshot (the game keeps at most 16).
pub const MAX_SYNC_STATES: usize = 16;

/// One enemy's state, owned by the host and mirrored on the client.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct EnemyState {
    /// Stable per-session enemy id agreed by both sides.
    pub id: i32,
    pub pos: [f32; 3],
    pub angle: f32,
    pub hp: i32,
    pub anim: i32,
}

/// Client -> host: everything the client has dealt to enemy `id` this
/// session, as TOTALS (cumulative, so a lost or repeated packet changes
/// nothing; the host applies only the increase of each field).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DamageReport {
    pub id: i32,
    /// HP the enemy lost to the client's hits.
    pub hp: i32,
    /// Posture the client's hits took off, in the host's units (posture.rs).
    pub posture: i32,
    /// Deathblows the client landed (each consumed one of the enemy's lives).
    pub deathblows: u16,
}

/// Host -> client (unreliable): one enemy's posture on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PostureState {
    pub id: i32,
    /// Current posture (counts DOWN towards the break) and its max.
    pub posture: i32,
    pub max: i32,
    /// Lives left (deathblows still needed to keep it from dying at 0 HP).
    pub lives: i8,
}

/// Most posture states in one datagram (64 * 13 bytes).
pub const MAX_POSTURE_PER_PACKET: usize = 64;

/// One behavior (hkb) event raised on a character's graph: what replays use
/// to animate ghosts. `n` counts the sender's events, so the receiver can
/// apply each once although packets repeat recent events (unreliable link).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct HkbEvent {
    pub n: u32,
    /// 0 = the sender's player; otherwise an enemy entity id.
    pub entity: i32,
    pub idx: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Packet {
    /// Client → host: join with the shared password's hash, our uid (Steam: SteamID64, UDP:
    /// a stable per-machine id; 0 = unknown) and a display name.
    Hello { password_hash: u64, uid: u64, name: String },
    /// Host → client: accepted as `slot` (1..=MAX_PEERS).
    Welcome { name: String, slot: u8 },
    /// Host → client: refused (`session::RejectReason` as u8: 1 password, 2 full, 3 version).
    Reject { reason: u8 },
    Snapshot(Snapshot),
    /// Keeps the session alive while no snapshots flow (loading screens).
    Heartbeat,
    /// Clean quit; the other side stops waiting at once.
    Bye,
    /// Host -> client (unreliable): up to `MAX_ENEMIES_PER_PACKET` enemy states.
    Enemies(Vec<EnemyState>),
    /// Client -> host: TOTAL damage (HP, posture, deathblows) the client has
    /// dealt to one enemy this session. Cumulative, so a lost packet loses
    /// nothing; the host applies only the increase.
    Damage(DamageReport),
    /// Host -> client (unreliable): host posture of enemies that are not at
    /// full posture (or just changed).
    Posture(Vec<PostureState>),
    /// Round-trip probe; `t` is the sender's clock in microseconds, echoed back.
    Ping { t: u64 },
    /// Reply to `Ping` carrying the same `t`.
    Pong { t: u64 },
    /// Reliable game event: re-sent until `Ack`ed, delivered once per `seq`.
    Event { seq: u32, kind: u8, a: i32, b: i32 },
    /// Acknowledges the `Event` with this `seq`.
    Ack { seq: u32 },
    /// Recent behavior events (unreliable, each repeated for a short while).
    Hkb(Vec<HkbEvent>),
    /// Host -> client (unreliable): NetAI records of host-driven enemies.
    NetAi(Vec<NetAiEntry>),
    /// Reliable bulk transfer: chunk `idx` of `count` (session.rs re-sends
    /// until acked; the receiver delivers the whole transfer once).
    Bulk { xfer: u16, kind: u8, idx: u16, count: u16, data: Vec<u8> },
    /// Acknowledges one bulk chunk.
    BulkAck { xfer: u16, idx: u16 },
    /// One packet the game's own player session sent (unreliable). `ptype`
    /// is the game's packet type (1 = the full player frame); `seq` counts
    /// the sender's raw packets, so the receiver drops older ones per type.
    PlayerRaw { seq: u32, ptype: u32, data: Vec<u8> },
}

// Hello and Reject keep their numbers in every version: a host answers an old client's
// Hello with a Reject that the old build can read (`version_reject`).
pub(crate) const K_HELLO: u8 = 1;
const K_WELCOME: u8 = 2;
pub(crate) const K_REJECT: u8 = 3;
const K_SNAPSHOT: u8 = 4;
const K_HEARTBEAT: u8 = 5;
const K_BYE: u8 = 6;
const K_ENEMIES: u8 = 7;
const K_DAMAGE: u8 = 8;
const K_PING: u8 = 9;
const K_PONG: u8 = 10;
const K_EVENT: u8 = 11;
const K_ACK: u8 = 12;
const K_HKB: u8 = 13;
const K_NETAI: u8 = 14;
const K_BULK: u8 = 15;
const K_BULK_ACK: u8 = 16;
const K_PLAYER_RAW: u8 = 17;
const K_POSTURE: u8 = 18;

const MAX_NAME: usize = 32;

impl Packet {
    /// Carries `src, dst` on the wire and may be relayed by the host.
    pub fn is_routed(&self) -> bool {
        matches!(
            self,
            Packet::Snapshot(_)
                | Packet::Enemies(_)
                | Packet::Damage(_)
                | Packet::Posture(_)
                | Packet::Event { .. }
                | Packet::Hkb(_)
                | Packet::NetAi(_)
                | Packet::Bulk { .. }
                | Packet::PlayerRaw { .. }
        )
    }
}

/// Version and kind of a datagram that starts like ours, whatever its version.
pub fn peek_header(buf: &[u8]) -> Option<(u16, u8)> {
    let mut r = Reader { buf, at: 0 };
    if r.take(4)? != MAGIC {
        return None;
    }
    Some((r.u16()?, r.u8()?))
}

/// A Reject{Version} that a build speaking `version` can read: v9 and older have a Reject
/// without a body (so does the released 2-player v10 of coop-v7.1), v11 one reason byte
/// (coop-multi's own v10 also had the byte but was never released).
pub fn version_reject(version: u16) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&MAGIC);
    v.extend_from_slice(&version.to_le_bytes());
    v.push(K_REJECT);
    if version >= 11 {
        v.push(3);
    }
    v
}

/// One member in the host's roster. `inc` counts the links the host has had for this slot
/// (wrapping): a rejoin with the same uid and name is still a new connection to the others.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    pub slot: u8,
    pub uid: u64,
    pub inc: u8,
    pub name: String,
}

/// The host's roster bulk (`ROSTER_KIND`): generation (u32), count (u8), then per member
/// slot (u8), uid (u64), inc (u8), name. The host itself is slot 0.
pub fn encode_roster(generation: u32, list: &[RosterEntry]) -> Vec<u8> {
    let list = &list[..list.len().min(MAX_PEERS + 1)];
    let mut v = Vec::with_capacity(5 + list.len() * (11 + MAX_NAME));
    v.extend_from_slice(&generation.to_le_bytes());
    v.push(list.len() as u8);
    for e in list {
        v.push(e.slot);
        v.extend_from_slice(&e.uid.to_le_bytes());
        v.push(e.inc);
        put_name(&mut v, &e.name);
    }
    v
}

pub fn decode_roster(buf: &[u8]) -> Option<(u32, Vec<RosterEntry>)> {
    let mut r = Reader { buf, at: 0 };
    let generation = r.u32()?;
    let n = r.u8()? as usize;
    if n > MAX_PEERS + 1 {
        return None;
    }
    let mut out: Vec<RosterEntry> = Vec::with_capacity(n);
    for _ in 0..n {
        let e = RosterEntry { slot: r.u8()?, uid: r.u64()?, inc: r.u8()?, name: r.name()? };
        if e.slot as usize > MAX_PEERS || out.iter().any(|o| o.slot == e.slot) {
            return None;
        }
        out.push(e);
    }
    (r.at == buf.len()).then_some((generation, out))
}

/// FNV-1a 64: stable across builds and machines (unlike std's hasher).
pub fn password_hash(password: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"sekiro-coop:".iter().chain(password.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// A packet as the host sends it to everyone (routed kinds: `src` 0, `dst` all).
pub fn encode(p: &Packet) -> Vec<u8> {
    encode_routed(p, 0, DST_ALL)
}

/// A packet with its route (ignored for the link-local kinds).
pub fn encode_routed(p: &Packet, src: u8, dst: u8) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    v.extend_from_slice(&MAGIC);
    v.extend_from_slice(&VERSION.to_le_bytes());
    // The route goes right after the kind byte, which every routed arm pushes first.
    let route = |v: &mut Vec<u8>| v.extend_from_slice(&[src, dst]);
    match p {
        Packet::Hello { password_hash, uid, name } => {
            v.push(K_HELLO);
            v.extend_from_slice(&password_hash.to_le_bytes());
            v.extend_from_slice(&uid.to_le_bytes());
            put_name(&mut v, name);
        }
        Packet::Welcome { name, slot } => {
            v.push(K_WELCOME);
            put_name(&mut v, name);
            v.push(*slot);
        }
        Packet::Reject { reason } => {
            v.push(K_REJECT);
            v.push(*reason);
        }
        Packet::Snapshot(s) => {
            v.push(K_SNAPSHOT);
            route(&mut v);
            v.extend_from_slice(&s.seq.to_le_bytes());
            for f in s.pos {
                v.extend_from_slice(&f.to_le_bytes());
            }
            v.extend_from_slice(&s.angle.to_le_bytes());
            v.extend_from_slice(&s.anim.to_le_bytes());
            v.extend_from_slice(&s.hp.to_le_bytes());
            v.extend_from_slice(&s.max_hp.to_le_bytes());
            v.extend_from_slice(&s.block.to_le_bytes());
            v.push(u8::from(s.hidden) | (u8::from(s.cutscene) << 1));
            for f in s.move_vec {
                v.extend_from_slice(&f.to_le_bytes());
            }
            v.push(s.dash as u8);
            let n = (s.n_states as usize).min(MAX_SYNC_STATES);
            v.push(n as u8);
            for st in &s.states[..n] {
                v.extend_from_slice(&st.to_le_bytes());
            }
            v.push(s.want_raw as u8);
        }
        Packet::Heartbeat => v.push(K_HEARTBEAT),
        Packet::Bye => v.push(K_BYE),
        Packet::Enemies(list) => {
            v.push(K_ENEMIES);
            route(&mut v);
            let list = &list[..list.len().min(MAX_ENEMIES_PER_PACKET)];
            v.push(list.len() as u8);
            for e in list {
                v.extend_from_slice(&e.id.to_le_bytes());
                for f in e.pos {
                    v.extend_from_slice(&f.to_le_bytes());
                }
                v.extend_from_slice(&e.angle.to_le_bytes());
                v.extend_from_slice(&e.hp.to_le_bytes());
                v.extend_from_slice(&e.anim.to_le_bytes());
            }
        }
        Packet::Damage(d) => {
            v.push(K_DAMAGE);
            route(&mut v);
            v.extend_from_slice(&d.id.to_le_bytes());
            v.extend_from_slice(&d.hp.to_le_bytes());
            v.extend_from_slice(&d.posture.to_le_bytes());
            v.extend_from_slice(&d.deathblows.to_le_bytes());
        }
        Packet::Posture(list) => {
            v.push(K_POSTURE);
            route(&mut v);
            let list = &list[..list.len().min(MAX_POSTURE_PER_PACKET)];
            v.push(list.len() as u8);
            for e in list {
                v.extend_from_slice(&e.id.to_le_bytes());
                v.extend_from_slice(&e.posture.to_le_bytes());
                v.extend_from_slice(&e.max.to_le_bytes());
                v.push(e.lives as u8);
            }
        }
        Packet::Ping { t } => {
            v.push(K_PING);
            v.extend_from_slice(&t.to_le_bytes());
        }
        Packet::Pong { t } => {
            v.push(K_PONG);
            v.extend_from_slice(&t.to_le_bytes());
        }
        Packet::Event { seq, kind, a, b } => {
            v.push(K_EVENT);
            route(&mut v);
            v.extend_from_slice(&seq.to_le_bytes());
            v.push(*kind);
            v.extend_from_slice(&a.to_le_bytes());
            v.extend_from_slice(&b.to_le_bytes());
        }
        Packet::Ack { seq } => {
            v.push(K_ACK);
            v.extend_from_slice(&seq.to_le_bytes());
        }
        Packet::Hkb(list) => {
            v.push(K_HKB);
            route(&mut v);
            let list = &list[..list.len().min(MAX_HKB_PER_PACKET)];
            v.push(list.len() as u8);
            for e in list {
                v.extend_from_slice(&e.n.to_le_bytes());
                v.extend_from_slice(&e.entity.to_le_bytes());
                v.extend_from_slice(&e.idx.to_le_bytes());
            }
        }
        Packet::NetAi(list) => {
            v.push(K_NETAI);
            route(&mut v);
            let list = &list[..list.len().min(MAX_NETAI_PER_PACKET)];
            v.push(list.len() as u8);
            for e in list {
                let mv = NetAiEntry::field_ok(&e.movement, NETAI_MOVE_LEN);
                let bh = NetAiEntry::field_ok(&e.behavior, NETAI_BEHAVIOR_LEN);
                let ac = NetAiEntry::field_ok(&e.action, NETAI_ACTION_LEN);
                v.extend_from_slice(&e.id.to_le_bytes());
                v.extend_from_slice(&e.hp.to_le_bytes());
                let bits = (mv as u8 * NETAI_HAS_MOVE) | (bh as u8 * NETAI_HAS_BEHAVIOR) | (ac as u8 * NETAI_HAS_ACTION);
                v.extend_from_slice(&[bits, e.beh_seq, e.act_seq]);
                for (ok, f) in [(mv, &e.movement), (bh, &e.behavior), (ac, &e.action)] {
                    if ok && let Some(b) = f {
                        v.extend_from_slice(b);
                    }
                }
            }
        }
        Packet::Bulk { xfer, kind, idx, count, data } => {
            v.push(K_BULK);
            route(&mut v);
            v.extend_from_slice(&xfer.to_le_bytes());
            v.push(*kind);
            v.extend_from_slice(&idx.to_le_bytes());
            v.extend_from_slice(&count.to_le_bytes());
            let n = data.len().min(BULK_CHUNK);
            v.extend_from_slice(&(n as u16).to_le_bytes());
            v.extend_from_slice(&data[..n]);
        }
        Packet::BulkAck { xfer, idx } => {
            v.push(K_BULK_ACK);
            v.extend_from_slice(&xfer.to_le_bytes());
            v.extend_from_slice(&idx.to_le_bytes());
        }
        Packet::PlayerRaw { seq, ptype, data } => {
            v.push(K_PLAYER_RAW);
            route(&mut v);
            v.extend_from_slice(&seq.to_le_bytes());
            v.extend_from_slice(&ptype.to_le_bytes());
            // Session::send_player_raw refuses bigger ones; never build an
            // oversized datagram even when called directly.
            let n = data.len().min(MAX_PLAYER_RAW);
            v.extend_from_slice(&(n as u16).to_le_bytes());
            v.extend_from_slice(&data[..n]);
        }
    }
    v
}

pub fn decode(buf: &[u8]) -> Option<Packet> {
    decode_routed(buf).map(|(p, _)| p)
}

/// A packet and, for the routed kinds, its `(src, dst)`.
pub fn decode_routed(buf: &[u8]) -> Option<(Packet, Option<(u8, u8)>)> {
    let mut r = Reader { buf, at: 0 };
    if r.take(4)? != MAGIC || r.u16()? != VERSION {
        return None;
    }
    let kind = r.u8()?;
    let routed =
        matches!(kind, K_SNAPSHOT | K_ENEMIES | K_DAMAGE | K_POSTURE | K_EVENT | K_HKB | K_NETAI | K_BULK | K_PLAYER_RAW);
    let route = if routed { Some((r.u8()?, r.u8()?)) } else { None };
    let p = match kind {
        K_HELLO => Packet::Hello { password_hash: r.u64()?, uid: r.u64()?, name: r.name()? },
        K_WELCOME => Packet::Welcome { name: r.name()?, slot: r.u8()? },
        K_REJECT => Packet::Reject { reason: r.u8()? },
        K_SNAPSHOT => {
            let seq = r.u32()?;
            let pos = [r.f32()?, r.f32()?, r.f32()?];
            let (angle, anim, hp, max_hp, block) = (r.f32()?, r.i32()?, r.i32()?, r.i32()?, r.i32()?);
            // Bit 0 hidden, bit 1 in a cutscene (VERSION 13).
            let flags = r.u8()?;
            let mut s = Snapshot {
                seq,
                pos,
                angle,
                anim,
                hp,
                max_hp,
                block,
                hidden: flags & 1 != 0,
                cutscene: flags & 2 != 0,
                move_vec: [r.f32()?, r.f32()?, r.f32()?, r.f32()?],
                dash: r.u8()? != 0,
                ..Default::default()
            };
            let n = r.u8()? as usize;
            if n > MAX_SYNC_STATES {
                return None;
            }
            s.n_states = n as u8;
            for i in 0..n {
                s.states[i] = r.u32()?;
            }
            s.want_raw = r.u8()? != 0;
            Packet::Snapshot(s)
        }
        K_HEARTBEAT => Packet::Heartbeat,
        K_BYE => Packet::Bye,
        K_ENEMIES => {
            let n = r.u8()? as usize;
            if n > MAX_ENEMIES_PER_PACKET {
                return None;
            }
            let mut list = Vec::with_capacity(n);
            for _ in 0..n {
                list.push(EnemyState {
                    id: r.i32()?,
                    pos: [r.f32()?, r.f32()?, r.f32()?],
                    angle: r.f32()?,
                    hp: r.i32()?,
                    anim: r.i32()?,
                });
            }
            Packet::Enemies(list)
        }
        K_DAMAGE => Packet::Damage(DamageReport { id: r.i32()?, hp: r.i32()?, posture: r.i32()?, deathblows: r.u16()? }),
        K_POSTURE => {
            let n = r.u8()? as usize;
            if n > MAX_POSTURE_PER_PACKET {
                return None;
            }
            let mut list = Vec::with_capacity(n);
            for _ in 0..n {
                list.push(PostureState { id: r.i32()?, posture: r.i32()?, max: r.i32()?, lives: r.u8()? as i8 });
            }
            Packet::Posture(list)
        }
        K_PING => Packet::Ping { t: r.u64()? },
        K_PONG => Packet::Pong { t: r.u64()? },
        K_EVENT => Packet::Event { seq: r.u32()?, kind: r.u8()?, a: r.i32()?, b: r.i32()? },
        K_ACK => Packet::Ack { seq: r.u32()? },
        K_HKB => {
            let count = r.u8()? as usize;
            if count > MAX_HKB_PER_PACKET {
                return None;
            }
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                list.push(HkbEvent { n: r.u32()?, entity: r.i32()?, idx: r.i32()? });
            }
            Packet::Hkb(list)
        }
        K_NETAI => {
            let n = r.u8()? as usize;
            let mut list = Vec::with_capacity(n);
            for _ in 0..n {
                let mut e = NetAiEntry { id: r.i32()?, hp: r.i32()?, ..Default::default() };
                let bits = r.u8()?;
                if bits & !(NETAI_HAS_MOVE | NETAI_HAS_BEHAVIOR | NETAI_HAS_ACTION) != 0 {
                    return None;
                }
                e.beh_seq = r.u8()?;
                e.act_seq = r.u8()?;
                if bits & NETAI_HAS_MOVE != 0 {
                    e.movement = Some(r.take(NETAI_MOVE_LEN)?.to_vec());
                }
                if bits & NETAI_HAS_BEHAVIOR != 0 {
                    e.behavior = Some(r.take(NETAI_BEHAVIOR_LEN)?.to_vec());
                }
                if bits & NETAI_HAS_ACTION != 0 {
                    e.action = Some(r.take(NETAI_ACTION_LEN)?.to_vec());
                }
                list.push(e);
            }
            Packet::NetAi(list)
        }
        K_BULK => {
            let xfer = r.u16()?;
            let kind = r.u8()?;
            let idx = r.u16()?;
            let count = r.u16()?;
            let n = r.u16()? as usize;
            if n > BULK_CHUNK || count as usize > MAX_BULK_CHUNKS || idx >= count {
                return None;
            }
            Packet::Bulk { xfer, kind, idx, count, data: r.take(n)?.to_vec() }
        }
        K_BULK_ACK => Packet::BulkAck { xfer: r.u16()?, idx: r.u16()? },
        K_PLAYER_RAW => {
            let seq = r.u32()?;
            let ptype = r.u32()?;
            let n = r.u16()? as usize;
            if n > MAX_PLAYER_RAW {
                return None;
            }
            Packet::PlayerRaw { seq, ptype, data: r.take(n)?.to_vec() }
        }
        _ => return None,
    };
    // Trailing bytes mean a different layout: reject rather than guess.
    (r.at == buf.len()).then_some((p, route))
}

fn put_name(v: &mut Vec<u8>, name: &str) {
    let mut end = name.len().min(MAX_NAME);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    v.push(end as u8);
    v.extend_from_slice(&name.as_bytes()[..end]);
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.buf.get(self.at..self.at + n)?;
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
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn name(&mut self) -> Option<String> {
        let n = self.u8()? as usize;
        if n > MAX_NAME {
            return None;
        }
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn all_packets() -> Vec<Packet> {
        vec![
            Packet::Hello { password_hash: password_hash("susi"), uid: 76561198000000001, name: "Tihvert".into() },
            Packet::Hello { password_hash: 0, uid: 0, name: String::new() },
            Packet::Welcome { name: "Kaveri".into(), slot: 3 },
            Packet::Reject { reason: 2 },
            Packet::Snapshot(Snapshot {
                seq: 42,
                pos: [164.2, -29.0, 22.4],
                angle: -2.86,
                anim: 20000,
                hp: 1000,
                max_hp: 1450,
                block: 1100,
                hidden: true,
                cutscene: true,
                move_vec: [0.5, 0.0, -0.25, 0.0],
                dash: true,
                n_states: 2,
                states: { let mut s = [0; MAX_SYNC_STATES]; s[0] = 7 | 3 << 16; s[1] = 1200; s },
                want_raw: true,
            }),
            Packet::Heartbeat,
            Packet::Bye,
            Packet::Enemies(vec![]),
            Packet::Enemies(vec![enemy(1)]),
            Packet::Enemies((0..48).map(enemy).collect()),
            Packet::Hkb(vec![]),
            Packet::Hkb((0..64).map(|i| HkbEvent { n: i, entity: i as i32 * 7, idx: -(i as i32) }).collect()),
            Packet::Damage(DamageReport { id: 7, hp: 1234, posture: 0, deathblows: 0 }),
            Packet::Damage(DamageReport { id: -1, hp: i32::MAX, posture: i32::MAX, deathblows: u16::MAX }),
            Packet::Damage(DamageReport { id: 1_100_500, hp: 0, posture: 345, deathblows: 2 }),
            Packet::Posture(vec![]),
            Packet::Posture((0..64).map(posture).collect()),
            Packet::Ping { t: 0 },
            Packet::Ping { t: u64::MAX },
            Packet::Pong { t: 123_456_789 },
            Packet::Event { seq: 7, kind: 3, a: -5, b: i32::MAX },
            Packet::Ack { seq: u32::MAX },
            Packet::NetAi(vec![]),
            Packet::NetAi(vec![netai(1, true, true, true), netai(2, true, false, false), netai(-3, false, true, false)]),
            Packet::Bulk { xfer: 3, kind: 1, idx: 0, count: 1, data: vec![] },
            Packet::Bulk { xfer: u16::MAX, kind: 9, idx: 4, count: 5, data: vec![7; BULK_CHUNK] },
            Packet::BulkAck { xfer: 65535, idx: 4 },
            Packet::PlayerRaw { seq: 0, ptype: 1, data: vec![] },
            Packet::PlayerRaw { seq: u32::MAX, ptype: 61, data: (0..MAX_PLAYER_RAW).map(|i| i as u8).collect() },
        ]
    }

    #[test]
    fn player_raw_fits_one_datagram_and_rejects_bad_input() {
        let full = encode(&Packet::PlayerRaw { seq: 9, ptype: 1, data: vec![0x5A; MAX_PLAYER_RAW] });
        assert_eq!(full.len(), PLAYER_RAW_HEADER + MAX_PLAYER_RAW);
        assert!(full.len() <= MAX_DATAGRAM, "{}", full.len());
        for cut in 0..full.len() {
            assert_eq!(decode(&full[..cut]), None, "cut at {cut}");
        }
        let mut extra = full.clone();
        extra.push(0);
        assert_eq!(decode(&extra), None);
        // A length over the limit in the header is refused, not read.
        let mut big = encode(&Packet::PlayerRaw { seq: 1, ptype: 1, data: vec![] });
        let at = big.len() - 2;
        big[at..].copy_from_slice(&((MAX_PLAYER_RAW + 1) as u16).to_le_bytes());
        big.extend(std::iter::repeat_n(0u8, MAX_PLAYER_RAW + 1));
        assert_eq!(decode(&big), None);
        // An oversized buffer encoded directly is cut, never oversized.
        let cut = encode(&Packet::PlayerRaw { seq: 1, ptype: 1, data: vec![1; 4000] });
        assert_eq!(cut.len(), PLAYER_RAW_HEADER + MAX_PLAYER_RAW);
    }

    pub(crate) fn netai(id: i32, mv: bool, bh: bool, ac: bool) -> NetAiEntry {
        NetAiEntry {
            id,
            hp: 100 * id,
            beh_seq: id as u8,
            act_seq: !(id as u8),
            movement: mv.then(|| (0..NETAI_MOVE_LEN as u8).collect()),
            behavior: bh.then(|| (0..NETAI_BEHAVIOR_LEN).map(|i| i as u8).collect()),
            action: ac.then(|| vec![0xAB; NETAI_ACTION_LEN]),
        }
    }

    #[test]
    fn netai_wire_len_matches_encoding_and_bad_fields_are_dropped() {
        for e in [netai(1, true, true, true), netai(2, false, false, false), netai(3, true, false, true)] {
            let bytes = encode(&Packet::NetAi(vec![e.clone()]));
            assert_eq!(bytes.len(), NETAI_PACKET_HEADER + e.wire_len());
        }
        let mut bad = netai(4, true, true, true);
        bad.behavior = Some(vec![1, 2, 3]);
        let Some(Packet::NetAi(got)) = decode(&encode(&Packet::NetAi(vec![bad.clone()]))) else { panic!() };
        assert_eq!(got[0].behavior, None);
        assert_eq!(got[0].movement, bad.movement);
        assert_eq!(got[0].action, bad.action);
    }

    #[test]
    fn netai_and_bulk_reject_truncation_and_trailing_bytes() {
        for p in [
            Packet::NetAi(vec![netai(1, true, true, true), netai(2, true, false, true)]),
            Packet::Bulk { xfer: 1, kind: 2, idx: 1, count: 3, data: vec![5; 40] },
            Packet::BulkAck { xfer: 1, idx: 2 },
        ] {
            let full = encode(&p);
            for cut in 0..full.len() {
                assert_eq!(decode(&full[..cut]), None, "{p:?} cut at {cut}");
            }
            let mut extra = full.clone();
            extra.push(0);
            assert_eq!(decode(&extra), None);
        }
    }

    #[test]
    fn bulk_chunk_index_out_of_range_is_rejected() {
        let bytes = encode(&Packet::Bulk { xfer: 1, kind: 1, idx: 3, count: 3, data: vec![] });
        assert_eq!(decode(&bytes), None);
        let bytes = encode(&Packet::Bulk { xfer: 1, kind: 1, idx: 0, count: (MAX_BULK_CHUNKS + 1) as u16, data: vec![] });
        assert_eq!(decode(&bytes), None);
    }

    #[test]
    fn biggest_netai_entry_and_bulk_chunk_fit_one_datagram() {
        assert!(NETAI_PACKET_HEADER + netai(1, true, true, true).wire_len() <= MAX_DATAGRAM);
        let chunk = encode(&Packet::Bulk { xfer: 1, kind: 1, idx: 0, count: 1, data: vec![0; BULK_CHUNK] });
        assert!(chunk.len() <= MAX_DATAGRAM, "{}", chunk.len());
    }

    #[test]
    fn flag_dump_roundtrip_and_rejects_garbage() {
        let recs = vec![(11_100_000u32, vec![0x80; 128]), (9000, vec![1, 2, 3]), (0, vec![])];
        let bytes = encode_flag_dump(FLAG_DUMP_OK, &recs);
        assert_eq!(decode_flag_dump(&bytes), Some((FLAG_DUMP_OK, recs)));
        assert_eq!(decode_flag_dump(&encode_flag_dump(FLAG_DUMP_DISABLED, &[])), Some((FLAG_DUMP_DISABLED, vec![])));
        for cut in 0..bytes.len() {
            assert_eq!(decode_flag_dump(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode_flag_dump(&extra), None);
        assert_eq!(decode_flag_dump(b"XXXX\x00\x00\x00"), None);
    }

    fn enemy(i: i32) -> EnemyState {
        EnemyState { id: i, pos: [i as f32, -1.5, 2.25], angle: 0.5 * i as f32, hp: 100 + i, anim: 3000 + i }
    }

    #[test]
    fn roundtrip_every_packet() {
        for p in all_packets() {
            assert_eq!(decode(&encode(&p)), Some(p.clone()), "{p:?}");
        }
    }

    #[test]
    fn rejects_garbage_wrong_version_and_truncation() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"XXXX\x01\x00\x05"), None);
        let mut wrong = encode(&Packet::Heartbeat);
        wrong[4] = 99;
        assert_eq!(decode(&wrong), None);
        let full = encode(&all_packets()[4]);
        for cut in 0..full.len() {
            assert_eq!(decode(&full[..cut]), None, "cut at {cut}");
        }
        let mut extra = full.clone();
        extra.push(0);
        assert_eq!(decode(&extra), None);
    }

    #[test]
    fn long_and_multibyte_names_are_cut_safely() {
        let name = "ä".repeat(40); // 80 bytes, 2 per char
        let Packet::Welcome { name: got, .. } = decode(&encode(&Packet::Welcome { name, slot: 1 })).unwrap() else {
            panic!()
        };
        assert!(got.len() <= MAX_NAME && got.chars().all(|c| c == 'ä'));
    }

    #[test]
    fn password_hash_is_stable_and_distinct() {
        assert_eq!(password_hash("susi"), password_hash("susi"));
        assert_ne!(password_hash("susi"), password_hash("Susi"));
    }

    #[test]
    fn enemies_over_limit_are_truncated_on_encode() {
        let bytes = encode(&Packet::Enemies((0..60).map(enemy).collect()));
        assert_eq!(bytes.len(), 4 + 2 + 1 + ROUTE_LEN + 1 + 48 * 28);
        assert!(bytes.len() < 1400);
        assert_eq!(decode(&bytes), Some(Packet::Enemies((0..48).map(enemy).collect())));
    }

    #[test]
    fn enemies_count_over_limit_is_rejected() {
        let mut bytes = encode(&Packet::Enemies(vec![]));
        *bytes.last_mut().unwrap() = 49;
        bytes.extend(std::iter::repeat_n(0u8, 49 * 28));
        assert_eq!(decode(&bytes), None);
    }

    fn posture(i: i32) -> PostureState {
        PostureState { id: 1_000_000 + i, posture: 10 * i - 5, max: 900 + i, lives: (i % 4) as i8 - 1 }
    }

    #[test]
    fn damage_and_posture_layout_and_limits() {
        // magic, version, kind, route + id, hp, posture, deathblows.
        let d = encode(&Packet::Damage(DamageReport { id: 1, hp: 2, posture: 3, deathblows: 4 }));
        assert_eq!(d.len(), 4 + 2 + 1 + ROUTE_LEN + 14);
        let full = encode(&Packet::Posture((0..64).map(posture).collect()));
        assert_eq!(full.len(), 4 + 2 + 1 + ROUTE_LEN + 1 + 64 * 13);
        assert!(full.len() <= MAX_DATAGRAM);
        // Over the limit: cut on encode, refused on decode.
        let cut = encode(&Packet::Posture((0..80).map(posture).collect()));
        assert_eq!(decode(&cut), Some(Packet::Posture((0..64).map(posture).collect())));
        let mut big = encode(&Packet::Posture(vec![]));
        *big.last_mut().unwrap() = 65;
        big.extend(std::iter::repeat_n(0u8, 65 * 13));
        assert_eq!(decode(&big), None);
        // A negative lives byte survives the trip (i8 on the wire).
        let neg = PostureState { id: 3, posture: -20, max: 100, lives: -1 };
        assert_eq!(decode(&encode(&Packet::Posture(vec![neg]))), Some(Packet::Posture(vec![neg])));
    }

    #[test]
    fn routed_kinds_carry_src_and_dst_and_link_local_ones_do_not() {
        for p in all_packets() {
            let bytes = encode_routed(&p, 4, 2);
            let (got, route) = decode_routed(&bytes).unwrap();
            assert_eq!(got, p);
            assert_eq!(route, p.is_routed().then_some((4, 2)), "{p:?}");
            if p.is_routed() {
                assert_eq!(&bytes[7..9], &[4, 2], "route right after the kind byte: {p:?}");
            } else {
                assert_eq!(bytes, encode(&p), "no route for link-local kinds: {p:?}");
            }
        }
        assert_eq!(decode_routed(&encode(&Packet::Snapshot(Snapshot::default()))).unwrap().1, Some((0, DST_ALL)));
        // Every v9 kind that is routed now and none that is not.
        let routed: Vec<bool> = all_packets().iter().map(Packet::is_routed).collect();
        assert!(routed.iter().any(|&r| r) && routed.iter().any(|&r| !r));
    }

    #[test]
    fn v9_datagrams_are_refused_and_answered_in_their_own_format() {
        // A v9 Hello: magic, version 9, kind 1, hash, name.
        let mut v9 = Vec::new();
        v9.extend_from_slice(&MAGIC);
        v9.extend_from_slice(&9u16.to_le_bytes());
        v9.push(K_HELLO);
        v9.extend_from_slice(&password_hash("susi").to_le_bytes());
        v9.push(0);
        assert_eq!(decode(&v9), None);
        assert_eq!(peek_header(&v9), Some((9, K_HELLO)));
        assert_eq!(peek_header(b"XXXX\x09\x00\x01"), None);
        // v9's Reject has no body: exactly magic, 9, kind 3.
        assert_eq!(version_reject(9), [b'S', b'K', b'C', b'O', 9, 0, K_REJECT]);
        // The released 2-player v10 (coop-v7.1) has no body either.
        assert_eq!(version_reject(10), [b'S', b'K', b'C', b'O', 10, 0, K_REJECT]);
        // Our own version decodes it as Reject { Version }.
        assert_eq!(decode(&version_reject(VERSION)), Some(Packet::Reject { reason: 3 }));
    }

    #[test]
    fn roster_roundtrip_and_rejects_garbage() {
        let e = |slot: u8, uid: u64, inc: u8, name: &str| RosterEntry { slot, uid, inc, name: name.into() };
        let list = vec![e(0, 7, 0, "host"), e(1, 9, 255, "Tihvert"), e(5, u64::MAX, 3, &"ä".repeat(20))];
        let bytes = encode_roster(42, &list);
        let (g, got) = decode_roster(&bytes).unwrap();
        assert_eq!(g, 42);
        assert_eq!(got[..2], list[..2]);
        assert_eq!((got[2].slot, got[2].uid, got[2].inc), (5, u64::MAX, 3));
        assert!(got[2].name.chars().all(|c| c == 'ä') && got[2].name.len() <= MAX_NAME);
        for cut in 0..bytes.len() {
            assert_eq!(decode_roster(&bytes[..cut]), None, "cut at {cut}");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(decode_roster(&extra), None);
        // A duplicate slot or one past MAX_PEERS is refused.
        assert_eq!(decode_roster(&encode_roster(1, &[e(1, 1, 0, "a"), e(1, 2, 0, "b")])), None);
        assert_eq!(decode_roster(&encode_roster(1, &[e(6, 1, 0, "a")])), None);
        // The roster always fits one bulk chunk.
        let full: Vec<_> = (0..=MAX_PEERS as u8).map(|i| e(i, u64::MAX, 9, &"x".repeat(64))).collect();
        assert!(encode_roster(u32::MAX, &full).len() <= BULK_CHUNK);
    }

    /// VERSION 13: the hidden byte carries two bits; each combination survives the trip.
    #[test]
    fn snapshot_hidden_byte_is_bits() {
        for (hidden, cutscene) in [(false, false), (true, false), (false, true), (true, true)] {
            let s = Snapshot { seq: 1, hp: 5, hidden, cutscene, ..Default::default() };
            let bytes = encode(&Packet::Snapshot(s));
            assert_eq!(decode(&bytes), Some(Packet::Snapshot(s)), "hidden {hidden} cutscene {cutscene}");
        }
        // Wire position: the byte after block is bit 0 hidden, bit 1 cutscene.
        let s = Snapshot { hidden: true, cutscene: true, ..Default::default() };
        let bytes = encode(&Packet::Snapshot(s));
        // magic 4 + version 2 + kind 1 + route 2 + seq 4 + pos 12 + angle 4 + anim 4 + hp 4 + max_hp 4 + block 4
        assert_eq!(bytes[4 + 2 + 1 + 2 + 4 + 12 + 4 + 4 + 4 + 4 + 4], 0b11);
        assert_eq!(VERSION, 14);
    }

    #[test]
    fn version_8_damage_is_rejected() {
        // The old 8-byte Damage body under the old version: never misread.
        let mut old = vec![];
        old.extend_from_slice(&MAGIC);
        old.extend_from_slice(&8u16.to_le_bytes());
        old.push(K_DAMAGE);
        old.extend_from_slice(&[0, DST_ALL]);
        old.extend_from_slice(&5i32.to_le_bytes());
        old.extend_from_slice(&9i32.to_le_bytes());
        assert_eq!(decode(&old), None);
        // Same body with the new version: too short, refused too.
        old[4..6].copy_from_slice(&VERSION.to_le_bytes());
        assert_eq!(decode(&old), None);
    }

    #[test]
    fn enemies_and_damage_reject_truncation_and_trailing_bytes() {
        for p in [
            Packet::Enemies((0..3).map(enemy).collect()),
            Packet::Damage(DamageReport { id: 5, hp: 9, posture: 30, deathblows: 1 }),
            Packet::Posture((0..3).map(posture).collect()),
        ] {
            let full = encode(&p);
            for cut in 0..full.len() {
                assert_eq!(decode(&full[..cut]), None, "cut at {cut}");
            }
            let mut extra = full.clone();
            extra.push(0);
            assert_eq!(decode(&extra), None);
        }
    }

    #[test]
    fn ping_pong_event_ack_reject_truncation_and_trailing_bytes() {
        for p in [
            Packet::Ping { t: 99 },
            Packet::Pong { t: 99 },
            Packet::Event { seq: 1, kind: 2, a: 3, b: 4 },
            Packet::Ack { seq: 5 },
        ] {
            let full = encode(&p);
            assert_eq!(decode(&full), Some(p.clone()));
            for cut in 0..full.len() {
                assert_eq!(decode(&full[..cut]), None, "{p:?} cut at {cut}");
            }
            let mut extra = full.clone();
            extra.push(0);
            assert_eq!(decode(&extra), None, "{p:?}");
        }
    }

    #[test]
    fn old_version_bytes_are_rejected() {
        let mut old = encode(&Packet::Heartbeat);
        old[4..6].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(decode(&old), None);
    }
}
