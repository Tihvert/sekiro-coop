//! Steam lobby and transport for the co-op session (settings: transport = steam | auto).
//!
//! The game already runs Steam, so we borrow its connection instead of linking Steamworks:
//! `steamclient64.dll` (already loaded) is asked for its interfaces with `CreateInterface`
//! and `ISteamClient::GetISteamGenericInterface`, using the user and pipe handles of the
//! game's own `steam_api64.dll` (`SteamAPI_GetHSteamUser` / `SteamAPI_GetHSteamPipe`). No
//! callback is registered anywhere: asynchronous results (lobby list, create, join) are
//! polled with `ISteamUtils::IsAPICallCompleted` / `GetAPICallResult`.
//!
//! Vtable orders (slot = position among the `virtual` methods, no destructor) are taken from
//! the public Steamworks SDK headers as mirrored in rlabrecque/Steamworks.NET `CodeGen/steam/`
//! (checked 2026-10-23): `isteamclient.h` (SteamClient023; slots up to GetISteamGenericInterface
//! are the same since SteamClient017), `isteammatchmaking.h` (SteamMatchMaking009),
//! `isteamutils.h` (SteamUtils010 still has the private `GetCSERIPPort` slot, SteamUtils011 does not),
//! `isteamuser.h`, `isteamfriends.h`, `isteamnetworkingmessages.h` (SteamNetworkingMessages002) and
//! `steamnetworkingtypes.h` (SteamNetworkingIdentity 136 bytes, SteamNetworkingMessage_t layout).
//! A wrong slot would call the wrong Steam function, so every step logs what it got
//! (`steam: ...` lines); the first run shows exactly which interface or call worked.
//!
//! Lobby: data key `sekiro_coop` = a salted hash of the password (never the password), `proto` =
//! the coop-net protocol version, size = `max_players` (the host included; a lobby we host
//! follows a changed value through SetLobbyMemberLimit). The host accepts
//! every lobby member's message session; the session on top is the normal one, with the Steam
//! users shown as made-up addresses (`coop_net::steam_addr`), and it refuses a client beyond
//! `max_players` with Reject(Full). A joiner that finds the lobby full says so (`take_full`).
//! SteamIDs are shortened in the log.

use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coop_net::protocol::VERSION;
use coop_net::{Datagram, steam_addr, steam_id_of};

use crate::log;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

// Vtable slots.
const CLIENT_GET_GENERIC_INTERFACE: usize = 12;
const USER_GET_STEAM_ID: usize = 2;
const FRIENDS_GET_PERSONA_NAME: usize = 0;
const MM_REQUEST_LOBBY_LIST: usize = 4;
const MM_ADD_STRING_FILTER: usize = 5;
const MM_ADD_SLOTS_FILTER: usize = 8;
const MM_ADD_DISTANCE_FILTER: usize = 9;
const MM_ADD_COUNT_FILTER: usize = 10;
const MM_GET_LOBBY_BY_INDEX: usize = 12;
const MM_CREATE_LOBBY: usize = 13;
const MM_JOIN_LOBBY: usize = 14;
const MM_LEAVE_LOBBY: usize = 15;
const MM_GET_NUM_MEMBERS: usize = 17;
const MM_GET_MEMBER_BY_INDEX: usize = 18;
const MM_GET_LOBBY_DATA: usize = 19;
const MM_SET_LOBBY_DATA: usize = 20;
/// `SetLobbyMemberLimit(CSteamID, int) -> bool` (ISteamMatchmaking 009: between
/// GetLobbyGameServer 30 and GetLobbyMemberLimit 32; 34 / 35 right after are in use). The
/// slot is from the public header, not verified live.
const MM_SET_MEMBER_LIMIT: usize = 31;
const MM_SET_JOINABLE: usize = 34;
const MM_GET_LOBBY_OWNER: usize = 35;
const MSG_SEND: usize = 0;
const MSG_RECEIVE: usize = 1;
const MSG_ACCEPT: usize = 2;

// Enums and callback ids.
const LOBBY_TYPE_PUBLIC: i32 = 2;
const LOBBY_TYPE_INVISIBLE: i32 = 3;
const COMPARISON_EQUAL: i32 = 0;
const DISTANCE_WORLDWIDE: i32 = 3;
const CB_LOBBY_ENTER: i32 = 504;
const CB_LOBBY_MATCH_LIST: i32 = 510;
const CB_LOBBY_CREATED: i32 = 513;
const RESULT_OK: i32 = 1;
/// k_EResultAccessDenied: Steam refuses a lobby when the account does not own the game (a
/// LuaTools/SteamTools install) or is offline. Seen on every lobby creation in our own tests.
const RESULT_ACCESS_DENIED: i32 = 15;
const ENTER_SUCCESS: u32 = 1;
/// EChatRoomEnterResponse: the lobby has no free slot.
const ENTER_FULL: u32 = 4;
const IDENTITY_TYPE_STEAM_ID: i32 = 16;
/// UnreliableNoNagle: the session does its own resending.
const SEND_UNRELIABLE_NO_NAGLE: i32 = 1;

const LOBBY_KEY: &str = "sekiro_coop";
const LOBBY_PROTO: &str = "proto";

fn short(id: u64) -> String {
    format!("..{:04}", id % 10000)
}

/// Salted hash of the password for the lobby data (FNV-1a 64). The password itself never
/// goes to Steam; the salt only keeps the value apart from the in-session password hash.
pub fn lobby_key(password: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in b"sekiro-coop-lobby\0".iter().chain(password.as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("v1:{h:016x}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn module(name: &str) -> *mut c_void {
    unsafe { GetModuleHandleW(wide(name).as_ptr()) }
}

fn export(module: *mut c_void, name: &str) -> usize {
    let c = CString::new(name).unwrap();
    unsafe { GetProcAddress(module, c.as_ptr() as *const u8) as usize }
}

/// Function pointer in slot `idx` of the object's vtable.
fn slot(obj: *mut c_void, idx: usize) -> usize {
    unsafe { *(*(obj as *const *const usize)).add(idx) }
}

/// Calls virtual method `idx` of a COM-style object: `vcall!(obj, slot, (ArgTypes) -> Ret, args)`.
macro_rules! vcall {
    ($obj:expr, $idx:expr, ($($t:ty),*) -> $r:ty $(, $a:expr)*) => {{
        let obj: *mut c_void = $obj;
        let f: extern "system" fn(*mut c_void $(, $t)*) -> $r = unsafe { std::mem::transmute(slot(obj, $idx)) };
        f(obj $(, $a)*)
    }};
}

/// Like `vcall!` for a method that returns a `CSteamID`. MSVC returns a class from a C++ member
/// function through a hidden pointer (rdx, right after `this`), whatever its size; calling it as
/// `-> u64` let Steam write the id to whatever rdx held (live 2026-10-23: write to 0 in
/// steamclient64 at `GetSteamID`, the game crashed at start).
macro_rules! vcall_id {
    ($obj:expr, $idx:expr $(, $t:ty = $a:expr)*) => {{
        let obj: *mut c_void = $obj;
        let f: extern "system" fn(*mut c_void, *mut u64 $(, $t)*) -> *mut u64 = unsafe { std::mem::transmute(slot(obj, $idx)) };
        let mut id = 0u64;
        f(obj, &mut id $(, $a)*);
        id
    }};
}

/// ISteamUtils slots of the two layouts: (IsAPICallCompleted, GetAPICallFailureReason, GetAPICallResult).
const UTILS_LAYOUTS: [(&str, [usize; 3]); 2] = [("SteamUtils010", [11, 12, 13]), ("SteamUtils011", [10, 11, 12])];

/// The Steam interfaces we use. Calls happen on the game thread only.
pub struct Api {
    mm: *mut c_void,
    utils: *mut c_void,
    msgs: *mut c_void,
    friends: *mut c_void,
    utils_slots: [usize; 3],
    /// Our own SteamID64.
    pub me: u64,
}

// SAFETY: the interface objects live as long as Steam does, and we only call them from the
// game's frame task.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

pub enum CallState {
    Pending,
    Done,
    Failed(String),
}

impl Api {
    /// Looks up every interface, logging each step. `Err` says what was missing.
    pub fn load() -> Result<Arc<Api>, String> {
        static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
        let first = ATTEMPTS.fetch_add(1, Relaxed) == 0;
        // Every step on the first attempt (logged at once, so a crash inside Steam shows the step)
        // and on success; a failing retry (every 2 s while the game logs in to Steam) stays
        // quiet, net.rs logs when the reason changes.
        let mut trace = Trace { lines: Vec::new(), live: first };
        let result = Self::load_steps(&mut trace);
        if !first && result.is_ok() {
            for line in &trace.lines {
                log::line(line);
            }
        }
        result
    }

    fn load_steps(trace: &mut Trace) -> Result<Arc<Api>, String> {
        let client_dll = module("steamclient64.dll");
        if client_dll.is_null() {
            return Err("steamclient64.dll is not loaded in the game".into());
        }
        trace.push(format!("steam: steamclient64.dll at {client_dll:p}"));
        let create = export(client_dll, "CreateInterface");
        if create == 0 {
            return Err("steamclient64.dll has no CreateInterface".into());
        }
        let create: extern "C" fn(*const c_char, *mut i32) -> *mut c_void = unsafe { std::mem::transmute(create) };
        let mut client = std::ptr::null_mut();
        for v in ["SteamClient023", "SteamClient022", "SteamClient021", "SteamClient020", "SteamClient019", "SteamClient018", "SteamClient017"] {
            let c = CString::new(v).unwrap();
            let mut code = 0i32;
            let p = create(c.as_ptr(), &mut code);
            trace.push(format!("steam: CreateInterface({v}) -> {p:p}"));
            if !p.is_null() {
                client = p;
                break;
            }
        }
        if client.is_null() {
            return Err("steamclient64.dll gave no ISteamClient (SteamClient017-023)".into());
        }

        let api_dll = module("steam_api64.dll");
        if api_dll.is_null() {
            return Err("steam_api64.dll is not loaded in the game".into());
        }
        let (get_user, get_pipe) = (export(api_dll, "SteamAPI_GetHSteamUser"), export(api_dll, "SteamAPI_GetHSteamPipe"));
        if get_user == 0 || get_pipe == 0 {
            return Err(format!("steam_api64.dll exports: GetHSteamUser {get_user:#x}, GetHSteamPipe {get_pipe:#x}"));
        }
        let user: i32 = unsafe { std::mem::transmute::<usize, extern "C" fn() -> i32>(get_user)() };
        let pipe: i32 = unsafe { std::mem::transmute::<usize, extern "C" fn() -> i32>(get_pipe)() };
        trace.push(format!("steam: game's HSteamUser {user}, HSteamPipe {pipe}"));
        if user == 0 || pipe == 0 {
            return Err("the game's Steam user/pipe handle is 0 (Steam not initialised yet)".into());
        }

        let mut generic = |versions: &[&str]| -> Option<(*mut c_void, String)> {
            for v in versions {
                let c = CString::new(*v).unwrap();
                let p = vcall!(client, CLIENT_GET_GENERIC_INTERFACE, (i32, i32, *const c_char) -> *mut c_void, user, pipe, c.as_ptr());
                trace.push(format!("steam: GetISteamGenericInterface({v}) -> {p:p}"));
                if !p.is_null() {
                    return Some((p, v.to_string()));
                }
            }
            None
        };
        let (mm, _) = generic(&["SteamMatchMaking009"]).ok_or("no ISteamMatchmaking (SteamMatchMaking009)")?;
        let (msgs, _) = generic(&["SteamNetworkingMessages002"]).ok_or("no ISteamNetworkingMessages (SteamNetworkingMessages002)")?;
        let (user_if, _) = generic(&["SteamUser023", "SteamUser022", "SteamUser021", "SteamUser020"]).ok_or("no ISteamUser (SteamUser020-023)")?;
        let (friends, _) = generic(&["SteamFriends018", "SteamFriends017"]).unwrap_or((std::ptr::null_mut(), String::new()));
        let mut utils = None;
        for (v, slots) in UTILS_LAYOUTS {
            if let Some((p, _)) = generic(&[v]) {
                utils = Some((p, slots));
                break;
            }
        }
        let (utils, utils_slots) = utils.ok_or("no ISteamUtils (SteamUtils010/011)")?;
        let me = vcall_id!(user_if, USER_GET_STEAM_ID);
        if me == 0 {
            return Err("ISteamUser::GetSteamID returned 0".into());
        }
        let api = Api { mm, utils, msgs, friends, utils_slots, me };
        trace.push(format!("steam: interfaces ready, own SteamID {}, persona name {:?}", short(me), api.persona_name()));
        Ok(Arc::new(api))
    }

    /// The Steam display name, if ISteamFriends was found.
    pub fn persona_name(&self) -> Option<String> {
        if self.friends.is_null() {
            return None;
        }
        let p = vcall!(self.friends, FRIENDS_GET_PERSONA_NAME, () -> *const c_char);
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().trim().to_owned()).filter(|n| !n.is_empty())
    }

    /// Progress of an asynchronous call; on `Done` the result is in `buf`.
    fn poll_call(&self, call: u64, expected: i32, buf: &mut [u8]) -> CallState {
        if call == 0 {
            return CallState::Failed("Steam returned no call handle".into());
        }
        let [completed, reason, result] = self.utils_slots;
        let mut failed = 0u8;
        if vcall!(self.utils, completed, (u64, *mut u8) -> u8, call, &mut failed) == 0 {
            return CallState::Pending;
        }
        if failed != 0 {
            let r = vcall!(self.utils, reason, (u64) -> i32, call);
            return CallState::Failed(format!("call failed (ESteamAPICallFailure {r})"));
        }
        let mut failed = 0u8;
        let ok = vcall!(self.utils, result, (u64, *mut c_void, i32, i32, *mut u8) -> u8, call, buf.as_mut_ptr() as *mut c_void, buf.len() as i32, expected, &mut failed);
        if ok != 0 && failed == 0 {
            CallState::Done
        } else {
            let r = vcall!(self.utils, reason, (u64) -> i32, call);
            CallState::Failed(format!("could not read the call result (ok {ok}, failed {failed}, reason {r})"))
        }
    }

    /// `open_only`: only lobbies with a free slot (else full ones too, to tell the joiner).
    fn request_lobby_list(&self, value: &str, open_only: bool) -> u64 {
        let key = CString::new(LOBBY_KEY).unwrap();
        let value = CString::new(value).unwrap();
        vcall!(self.mm, MM_ADD_STRING_FILTER, (*const c_char, *const c_char, i32) -> (), key.as_ptr(), value.as_ptr(), COMPARISON_EQUAL);
        if open_only {
            vcall!(self.mm, MM_ADD_SLOTS_FILTER, (i32) -> (), 1);
        }
        vcall!(self.mm, MM_ADD_DISTANCE_FILTER, (i32) -> (), DISTANCE_WORLDWIDE);
        vcall!(self.mm, MM_ADD_COUNT_FILTER, (i32) -> (), 20);
        vcall!(self.mm, MM_REQUEST_LOBBY_LIST, () -> u64)
    }

    fn lobby_by_index(&self, i: i32) -> u64 {
        vcall_id!(self.mm, MM_GET_LOBBY_BY_INDEX, i32 = i)
    }

    fn create_lobby(&self, lobby_type: i32, max_members: i32) -> u64 {
        vcall!(self.mm, MM_CREATE_LOBBY, (i32, i32) -> u64, lobby_type, max_members)
    }

    fn join_lobby(&self, lobby: u64) -> u64 {
        vcall!(self.mm, MM_JOIN_LOBBY, (u64) -> u64, lobby)
    }

    fn leave_lobby(&self, lobby: u64) {
        vcall!(self.mm, MM_LEAVE_LOBBY, (u64) -> (), lobby)
    }

    fn lobby_owner(&self, lobby: u64) -> u64 {
        vcall_id!(self.mm, MM_GET_LOBBY_OWNER, u64 = lobby)
    }

    fn set_joinable(&self, lobby: u64, joinable: bool) {
        vcall!(self.mm, MM_SET_JOINABLE, (u64, u8) -> u8, lobby, joinable as u8);
    }

    /// Lobby owner: the lobby takes this many members (the host included).
    fn set_member_limit(&self, lobby: u64, members: i32) -> bool {
        vcall!(self.mm, MM_SET_MEMBER_LIMIT, (u64, i32) -> u8, lobby, members) != 0
    }

    fn lobby_data(&self, lobby: u64, key: &str) -> String {
        let k = CString::new(key).unwrap();
        let p = vcall!(self.mm, MM_GET_LOBBY_DATA, (u64, *const c_char) -> *const c_char, lobby, k.as_ptr());
        if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    }

    fn set_lobby_data(&self, lobby: u64, key: &str, value: &str) -> bool {
        let (k, v) = (CString::new(key).unwrap(), CString::new(value).unwrap());
        vcall!(self.mm, MM_SET_LOBBY_DATA, (u64, *const c_char, *const c_char) -> u8, lobby, k.as_ptr(), v.as_ptr()) != 0
    }

    fn members(&self, lobby: u64) -> Vec<u64> {
        let n = vcall!(self.mm, MM_GET_NUM_MEMBERS, (u64) -> i32, lobby).clamp(0, 16);
        (0..n).map(|i| vcall_id!(self.mm, MM_GET_MEMBER_BY_INDEX, u64 = lobby, i32 = i)).filter(|&m| m != 0).collect()
    }

    /// `SteamNetworkingIdentity` for a SteamID64: type 16, size 8, the id (136 bytes in all).
    fn identity(id: u64) -> [u8; 136] {
        let mut b = [0u8; 136];
        b[0..4].copy_from_slice(&IDENTITY_TYPE_STEAM_ID.to_le_bytes());
        b[4..8].copy_from_slice(&8i32.to_le_bytes());
        b[8..16].copy_from_slice(&id.to_le_bytes());
        b
    }

    /// EResult of ISteamNetworkingMessages::SendMessageToUser (1 = OK).
    fn send(&self, to: u64, data: &[u8]) -> i32 {
        let ident = Self::identity(to);
        vcall!(self.msgs, MSG_SEND, (*const c_void, *const c_void, u32, i32, i32) -> i32, ident.as_ptr() as *const c_void, data.as_ptr() as *const c_void, data.len() as u32, SEND_UNRELIABLE_NO_NAGLE, 0)
    }

    fn accept(&self, id: u64) -> bool {
        let ident = Self::identity(id);
        vcall!(self.msgs, MSG_ACCEPT, (*const c_void) -> u8, ident.as_ptr() as *const c_void) != 0
    }

    /// Takes the messages waiting on channel 0 (data and sender), releasing each.
    fn receive(&self, out: &mut VecDeque<(Vec<u8>, u64)>) {
        let mut ptrs = [std::ptr::null_mut::<u8>(); 16];
        let n = vcall!(self.msgs, MSG_RECEIVE, (i32, *mut *mut u8, i32) -> i32, 0, ptrs.as_mut_ptr(), ptrs.len() as i32);
        for &m in ptrs.iter().take(n.clamp(0, 16) as usize) {
            if m.is_null() {
                continue;
            }
            // SteamNetworkingMessage_t: m_pData 0, m_cbSize 8, m_identityPeer 16 (type, size,
            // SteamID64 at +8), m_pfnRelease 184.
            unsafe {
                let data = (m as *const *const u8).read_unaligned();
                let size = (m.add(8) as *const i32).read_unaligned();
                let ident_type = (m.add(16) as *const i32).read_unaligned();
                let id = (m.add(24) as *const u64).read_unaligned();
                if !data.is_null() && size > 0 && ident_type == IDENTITY_TYPE_STEAM_ID && out.len() < 256 {
                    out.push_back((std::slice::from_raw_parts(data, size as usize).to_vec(), id));
                }
                let release = (m.add(184) as *const usize).read_unaligned();
                if release != 0 {
                    std::mem::transmute::<usize, extern "C" fn(*mut u8)>(release)(m);
                }
            }
        }
    }
}

/// `coop_net::Datagram` over ISteamNetworkingMessages (channel 0); peers are `steam_addr`s.
pub struct SteamTransport {
    api: Arc<Api>,
    inbox: Mutex<VecDeque<(Vec<u8>, u64)>>,
    send_failures: AtomicU32,
    received: AtomicU32,
}

impl SteamTransport {
    pub fn new(api: Arc<Api>) -> Self {
        Self { api, inbox: Mutex::new(VecDeque::new()), send_failures: AtomicU32::new(0), received: AtomicU32::new(0) }
    }
}

impl Datagram for SteamTransport {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        let id = steam_id_of(&to).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a Steam peer"))?;
        let r = self.api.send(id, buf);
        if r == RESULT_OK {
            return Ok(buf.len());
        }
        // Until the other side accepted the session the first sends fail or wait: not an error to shout about.
        let n = self.send_failures.fetch_add(1, Relaxed) + 1;
        if n <= 3 || n % 500 == 0 {
            log::line(&format!("steam: send to {} failed, EResult {r} (failure #{n})", short(id)));
        }
        Err(io::Error::other(format!("EResult {r}")))
    }

    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let mut inbox = self.inbox.lock().map_err(|_| io::Error::other("steam inbox poisoned"))?;
        if inbox.is_empty() {
            self.api.receive(&mut inbox);
        }
        while let Some((data, from)) = inbox.pop_front() {
            if data.len() > buf.len() {
                continue;
            }
            if self.received.fetch_add(1, Relaxed) == 0 {
                log::line(&format!("steam: first message received (from {}, {} bytes)", short(from), data.len()));
            }
            buf[..data.len()].copy_from_slice(&data);
            return Ok((data.len(), steam_addr(from)));
        }
        Err(io::ErrorKind::WouldBlock.into())
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(steam_addr(self.api.me))
    }
}

/// What the player's `role` setting asks of the lobby.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    Auto,
    Host,
    Join,
}

#[derive(Clone, Copy)]
enum Phase {
    /// Nothing running; (re)start at `at`.
    Idle { at: Instant },
    /// `open_only`: the search asked for lobbies with a free slot only.
    Searching { call: u64, open_only: bool },
    Joining { call: u64 },
    Creating { call: u64, lobby_type: i32 },
    Hosting,
    Joined,
}

/// A change the session must follow.
pub enum Out {
    /// We own a lobby and wait for the friend.
    Hosting,
    /// We joined a lobby; the friend (its owner) is this SteamID64.
    JoinedHost(u64),
    /// The lobby we were in is gone: back to waiting.
    HostLeft,
}

const HOUSEKEEPING_EVERY: Duration = Duration::from_secs(1);
const WAITING_MEMBER_EVERY: Duration = Duration::from_millis(100);
/// Sent to a lobby member before its first Hello. Not a valid packet (the session ignores it);
/// it only makes the host open its own message session to the member.
const NUDGE: &[u8] = b"SKCO";
const PROBE_EVERY: Duration = Duration::from_secs(8);
const HOST_GONE_AFTER: Duration = Duration::from_secs(6);
const HOST_SILENT_AFTER: Duration = Duration::from_secs(30);

/// Steam lobby state machine, driven by `tick` once per frame.
pub struct Lobby {
    api: Arc<Api>,
    key: String,
    want: Want,
    /// Lobby size when we create one (`max_players`, the host included).
    max_members: i32,
    phase: Phase,
    lobby: u64,
    /// Joined: the lobby's host.
    host: Option<u64>,
    /// Hosting: the other lobby members whose message sessions we accepted (all of them, in
    /// lobby order; the session refuses one beyond `max_players` with Reject(Full)).
    members: Vec<u64>,
    /// Hosting (auto role): a second search for a lobby that should win over ours.
    probe: Option<u64>,
    probe_at: Instant,
    housekeeping_at: Instant,
    host_missing_since: Option<Instant>,
    last_connected: Instant,
    /// The newest search found a lobby with the right password but another protocol version.
    other_version: bool,
    last_error: Option<String>,
    /// Joiner: the lobby we tried to enter was full (until we enter one).
    full: bool,
    /// Joiner: `full` became true and nobody was told yet (`take_full`).
    full_news: bool,
}

impl Lobby {
    /// `max_members`: lobby size for a lobby we create (`tuning::max_players()`).
    pub fn new(api: Arc<Api>, password: &str, want: Want, max_members: i32) -> Self {
        let now = Instant::now();
        Self {
            api,
            key: lobby_key(password),
            want,
            max_members: max_members.clamp(2, crate::peers::MAX_PLAYERS as i32),
            phase: Phase::Idle { at: now },
            lobby: 0,
            host: None,
            members: Vec::new(),
            probe: None,
            probe_at: now,
            housekeeping_at: now,
            host_missing_since: None,
            last_connected: now,
            other_version: false,
            last_error: None,
            full: false,
            full_news: false,
        }
    }

    /// `max_players` changed in the settings: the lobby we host follows (it was created with
    /// the old size, so a friend beyond it was refused by Steam). Not hosting yet: the value
    /// is used when the lobby is created.
    pub fn set_max_members(&mut self, members: i32) {
        let members = members.clamp(2, crate::peers::MAX_PLAYERS as i32);
        if members == self.max_members {
            return;
        }
        self.max_members = members;
        if matches!(self.phase, Phase::Hosting) && self.lobby != 0 {
            let ok = self.api.set_member_limit(self.lobby, members);
            log::line(&format!("steam: lobby {} size set to {members} players (accepted: {ok})", short(self.lobby)));
        }
    }

    /// Joiner: true once each time the lobby we tried turned out full.
    pub fn take_full(&mut self) -> bool {
        std::mem::take(&mut self.full_news)
    }

    /// The newest search found only lobbies of another co-op version (a 7.5 host, say).
    pub fn other_version(&self) -> bool {
        self.other_version
    }

    /// Hosting or joined: the lobby part is settled.
    pub fn ready(&self) -> bool {
        matches!(self.phase, Phase::Hosting | Phase::Joined)
    }

    pub fn status(&self) -> String {
        match self.phase {
            Phase::Idle { .. } | Phase::Searching { .. } => {
                if self.other_version {
                    "found a lobby with another co-op version (update the mod)".into()
                } else if self.full {
                    "the host's world is full (Steam), trying again".into()
                } else if let Some(e) = &self.last_error {
                    format!("Steam lobby problem: {e}")
                } else if self.want == Want::Join {
                    "searching for the friend's lobby (Steam)".into()
                } else {
                    "searching for a lobby (Steam)".into()
                }
            }
            Phase::Creating { .. } => "opening a lobby (Steam)".into(),
            Phase::Joining { .. } => "joining the friend's lobby (Steam)".into(),
            Phase::Hosting => "hosting (Steam) - waiting for friend".into(),
            Phase::Joined => "joined the lobby (Steam), connecting to host".into(),
        }
    }

    fn search(&mut self, open_only: bool) {
        let call = self.api.request_lobby_list(&self.key, open_only);
        let what = if open_only { "" } else { ", full lobbies included" };
        log::line(&format!("steam: lobby search started (call {call}{what})"));
        self.phase = Phase::Searching { call, open_only };
    }

    fn create(&mut self, lobby_type: i32) {
        let call = self.api.create_lobby(lobby_type, self.max_members);
        log::line(&format!("steam: creating a lobby (type {lobby_type}, {} players, call {call})", self.max_members));
        self.phase = Phase::Creating { call, lobby_type };
    }

    fn idle(&mut self, secs: u64) {
        self.phase = Phase::Idle { at: Instant::now() + Duration::from_secs(secs) };
    }

    /// Leaves the current lobby, if any.
    fn leave(&mut self) {
        if self.lobby != 0 {
            self.api.leave_lobby(self.lobby);
            log::line(&format!("steam: left lobby {}", short(self.lobby)));
        }
        self.lobby = 0;
        self.host = None;
        self.members.clear();
        self.host_missing_since = None;
        self.probe = None;
    }

    /// Best lobby of a finished search: right password and protocol, not ours; the lowest id
    /// (both sides apply the same rule, so two players who opened lobbies at once meet).
    fn pick(&mut self, n: u32) -> Option<u64> {
        let mut best: Option<u64> = None;
        let mut other_version = false;
        for i in 0..n.min(50) {
            let id = self.api.lobby_by_index(i as i32);
            if id == 0 || id == self.lobby || self.api.lobby_data(id, LOBBY_KEY) != self.key {
                continue;
            }
            if self.api.lobby_data(id, LOBBY_PROTO) != VERSION.to_string() {
                other_version = true;
                continue;
            }
            best = Some(best.map_or(id, |b| b.min(id)));
        }
        self.other_version = best.is_none() && other_version;
        best
    }

    /// Call every frame. `connected` = the session has a live link; `linked` = the SteamID64s
    /// it has a live link to (host: the clients; client: the host).
    pub fn tick(&mut self, connected: bool, linked: &[u64]) -> Option<Out> {
        let now = Instant::now();
        if connected {
            self.last_connected = now;
        }
        match self.phase {
            Phase::Idle { at } => {
                if now >= at {
                    if self.want == Want::Host { self.create(LOBBY_TYPE_INVISIBLE) } else { self.search(true) }
                }
                None
            }
            Phase::Searching { call, open_only } => {
                let mut buf = [0u8; 8];
                match self.api.poll_call(call, CB_LOBBY_MATCH_LIST, &mut buf[..4]) {
                    CallState::Pending => None,
                    CallState::Failed(e) => {
                        log::line(&format!("steam: lobby search failed: {e}"));
                        self.last_error = Some(e);
                        self.idle(5);
                        None
                    }
                    CallState::Done => {
                        self.last_error = None;
                        let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        log::line(&format!("steam: lobby search done, {n} lobbies with this password"));
                        match self.pick(n) {
                            Some(id) => {
                                let call = self.api.join_lobby(id);
                                log::line(&format!("steam: joining lobby {} (call {call})", short(id)));
                                self.lobby = id;
                                self.phase = Phase::Joining { call };
                            }
                            None if self.want == Want::Auto => {
                                log::line("steam: no lobby found, opening our own");
                                self.create(LOBBY_TYPE_INVISIBLE);
                            }
                            // Joiner, nothing with a free slot: look once with full lobbies
                            // included, so a full world is reported instead of "not found".
                            None if open_only => self.search(false),
                            None => {
                                log::line("steam: no lobby found yet, searching again in 5 s");
                                self.idle(5);
                            }
                        }
                        None
                    }
                }
            }
            Phase::Joining { call } => {
                let mut buf = [0u8; 32];
                match self.api.poll_call(call, CB_LOBBY_ENTER, &mut buf[..24]) {
                    CallState::Pending => None,
                    CallState::Failed(e) => {
                        log::line(&format!("steam: join failed: {e}"));
                        self.last_error = Some(e);
                        self.lobby = 0;
                        self.idle(3);
                        None
                    }
                    CallState::Done => {
                        let entered = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                        let response = u32::from_le_bytes(buf[16..20].try_into().unwrap());
                        if response == ENTER_SUCCESS {
                            log::line(&format!("steam: entered lobby {}", short(entered)));
                            self.lobby = entered;
                            self.last_error = None;
                            self.full = false;
                            self.last_connected = now;
                            self.phase = Phase::Joined;
                        } else if response == ENTER_FULL {
                            log::line("steam: the lobby is full (EChatRoomEnterResponse 4), trying again in 5 s");
                            self.full_news |= !self.full;
                            self.full = true;
                            self.lobby = 0;
                            self.idle(5);
                        } else {
                            log::line(&format!("steam: could not enter the lobby, EChatRoomEnterResponse {response}"));
                            self.last_error = Some(format!("lobby refused us (response {response})"));
                            self.lobby = 0;
                            self.idle(3);
                        }
                        None
                    }
                }
            }
            Phase::Creating { call, lobby_type } => {
                let mut buf = [0u8; 16];
                match self.api.poll_call(call, CB_LOBBY_CREATED, &mut buf) {
                    CallState::Pending => None,
                    CallState::Failed(e) => self.create_failed(lobby_type, e),
                    CallState::Done => {
                        let result = i32::from_le_bytes(buf[0..4].try_into().unwrap());
                        // u64 at +8 with Windows' 8-byte callback packing; +4 if it was packed to 4.
                        let valid = |id: u64| id >> 56 == 1 && (id >> 52) & 0xF == 8;
                        let at8 = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                        let at4 = u64::from_le_bytes(buf[4..12].try_into().unwrap());
                        let lobby = if valid(at8) { at8 } else if valid(at4) { at4 } else { at8 };
                        if result == RESULT_ACCESS_DENIED {
                            return self.create_failed(
                                lobby_type,
                                format!("LobbyCreated EResult {result} (access denied: needs Sekiro owned on this Steam account and Steam online; use transport = udp)"),
                            );
                        }
                        if result != RESULT_OK || lobby == 0 {
                            return self.create_failed(lobby_type, format!("LobbyCreated EResult {result}"));
                        }
                        self.lobby = lobby;
                        let ok_key = self.api.set_lobby_data(lobby, LOBBY_KEY, &self.key);
                        let ok_proto = self.api.set_lobby_data(lobby, LOBBY_PROTO, &VERSION.to_string());
                        self.api.set_joinable(lobby, true);
                        log::line(&format!(
                            "steam: lobby {} created (type {lobby_type}, data set: key {ok_key}, proto {ok_proto}, protocol {VERSION})",
                            short(lobby)
                        ));
                        self.last_error = None;
                        self.phase = Phase::Hosting;
                        self.probe_at = now + PROBE_EVERY;
                        Some(Out::Hosting)
                    }
                }
            }
            Phase::Hosting => self.tick_hosting(now, connected, linked),
            Phase::Joined => self.tick_joined(now),
        }
    }

    fn create_failed(&mut self, lobby_type: i32, why: String) -> Option<Out> {
        log::line(&format!("steam: lobby creation failed: {why}"));
        if lobby_type == LOBBY_TYPE_INVISIBLE {
            // Invisible lobbies are found by search but hidden from friends lists; a public one
            // is the same for the search, so try that before giving up.
            self.create(LOBBY_TYPE_PUBLIC);
        } else {
            self.last_error = Some(why);
            self.idle(5);
        }
        None
    }

    fn tick_hosting(&mut self, now: Instant, connected: bool, linked: &[u64]) -> Option<Out> {
        // A member we have not heard from yet is polled fast: its first message only gets
        // through once its session is accepted (no request callback is registered, so we
        // accept up front and also send first, which opens our own session to it).
        let waiting = self.members.iter().any(|m| !linked.contains(m));
        let every = if waiting { WAITING_MEMBER_EVERY } else { HOUSEKEEPING_EVERY };
        if now.duration_since(self.housekeeping_at) >= every {
            self.housekeeping_at = now;
            let me = self.api.me;
            let members: Vec<u64> = self.api.members(self.lobby).into_iter().filter(|&m| m != me).collect();
            for &m in &members {
                let ok = self.api.accept(m);
                if !linked.contains(&m) {
                    self.api.send(m, NUDGE);
                }
                if !self.members.contains(&m) {
                    log::line(&format!("steam: lobby member {} appeared, accepted its message session (ok = {ok})", short(m)));
                }
            }
            self.members = members;
        }
        // Auto role, still alone: a friend who opened a lobby at the same moment is a second
        // lobby. The lowest id wins; the other side switches.
        if self.want == Want::Auto && !connected {
            if let Some(call) = self.probe {
                let mut buf = [0u8; 8];
                match self.api.poll_call(call, CB_LOBBY_MATCH_LIST, &mut buf[..4]) {
                    CallState::Pending => {}
                    CallState::Failed(_) => self.probe = None,
                    CallState::Done => {
                        self.probe = None;
                        let n = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        let alone = self.api.members(self.lobby).iter().all(|&m| m == self.api.me);
                        if let Some(other) = self.pick(n).filter(|&o| o < self.lobby && alone) {
                            log::line(&format!(
                                "steam: found lobby {} that wins over ours ({}), switching",
                                short(other),
                                short(self.lobby)
                            ));
                            self.leave();
                            let call = self.api.join_lobby(other);
                            self.lobby = other;
                            self.phase = Phase::Joining { call };
                            return Some(Out::HostLeft);
                        }
                    }
                }
            } else if now >= self.probe_at {
                self.probe_at = now + PROBE_EVERY;
                self.probe = Some(self.api.request_lobby_list(&self.key, true));
            }
        }
        None
    }

    fn tick_joined(&mut self, now: Instant) -> Option<Out> {
        if now.duration_since(self.housekeeping_at) < HOUSEKEEPING_EVERY {
            return None;
        }
        self.housekeeping_at = now;
        let members = self.api.members(self.lobby);
        let Some(host) = self.host else {
            // The lobby owner is the host; owner data of a lobby we just entered can lag a moment.
            let owner = self.api.lobby_owner(self.lobby);
            let host = Some(owner).filter(|&o| o != 0 && o != self.api.me).or_else(|| members.iter().copied().find(|&m| m != self.api.me));
            if let Some(h) = host {
                let ok = self.api.accept(h);
                log::line(&format!("steam: host is {} (accepted its message session: {ok})", short(h)));
                self.host = Some(h);
                return Some(Out::JoinedHost(h));
            }
            return None;
        };
        self.api.accept(host);
        let present = members.contains(&host);
        let missing = if present { None } else { Some(*self.host_missing_since.get_or_insert(now)) };
        let host_gone = missing.is_some_and(|t| now.duration_since(t) >= HOST_GONE_AFTER);
        let host_silent = now.duration_since(self.last_connected) >= HOST_SILENT_AFTER;
        if host_gone || host_silent {
            log::line(&format!(
                "steam: host {} {}, leaving the lobby and searching again",
                short(host),
                if host_gone { "left the lobby" } else { "does not answer" }
            ));
            self.leave();
            self.idle(0);
            return Some(Out::HostLeft);
        }
        None
    }
}

impl Drop for Lobby {
    fn drop(&mut self) {
        if self.lobby != 0 {
            self.api.leave_lobby(self.lobby);
            log::line(&format!("steam: left lobby {}", short(self.lobby)));
        }
    }
}

/// Steps of `Api::load`: logged as they happen (`live`) or kept for later.
struct Trace {
    lines: Vec<String>,
    live: bool,
}

impl Trace {
    fn push(&mut self, line: String) {
        if self.live {
            log::line(&line);
        } else {
            self.lines.push(line);
        }
    }
}
