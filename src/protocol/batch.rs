//! BatchPacket (0x92): `u32be compressed_len` + zlib stream containing
//! repeated `u32be len` + packet bytes (packet id first).

use std::io::{self, Read, Write};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use super::ids;
use crate::binary::ByteReader;

/// Upper bound for inflated client batches (decompression-bomb guard).
pub const MAX_INFLATED: usize = 2 * 1024 * 1024;
const MAX_PACKETS_PER_BATCH: usize = 256;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Builds a complete batch packet (including the 0x92 id byte).
pub fn encode_batch(packets: &[&[u8]], level: u32) -> Vec<u8> {
    let raw_len: usize = packets.iter().map(|p| 4 + p.len()).sum();
    let mut raw = Vec::with_capacity(raw_len);
    for p in packets {
        raw.extend_from_slice(&(p.len() as u32).to_be_bytes());
        raw.extend_from_slice(p);
    }
    let mut enc = ZlibEncoder::new(Vec::with_capacity(raw_len / 4 + 32), Compression::new(level.min(9)));
    enc.write_all(&raw).expect("writing to a Vec cannot fail");
    let compressed = enc.finish().expect("finishing into a Vec cannot fail");

    let mut out = Vec::with_capacity(5 + compressed.len());
    out.push(ids::BATCH);
    out.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    out.extend_from_slice(&compressed);
    out
}

/// `body` is the batch packet without its id byte. Returns the inflated
/// stream; walk it with [`for_each_packet`].
pub fn decode_batch(body: &[u8]) -> io::Result<Vec<u8>> {
    let mut r = ByteReader::new(body);
    let len = r.u32_be().map_err(|e| invalid(e.to_string()))? as usize;
    let compressed = r.bytes(len).map_err(|e| invalid(e.to_string()))?;
    let mut out = Vec::new();
    ZlibDecoder::new(compressed).take(MAX_INFLATED as u64 + 1).read_to_end(&mut out)?;
    if out.len() > MAX_INFLATED {
        return Err(invalid("inflated batch too large"));
    }
    Ok(out)
}

/// Calls `f` for every packet in an inflated batch. Returns false if the
/// stream was malformed (packets before the error were still delivered).
pub fn for_each_packet(raw: &[u8], mut f: impl FnMut(&[u8])) -> bool {
    let mut r = ByteReader::new(raw);
    let mut n = 0;
    while !r.is_empty() {
        n += 1;
        if n > MAX_PACKETS_PER_BATCH {
            return false;
        }
        let Ok(len) = r.u32_be() else { return false };
        let Ok(p) = r.bytes(len as usize) else { return false };
        if !p.is_empty() {
            f(p);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let a = [0x94u8, 1, 2, 3];
        let b = vec![0x9du8; 500];
        let pkt = encode_batch(&[&a, &b], 6);
        assert_eq!(pkt[0], ids::BATCH);
        let raw = decode_batch(&pkt[1..]).unwrap();
        let mut got = Vec::new();
        assert!(for_each_packet(&raw, |p| got.push(p.to_vec())));
        assert_eq!(got, vec![a.to_vec(), b]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_batch(&[0, 0, 0, 4, 1, 2, 3, 4]).is_err());
        assert!(!for_each_packet(&[0, 0, 0, 9, 1], |_| {}));
    }
}
