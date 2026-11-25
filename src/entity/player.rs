use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;

use crate::binary::Vec3;
use crate::protocol::item::Slot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    /// Join sequence sent; waiting for RequestChunkRadius.
    LoggedIn,
    /// Streaming the initial chunks.
    Loading,
    /// PlayStatus(PLAYER_SPAWN) sent.
    Spawned,
}

pub struct Player {
    pub addr: SocketAddr,
    pub eid: i64,
    pub name: String,
    pub uuid: [u8; 16],
    pub client_id: i64,
    pub skin_slim: bool,
    pub skin: Box<[u8]>,
    pub state: PlayerState,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub view_radius: i32,
    pub center: (i32, i32),
    pub sent_chunks: HashSet<(i32, i32)>,
    /// Chunks whose FullChunkData packets have actually been queued to RakNet.
    /// Unlike sent_chunks, this is safe to use as a teleport-readiness check.
    pub loaded_chunks: HashSet<(i32, i32)>,
    pub chunk_queue: VecDeque<(i32, i32)>,
    /// Number of initial join chunks actually queued to RakNet.
    pub initial_chunks_sent: usize,
    /// PMMP-style spawn threshold, capped to the number of available chunks.
    pub initial_chunks_needed: usize,
    pub health: f32,
    pub dead: bool,
    pub gamemode: i32,
    pub inventory: [Slot; 36],
    /// Selected hotbar index 0..9 (hotbar = inventory slots 0..9).
    pub held: u8,
    /// Hotbar link table (like PocketMine): hotbar slot -> inventory index. The
    /// survival client re-links slots itself and reports it in MobEquipment.
    pub hotbar: [u8; 9],
    pub armor: [Slot; 4],
    pub spawn_override: Option<(f32, f32, f32)>,
    /// Per-player knockback (horizontal, vertical) applied when this player is hit.
    pub kb: Option<(f32, f32)>,
    pub last_attack_tick: u64,
    /// Server tick at which the join sequence was sent.
    pub login_tick: u64,
    /// World instance the player is in (0 = original template map).
    pub world: u32,
    /// Server-controlled fake player (no connection).
    pub is_bot: bool,
    /// Moving between worlds: new chunks are streaming, input is ignored.
    pub switching: bool,
    /// Same-world teleport waiting for the target chunks to stream (input ignored).
    pub tp_wait: bool,
    /// A same-world entity correction to broadcast after destination chunks finish streaming.
    pub tp_broadcast_pending: bool,
    /// Teleport the client has not confirmed yet: target eye position and stale-move count.
    pub tp_target: Option<(Vec3, u32)>,
    /// The entity id this client uses for itself, learned from its own MovePlayer packets.
    pub local_eid: i64,
    /// Tick a teleport / world switch started holding the player; a timeout releases it if the
    /// destination chunks are never confirmed (otherwise the player stays frozen server-side).
    pub hold_since: u64,
    /// Survival mining in progress: block position and the tick START_BREAK arrived.
    pub break_start: Option<(i32, i32, i32, u64)>,
    /// Tick of this player's last successful block break (PocketMine `lastBreak`).
    pub last_break: Option<u64>,
    /// Invisible to everyone else (spectators).
    pub hidden: bool,
    /// Blocks fallen since the last time the player stood on something.
    pub fall_distance: f32,
    /// Blocks fallen between the last two movement packets (used by the slime platform).
    pub fall_speed: f32,
}

/// Maximum stack size: tools, weapons and armour do not stack.
pub fn max_stack(id: i16) -> u8 {
    match id {
        256..=259 | 267..=279 | 283..=286 | 290..=294 | 298..=317 | 359 => 1,
        _ => 64,
    }
}

/// Armour points of a worn piece (leather, chain, iron, diamond, gold).
pub fn armor_points(id: i16) -> u32 {
    match id {
        298 => 1,
        299 => 3,
        300 => 2,
        301 => 1,
        302 | 306 | 314 => 2,
        303 => 5,
        307 => 6,
        315 => 5,
        304 => 4,
        308 => 5,
        316 => 3,
        305 | 317 => 1,
        309 => 2,
        310 => 3,
        311 => 8,
        312 => 6,
        313 => 3,
        _ => 0,
    }
}

impl Player {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        addr: SocketAddr,
        eid: i64,
        name: String,
        uuid: [u8; 16],
        client_id: i64,
        skin_slim: bool,
        skin: Vec<u8>,
        pos: (f32, f32, f32),
    ) -> Self {
        let mut p = Self {
            addr,
            eid,
            name,
            uuid,
            client_id,
            skin_slim,
            skin: skin.into_boxed_slice(),
            state: PlayerState::LoggedIn,
            x: pos.0,
            y: pos.1,
            z: pos.2,
            yaw: 0.0,
            pitch: 0.0,
            view_radius: 0,
            center: (0, 0),
            sent_chunks: HashSet::new(),
            loaded_chunks: HashSet::new(),
            chunk_queue: VecDeque::new(),
            initial_chunks_sent: 0,
            initial_chunks_needed: 0,
            health: 20.0,
            dead: false,
            gamemode: 0,
            inventory: [Slot::EMPTY; 36],
            held: 0,
            hotbar: [0, 1, 2, 3, 4, 5, 6, 7, 8],
            armor: [Slot::EMPTY; 4],
            spawn_override: None,
            kb: None,
            last_attack_tick: 0,
            login_tick: 0,
            world: 0,
            is_bot: false,
            switching: false,
            tp_wait: false,
            tp_broadcast_pending: false,
            tp_target: None,
            local_eid: crate::protocol::packets::LOCAL_PLAYER_EID,
            hold_since: 0,
            break_start: None,
            last_break: None,
            hidden: false,
            fall_distance: 0.0,
            fall_speed: 0.0,
        };
        p.center = p.chunk_pos();
        p
    }

    /// Inventory index the selected hotbar slot links to.
    pub fn held_slot(&self) -> usize {
        self.hotbar[self.held as usize % 9] as usize % 36
    }

    pub fn held_item(&self) -> Slot {
        self.inventory[self.held_slot()]
    }

    pub fn armor_total(&self) -> u32 {
        self.armor.iter().map(|a| armor_points(a.id)).sum::<u32>().min(20)
    }

    pub fn clear_inventory(&mut self) {
        self.inventory = [Slot::EMPTY; 36];
        self.hotbar = [0, 1, 2, 3, 4, 5, 6, 7, 8];
    }

    /// Adds an item, merging stacks first (hotbar first). Returns the count
    /// that did not fit.
    pub fn give(&mut self, mut item: Slot) -> u8 {
        if item.is_empty() {
            return 0;
        }
        let max = max_stack(item.id);
        for s in self.inventory.iter_mut() {
            if item.count == 0 {
                break;
            }
            if !s.is_empty() && s.id == item.id && s.damage == item.damage && s.count < max {
                let add = (max - s.count).min(item.count);
                s.count += add;
                item.count -= add;
            }
        }
        for s in self.inventory.iter_mut() {
            if item.count == 0 {
                break;
            }
            if s.is_empty() {
                let put = max.min(item.count);
                *s = Slot { count: put, ..item };
                item.count -= put;
            }
        }
        item.count
    }

    /// Removes one item matching both id and damage (held slot first).
    /// Damage is significant for legacy blocks such as wool, planks and dyes.
    pub fn consume_one_matching(&mut self, id: i16, damage: i16) -> bool {
        let held = self.held_slot();
        let matches = |s: &Slot| !s.is_empty() && s.id == id && s.damage == damage;
        let idx = if matches(&self.inventory[held]) {
            Some(held)
        } else {
            self.inventory.iter().position(matches)
        };
        let Some(i) = idx else { return false };
        let s = &mut self.inventory[i];
        s.count -= 1;
        if s.count == 0 {
            *s = Slot::EMPTY;
        }
        true
    }

    /// Removes one item of `id` (held slot first). Kept for commands which
    /// intentionally consume by item type rather than a metadata variant.
    pub fn consume_one(&mut self, id: i16) -> bool {
        let damage = self.held_item().damage;
        if self.held_item().id == id && self.consume_one_matching(id, damage) {
            return true;
        }
        let Some(item) = self.inventory.iter().find(|s| !s.is_empty() && s.id == id).copied() else {
            return false;
        };
        self.consume_one_matching(id, item.damage)
    }

    /// Total count of an item id across the inventory.
    pub fn count_item(&self, id: i16) -> u32 {
        self.inventory.iter().filter(|s| s.id == id && !s.is_empty()).map(|s| s.count as u32).sum()
    }

    /// Removes `n` of an item id (all or nothing). Returns false, changing
    /// nothing, if the player has fewer.
    pub fn take_items(&mut self, id: i16, mut n: u32) -> bool {
        if self.count_item(id) < n {
            return false;
        }
        for s in self.inventory.iter_mut().rev() {
            if n == 0 {
                break;
            }
            if s.id == id && !s.is_empty() {
                let t = (s.count as u32).min(n);
                s.count -= t as u8;
                n -= t;
                if s.count == 0 {
                    *s = Slot::EMPTY;
                }
            }
        }
        true
    }

    pub fn has_item(&self, id: i16) -> bool {
        self.inventory.iter().any(|s| s.id == id && !s.is_empty())
    }

    pub fn has_item_matching(&self, id: i16, damage: i16) -> bool {
        self.inventory.iter().any(|s| s.id == id && s.damage == damage && !s.is_empty())
    }

    pub fn chunk_pos(&self) -> (i32, i32) {
        ((self.x.floor() as i32) >> 4, (self.z.floor() as i32) >> 4)
    }

    /// Queues chunks inside the view circle that were not sent yet, nearest
    /// first, and forgets chunks that fell far outside it.
    pub fn plan_chunks(&mut self, in_bounds: &dyn Fn(i32, i32) -> bool) {
        let (cx, cz) = self.chunk_pos();
        self.center = (cx, cz);
        // A teleport needs the destination chunk plus its 8 neighbours. A
        // circular radius of 1 omits diagonal neighbours, so keep at least
        // radius 2 (13 chunks) to guarantee the complete 3x3 square.
        let r = self.view_radius.max(2);

        // The queue may contain chunks planned around the previous centre.
        // Keeping those entries after a long move/teleport wastes the stream
        // budget and can delay the destination teleport until unrelated chunks
        // have been sent. Drop out-of-view pending entries and make them
        // eligible for a future request again.
        let keep = r + 2;
        let mut dropped = Vec::new();
        self.chunk_queue.retain(|&(x, z)| {
            let keep_chunk = (x - cx).abs() <= keep && (z - cz).abs() <= keep;
            if !keep_chunk {
                dropped.push((x, z));
            }
            keep_chunk
        });
        for chunk in dropped {
            self.sent_chunks.remove(&chunk);
            self.loaded_chunks.remove(&chunk);
        }

        let mut want: Vec<(i32, (i32, i32))> = Vec::new();
        for dz in -r..=r {
            for dx in -r..=r {
                let d2 = dx * dx + dz * dz;
                let p = (cx + dx, cz + dz);
                if d2 <= r * r && !self.sent_chunks.contains(&p) && in_bounds(p.0, p.1) {
                    want.push((d2, p));
                }
            }
        }
        want.sort_by_key(|w| w.0);
        for (_, p) in want {
            self.sent_chunks.insert(p);
            self.chunk_queue.push_back(p);
        }
        self.sent_chunks.retain(|&(x, z)| (x - cx).abs() <= keep && (z - cz).abs() <= keep);
        self.loaded_chunks.retain(|&(x, z)| (x - cx).abs() <= keep && (z - cz).abs() <= keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player() -> Player {
        Player::new("127.0.0.1:1".parse().unwrap(), 1, "Steve".into(), [0; 16], 0, false, vec![0; 8192], (0.5, 5.62, 0.5))
    }

    #[test]
    fn plan_radius_two_is_nearest_first() {
        let mut p = player();
        p.view_radius = 2;
        p.plan_chunks(&|_, _| true);
        assert_eq!(p.chunk_queue.len(), 13);
        assert_eq!(p.chunk_queue[0], (0, 0));
        p.plan_chunks(&|_, _| true); // nothing new
        assert_eq!(p.chunk_queue.len(), 13);
    }

    #[test]
    fn moving_queues_only_new_chunks() {
        let mut p = player();
        p.view_radius = 2;
        p.plan_chunks(&|_, _| true);
        p.chunk_queue.clear();
        p.x = 17.0; // chunk x = 1
        p.plan_chunks(&|_, _| true);
        assert!(!p.chunk_queue.is_empty() && p.chunk_queue.len() < 13);
        assert!(p.chunk_queue.iter().all(|&(x, _)| x >= 1));
    }

    #[test]
    fn far_move_discards_stale_pending_chunks() {
        let mut p = player();
        p.view_radius = 2;
        p.plan_chunks(&|_, _| true);
        assert_eq!(p.chunk_queue.len(), 13);

        // Simulate a teleport while the initial chunk queue is still pending.
        p.x = 1600.5; // chunk x = 100
        p.z = 0.5;
        p.plan_chunks(&|_, _| true);

        assert!(p.chunk_queue.iter().all(|&(x, z)| {
            (x - 100).abs() <= 2 && z.abs() <= 2
        }));
        assert!(p.chunk_queue.len() <= 13);
        assert!(p.sent_chunks.iter().all(|&(x, z)| {
            (x - 100).abs() <= 4 && z.abs() <= 4
        }));
    }

    #[test]
    fn inventory_stacks_and_overflow() {
        let mut p = player();
        assert_eq!(p.give(Slot::new(35, 40, 14)), 0);
        assert_eq!(p.give(Slot::new(35, 40, 14)), 0); // 64 + 16
        assert_eq!(p.inventory[0].count, 64);
        assert_eq!(p.inventory[1].count, 16);
        assert_eq!(p.give(Slot::new(268, 1, 0)), 0);
        assert_eq!(p.give(Slot::new(268, 1, 0)), 0); // tools do not stack
        assert!(p.inventory[2].id == 268 && p.inventory[3].id == 268);
        assert!(p.consume_one(35));
        assert_eq!(p.inventory[0].count, 63);

        // Metadata variants must not consume a different colour/material stack.
        p.inventory[0] = Slot::new(35, 2, 14);
        p.inventory[1] = Slot::new(35, 3, 1);
        assert!(p.consume_one_matching(35, 1));
        assert_eq!(p.inventory[0], Slot::new(35, 2, 14));
        assert_eq!(p.inventory[1], Slot::new(35, 2, 1));
        assert!(!p.consume_one_matching(35, 5));
        assert!(p.has_item_matching(35, 14));
        assert!(!p.has_item_matching(35, 5));

        p.clear_inventory();
        assert!(!p.has_item(35));
    }

    #[test]
    fn count_and_take_items() {
        let mut p = player();
        p.give(Slot::new(265, 64, 0));
        p.give(Slot::new(265, 10, 0));
        assert_eq!(p.count_item(265), 74);
        assert!(!p.take_items(265, 75));
        assert_eq!(p.count_item(265), 74);
        assert!(p.take_items(265, 70));
        assert_eq!(p.count_item(265), 4);
        assert!(p.take_items(265, 4));
        assert!(!p.has_item(265));
    }

    #[test]
    fn negative_coordinates_floor() {
        let mut p = player();
        p.x = -0.5;
        p.z = -16.5;
        assert_eq!(p.chunk_pos(), (-1, -2));
    }
}
