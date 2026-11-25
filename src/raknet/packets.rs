//! Offline (unconnected) RakNet packets and shared address codec.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use super::consts::*;
use crate::binary::{BinResult, BinaryError, ByteReader, ByteWriter};

fn check_magic(r: &mut ByteReader<'_>) -> BinResult<()> {
    if r.bytes(16)? == &MAGIC[..] {
        Ok(())
    } else {
        Err(BinaryError::Invalid("bad magic"))
    }
}

// --- RakNet system address -------------------------------------------------

fn put_v4(w: &mut ByteWriter, octets: [u8; 4], port: u16) {
    w.put_u8(4);
    for b in octets {
        w.put_u8(!b);
    }
    w.put_u16_be(port);
}

/// Writes an address in RakNet form. IPv6 is only supported when it is an
/// IPv4-mapped address; anything else is written as 0.0.0.0 (MCPE 0.14 is
/// IPv4-only).
pub fn put_address(w: &mut ByteWriter, addr: SocketAddr) {
    match addr {
        SocketAddr::V4(a) => put_v4(w, a.ip().octets(), a.port()),
        SocketAddr::V6(a) => match a.ip().to_ipv4_mapped() {
            Some(v4) => put_v4(w, v4.octets(), a.port()),
            None => put_v4(w, [0; 4], a.port()),
        },
    }
}

pub fn read_address(r: &mut ByteReader<'_>) -> BinResult<SocketAddr> {
    match r.u8()? {
        4 => {
            let b = r.bytes(4)?;
            let ip = Ipv4Addr::new(!b[0], !b[1], !b[2], !b[3]);
            let port = r.u16_be()?;
            Ok(SocketAddr::V4(SocketAddrV4::new(ip, port)))
        }
        6 => {
            let _family = r.u16_le()?;
            let port = r.u16_be()?;
            let flow = r.u32_be()?;
            let mut octets = [0u8; 16];
            octets.copy_from_slice(r.bytes(16)?);
            let scope = r.u32_be()?;
            Ok(SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(octets), port, flow, scope)))
        }
        _ => Err(BinaryError::Invalid("address version")),
    }
}

// --- Unconnected ping / pong -----------------------------------------------

/// Parses `0x01` / `0x02`; returns the client's ping timestamp.
pub fn parse_unconnected_ping(data: &[u8]) -> BinResult<i64> {
    let mut r = ByteReader::new(data);
    r.u8()?;
    let time = r.i64_be()?;
    check_magic(&mut r)?;
    Ok(time)
}

pub fn write_unconnected_pong(w: &mut ByteWriter, time: i64, guid: i64, motd: &str) {
    w.put_u8(ID_UNCONNECTED_PONG);
    w.put_i64_be(time);
    w.put_i64_be(guid);
    w.put_bytes(&MAGIC);
    w.put_string_be(motd);
}

// --- Open connection handshake ---------------------------------------------

/// Parses `0x05`; returns `(raknet_protocol, mtu)` where mtu is the UDP
/// payload length plus IP/UDP headers (the client pads the packet to probe
/// the path MTU).
pub fn parse_open_request1(data: &[u8]) -> BinResult<(u8, usize)> {
    let mut r = ByteReader::new(data);
    r.u8()?;
    check_magic(&mut r)?;
    let protocol = r.u8()?;
    Ok((protocol, data.len() + UDP_HEADER_SIZE))
}

pub fn write_open_reply1(w: &mut ByteWriter, guid: i64, mtu: u16) {
    w.put_u8(ID_OPEN_REPLY_1);
    w.put_bytes(&MAGIC);
    w.put_i64_be(guid);
    w.put_u8(0); // use security = false
    w.put_u16_be(mtu);
}

#[derive(Debug, Clone, Copy)]
pub struct OpenRequest2 {
    pub server_addr: SocketAddr,
    pub mtu: u16,
    pub client_guid: i64,
}

pub fn parse_open_request2(data: &[u8]) -> BinResult<OpenRequest2> {
    let mut r = ByteReader::new(data);
    r.u8()?;
    check_magic(&mut r)?;
    let server_addr = read_address(&mut r)?;
    let mtu = r.u16_be()?;
    let client_guid = r.i64_be()?;
    Ok(OpenRequest2 { server_addr, mtu, client_guid })
}

pub fn write_open_reply2(w: &mut ByteWriter, guid: i64, client: SocketAddr, mtu: u16) {
    w.put_u8(ID_OPEN_REPLY_2);
    w.put_bytes(&MAGIC);
    w.put_i64_be(guid);
    put_address(w, client);
    w.put_u16_be(mtu);
    w.put_u8(0); // security
}

pub fn write_incompatible_protocol(w: &mut ByteWriter, guid: i64) {
    w.put_u8(ID_INCOMPATIBLE_PROTOCOL);
    w.put_u8(RAKNET_PROTOCOL);
    w.put_bytes(&MAGIC);
    w.put_i64_be(guid);
}

pub fn write_no_free_connections(w: &mut ByteWriter, guid: i64) {
    w.put_u8(ID_NO_FREE_CONNECTIONS);
    w.put_bytes(&MAGIC);
    w.put_i64_be(guid);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_roundtrip() {
        let a: SocketAddr = "192.168.1.7:19132".parse().unwrap();
        let mut w = ByteWriter::new();
        put_address(&mut w, a);
        assert_eq!(w.len(), 7);
        assert_eq!(read_address(&mut ByteReader::new(w.as_slice())).unwrap(), a);
    }

    #[test]
    fn request1_mtu_probe() {
        let mut w = ByteWriter::new();
        w.put_u8(ID_OPEN_REQUEST_1);
        w.put_bytes(&MAGIC);
        w.put_u8(RAKNET_PROTOCOL);
        w.put_bytes(&[0u8; 1000]);
        let (proto, mtu) = parse_open_request1(w.as_slice()).unwrap();
        assert_eq!(proto, 7);
        assert_eq!(mtu, 1 + 16 + 1 + 1000 + 28);
    }

    #[test]
    fn request2_parse() {
        let mut w = ByteWriter::new();
        w.put_u8(ID_OPEN_REQUEST_2);
        w.put_bytes(&MAGIC);
        put_address(&mut w, "10.0.0.1:19132".parse().unwrap());
        w.put_u16_be(1400);
        w.put_i64_be(42);
        let req = parse_open_request2(w.as_slice()).unwrap();
        assert_eq!(req.mtu, 1400);
        assert_eq!(req.client_guid, 42);
    }
}
