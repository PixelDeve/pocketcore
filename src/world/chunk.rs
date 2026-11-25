//! 16x128x16 legacy column chunks.
//!
//! Block ids and metadata nibbles live in `Arc`s so identical static chunks
//! (void / flat arena floors) share one allocation; the first write clones
//! (copy-on-write). The wire-ready, zlib-compressed FullChunkData batch is
//! cached per chunk and dropped on modification.

use std::sync::Arc;
use std::collections::VecDeque;

use crate::protocol::{batch, packets};

pub const BLOCKS: usize = 16 * 16 * 128; // 32768
pub const META: usize = BLOCKS / 2; // 16384 nibbles
pub const HEIGHT: usize = 128;

pub mod block {
    pub const AIR: u8 = 0;
    pub const STONE: u8 = 1;
    pub const GRASS: u8 = 2;
    pub const DIRT: u8 = 3;
    pub const BEDROCK: u8 = 7;
}

/// 0.14 column order: x major, then z, y fastest.
#[inline]
fn idx(x: usize, y: usize, z: usize) -> usize {
    (x << 11) | (z << 7) | y
}

fn zeroed<const N: usize>() -> Box<[u8; N]> {
    vec![0u8; N].into_boxed_slice().try_into().expect("exact size")
}

// Legacy 0.14 biome-color entries are four ordered bytes: biome ID, red, green, blue.
// Keep this as bytes rather than a host-endian u32: serializing the old packed
// integer with to_le_bytes() reverses the channel order and can tint grass orange.
const BIOME_COLOR: [u8; 4] = [1, 0x85, 0xB2, 0x4A]; // plains biome, green grass tint
/// 15 = full sky light everywhere (static arena worlds).
const SKY_LIGHT_BYTE: u8 = 0xff;

/// Shared immutable starting contents for new chunks.
#[derive(Clone)]
pub struct ChunkTemplate {
    blocks: Arc<[u8; BLOCKS]>,
    meta: Arc<[u8; META]>,
}

impl ChunkTemplate {
    pub fn void() -> Self {
        Self { blocks: Arc::from(zeroed::<BLOCKS>()), meta: Arc::from(zeroed::<META>()) }
    }

    /// bedrock, 2x dirt, grass: surface block at y = 3.
    pub fn flat() -> Self {
        let mut blocks = zeroed::<BLOCKS>();
        for x in 0..16 {
            for z in 0..16 {
                blocks[idx(x, 0, z)] = block::BEDROCK;
                blocks[idx(x, 1, z)] = block::DIRT;
                blocks[idx(x, 2, z)] = block::DIRT;
                blocks[idx(x, 3, z)] = block::GRASS;
            }
        }
        Self { blocks: Arc::from(blocks), meta: Arc::from(zeroed::<META>()) }
    }
}

/// Cloning shares block storage and the cached packet (copy-on-write), so
/// forking a whole world costs one Arc bump per chunk.
#[derive(Clone)]
pub struct Chunk {
    pub x: i32,
    pub z: i32,
    blocks: Arc<[u8; BLOCKS]>,
    meta: Arc<[u8; META]>,
    cache: Option<Arc<[u8]>>,
}

impl Chunk {
    pub fn from_template(x: i32, z: i32, t: &ChunkTemplate) -> Self {
        Self { x, z, blocks: Arc::clone(&t.blocks), meta: Arc::clone(&t.meta), cache: None }
    }

    /// Builds a chunk from raw column data (`BLOCKS` ids + `META` nibbles).
    pub fn from_raw(x: i32, z: i32, blocks: &[u8], meta: &[u8]) -> Option<Self> {
        let b: Box<[u8; BLOCKS]> = blocks.to_vec().into_boxed_slice().try_into().ok()?;
        let m: Box<[u8; META]> = meta.to_vec().into_boxed_slice().try_into().ok()?;
        Some(Self { x, z, blocks: Arc::from(b), meta: Arc::from(m), cache: None })
    }

    /// Raw block ids and metadata nibbles (for saving).
    pub fn raw(&self) -> (&[u8], &[u8]) {
        (&self.blocks[..], &self.meta[..])
    }

    /// True if every block is air.
    pub fn is_air(&self) -> bool {
        self.blocks.iter().all(|&b| b == block::AIR)
    }

    /// Local coordinates: x, z in 0..16, y in 0..128.
    pub fn get(&self, x: usize, y: usize, z: usize) -> (u8, u8) {
        let i = idx(x, y, z);
        let m = self.meta[i >> 1];
        (self.blocks[i], if i & 1 == 0 { m & 0x0f } else { m >> 4 })
    }

    pub fn set(&mut self, x: usize, y: usize, z: usize, id: u8, meta: u8) {
        let i = idx(x, y, z);
        Arc::make_mut(&mut self.blocks)[i] = id;
        let m = &mut Arc::make_mut(&mut self.meta)[i >> 1];
        let meta = meta & 0x0f;
        *m = if i & 1 == 0 { (*m & 0xf0) | meta } else { (*m & 0x0f) | (meta << 4) };
        self.cache = None;
    }

    pub fn is_shared(&self) -> bool {
        Arc::strong_count(&self.blocks) > 1
    }

    fn height_map(&self) -> [u8; 256] {
        let mut hm = [0u8; 256];
        for z in 0..16 {
            for x in 0..16 {
                let mut y = HEIGHT;
                while y > 0 && self.blocks[idx(x, y - 1, z)] == block::AIR {
                    y -= 1;
                }
                hm[(z << 4) | x] = y as u8;
            }
        }
        hm
    }

    /// Protocol-70 chunk payload: ids, metadata, sky light, block light,
    /// 256-byte height map, then 256 four-byte biome-color entries.
    pub fn network_data(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(BLOCKS + META * 3 + 256 + 1024);
        d.extend_from_slice(&self.blocks[..]);
        d.extend_from_slice(&self.meta[..]);
        d.resize(d.len() + META, SKY_LIGHT_BYTE);
        // Legacy Bedrock expects packed block-light nibbles. Populate light emitted
        // by common 0.14 blocks (torches, glowstone, fire, lava, lit pumpkins).
        d.extend_from_slice(&self.block_light());
        d.extend_from_slice(&self.height_map());
        for _ in 0..256 {
            d.extend_from_slice(&BIOME_COLOR);
        }
        // Protocol 70's legacy FullChunkData payload ends after the 256 biome-color
        // entries. Do not append the later-format block-extra-data count here.
        d
    }

    fn block_light(&self) -> Vec<u8> {
        let mut levels = vec![0u8; BLOCKS];
        let mut queue = VecDeque::new();
        for x in 0..16 { for z in 0..16 { for y in 0..128 {
            let id = self.blocks[idx(x, y, z)];
            let level = match id {
                50 | 76 | 91 => 14,       // torch, redstone torch, jack-o-lantern
                89 | 90 | 124 | 169 => 15, // glowstone, portal, lit lamp, sea lantern
                10 | 11 | 51 => 15,        // lava and fire
                62 => 13,                 // lit furnace
                _ => 0,
            };
            if level > 0 { let i = idx(x, y, z); levels[i] = level; queue.push_back((x, y, z, level)); }
        }}}
        while let Some((x, y, z, level)) = queue.pop_front() {
            if level <= 1 { continue; }
            let next = level - 1;
            let neighbors = [
                (x.wrapping_sub(1), y, z), (x + 1, y, z),
                (x, y.wrapping_sub(1), z), (x, y + 1, z),
                (x, y, z.wrapping_sub(1)), (x, y, z + 1),
            ];
            for (nx, ny, nz) in neighbors {
                if nx >= 16 || ny >= 128 || nz >= 16 { continue; }
                let ni = idx(nx, ny, nz);
                // Air and transparent blocks transmit light; solid blocks stop it.
                let bid = self.blocks[ni];
                let transparent = matches!(bid, 0 | 8..=11 | 20 | 30 | 31 | 32 | 37..=40 | 50 | 51 | 55 | 59 | 63..=68 | 75..=76 | 78 | 83 | 90 | 93..=94 | 101 | 102 | 104..=106 | 111 | 127 | 131 | 132 | 139 | 141..=142 | 149..=150 | 157 | 160 | 171 | 175 | 176..=177);
                if transparent && levels[ni] < next { levels[ni] = next; queue.push_back((nx, ny, nz, next)); }
            }
        }
        let mut packed = vec![0u8; META];
        for i in 0..BLOCKS {
            let shift = if i & 1 == 0 { 0 } else { 4 };
            packed[i >> 1] |= (levels[i] & 0x0f) << shift;
        }
        packed
    }

    /// Wire-ready batch packet, compressed once and cached.
    pub fn packet(&mut self, order_byte: bool) -> Arc<[u8]> {
        if let Some(c) = &self.cache {
            return Arc::clone(c);
        }
        let body = packets::full_chunk_data(self.x, self.z, &self.network_data(), order_byte);
        let pkt: Arc<[u8]> = batch::encode_batch(&[&body], 9).into();
        self.cache = Some(Arc::clone(&pkt));
        pkt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_size_and_heightmap() {
        let c = Chunk::from_template(0, 0, &ChunkTemplate::flat());
        let d = c.network_data();
        assert_eq!(d.len(), 32768 + 16384 * 3 + 256 + 1024);
        let biome_at = 32768 + 16384 * 3 + 256;
        assert_eq!(&d[biome_at..biome_at + 4], &[1, 0x85, 0xB2, 0x4A]);
        assert_eq!(&d[biome_at + 4..biome_at + 8], &[1, 0x85, 0xB2, 0x4A]);
        let hm = c.height_map();
        assert!(hm.iter().all(|&h| h == 4));
    }

    #[test]
    fn copy_on_write_and_cache() {
        let t = ChunkTemplate::flat();
        let mut a = Chunk::from_template(0, 0, &t);
        let b = Chunk::from_template(1, 0, &t);
        assert!(a.is_shared());
        let p1 = a.packet(true);
        assert!(Arc::ptr_eq(&p1, &a.packet(true)));
        a.set(3, 10, 4, block::STONE, 5);
        assert_eq!(a.get(3, 10, 4), (block::STONE, 5));
        assert_eq!(a.get(3, 11, 4), (block::AIR, 0));
        assert_eq!(b.get(3, 10, 4), (block::AIR, 0)); // template untouched
        assert!(!Arc::ptr_eq(&p1, &a.packet(true))); // cache invalidated
    }

    #[test]
    fn torches_and_glowstone_emit_block_light() {
        let mut c = Chunk::from_template(0, 0, &ChunkTemplate::void());
        c.set(8, 20, 8, 50, 0);
        let light = c.block_light();
        let source = idx(8, 20, 8);
        assert_eq!(light[source >> 1] >> ((source & 1) * 4) & 0x0f, 14);
        let near = idx(9, 20, 8);
        assert_eq!(light[near >> 1] >> ((near & 1) * 4) & 0x0f, 13);
    }

    #[test]
    fn nibbles_are_independent() {
        let mut c = Chunk::from_template(0, 0, &ChunkTemplate::void());
        c.set(0, 0, 0, 1, 0xa);
        c.set(0, 1, 0, 1, 0x5);
        assert_eq!(c.get(0, 0, 0).1, 0xa);
        assert_eq!(c.get(0, 1, 0).1, 0x5);
    }
}
