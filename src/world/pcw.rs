//! `.pcw`: pocketcore's own world file, used to persist the template map.
//!
//! Layout: `"PCW1"` then one zlib stream of
//! `i32le spawn_x, spawn_y, spawn_z, u32le chunk_count` followed by
//! `chunk_count` x (`i32le cx, i32le cz, BLOCKS id bytes, META nibble bytes`).

use std::io::{self, Read, Write};
use std::path::Path;

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;

use super::chunk::{Chunk, BLOCKS, META};
use super::World;

const MAGIC: &[u8; 4] = b"PCW1";
const MAX_CHUNKS: u32 = 65_536;

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_owned())
}

pub fn encode(world: &World) -> io::Result<Vec<u8>> {
    let chunks = world.solid_chunks();
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::new(6));
    for v in [world.spawn.0, world.spawn.1, world.spawn.2] {
        enc.write_all(&v.to_le_bytes())?;
    }
    enc.write_all(&(chunks.len() as u32).to_le_bytes())?;
    for c in chunks {
        enc.write_all(&c.x.to_le_bytes())?;
        enc.write_all(&c.z.to_le_bytes())?;
        let (b, m) = c.raw();
        enc.write_all(b)?;
        enc.write_all(m)?;
    }
    let body = enc.finish()?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&body);
    Ok(out)
}

/// Parses a `.pcw` file; `pad` extra void chunks surround the stored area.
pub fn decode(bytes: &[u8], order_byte: bool, pad: i32) -> io::Result<World> {
    let body = bytes.strip_prefix(MAGIC.as_slice()).ok_or_else(|| bad("not a .pcw file"))?;
    let mut dec = ZlibDecoder::new(body);
    let mut head = [0u8; 16];
    dec.read_exact(&mut head)?;
    let int = |i: usize| i32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    let spawn = (int(0), int(4), int(8));
    let count = u32::from_le_bytes([head[12], head[13], head[14], head[15]]);
    if count > MAX_CHUNKS {
        return Err(bad("implausible chunk count"));
    }
    let mut chunks = Vec::with_capacity(count as usize);
    let mut buf_b = vec![0u8; BLOCKS];
    let mut buf_m = vec![0u8; META];
    for _ in 0..count {
        let mut pos = [0u8; 8];
        dec.read_exact(&mut pos)?;
        dec.read_exact(&mut buf_b)?;
        dec.read_exact(&mut buf_m)?;
        let cx = i32::from_le_bytes([pos[0], pos[1], pos[2], pos[3]]);
        let cz = i32::from_le_bytes([pos[4], pos[5], pos[6], pos[7]]);
        chunks.push(Chunk::from_raw(cx, cz, &buf_b, &buf_m).ok_or_else(|| bad("bad chunk size"))?);
    }
    Ok(World::from_chunks(chunks, spawn, order_byte, pad))
}

pub fn save(world: &World, path: &Path) -> io::Result<()> {
    let bytes = encode(world)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

pub fn load(path: &Path, order_byte: bool, pad: i32) -> io::Result<World> {
    decode(&std::fs::read(path)?, order_byte, pad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Generator;

    #[test]
    fn roundtrip_keeps_blocks_spawn_and_bounds() {
        let mut w = World::new(Generator::Flat, Some(2), (3, 70, -4), true);
        w.materialize_all();
        w.set_block(5, 3, -20, 35, 14);
        w.set_block(-20, 90, 33, 49, 0);
        assert!(w.has_unsaved());
        let bytes = encode(&w).unwrap();
        let mut back = decode(&bytes, true, 1).unwrap();
        assert_eq!(back.spawn, (3, 70, -4));
        assert_eq!(back.get_block(5, 3, -20), (35, 14));
        assert_eq!(back.get_block(-20, 90, 33), (49, 0));
        assert_eq!(back.get_block(0, 3, 0), (2, 0));
        assert!(back.in_bounds(0, 0) && !back.in_bounds(100, 0));
        assert!(decode(b"nope", true, 0).is_err());
    }

    #[test]
    fn fork_is_independent() {
        let mut t = World::new(Generator::Flat, Some(2), (0, 4, 0), true);
        t.set_block(1, 10, 1, 4, 0);
        let mut f = t.fork();
        assert_eq!(f.get_block(1, 10, 1), (4, 0));
        f.set_block(1, 10, 1, 0, 0);
        f.set_block(2, 10, 2, 5, 0);
        assert_eq!(t.get_block(1, 10, 1), (4, 0));
        assert_eq!(t.get_block(2, 10, 2), (0, 0));
    }
}
