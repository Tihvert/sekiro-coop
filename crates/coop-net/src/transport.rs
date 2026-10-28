//! Datagram transports under a `Session`. The session only needs "send a datagram to an
//! address" and "give me the next datagram" (non-blocking), so a plain UDP socket and a
//! Steam relay connection are interchangeable. Steam peers are shown to the session as
//! made-up IPv6 addresses (`steam_addr`), so the session's peer bookkeeping (who said
//! Hello, who do we answer) does not change.

use std::io;
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};

/// What a `Session` sends and receives through. All calls must be non-blocking.
pub trait Datagram {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize>;
    /// The next received datagram; `ErrorKind::WouldBlock` when there is none.
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)>;
    fn local_addr(&self) -> io::Result<SocketAddr>;
}

impl Datagram for UdpSocket {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<usize> {
        UdpSocket::send_to(self, buf, to)
    }
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        UdpSocket::recv_from(self, buf)
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        UdpSocket::local_addr(self)
    }
}

/// A transport that goes nowhere: sends vanish, nothing arrives. Stands in for a session
/// whose real transport (Steam) is not up yet.
pub struct Inert;

impl Datagram for Inert {
    fn send_to(&self, buf: &[u8], _to: SocketAddr) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn recv_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        Err(io::ErrorKind::WouldBlock.into())
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0))
    }
}

/// Prefix of the made-up addresses: `fd53:656b:6972:6f00::/64` ("S", "ek", "ir", "o"), a
/// unique-local range no real UDP peer of ours uses. The low 64 bits are the SteamID64.
const STEAM_PREFIX: [u16; 4] = [0xfd53, 0x656b, 0x6972, 0x6f00];

/// The address that stands for a Steam user (port 0).
pub fn steam_addr(steam_id: u64) -> SocketAddr {
    let s = steam_id.to_be_bytes();
    let seg = |i: usize| u16::from_be_bytes([s[2 * i], s[2 * i + 1]]);
    let ip = Ipv6Addr::new(
        STEAM_PREFIX[0],
        STEAM_PREFIX[1],
        STEAM_PREFIX[2],
        STEAM_PREFIX[3],
        seg(0),
        seg(1),
        seg(2),
        seg(3),
    );
    SocketAddr::new(ip.into(), 0)
}

/// The SteamID64 behind an address made by `steam_addr`, `None` for any other address.
pub fn steam_id_of(addr: &SocketAddr) -> Option<u64> {
    let SocketAddr::V6(v6) = addr else { return None };
    let seg = v6.ip().segments();
    if seg[..4] != STEAM_PREFIX {
        return None;
    }
    Some(seg[4..].iter().fold(0u64, |acc, s| acc << 16 | *s as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_addresses_round_trip() {
        for id in [0u64, 1, 76561198000000000, u64::MAX, 0x0110_0001_0000_0001] {
            let a = steam_addr(id);
            assert_eq!(steam_id_of(&a), Some(id));
            assert_eq!(a.port(), 0);
        }
    }

    #[test]
    fn ordinary_addresses_are_not_steam_peers() {
        assert_eq!(steam_id_of(&"127.0.0.1:28100".parse().unwrap()), None);
        assert_eq!(steam_id_of(&"[::1]:28100".parse().unwrap()), None);
        assert_eq!(steam_id_of(&"[2001:db8::1]:1".parse().unwrap()), None);
    }
}
