//! Test builds only (cargo feature `testsync`, never in a public release):
//! makes testing with a friend less manual.
//!
//! - Log: the client streams its `log.txt` (this game session) to the host over
//!   the reliable bulk channel every few seconds; the host writes it to
//!   `friend-log.txt` next to its own log. Nothing is lost when the game
//!   crashes, and nobody has to send files.
//! - Update: the host offers a hash of its own DLL on every connection. A
//!   client running a different build asks for it, receives the file in parts
//!   and stores it as `update\sekiro_coop.dll`; `play.cmd` copies it over the
//!   mod before the next launch. The link needs a matching protocol version, so
//!   a protocol bump still needs a manual zip.
//!
//! The DLL is only ever written to the update folder, never loaded here.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::log;
use crate::net::{Dest, Net};
use crate::peers::PeerId;
use crate::settings::own_dll_path;

const BULK_LOG: u8 = 20;
const BULK_OFFER: u8 = 21;
const BULK_WANT: u8 = 22;
const BULK_DLL: u8 = 23;

const LOG_EVERY: Duration = Duration::from_secs(5);
const LOG_CHUNK: usize = 150 * 1024;
const DLL_PART: usize = 200 * 1024;
const MAX_DLL: usize = 8 << 20;
const MAX_PARTS: u32 = 64;

fn base_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("sekiro-coop")
}

/// FNV-1a 64: a build fingerprint, not a security check (both sides trust
/// each other; this only tells "same file" from "different file").
fn fnv(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[derive(Default)]
pub struct TestSync {
    epoch: u64,
    http_started: bool,
    // Shared.
    own: Option<(u64, Vec<u8>)>,
    notice: Option<String>,
    // Client: log streaming.
    log_off: Option<u64>,
    last_log: Option<Instant>,
    first_chunk: bool,
    // Client: update download.
    want_sent: bool,
    dll_id: u64,
    dll_parts: Vec<Vec<u8>>,
    // Host: per client link (PLAN-MULTIPLAYER P4: a second client's join is not a
    // reconnect of the first), the link epoch our offer went out on, and log bytes per slot.
    offered: std::collections::BTreeMap<PeerId, u64>,
    friend_log_bytes: std::collections::BTreeMap<PeerId, u64>,
}

impl TestSync {
    /// Host, every in-world frame whether or not a friend is connected: the
    /// friend's play.cmd downloads BEFORE he starts the game (live 2026-10-23:
    /// the server only started once he had joined, so play.cmd always missed it).
    pub fn ensure_server(&mut self) {
        if !self.http_started {
            self.http_started = true;
            start_http_server();
        }
    }

    /// Short text for the overlay, when there is something to tell.
    pub fn notice(&self) -> Option<String> {
        self.notice.clone()
    }

    fn own_build(&mut self) -> Option<&(u64, Vec<u8>)> {
        if self.own.is_none() {
            let bytes = fs::read(own_dll_path()?).ok()?;
            self.own = Some((fnv(&bytes), bytes));
        }
        self.own.as_ref()
    }

    pub fn on_frame(&mut self, net: &mut Net) {
        if !net.connected() {
            return;
        }
        // Client: the host link's epoch (the host keeps its state per client link).
        if net.is_client() && net.epoch() != self.epoch {
            self.epoch = net.epoch();
            self.want_sent = false;
            self.dll_parts.clear();
            // v8.45: only the first connection of this game session starts a new friend-log.txt on the host. A
            // reconnect used to start a new one every time and overwrite the previous one (the fight before it
            // was lost); now the stream just continues from the same offset.
            if self.log_off.is_none() {
                self.first_chunk = true;
            }
        }
        if net.is_host() {
            self.host(net);
        } else if net.is_client() {
            self.client(net);
        }
    }

    fn host(&mut self, net: &mut Net) {
        let links: Vec<(PeerId, u64)> = net.peers().map(|p| (p.id(), p.epoch())).collect();
        self.offered.retain(|id, _| links.iter().any(|(l, _)| l == id));
        for (peer, epoch) in links {
            if self.offered.get(&peer) == Some(&epoch) {
                continue;
            }
            self.offered.insert(peer, epoch);
            if let Some(&(id, ref bytes)) = self.own_build() {
                let mut msg = id.to_le_bytes().to_vec();
                msg.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                net.send_bulk_to(Dest::Peer(peer), BULK_OFFER, &msg);
                log::line(&format!("testsync: offered build {id:016x} ({} bytes) to peer {}", bytes.len(), peer.slot()));
            } else {
                log::line("testsync: could not read our own DLL, no update offer");
            }
        }
        while let Some((from, d)) = net.take_bulk_from(BULK_WANT) {
            let Some(id) = d.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { continue };
            let Some((own_id, bytes)) = self.own_build().cloned() else { continue };
            if id != own_id || bytes.len() > MAX_DLL {
                continue;
            }
            let total = bytes.len().div_ceil(DLL_PART) as u32;
            for (i, part) in bytes.chunks(DLL_PART).enumerate() {
                let mut msg = Vec::with_capacity(16 + part.len());
                msg.extend_from_slice(&(i as u32).to_le_bytes());
                msg.extend_from_slice(&total.to_le_bytes());
                msg.extend_from_slice(&own_id.to_le_bytes());
                msg.extend_from_slice(part);
                net.send_bulk_to(Dest::Peer(from), BULK_DLL, &msg);
            }
            log::line(&format!("testsync: peer {} asked for the build, sending {total} parts", from.slot()));
        }
        while let Some((from, d)) = net.take_bulk_from(BULK_LOG) {
            let Some((&first, bytes)) = d.split_first() else { continue };
            // Slot 1 keeps the old file name; further clients get their own.
            let name = if from.slot() <= 1 { "friend-log".to_owned() } else { format!("friend-log-{}", from.slot()) };
            let path = base_dir().join(format!("{name}.txt"));
            if first == 1 {
                // Keep every earlier stream (v8.45): a stamped name instead of one `-prev` file that was overwritten.
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                if fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
                    let _ = fs::rename(&path, base_dir().join(format!("{name}-prev-{stamp}.txt")));
                }
                self.friend_log_bytes.insert(from, 0);
            }
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
                let _ = f.write_all(bytes);
                *self.friend_log_bytes.entry(from).or_insert(0) += bytes.len() as u64;
            }
        }
    }

    fn client(&mut self, net: &mut Net) {
        while let Some((from, d)) = net.take_bulk_from(BULK_OFFER) {
            if !from.is_host() {
                continue;
            }
            let Some(id) = d.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { continue };
            let mine = self.own_build().map(|b| b.0);
            if mine == Some(id) {
                log::line(&format!("testsync: our build matches the host's ({id:016x})"));
                self.notice = None;
            } else {
                // No download while the game runs (user rule 2026-10-23): play.cmd
                // fetches the host's build before the next launch.
                self.notice = Some("Eri versio kuin hostilla: sulje peli ja aja play.cmd (päivittää ennen käynnistystä)".into());
                log::line(&format!("testsync: host has build {id:016x}, ours {mine:016x?}: run play.cmd to update"));
            }
        }
        while let Some((from, d)) = net.take_bulk_from(BULK_DLL) {
            if !from.is_host() {
                continue;
            }
            if d.len() < 16 {
                continue;
            }
            let part = u32::from_le_bytes(d[0..4].try_into().unwrap());
            let total = u32::from_le_bytes(d[4..8].try_into().unwrap());
            let id = u64::from_le_bytes(d[8..16].try_into().unwrap());
            if id != self.dll_id || total == 0 || total > MAX_PARTS || part != self.dll_parts.len() as u32 {
                continue;
            }
            self.dll_parts.push(d[16..].to_vec());
            if self.dll_parts.len() as u32 == total {
                let all: Vec<u8> = self.dll_parts.drain(..).flatten().collect();
                self.finish_download(id, &all);
            }
        }
        self.stream_log(net);
    }

    fn finish_download(&mut self, id: u64, all: &[u8]) {
        if fnv(all) != id || all.len() > MAX_DLL || all.get(..2) != Some(b"MZ") {
            log::line("testsync: downloaded build does not match its fingerprint, dropped");
            self.notice = Some("Päivitys epäonnistui (tiedosto vioittunut)".into());
            self.want_sent = false;
            return;
        }
        let dir = base_dir().join("update");
        let tmp = dir.join("sekiro_coop.dll.part");
        let fin = dir.join("sekiro_coop.dll");
        let ok = fs::create_dir_all(&dir).is_ok() && fs::write(&tmp, all).is_ok() && fs::rename(&tmp, &fin).is_ok();
        log::line(&format!("testsync: build {id:016x} downloaded ({} bytes), stored in {} (ok = {ok})", all.len(), fin.display()));
        self.notice = Some(if ok {
            "Päivitys ladattu: sulje peli ja käynnistä se play.cmd:llä".into()
        } else {
            "Päivityksen tallennus epäonnistui".into()
        });
    }

    fn stream_log(&mut self, net: &mut Net) {
        if self.last_log.is_some_and(|t| t.elapsed() < LOG_EVERY) {
            return;
        }
        self.last_log = Some(Instant::now());
        let path = base_dir().join("log.txt");
        let Ok(bytes) = fs::read(&path) else { return };
        let off = match self.log_off {
            Some(o) => o as usize,
            None => {
                // This game session only: from the last "task system ready" line.
                let needle = b"coop: task system ready";
                let pos = bytes.windows(needle.len()).rposition(|w| w == needle).unwrap_or(0);
                bytes[..pos].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)
            }
        };
        let off = off.min(bytes.len());
        if off == bytes.len() {
            self.log_off = Some(off as u64);
            return;
        }
        let end = (off + LOG_CHUNK).min(bytes.len());
        let mut msg = vec![self.first_chunk as u8];
        msg.extend_from_slice(&bytes[off..end]);
        if net.send_bulk_to(Dest::Host, BULK_LOG, &msg) {
            self.first_chunk = false;
            self.log_off = Some(end as u64);
        }
    }
}

// ---------------------------------------------------------------------------
// Host-side download server: `play.cmd` (update.ps1) fetches the host's DLL
// over plain HTTP before the friend launches the game, so the friend never has
// to restart just to update. Port = session port + 1 (28101 by default); the
// DLL is not secret. Only runs while this game hosts.
// ---------------------------------------------------------------------------

pub const HTTP_PORT: u16 = 28101;

pub fn start_http_server() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    use std::io::Read;
    use std::net::TcpListener;
    let Ok(listener) = TcpListener::bind(("0.0.0.0", HTTP_PORT)) else {
        log::line(&format!("testsync: could not open the download port {HTTP_PORT}"));
        return;
    };
    log::line(&format!("testsync: serving our DLL over HTTP on port {HTTP_PORT} (update.ps1 fetches it)"));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(20)));
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let ok = req.starts_with("GET /sekiro_coop.dll");
            let body = if ok { own_dll_path().and_then(|p| fs::read(p).ok()) } else { None };
            let _ = match body {
                Some(b) => {
                    let head = format!("HTTP/1.0 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n", b.len());
                    stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&b))
                }
                None => stream.write_all(b"HTTP/1.0 404 Not Found\r\nContent-Length: 0\r\n\r\n"),
            };
            if ok {
                log::line("testsync: a friend downloaded the DLL over HTTP");
            }
        }
    });
}
