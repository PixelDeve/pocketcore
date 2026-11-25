//! Static arena worlds: lazily generated shared-template chunks plus an
//! arena dirty-block tracker for instant match resets.

pub mod chunk;
pub mod leveldb;
pub mod pcw;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chunk::{Chunk, ChunkTemplate, HEIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generator {
    Flat,
    Void,
}

#[derive(Clone)]
pub struct World {
    pub spawn: (i32, i32, i32),
    generator: Generator,
    /// Chunk radius around the origin that exists; `None` = unbounded.
    limit: Option<i32>,
    /// Loaded maps: inclusive chunk rectangle (min_x, max_x, min_z, max_z) that exists.
    rect: Option<(i32, i32, i32, i32)>,
    order_byte: bool,
    flat: ChunkTemplate,
    void: ChunkTemplate,
    chunks: HashMap<(i32, i32), Chunk>,
    /// (x, y, z) -> original (id, meta), recorded on first modification.
    dirty: HashMap<(i32, u8, i32), (u8, u8)>,
    track_dirty: bool,
    /// Chunks changed since the last save (template persistence).
    unsaved: HashSet<(i32, i32)>,
}

impl World {
    pub fn new(generator: Generator, limit: Option<i32>, spawn: (i32, i32, i32), order_byte: bool) -> Self {
        Self {
            spawn,
            generator,
            limit,
            rect: None,
            order_byte,
            flat: ChunkTemplate::flat(),
            void: ChunkTemplate::void(),
            chunks: HashMap::new(),
            dirty: HashMap::new(),
            track_dirty: true,
            unsaved: HashSet::new(),
        }
    }

    /// World made of loaded chunks `(cx, cz, chunk)`; gaps inside the extent
    /// (plus `pad` chunks around it) are void.
    pub fn from_chunks(chunks: Vec<Chunk>, spawn: (i32, i32, i32), order_byte: bool, pad: i32) -> Self {
        let mut w = World::new(Generator::Void, None, spawn, order_byte);
        let mut r = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for c in chunks {
            r = (r.0.min(c.x), r.1.max(c.x), r.2.min(c.z), r.3.max(c.z));
            w.chunks.insert((c.x, c.z), c);
        }
        if r.0 <= r.1 {
            w.rect = Some((r.0 - pad, r.1 + pad, r.2 - pad, r.3 + pad));
        }
        w
    }

    /// Independent copy for a match instance: chunk data is shared until
    /// written (copy-on-write), change tracking starts fresh.
    pub fn fork(&self) -> World {
        let mut w = self.clone();
        w.dirty.clear();
        w.unsaved.clear();
        w.track_dirty = false;
        w
    }

    /// Creates every in-bounds chunk of a generated world so a save captures
    /// the generated floor too. No-op for unbounded worlds.
    pub fn materialize_all(&mut self) {
        let (x0, x1, z0, z1) = match (self.rect, self.limit) {
            (Some(r), _) => r,
            (None, Some(r)) => (-r, r, -r, r),
            (None, None) => return,
        };
        for cx in x0..=x1 {
            for cz in z0..=z1 {
                self.ensure(cx, cz);
            }
        }
    }

    pub fn has_unsaved(&self) -> bool {
        !self.unsaved.is_empty()
    }

    pub fn mark_saved(&mut self) {
        self.unsaved.clear();
    }

    /// Every materialised chunk that holds at least one block (for saving).
    pub fn solid_chunks(&self) -> Vec<&Chunk> {
        let mut v: Vec<&Chunk> = self.chunks.values().filter(|c| !c.is_air()).collect();
        v.sort_by_key(|c| (c.x, c.z));
        v
    }

    pub fn in_bounds(&self, cx: i32, cz: i32) -> bool {
        if let Some((x0, x1, z0, z1)) = self.rect {
            return (x0..=x1).contains(&cx) && (z0..=z1).contains(&cz);
        }
        match self.limit {
            None => true,
            Some(r) => cx.abs() <= r && cz.abs() <= r,
        }
    }

    pub fn loaded_chunks(&self) -> usize {
        self.chunks.len()
    }

    pub fn dirty_blocks(&self) -> usize {
        self.dirty.len()
    }

    /// Enable/disable dirty tracking (e.g. off while building an arena).
    pub fn set_tracking(&mut self, on: bool) {
        self.track_dirty = on;
    }

    /// Forget recorded changes: the current state becomes the reset baseline.
    pub fn commit_baseline(&mut self) {
        self.dirty.clear();
    }

    fn ensure(&mut self, cx: i32, cz: i32) -> Option<&mut Chunk> {
        if !self.chunks.contains_key(&(cx, cz)) {
            if !self.in_bounds(cx, cz) {
                return None;
            }
            let t = match self.generator {
                Generator::Flat => &self.flat,
                Generator::Void => &self.void,
            };
            let c = Chunk::from_template(cx, cz, t);
            self.chunks.insert((cx, cz), c);
        }
        self.chunks.get_mut(&(cx, cz))
    }

    /// Cached wire packet for a chunk; `None` outside the world bounds.
    pub fn chunk_packet(&mut self, cx: i32, cz: i32) -> Option<Arc<[u8]>> {
        let order = self.order_byte;
        self.ensure(cx, cz).map(|c| c.packet(order))
    }

    pub fn get_block(&mut self, x: i32, y: i32, z: i32) -> (u8, u8) {
        if !(0..HEIGHT as i32).contains(&y) {
            return (0, 0);
        }
        match self.ensure(x >> 4, z >> 4) {
            Some(c) => c.get((x & 15) as usize, y as usize, (z & 15) as usize),
            None => (0, 0),
        }
    }

    /// Returns false if the position is outside the world.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, id: u8, meta: u8) -> bool {
        if !(0..HEIGHT as i32).contains(&y) {
            return false;
        }
        let track = self.track_dirty;
        let Some(c) = self.ensure(x >> 4, z >> 4) else { return false };
        let (lx, lz) = ((x & 15) as usize, (z & 15) as usize);
        let old = c.get(lx, y as usize, lz);
        let meta = meta & 0x0f;
        if old == (id, meta) {
            return true;
        }
        c.set(lx, y as usize, lz, id, meta);
        self.unsaved.insert((x >> 4, z >> 4));
        if track {
            self.dirty.entry((x, y as u8, z)).or_insert(old);
        }
        true
    }

    /// Restores every modified block; returns the restored positions so the
    /// caller can broadcast block updates.
    pub fn reset_arena(&mut self) -> Vec<(i32, i32, i32)> {
        let entries: Vec<_> = self.dirty.drain().collect();
        let prev = std::mem::replace(&mut self.track_dirty, false);
        let mut restored = Vec::with_capacity(entries.len());
        for ((x, y, z), (id, meta)) in entries {
            self.set_block(x, y as i32, z, id, meta);
            restored.push((x, y as i32, z));
        }
        self.track_dirty = prev;
        restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_reset_restores_original() {
        let mut w = World::new(Generator::Flat, Some(4), (0, 4, 0), true);
        assert_eq!(w.get_block(5, 3, -20), (chunk::block::GRASS, 0));
        w.set_block(5, 3, -20, chunk::block::STONE, 2);
        w.set_block(5, 3, -20, chunk::block::DIRT, 0); // second change keeps first original
        w.set_block(40, 50, 3, 20, 0);
        assert_eq!(w.dirty_blocks(), 2);
        let restored = w.reset_arena();
        assert_eq!(restored.len(), 2);
        assert_eq!(w.get_block(5, 3, -20), (chunk::block::GRASS, 0));
        assert_eq!(w.get_block(40, 50, 3), (0, 0));
        assert_eq!(w.dirty_blocks(), 0);
    }

    #[test]
    fn bounds_and_sharing() {
        let mut w = World::new(Generator::Flat, Some(1), (0, 4, 0), true);
        assert!(w.chunk_packet(1, -1).is_some());
        assert!(w.chunk_packet(2, 0).is_none());
        assert!(!w.set_block(64, 4, 0, 1, 0));
        assert!(!w.set_block(0, 200, 0, 1, 0));
    }
}
