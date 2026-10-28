//! Platform-independent co-op networking (host-testable: `cargo test -p coop-net`).

pub mod protocol;
pub mod session;
pub mod transport;
pub mod upnp;

pub use protocol::{
    DST_ALL, DamageReport, EnemyState, FLAG_DUMP_DISABLED, FLAG_DUMP_OK, FLAG_DUMP_UNAVAILABLE, HkbEvent,
    MAX_ENEMIES_PER_PACKET, MAX_HKB_PER_PACKET, MAX_PEERS, MAX_PLAYER_RAW, MAX_POSTURE_PER_PACKET, MAX_SYNC_STATES,
    NETAI_ACTION_LEN, NETAI_BEHAVIOR_LEN, NETAI_MOVE_LEN, NetAiEntry, Packet, PostureState, Snapshot, decode_flag_dump,
    encode_flag_dump,
};
pub use transport::{Datagram, Inert, steam_addr, steam_id_of};
pub use session::{
    EVENT_RESEND_EVERY, Event, LeaveWhy, MAX_PENDING_EVENTS, PING_EVERY, RejectReason, Role, Session, State,
};
