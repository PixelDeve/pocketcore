//! Shared game state and the gameplay operations used by both the packet
//! handlers (`game.rs`), the Bedwars layer (`bedwars/`) and the Luau bridge.
//!
//! Everything lives in one `Rc<RefCell<Core>>`; no borrow is ever held across
//! a call into Lua. Outgoing packets are queued in `out` and drained by the
//! game handler after each callback.
//!
//! Multi-world: `worlds` holds the original template map (id 0) plus one
//! forked copy per match. Players, dropped items and NPCs carry a world id and
//! every broadcast is scoped to one world, so matches never see each other.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;

use crate::binary::Vec3;
use crate::entity::player::{Player, PlayerState};
use crate::protocol::item::Slot;
use crate::protocol::packets::{self, adventure, status, LOCAL_PLAYER_EID, MOVE_RESET};
use crate::protocol::play::{self, event, text_type, BlockRecord, PlayerListEntry};
use crate::protocol::Quirks;
use crate::world::World;

pub type WorldId = u32;
/// The original template map. Never hosts a match; operators edit it here.
pub const TEMPLATE: WorldId = 0;

pub const EYE_HEIGHT: f32 = 1.62;
pub const MAX_HEALTH: f32 = 20.0;
const CHUNKS_PER_TICK: usize = 4;
/// PocketMine 1.6-era default: spawn after 56 chunks, or sooner when the world has fewer.
const SPAWN_CHUNK_THRESHOLD: usize = 56;
pub const NOON: i32 = 6000;
/// The clock is re-sent this often so every client stays on permanent day.
pub const TIME_REFRESH_TICKS: u64 = 100;
/// A held teleport or world switch is released after this long even if chunk bookkeeping never confirms it.
const HOLD_TIMEOUT_TICKS: u64 = 100;
const ITEM_LIFETIME_TICKS: u32 = 6000;
const ITEM_PICKUP_DELAY: u32 = 10;
/// If a client has not sent RequestChunkRadius this many ticks after login,
/// start streaming chunks anyway (2 s) so the loading screen cannot hang.
const RADIUS_FALLBACK_TICKS: u64 = 40;

#[derive(Clone, Debug)]
pub struct Pvp {
    pub knockback_h: f32,
    pub knockback_v: f32,
    pub attack_cooldown_ticks: u64,
    pub max_reach: f32,
}

pub struct GameConfig {
    pub max_players: usize,
    pub max_view: i32,
    pub gamemode: i32,
    pub quirks: Quirks,
    pub trace: bool,
    /// Placeholder slots before the real inventory in ContainerSetContent
    /// (hotbar links point at `offset + i`). 9 in PocketMine 0.14; 0 = plain.
    pub hotbar_offset: usize,
    pub pvp: Pvp,
    /// Movement / animation broadcasts only reach players within this many blocks.
    pub broadcast_radius: f32,
}

pub struct OutPacket {
    pub addr: SocketAddr,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy)]
pub struct DroppedItem {
    pub eid: i64,
    pub item: Slot,
    pub pos: Vec3,
    pub age: u32,
    pub world: WorldId,
}

/// A static fake player (shop keeper). Not in `players`, so it can never be
/// damaged or targeted.
pub struct Npc {
    pub eid: i64,
    pub uuid: [u8; 16],
    pub world: WorldId,
    /// Feet position.
    pub pos: Vec3,
    pub yaw: f32,
    pub name: String,
    pub skin: Box<[u8]>,
    pub hologram: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Ignored,
    Alive,
    Died,
}

#[derive(Default, Debug)]
pub struct ChunkTick {
    /// Finished the initial join: introduce them and fire join events.
    pub joined: Vec<i64>,
    /// Finished loading the world they were moved to.
    pub switched: Vec<i64>,
}

pub struct Core {
    pub cfg: GameConfig,
    pub worlds: HashMap<WorldId, World>,
    next_world: WorldId,
    pub players: HashMap<i64, Player>,
    pub by_addr: HashMap<SocketAddr, i64>,
    pub items: HashMap<i64, DroppedItem>,
    pub npcs: HashMap<i64, Npc>,
    pub next_eid: i64,
    pub tick: u64,
    pub out: Vec<OutPacket>,
    pub disconnects: Vec<SocketAddr>,
    /// Lower-case names; persisted in ops.txt / banned.txt / muted.txt.
    pub ops: HashSet<String>,
    pub banned: HashSet<String>,
    pub muted: HashSet<String>,
}

/// The local player is always addressed as -1 by its own client.
#[inline]
pub fn wire_eid(recipient: i64, target: i64) -> i64 {
    if recipient == target {
        LOCAL_PLAYER_EID
    } else {
        target
    }
}

/// Base melee damage by held item id (0.14 values; fist = 1).
pub fn weapon_damage(id: i16) -> f32 {
    match id {
        268 | 283 => 4.0,       // wood / gold sword
        272 => 5.0,             // stone sword
        267 => 6.0,             // iron sword
        276 => 7.0,             // diamond sword
        271 | 286 => 3.0,       // wood / gold axe
        275 => 4.0,             // stone axe
        258 => 5.0,             // iron axe
        279 => 6.0,             // diamond axe
        270 | 285 => 2.0,       // wood / gold pickaxe
        274 => 3.0,
        257 => 4.0,
        278 => 5.0,
        _ => 1.0,
    }
}

/// Blocks a placement may overwrite.
fn replaceable(id: u8) -> bool {
    matches!(id, 0 | 8 | 9 | 10 | 11 | 31 | 78)
}

/// Client-side adventure flags for a game mode.
///
/// `legacy` selects the 0.14 bit table (allow flight 0x80, no clip 0x100); otherwise the
/// 0.15+ table (allow flight 0x40, no clip 0x80, flying 0x200) is used.
///
/// Auto jump (legacy 0x40 / modern 0x20) is switched on for every game mode so players
/// step up single blocks without having to jump.
pub fn adventure_flags(mode: i32, legacy: bool) -> u32 {
    let (fly, noclip, auto_jump) = if legacy {
        (adventure::LEGACY_ALLOW_FLIGHT, adventure::LEGACY_NO_CLIP, adventure::LEGACY_AUTO_JUMP)
    } else {
        (adventure::ALLOW_FLIGHT, adventure::NO_CLIP, adventure::AUTO_JUMP)
    };
    let base = match mode {
        1 => fly,
        2 => adventure::IMMUTABLE_WORLD,
        3 => {
            let mut f = adventure::IMMUTABLE_WORLD
                | adventure::NO_PVP
                | adventure::NO_PVM
                | adventure::NO_MVP
                | fly
                | noclip;
            if !legacy {
                f |= adventure::FLYING;
            }
            f
        }
        _ => 0,
    };
    base | auto_jump
}

fn npc_uuid(eid: i64) -> [u8; 16] {
    let mut u = [0xA5u8; 16];
    u[8..].copy_from_slice(&eid.to_be_bytes());
    u
}

impl Core {
    pub fn new(cfg: GameConfig, world: World) -> Self {
        let mut worlds = HashMap::new();
        worlds.insert(TEMPLATE, world);
        Self {
            cfg,
            worlds,
            next_world: 1,
            players: HashMap::new(),
            by_addr: HashMap::new(),
            items: HashMap::new(),
            npcs: HashMap::new(),
            next_eid: 1,
            tick: 0,
            out: Vec::new(),
            disconnects: Vec::new(),
            ops: HashSet::new(),
            banned: HashSet::new(),
            muted: HashSet::new(),
        }
    }

    pub fn alloc_eid(&mut self) -> i64 {
        let e = self.next_eid;
        self.next_eid += 1;
        e
    }

    // ---- worlds -----------------------------------------------------------------------------

    /// Spawn point of a world (block coordinates).
    pub fn spawn_of(&self, wid: WorldId) -> (i32, i32, i32) {
        self.worlds.get(&wid).map_or((0, 4, 0), |w| w.spawn)
    }

    pub fn world_of(&self, eid: i64) -> WorldId {
        self.players.get(&eid).map_or(TEMPLATE, |p| p.world)
    }

    /// New match world: a copy-on-write fork of the template (cheap).
    pub fn fork_world(&mut self) -> WorldId {
        let id = self.next_world;
        self.next_world += 1;
        let copy = self.worlds.get(&TEMPLATE).expect("template world exists").fork();
        self.worlds.insert(id, copy);
        id
    }

    /// Removes a match world together with its items and NPCs. Refuses (false)
    /// while anybody is still in it or for the template.
    pub fn drop_world(&mut self, wid: WorldId) -> bool {
        if wid == TEMPLATE || self.players.values().any(|p| p.world == wid) {
            return false;
        }
        self.items.retain(|_, i| i.world != wid);
        self.npcs.retain(|_, n| n.world != wid);
        self.worlds.remove(&wid).is_some()
    }

    pub fn players_in(&self, wid: WorldId) -> Vec<i64> {
        let mut v: Vec<i64> = self.players.values().filter(|p| p.world == wid).map(|p| p.eid).collect();
        v.sort_unstable();
        v
    }

    pub fn get_block(&mut self, wid: WorldId, x: i32, y: i32, z: i32) -> (u8, u8) {
        self.worlds.get_mut(&wid).map_or((0, 0), |w| w.get_block(x, y, z))
    }

    // ---- sending ------------------------------------------------------------------------------

    pub fn push(&mut self, addr: SocketAddr, data: Vec<u8>) {
        self.out.push(OutPacket { addr, data });
    }

    pub fn send(&mut self, eid: i64, data: Vec<u8>) {
        if let Some(p) = self.players.get(&eid) {
            if !p.is_bot {
                self.out.push(OutPacket { addr: p.addr, data });
            }
        }
    }

    /// Real players of `wid` that are fully in the world (bots have no socket).
    fn viewer_addrs(&self, wid: WorldId, except: i64) -> Vec<(i64, SocketAddr)> {
        self.players
            .values()
            .filter(|p| p.world == wid && p.eid != except && !p.is_bot && !p.switching && p.state == PlayerState::Spawned)
            .map(|p| (p.eid, p.addr))
            .collect()
    }

    /// To every fully spawned player of one world.
    pub fn broadcast(&mut self, wid: WorldId, data: &[u8]) {
        for (_, addr) in self.viewer_addrs(wid, i64::MIN) {
            self.out.push(OutPacket { addr, data: data.to_vec() });
        }
    }

    pub fn broadcast_except(&mut self, wid: WorldId, except: i64, data: &[u8]) {
        for (_, addr) in self.viewer_addrs(wid, except) {
            self.out.push(OutPacket { addr, data: data.to_vec() });
        }
    }

    pub fn broadcast_near(&mut self, wid: WorldId, origin: Vec3, except: i64, data: &[u8]) {
        let r2 = self.cfg.broadcast_radius * self.cfg.broadcast_radius;
        for p in self.players.values() {
            if p.world != wid || p.eid == except || p.is_bot || p.switching || p.state != PlayerState::Spawned {
                continue;
            }
            let (dx, dz) = (p.x - origin.x, p.z - origin.z);
            if dx * dx + dz * dz <= r2 {
                self.out.push(OutPacket { addr: p.addr, data: data.to_vec() });
            }
        }
    }

    /// Builds one packet per recipient (for packets that contain an entity id).
    pub fn broadcast_with(&mut self, wid: WorldId, mut build: impl FnMut(i64) -> Vec<u8>) {
        for (eid, addr) in self.viewer_addrs(wid, i64::MIN) {
            self.out.push(OutPacket { addr, data: build(eid) });
        }
    }

    pub(crate) fn send_to_chunk_viewers(&mut self, wid: WorldId, cx: i32, cz: i32, data: Vec<u8>) {
        for p in self.players.values() {
            if p.world == wid && !p.is_bot && p.state != PlayerState::LoggedIn && p.sent_chunks.contains(&(cx, cz)) {
                self.out.push(OutPacket { addr: p.addr, data: data.clone() });
            }
        }
    }

    pub fn send_text(&mut self, eid: i64, kind: u8, source: &str, msg: &str) {
        let d = play::text(kind, source, msg);
        self.send(eid, d);
    }

    /// Chat / announcements reach one world only.
    pub fn broadcast_text(&mut self, wid: WorldId, kind: u8, source: &str, msg: &str) {
        let d = play::text(kind, source, msg);
        self.broadcast(wid, &d);
    }

    /// Server-wide announcement (every world).
    pub fn broadcast_all_text(&mut self, kind: u8, msg: &str) {
        let d = play::text(kind, "", msg);
        let all: Vec<SocketAddr> = self.players.values().filter(|p| !p.is_bot && p.state == PlayerState::Spawned).map(|p| p.addr).collect();
        for addr in all {
            self.out.push(OutPacket { addr, data: d.clone() });
        }
    }

    pub fn kick(&mut self, eid: i64, reason: &str) {
        let Some(addr) = self.players.get(&eid).map(|p| p.addr) else { return };
        self.out.push(OutPacket { addr, data: packets::disconnect(reason) });
        self.disconnects.push(addr);
    }

    // ---- join / leave -----------------------------------------------------------------------------

    fn introduce(&mut self, viewer: i64, target: i64) {
        let (Some(v), Some(t)) = (self.players.get(&viewer), self.players.get(&target)) else { return };
        if v.is_bot || t.hidden {
            return;
        }
        let list = play::player_list_add(&[PlayerListEntry {
            uuid: &t.uuid,
            eid: t.eid,
            name: &t.name,
            slim: t.skin_slim,
            skin: &t.skin,
        }]);
        let pos = Vec3 { x: t.x, y: t.y - EYE_HEIGHT, z: t.z };
        let add = play::add_player(&t.uuid, &t.name, t.eid, pos, t.yaw, t.pitch, &t.held_item());
        let (armor, held, teid) = (t.armor, t.held_item(), t.eid);
        let addr = v.addr;
        self.out.push(OutPacket { addr, data: list });
        self.out.push(OutPacket { addr, data: add });
        if armor.iter().any(|a| !a.is_empty()) {
            self.out.push(OutPacket { addr, data: play::mob_armor_equipment(teid, &armor) });
        }
        if !held.is_empty() {
            let off = self.cfg.hotbar_offset as u8;
            let idx = self.players.get(&target).map_or(0, |t| t.held);
            self.out.push(OutPacket { addr, data: play::mob_equipment(teid, &held, idx + off, idx) });
        }
    }

    fn show_npc(&mut self, viewer: i64, npc: i64) {
        let (Some(v), Some(n)) = (self.players.get(&viewer), self.npcs.get(&npc)) else { return };
        if v.is_bot {
            return;
        }
        let list = play::player_list_add(&[PlayerListEntry { uuid: &n.uuid, eid: n.eid, name: &n.name, slim: false, skin: &n.skin }]);
        let add = if n.hologram {
            play::add_hologram(&n.uuid, &n.name, n.eid, n.pos)
        } else {
            play::add_player(&n.uuid, &n.name, n.eid, n.pos, n.yaw, 0.0, &Slot::EMPTY)
        };
        let addr = v.addr;
        self.out.push(OutPacket { addr, data: list });
        self.out.push(OutPacket { addr, data: add });
        if n.hologram {
            // Keep the fake player out of the tab list after its skin/entity data
            // has been introduced; the entity and its name tag remain in-world.
            self.out.push(OutPacket { addr, data: play::player_list_remove(&[n.uuid]) });
        }
    }

    /// Shows everything in the player's world to them and them to everyone
    /// there. Used after the first join and after every world switch.
    pub fn enter_world_view(&mut self, eid: i64) {
        let wid = self.world_of(eid);
        let others: Vec<i64> = self
            .players
            .values()
            .filter(|p| p.eid != eid && p.world == wid && p.state == PlayerState::Spawned && !p.switching)
            .map(|p| p.eid)
            .collect();
        for o in others {
            self.introduce(eid, o);
            self.introduce(o, eid);
        }
        let npcs: Vec<i64> = self.npcs.values().filter(|n| n.world == wid).map(|n| n.eid).collect();
        for n in npcs {
            self.show_npc(eid, n);
        }
        let items: Vec<DroppedItem> = self.items.values().filter(|i| i.world == wid).copied().collect();
        for it in items {
            let d = play::add_item_entity(it.eid, &it.item, it.pos, Vec3::default());
            self.send(eid, d);
        }
        let hp = self.players.get(&eid).map_or(MAX_HEALTH, |p| p.health);
        self.send(eid, play::set_health(hp.ceil() as i32));
        self.send_inventory(eid);
    }

    /// Called once a player's initial chunks are out.
    pub fn finish_join(&mut self, eid: i64) {
        if self.players.contains_key(&eid) {
            self.send_adventure(eid);
            self.send_creative(eid);
        }
        self.enter_world_view(eid);
        if let Some((name, wid)) = self.players.get(&eid).map(|p| (p.name.clone(), p.world)) {
            log::debug!("{name} (eid {eid}) finished join sequence");
            self.broadcast_text(wid, text_type::RAW, "", &format!("\u{a7}e{name} joined the game"));
        }
    }

    pub fn remove_player(&mut self, eid: i64) {
        let Some(p) = self.players.remove(&eid) else { return };
        self.by_addr.remove(&p.addr);
        if p.state == PlayerState::Spawned && !p.hidden {
            self.broadcast(p.world, &play::remove_player(eid, &p.uuid));
            self.broadcast(p.world, &play::player_list_remove(&[p.uuid]));
            if !p.is_bot {
                self.broadcast_text(p.world, text_type::RAW, "", &format!("\u{a7}e{} left the game", p.name));
            }
        }
    }

    /// Streams queued chunks; reports players that finished loading.
    pub fn tick_chunks(&mut self) -> ChunkTick {
        let mut res = ChunkTick::default();
        let mut deferred_entity_moves: Vec<(WorldId, i64, Vec<u8>)> = Vec::new();
        let Self { players, worlds, out, tick, cfg, .. } = self;
        for p in players.values_mut() {
            if p.is_bot {
                continue;
            }
            let Some(world) = worlds.get_mut(&p.world) else { continue };
            if p.state == PlayerState::LoggedIn {
                // Some clients never send RequestChunkRadius; stream with the
                // default radius after a short grace period instead of hanging.
                if *tick < p.login_tick + RADIUS_FALLBACK_TICKS {
                    continue;
                }
                log::info!(
                    "{}: no chunk radius request after {} ticks, streaming with radius {}",
                    p.name,
                    RADIUS_FALLBACK_TICKS,
                    cfg.max_view
                );
                p.view_radius = cfg.max_view;
                p.state = PlayerState::Loading;
                let w: &World = &*world;
                p.plan_chunks(&|x, z| w.in_bounds(x, z));
                p.initial_chunks_sent = 0;
                p.initial_chunks_needed = p.chunk_queue.len().min(SPAWN_CHUNK_THRESHOLD);
                out.push(OutPacket { addr: p.addr, data: packets::chunk_radius_update(cfg.max_view) });
            }
            let mut sent = 0;
            while sent < CHUNKS_PER_TICK {
                let Some((cx, cz)) = p.chunk_queue.pop_front() else { break };
                if let Some(pkt) = world.chunk_packet(cx, cz) {
                    out.push(OutPacket { addr: p.addr, data: pkt.to_vec() });
                    p.loaded_chunks.insert((cx, cz));
                    if p.state == PlayerState::Loading {
                        p.initial_chunks_sent = p.initial_chunks_sent.saturating_add(1);
                    }
                    sent += 1;
                } else {
                    // A failed chunk build must not remain marked as already sent.
                    p.sent_chunks.remove(&(cx, cz));
                    log::warn!("{}: chunk ({cx}, {cz}) could not be built; it will be retried", p.name);
                }
            }
            if p.state == PlayerState::Loading
                && (p.chunk_queue.is_empty()
                    || (p.initial_chunks_needed > 0 && p.initial_chunks_sent >= p.initial_chunks_needed)) {
                log::info!("{}: chunks sent, sending PLAYER_SPAWN", p.name);
                p.state = PlayerState::Spawned;
                if cfg.quirks.spawn_respawn {
                    // PocketMine doFirstSpawn: settings, time, Respawn, then PLAYER_SPAWN.
                    let flags = adventure_flags(p.gamemode, cfg.quirks.legacy_adventure_flags);
                    let perm = cfg.quirks.adventure_permission.then_some(2);
                    out.push(OutPacket { addr: p.addr, data: packets::adventure_settings_ext(flags, perm) });
                    out.push(OutPacket { addr: p.addr, data: packets::set_time(NOON, false) });
                    out.push(OutPacket { addr: p.addr, data: packets::respawn(Vec3 { x: p.x, y: p.y, z: p.z }) });
                }
                out.push(OutPacket { addr: p.addr, data: play::set_health(MAX_HEALTH as i32) });
                out.push(OutPacket { addr: p.addr, data: packets::play_status(status::PLAYER_SPAWN) });
                out.push(OutPacket { addr: p.addr, data: packets::set_time(NOON, false) });
                res.joined.push(p.eid);
            } else {
                let (tcx, tcz) = p.chunk_pos();
                let destination_ready = (-1..=1).all(|dz| (-1..=1).all(|dx| {
                    let chunk = (tcx + dx, tcz + dz);
                    !world.in_bounds(chunk.0, chunk.1) || p.loaded_chunks.contains(&chunk)
                }));
                // PocketMine teleports once the destination area is ready; the rest of
                // the view-distance queue continues streaming after the player arrives.
                let timed_out = *tick >= p.hold_since + HOLD_TIMEOUT_TICKS;
                if (p.switching || p.tp_wait) && !destination_ready && timed_out {
                    log::warn!("{}: destination chunks never confirmed, releasing the held teleport anyway", p.name);
                }
                if p.switching && (destination_ready || timed_out) {
                    p.switching = false;
                    let pos = Vec3 { x: p.x, y: p.y, z: p.z };
                    out.push(OutPacket { addr: p.addr, data: packets::move_player(p.local_eid, pos, p.yaw, p.yaw, p.pitch, MOVE_RESET, true) });
                    out.push(OutPacket { addr: p.addr, data: packets::set_time(NOON, false) });
                    p.tp_target = Some((pos, 0));
                    res.switched.push(p.eid);
                } else if p.tp_wait && p.state == PlayerState::Spawned && (destination_ready || timed_out) {
                    p.tp_wait = false;
                    let pos = Vec3 { x: p.x, y: p.y, z: p.z };
                    log::info!("{}: destination 3x3 chunks streamed, moving to ({:.1}, {:.1}, {:.1})", p.name, pos.x, pos.y, pos.z);
                    p.tp_target = Some((pos, 0));
                    out.push(OutPacket { addr: p.addr, data: packets::move_player(p.local_eid, pos, p.yaw, p.yaw, p.pitch, MOVE_RESET, true) });
                    if p.tp_broadcast_pending {
                        deferred_entity_moves.push((p.world, p.eid, packets::move_player(p.eid, pos, p.yaw, p.yaw, p.pitch, MOVE_RESET, true)));
                        p.tp_broadcast_pending = false;
                    }
                }
            }
        }
        // Match GenisysPro 0.14.x checkTeleportPosition(): release a teleport
        // as soon as the destination 3x3 is ready. Do not wait for the entire
        // view-distance queue; distant scenery may still be streaming.
        for (wid, eid, packet) in deferred_entity_moves {
            for other in players.values() {
                if other.world == wid && other.eid != eid && !other.is_bot && !other.switching && other.state == PlayerState::Spawned {
                    out.push(OutPacket { addr: other.addr, data: packet.clone() });
                }
            }
        }
        res
    }

    /// Re-sends the clock so every client stays on permanent day.
    pub fn tick_time(&mut self) {
        if self.tick % TIME_REFRESH_TICKS != 0 {
            return;
        }
        let d = packets::set_time(NOON, false);
        let addrs: Vec<SocketAddr> = self.players.values().filter(|p| !p.is_bot && p.state == PlayerState::Spawned).map(|p| p.addr).collect();
        for addr in addrs {
            self.out.push(OutPacket { addr, data: d.clone() });
        }
    }

    // ---- movement -----------------------------------------------------------------------------------

    /// Teleports to feet position (x, y, z) inside the player's current world.
    ///
    /// Like GenisysPro's checkTeleportPosition(), a client teleport is not
    /// issued until the destination chunk and its eight neighbours have been
    /// sent. This also applies to short teleports near a chunk edge.
    pub fn teleport(&mut self, eid: i64, x: f32, y: f32, z: f32) {
        let Some(p) = self.players.get_mut(&eid) else { return };
        p.x = x;
        p.y = y + EYE_HEIGHT;
        p.z = z;
        p.fall_distance = 0.0;
        let wid = p.world;
        let pos = Vec3 { x, y: y + EYE_HEIGHT, z };
        let (yaw, pitch, addr, bot, state, local) = (p.yaw, p.pitch, p.addr, p.is_bot, p.state, p.local_eid);

        if bot {
            p.tp_wait = false;
            p.tp_target = None;
            p.tp_broadcast_pending = false;
            let others = packets::move_player(eid, pos, yaw, yaw, pitch, MOVE_RESET, true);
            self.broadcast(wid, &others);
            return;
        }

        if state != PlayerState::LoggedIn {
            if let Some(world) = self.worlds.get(&wid) {
                p.plan_chunks(&|cx, cz| world.in_bounds(cx, cz));
                if state == PlayerState::Loading {
                    p.initial_chunks_sent = 0;
                    p.initial_chunks_needed = p.chunk_queue.len().min(SPAWN_CHUNK_THRESHOLD);
                }

                // GenisysPro's 0.14.x checkTeleportPosition() waits until the
                // destination's complete 3x3 chunk area is in usedChunks.
                // plan_chunks() normally deduplicates by sent_chunks, but that
                // set can contain a chunk which was queued earlier and later
                // dropped from the queue. Force every missing destination
                // chunk back into the front of the stream so a teleport can
                // never wait forever on a stale sent_chunks entry.
                let (cx, cz) = p.chunk_pos();
                let mut destination = Vec::with_capacity(9);
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        let chunk = (cx + dx, cz + dz);
                        if world.in_bounds(chunk.0, chunk.1) {
                            destination.push(chunk);
                        }
                    }
                }
                let loaded_chunks = &p.loaded_chunks;
                p.chunk_queue.retain(|chunk| {
                    !destination.contains(chunk) || loaded_chunks.contains(chunk)
                });
                for chunk in destination.into_iter().rev() {
                    if !p.loaded_chunks.contains(&chunk) {
                        p.sent_chunks.insert(chunk);
                        p.chunk_queue.push_front(chunk);
                    }
                }
            }
        }

        let ready = if state == PlayerState::Spawned {
            self.worlds.get(&wid).map_or(false, |world| {
                let (cx, cz) = p.chunk_pos();
                (-1..=1).all(|dz| (-1..=1).all(|dx| {
                    let chunk = (cx + dx, cz + dz);
                    !world.in_bounds(chunk.0, chunk.1) || p.loaded_chunks.contains(&chunk)
                }))
            })
        } else {
            false
        };

        // Keep the teleport pending even if match startup happens while the
        // client is still loading its initial chunks. tick_chunks() will apply
        // it immediately after PLAYER_SPAWN and destination chunk delivery.
        p.hold_since = self.tick;
        p.tp_wait = !ready;
        p.tp_broadcast_pending = state == PlayerState::Spawned && !ready;
        p.tp_target = if ready { Some((pos, 0)) } else { None };

        if ready {
            self.out.push(OutPacket {
                addr,
                data: packets::move_player(local, pos, yaw, yaw, pitch, MOVE_RESET, true),
            });
        } else if state == PlayerState::Spawned {
            log::info!(
                "{}: holding teleport until destination chunks arrive ({:.1}, {:.1}, {:.1})",
                p.name, x, y, z
            );
        }

        if ready {
            let others = packets::move_player(eid, pos, yaw, yaw, pitch, MOVE_RESET, true);
            self.broadcast_except(wid, eid, &others);
        }
    }

    /// Teleport with explicit facing (same world).
    pub fn teleport_facing(&mut self, eid: i64, x: f32, y: f32, z: f32, yaw: f32, pitch: f32) {
        if let Some(p) = self.players.get_mut(&eid) {
            p.yaw = yaw;
            p.pitch = pitch;
        }
        self.teleport(eid, x, y, z);
    }

    /// Moves a player to feet position (x, y, z) in world `wid`. Between
    /// worlds a connected player is frozen while the new world's chunks
    /// stream, then teleported (see `tick_chunks`). Bots switch instantly.
    pub fn move_to_world(&mut self, eid: i64, wid: WorldId, x: f32, y: f32, z: f32, yaw: f32, pitch: f32) -> bool {
        if !self.worlds.contains_key(&wid) {
            return false;
        }
        let Some(p) = self.players.get(&eid) else { return false };
        let (old, state, bot, hidden, uuid) = (p.world, p.state, p.is_bot, p.hidden, p.uuid);
        if old == wid {
            self.teleport_facing(eid, x, y, z, yaw, pitch);
            return true;
        }
        if state == PlayerState::Spawned && !hidden {
            self.broadcast_except(old, eid, &play::remove_player(eid, &uuid));
            self.broadcast_except(old, eid, &play::player_list_remove(&[uuid]));
        }
        if !bot && state == PlayerState::Spawned {
            self.forget_world(eid, old);
        }
        let world = &self.worlds[&wid];
        let Some(p) = self.players.get_mut(&eid) else { return false };
        p.world = wid;
        p.x = x;
        p.y = y + EYE_HEIGHT;
        p.z = z;
        p.yaw = yaw;
        p.pitch = pitch;
        p.dead = false;
        p.fall_distance = 0.0;
        if bot {
            self.enter_world_view_bot(eid);
            return true;
        }
        p.sent_chunks.clear();
        p.loaded_chunks.clear();
        p.chunk_queue.clear();
        p.initial_chunks_sent = 0;
        p.initial_chunks_needed = 0;
        p.tp_wait = false;
        p.tp_target = None;
        p.tp_broadcast_pending = false;
        if state == PlayerState::Spawned {
            p.switching = true;
            p.hold_since = self.tick;
        }
        if state != PlayerState::LoggedIn {
            p.plan_chunks(&|cx, cz| world.in_bounds(cx, cz));
            if state == PlayerState::Loading {
                p.initial_chunks_sent = 0;
                p.initial_chunks_needed = p.chunk_queue.len().min(SPAWN_CHUNK_THRESHOLD);
            }
        }
        true
    }

    /// Bots have no client; they only need to be shown to the new world's humans.
    fn enter_world_view_bot(&mut self, eid: i64) {
        let wid = self.world_of(eid);
        let viewers: Vec<i64> = self.viewer_addrs(wid, eid).into_iter().map(|(e, _)| e).collect();
        for v in viewers {
            self.introduce(v, eid);
        }
    }

    /// Tells one client to drop every entity of the world it is leaving.
    fn forget_world(&mut self, eid: i64, old: WorldId) {
        let mut packs: Vec<Vec<u8>> = Vec::new();
        for o in self.players.values().filter(|o| o.eid != eid && o.world == old && o.state == PlayerState::Spawned && !o.hidden) {
            packs.push(play::remove_player(o.eid, &o.uuid));
            packs.push(play::player_list_remove(&[o.uuid]));
        }
        for n in self.npcs.values().filter(|n| n.world == old) {
            packs.push(play::remove_player(n.eid, &n.uuid));
            packs.push(play::player_list_remove(&[n.uuid]));
        }
        for i in self.items.values().filter(|i| i.world == old) {
            packs.push(play::remove_entity(i.eid));
        }
        for d in packs {
            self.send(eid, d);
        }
    }

    pub fn set_gamemode(&mut self, eid: i64, mode: i32) {
        let Some(p) = self.players.get_mut(&eid) else { return };
        let was_hidden = p.hidden;
        p.gamemode = mode;
        p.hidden = mode == 3;
        let (wid, uuid, spawned) = (p.world, p.uuid, p.state == PlayerState::Spawned);
        p.fall_distance = 0.0;
        self.send(eid, play::set_player_gametype(mode));
        self.send_adventure(eid);
        self.send_creative(eid);
        if spawned && mode == 3 && !was_hidden {
            self.broadcast_except(wid, eid, &play::remove_player(eid, &uuid));
            self.broadcast_except(wid, eid, &play::player_list_remove(&[uuid]));
        } else if spawned && mode != 3 && was_hidden {
            let viewers: Vec<i64> = self.viewer_addrs(wid, eid).into_iter().map(|(e, _)| e).collect();
            for v in viewers {
                self.introduce(v, eid);
            }
        }
    }

    /// Fills the client's creative menu (empty for spectators, who have no menu).
    pub fn send_creative(&mut self, eid: i64) {
        let Some(p) = self.players.get(&eid) else { return };
        if p.is_bot {
            return;
        }
        let items = if p.gamemode == 3 { Vec::new() } else { crate::creative::items() };
        self.send(eid, play::container_set_content(play::WINDOW_CREATIVE, &items, &[]));
    }

    /// Creative players move items from the menu into their inventory client-side
    /// and tell us afterwards. True if the change was accepted (no re-sync needed).
    pub fn creative_set_slot(&mut self, eid: i64, win: u8, slot: i16, item: Slot) -> bool {
        let off = self.cfg.hotbar_offset as i16;
        let Some(p) = self.players.get_mut(&eid) else { return false };
        if p.gamemode != 1 {
            return false;
        }
        log::info!("{}: creative slot update window={win} slot={slot} item={item:?}", p.name);
        if win != play::WINDOW_INVENTORY {
            // the creative menu itself (or anything else): nothing to store, never re-sync it away
            return win == play::WINDOW_CREATIVE;
        }
        let idx = if slot >= off { (slot - off) as usize } else if slot >= 0 { slot as usize } else { return true };
        if idx >= p.inventory.len() {
            return true;
        }
        p.inventory[idx] = if item.is_empty() {
            Slot::EMPTY
        } else {
            Slot { id: item.id, count: item.count.min(64), damage: item.damage }
        };
        true
    }

    /// Sends the MCPE 0.14.x game-mode flags packet (flags only in this protocol).
    pub fn send_adventure(&mut self, eid: i64) {
        let Some(p) = self.players.get(&eid) else { return };
        let flags = adventure_flags(p.gamemode, self.cfg.quirks.legacy_adventure_flags);
        self.send(eid, packets::adventure_settings_ext(flags, self.cfg.quirks.adventure_permission.then_some(2)));
    }

    /// Called for every movement packet of a survival player. Returns the fall
    /// damage to apply when the player just landed.
    pub fn track_fall(&mut self, eid: i64, prev_feet_y: f32, feet: (f32, f32, f32), on_ground: bool) -> Option<f32> {
        if let Some(p) = self.players.get_mut(&eid) {
            p.fall_speed = (prev_feet_y - feet.1).max(0.0);
        }
        let (wid, gm, dead) = {
            let p = self.players.get(&eid)?;
            (p.world, p.gamemode, p.dead)
        };
        if gm != 0 || dead {
            self.players.get_mut(&eid)?.fall_distance = 0.0;
            return None;
        }
        let (bx, by, bz) = (feet.0.floor() as i32, feet.1.floor() as i32, feet.2.floor() as i32);
        // water, lava, cobweb, ladder and vines break a fall
        let soft = [by, by + 1].iter().any(|&y| matches!(self.get_block(wid, bx, y, bz).0, 8..=11 | 30 | 65 | 106));
        let p = self.players.get_mut(&eid)?;
        if soft {
            p.fall_distance = 0.0;
            return None;
        }
        let dy = feet.1 - prev_feet_y;
        if on_ground {
            let fallen = p.fall_distance;
            p.fall_distance = 0.0;
            if fallen > 3.0 {
                return Some((fallen - 3.0).ceil());
            }
        } else if dy < 0.0 {
            p.fall_distance += -dy;
        }
        None
    }

    // ---- NPCs ---------------------------------------------------------------------------------------------

    pub fn add_npc(&mut self, wid: WorldId, pos: Vec3, yaw: f32, name: &str, skin: Vec<u8>) -> i64 {
        let eid = self.alloc_eid();
        let npc = Npc { eid, uuid: npc_uuid(eid), world: wid, pos, yaw, name: name.to_owned(), skin: skin.into_boxed_slice(), hologram: false };
        self.npcs.insert(eid, npc);
        let viewers: Vec<i64> = self.viewer_addrs(wid, i64::MIN).into_iter().map(|(e, _)| e).collect();
        for v in viewers {
            self.show_npc(v, eid);
        }
        eid
    }

    /// Adds an invisible fake-player entity with a visible name tag.
    pub fn add_hologram(&mut self, wid: WorldId, pos: Vec3, name: &str) -> i64 {
        let eid = self.alloc_eid();
        let npc = Npc {
            eid,
            uuid: npc_uuid(eid),
            world: wid,
            pos,
            yaw: 0.0,
            name: name.to_owned(),
            skin: vec![0; 8192].into_boxed_slice(),
            hologram: true,
        };
        self.npcs.insert(eid, npc);
        let viewers: Vec<i64> = self.viewer_addrs(wid, i64::MIN).into_iter().map(|(e, _)| e).collect();
        for viewer in viewers {
            self.show_npc(viewer, eid);
        }
        eid
    }

    /// Updates a fake player's visible name tag without despawning the entity.
    pub fn set_npc_name(&mut self, eid: i64, name: &str) {
        let Some(npc) = self.npcs.get_mut(&eid) else { return };
        if npc.name == name {
            return;
        }
        npc.name = name.to_owned();
        let wid = npc.world;
        let packet = play::set_entity_name_tag(eid, name);
        self.broadcast(wid, &packet);
    }

    pub fn remove_npc(&mut self, eid: i64) {
        let Some(n) = self.npcs.remove(&eid) else { return };
        self.broadcast(n.world, &play::remove_player(n.eid, &n.uuid));
        self.broadcast(n.world, &play::player_list_remove(&[n.uuid]));
    }

    pub fn clear_npcs(&mut self, wid: WorldId) {
        let ids: Vec<i64> = self.npcs.values().filter(|n| n.world == wid).map(|n| n.eid).collect();
        for id in ids {
            self.remove_npc(id);
        }
    }

    // ---- bots -------------------------------------------------------------------------------------------------

    /// Adds a server-controlled player to a world and shows it to the humans there.
    pub fn add_bot(&mut self, wid: WorldId, name: &str, skin: Vec<u8>, x: f32, y: f32, z: f32, yaw: f32) -> i64 {
        let eid = self.alloc_eid();
        let addr = SocketAddr::from(([0, 0, 0, 0], 0));
        let mut uuid = [0xB0u8; 16];
        uuid[8..].copy_from_slice(&eid.to_be_bytes());
        let mut p = Player::new(addr, eid, name.to_owned(), uuid, 0, false, skin, (x, y + EYE_HEIGHT, z));
        p.state = PlayerState::Spawned;
        p.is_bot = true;
        p.world = wid;
        p.yaw = yaw;
        self.players.insert(eid, p);
        self.enter_world_view_bot(eid);
        eid
    }

    // ---- inventory ------------------------------------------------------------------------------------------

    pub fn send_inventory(&mut self, eid: i64) {
        let off = self.cfg.hotbar_offset;
        let Some(p) = self.players.get(&eid) else { return };
        if p.is_bot {
            self.send_equipment(eid); // bots have no client, but others must see what they hold
            return;
        }
        let mut slots = if self.cfg.quirks.inventory_placeholders { vec![Slot::EMPTY; off] } else { Vec::new() };
        slots.extend_from_slice(&p.inventory);
        let hotbar: Vec<i32> = (0..9).map(|i| (off + p.hotbar[i] as usize) as i32).collect();
        let data = play::container_set_content(play::WINDOW_INVENTORY, &slots, &hotbar);
        self.send(eid, data);
        self.send_equipment(eid);
    }

    pub fn send_equipment(&mut self, eid: i64) {
        let off = self.cfg.hotbar_offset;
        let Some(p) = self.players.get(&eid) else { return };
        let (held, idx, wid) = (p.held_item(), p.held, p.world);
        let pos = Vec3 { x: p.x, y: p.y, z: p.z };
        let slot = p.held_slot() as u8 + off as u8;
        self.send(eid, play::mob_equipment(self.players.get(&eid).map_or(LOCAL_PLAYER_EID, |p| p.local_eid), &held, slot, idx));
        let others = play::mob_equipment(eid, &held, slot, idx);
        self.broadcast_near(wid, pos, eid, &others);
    }

    /// Returns the count that did not fit.
    pub fn give_item(&mut self, eid: i64, id: i16, count: u8, damage: i16) -> u8 {
        let Some(p) = self.players.get_mut(&eid) else { return count };
        let left = p.give(Slot::new(id, count, damage));
        self.send_inventory(eid);
        left
    }

    pub fn clear_inventory(&mut self, eid: i64) {
        let Some(p) = self.players.get_mut(&eid) else { return };
        p.clear_inventory();
        self.send_inventory(eid);
    }

    /// `ids` = helmet, chestplate, leggings, boots (0 = none).
    pub fn set_armor(&mut self, eid: i64, ids: [i16; 4]) {
        let Some(p) = self.players.get_mut(&eid) else { return };
        p.armor = ids.map(|id| if id > 0 { Slot::new(id, 1, 0) } else { Slot::EMPTY });
        let (armor, pos, wid) = (p.armor, Vec3 { x: p.x, y: p.y, z: p.z }, p.world);
        self.send(eid, play::mob_armor_equipment(self.players.get(&eid).map_or(LOCAL_PLAYER_EID, |p| p.local_eid), &armor));
        self.send(eid, play::container_set_content(play::WINDOW_ARMOR, &armor, &[]));
        let others = play::mob_armor_equipment(eid, &armor);
        self.broadcast_near(wid, pos, eid, &others);
    }

    // ---- world --------------------------------------------------------------------------------------------------

    /// Sets a block and tells every player of that world who has the chunk.
    pub fn set_block(&mut self, wid: WorldId, x: i32, y: i32, z: i32, id: u8, meta: u8) -> bool {
        let Some(w) = self.worlds.get_mut(&wid) else { return false };
        if !w.set_block(x, y, z, id, meta) {
            return false;
        }
        let d = play::update_block(&[BlockRecord { x, y, z, id, meta: meta & 0x0f }]);
        self.send_to_chunk_viewers(wid, x >> 4, z >> 4, d);
        true
    }

    /// Re-sends the real block at a position to one player (reverts a
    /// client-side prediction).
    pub fn resync_block(&mut self, eid: i64, x: i32, y: i32, z: i32) {
        let wid = self.world_of(eid);
        let (id, meta) = self.get_block(wid, x, y, z);
        self.send(eid, play::update_block(&[BlockRecord { x, y, z, id, meta }]));
    }

    pub fn spawn_item(&mut self, wid: WorldId, pos: Vec3, item: Slot) -> i64 {
        let eid = self.alloc_eid();
        self.items.insert(eid, DroppedItem { eid, item, pos, age: 0, world: wid });
        let d = play::add_item_entity(eid, &item, pos, Vec3::default());
        self.broadcast(wid, &d);
        eid
    }

    pub fn clear_items(&mut self, wid: WorldId) {
        let ids: Vec<i64> = self.items.values().filter(|i| i.world == wid).map(|i| i.eid).collect();
        for id in ids {
            self.items.remove(&id);
            self.broadcast(wid, &play::remove_entity(id));
        }
    }

    /// Dropped items of this world within `radius` of a point that have this id.
    pub fn count_items_near(&self, wid: WorldId, pos: Vec3, radius: f32, id: i16) -> u32 {
        self.items
            .values()
            .filter(|i| i.world == wid && i.item.id == id)
            .filter(|i| {
                let (dx, dy, dz) = (i.pos.x - pos.x, i.pos.y - pos.y, i.pos.z - pos.z);
                dx * dx + dy * dy + dz * dz <= radius * radius
            })
            .map(|i| i.item.count as u32)
            .sum()
    }

    /// Restores all modified blocks, clears dropped items, tells viewers.
    pub fn reset_arena(&mut self, wid: WorldId) -> usize {
        let Some(w) = self.worlds.get_mut(&wid) else { return 0 };
        let restored = w.reset_arena();
        let mut by_chunk: HashMap<(i32, i32), Vec<BlockRecord>> = HashMap::new();
        for &(x, y, z) in &restored {
            let (id, meta) = self.worlds.get_mut(&wid).map_or((0, 0), |w| w.get_block(x, y, z));
            by_chunk.entry((x >> 4, z >> 4)).or_default().push(BlockRecord { x, y, z, id, meta });
        }
        for ((cx, cz), recs) in by_chunk {
            for part in recs.chunks(256) {
                self.send_to_chunk_viewers(wid, cx, cz, play::update_block(part));
            }
        }
        self.clear_items(wid);
        restored.len()
    }

    /// Distance check from a player's eye to the centre of a block.
    pub fn in_reach(&self, eid: i64, x: i32, y: i32, z: i32) -> bool {
        let Some(p) = self.players.get(&eid) else { return false };
        let reach = if p.gamemode == 1 { 13.0 } else { self.cfg.pvp.max_reach + 1.5 };
        let (dx, dy, dz) = (p.x - (x as f32 + 0.5), p.y - (y as f32 + 0.5), p.z - (z as f32 + 0.5));
        dx * dx + dy * dy + dz * dz <= reach * reach
    }

    /// True if a (non-spectator) player's body overlaps the block cell, so a block
    /// must not be placed there.
    pub fn player_in_cell(&self, wid: WorldId, x: i32, y: i32, z: i32) -> bool {
        const HALF_W: f32 = 0.3;
        const HEIGHT: f32 = 1.8;
        const EPS: f32 = 0.001;
        self.players.values().any(|p| {
            if p.world != wid || p.hidden || p.gamemode == 3 || p.state != PlayerState::Spawned {
                return false;
            }
            let feet = p.y - EYE_HEIGHT;
            p.x - HALF_W < (x + 1) as f32 - EPS
                && p.x + HALF_W > x as f32 + EPS
                && feet < (y + 1) as f32 - EPS
                && feet + HEIGHT > y as f32 + EPS
                && p.z - HALF_W < (z + 1) as f32 - EPS
                && p.z + HALF_W > z as f32 + EPS
        })
    }

    pub fn can_place_at(&mut self, wid: WorldId, x: i32, y: i32, z: i32) -> bool {
        (0..128).contains(&y) && replaceable(self.get_block(wid, x, y, z).0)
    }

    // ---- combat ---------------------------------------------------------------------------------------------------

    /// Validates an attack and returns the base damage after armour; `None`
    /// if the attack is not allowed (cooldown, reach, dead, wrong state or
    /// different worlds).
    pub fn compute_attack(&mut self, attacker: i64, victim: i64) -> Option<f32> {
        if attacker == victim {
            return None;
        }
        let a = self.players.get(&attacker)?;
        let v = self.players.get(&victim)?;
        if a.world != v.world || a.switching || v.switching {
            return None;
        }
        if a.state != PlayerState::Spawned || v.state != PlayerState::Spawned || a.dead || v.dead {
            return None;
        }
        if a.gamemode == 3 || v.gamemode == 1 || v.gamemode == 3 {
            return None;
        }
        if self.tick.saturating_sub(a.last_attack_tick) < self.cfg.pvp.attack_cooldown_ticks {
            return None;
        }
        let (dx, dy, dz) = (a.x - v.x, a.y - v.y, a.z - v.z);
        let reach = self.cfg.pvp.max_reach;
        if dx * dx + dy * dy + dz * dz > reach * reach {
            return None;
        }
        let dmg = weapon_damage(a.held_item().id) * (1.0 - 0.04 * v.armor_total() as f32);
        let tick = self.tick;
        self.players.get_mut(&attacker)?.last_attack_tick = tick;
        Some(dmg)
    }

    pub fn apply_damage(&mut self, victim: i64, attacker: Option<i64>, dmg: f32) -> Hit {
        if dmg <= 0.0 {
            return Hit::Ignored;
        }
        let apos = attacker.and_then(|a| self.players.get(&a)).map(|a| (a.x, a.z));
        let (kb_h, kb_v) = (self.cfg.pvp.knockback_h, self.cfg.pvp.knockback_v);
        let Some(v) = self.players.get_mut(&victim) else { return Hit::Ignored };
        if v.dead || v.switching {
            return Hit::Ignored;
        }
        v.health = (v.health - dmg).max(0.0);
        let hp = v.health;
        let died = hp <= 0.0;
        if died {
            v.dead = true;
        }
        let (kh, kv) = v.kb.unwrap_or((kb_h, kb_v));
        let (vpos, wid) = ((v.x, v.z), v.world);
        self.send(victim, play::set_health(hp.ceil() as i32));
        let ev = if died { event::DEATH } else { event::HURT };
        self.broadcast_with(wid, |rcv| play::entity_event(wire_eid(rcv, victim), ev));
        if !died {
            if let Some((ax, az)) = apos {
                let (dx, dz) = (vpos.0 - ax, vpos.1 - az);
                let len = (dx * dx + dz * dz).sqrt().max(0.0001);
                let motion = Vec3 { x: dx / len * kh, y: kv, z: dz / len * kh };
                self.broadcast_with(wid, |rcv| play::set_entity_motion(&[(wire_eid(rcv, victim), motion)]));
            }
        }
        if died {
            Hit::Died
        } else {
            Hit::Alive
        }
    }

    pub fn set_health(&mut self, eid: i64, hp: f32) -> Hit {
        let Some(p) = self.players.get_mut(&eid) else { return Hit::Ignored };
        if p.dead {
            return Hit::Ignored;
        }
        p.health = hp.clamp(0.0, MAX_HEALTH);
        let h = p.health;
        self.send(eid, play::set_health(h.ceil() as i32));
        if h <= 0.0 {
            return self.apply_damage(eid, None, 0.001);
        }
        Hit::Alive
    }

    /// Respawns a dead player at their spawn point (override or world spawn).
    pub fn respawn(&mut self, eid: i64) -> bool {
        let Some(wid) = self.players.get(&eid).map(|p| p.world) else { return false };
        let spawn = self.spawn_of(wid);
        let Some(p) = self.players.get_mut(&eid) else { return false };
        if !p.dead {
            return false;
        }
        p.dead = false;
        p.health = MAX_HEALTH;
        let (x, y, z) = p.spawn_override.unwrap_or((spawn.0 as f32 + 0.5, spawn.1 as f32, spawn.2 as f32 + 0.5));
        self.send(eid, play::set_health(MAX_HEALTH as i32));
        self.send(eid, packets::respawn(Vec3 { x, y: y + EYE_HEIGHT, z }));
        self.teleport(eid, x, y, z);
        // clear the death pose on other clients by re-spawning the entity
        let Some(p) = self.players.get(&eid) else { return true };
        let (uuid, hidden) = (p.uuid, p.hidden);
        self.broadcast_except(wid, eid, &play::remove_player(eid, &uuid));
        if !hidden {
            let others: Vec<i64> =
                self.players.values().filter(|q| q.eid != eid && q.world == wid && q.state == PlayerState::Spawned).map(|q| q.eid).collect();
            for o in others {
                self.introduce(o, eid);
            }
        }
        true
    }

    // ---- dropped items ----------------------------------------------------------------------------------------------

    pub fn tick_items(&mut self) {
        if self.items.is_empty() {
            return;
        }
        let mut gone: Vec<(i64, WorldId)> = Vec::new();
        let mut picks: Vec<(i64, Vec<i64>)> = Vec::new();
        for it in self.items.values_mut() {
            it.age += 1;
            if it.age > ITEM_LIFETIME_TICKS {
                gone.push((it.eid, it.world));
                continue;
            }
            if it.age < ITEM_PICKUP_DELAY {
                continue;
            }
            let mut candidates: Vec<(f32, i64)> = self.players.values().filter_map(|p| {
                if p.world != it.world || p.state != PlayerState::Spawned || p.switching || p.dead || p.gamemode == 1 || p.gamemode == 3 {
                    return None;
                }
                let (dx, dz) = (p.x - it.pos.x, p.z - it.pos.z);
                let dy = (p.y - EYE_HEIGHT) - it.pos.y;
                if dx.abs() <= 1.3 && dz.abs() <= 1.3 && (-1.0..=2.0).contains(&dy) {
                    Some((dx * dx + dy * dy + dz * dz, p.eid))
                } else {
                    None
                }
            }).collect();
            candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
            if !candidates.is_empty() {
                picks.push((it.eid, candidates.into_iter().map(|(_, eid)| eid).collect()));
            }
        }
        for (id, wid) in gone {
            self.items.remove(&id);
            self.broadcast(wid, &play::remove_entity(id));
        }
        for (ieid, candidates) in picks {
            let Some(it) = self.items.get(&ieid).copied() else { continue };
            // Try the nearest player first, but don't let a full inventory on one
            // player prevent somebody else standing on the generator from picking it up.
            let mut picked: Option<(i64, u8)> = None;
            for peid in candidates {
                let Some(p) = self.players.get_mut(&peid) else { continue };
                let left = p.give(it.item);
                if left < it.item.count {
                    picked = Some((peid, left));
                    break;
                }
            }
            let Some((peid, left)) = picked else { continue };
            self.broadcast_with(it.world, |rcv| play::take_item_entity(ieid, wire_eid(rcv, peid)));
            self.items.remove(&ieid);
            self.broadcast(it.world, &play::remove_entity(ieid));
            // Partial stack pickups replace the old entity with a smaller one,
            // so every client sees the same remaining count.
            if left > 0 {
                self.spawn_item(it.world, it.pos, Slot::new(it.item.id, left, it.item.damage));
            }
            self.send_inventory(peid);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Generator;

    fn core() -> Core {
        let cfg = GameConfig {
            max_players: 10,
            max_view: 4,
            gamemode: 0,
            quirks: Quirks::default(),
            trace: false,
            hotbar_offset: 9,
            pvp: Pvp { knockback_h: 0.4, knockback_v: 0.4, attack_cooldown_ticks: 8, max_reach: 6.0 },
            broadcast_radius: 64.0,
        };
        Core::new(cfg, World::new(Generator::Flat, Some(4), (0, 4, 0), true))
    }

    fn add_in(c: &mut Core, wid: WorldId, port: u16, x: f32) -> i64 {
        let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let eid = c.alloc_eid();
        let mut p = Player::new(addr, eid, format!("P{eid}"), [eid as u8; 16], 0, false, vec![0; 8192], (x, 5.62, 0.5));
        p.state = PlayerState::Spawned;
        p.world = wid;
        c.players.insert(eid, p);
        c.by_addr.insert(addr, eid);
        eid
    }

    fn add(c: &mut Core, port: u16, x: f32) -> i64 {
        add_in(c, TEMPLATE, port, x)
    }

    #[test]
    fn attack_damage_knockback_and_death() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        let v = add(&mut c, 2, 2.5);
        c.tick = 100;
        c.players.get_mut(&a).unwrap().give(Slot::new(267, 1, 0)); // iron sword in slot 0
        let dmg = c.compute_attack(a, v).unwrap();
        assert_eq!(dmg, 6.0);
        assert!(c.compute_attack(a, v).is_none()); // cooldown
        c.out.clear();
        assert_eq!(c.apply_damage(v, Some(a), dmg), Hit::Alive);
        assert_eq!(c.players[&v].health, 14.0);
        // set_health + hurt event x2 recipients + motion x2 recipients
        assert_eq!(c.out.len(), 1 + 2 + 2);
        assert_eq!(c.apply_damage(v, Some(a), 100.0), Hit::Died);
        assert!(c.players[&v].dead);
        assert!(c.respawn(v));
        assert_eq!(c.players[&v].health, MAX_HEALTH);
    }

    #[test]
    fn block_edits_are_broadcast_and_reset() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.players.get_mut(&a).unwrap().sent_chunks.insert((0, 0));
        assert!(c.set_block(TEMPLATE, 1, 4, 1, 35, 14));
        assert_eq!(c.out.len(), 1);
        assert_eq!(c.get_block(TEMPLATE, 1, 4, 1), (35, 14));
        assert_eq!(c.reset_arena(TEMPLATE), 1);
        assert_eq!(c.get_block(TEMPLATE, 1, 4, 1), (0, 0));
    }

    #[test]
    fn items_are_picked_up() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        let it = c.spawn_item(TEMPLATE, Vec3 { x: 0.5, y: 4.0, z: 0.5 }, Slot::new(265, 3, 0));
        for _ in 0..12 {
            c.tick_items();
        }
        assert!(!c.items.contains_key(&it));
        assert_eq!(c.players[&a].inventory[0], Slot::new(265, 3, 0));
    }

    #[test]
    fn full_nearest_inventory_does_not_block_another_player_from_pickup() {
        let mut c = core();
        let full = add(&mut c, 1, 0.5);
        let available = add(&mut c, 2, 0.5);
        c.players.get_mut(&full).unwrap().inventory = [Slot::new(1, 64, 0); 36];
        let it = c.spawn_item(TEMPLATE, Vec3 { x: 0.5, y: 4.0, z: 0.5 }, Slot::new(265, 1, 0));
        for _ in 0..12 {
            c.tick_items();
        }
        assert!(!c.items.contains_key(&it));
        assert_eq!(c.players[&available].count_item(265), 1);
        assert_eq!(c.players[&full].count_item(265), 0);
    }

    #[test]
    fn pmmp_inventory_uses_36_slots_and_hotbar_references_offset_nine() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.cfg.quirks.inventory_placeholders = false;
        c.cfg.hotbar_offset = 9;
        c.players.get_mut(&a).unwrap().give(Slot::new(265, 3, 0));
        c.out.clear();
        c.send_inventory(a);
        let packet = c.out.iter().find(|o| o.data.first() == Some(&crate::protocol::ids::CONTAINER_SET_CONTENT)).unwrap();
        assert_eq!(i16::from_be_bytes([packet.data[2], packet.data[3]]), 36);
        assert_eq!(&packet.data[4..6], &[1, 9]); // item is in inventory slot zero
        // One non-empty slot (7 bytes), 35 empty slots (2 bytes each), then hotbar data.
        let hotbar_count_at = 4 + 7 + 35 * 2;
        assert_eq!(i16::from_be_bytes([packet.data[hotbar_count_at], packet.data[hotbar_count_at + 1]]), 9);
        assert_eq!(i32::from_be_bytes(packet.data[hotbar_count_at + 2..hotbar_count_at + 6].try_into().unwrap()), 9);
    }

    #[test]
    fn chunks_stream_even_without_a_radius_request() {
        let mut c = core();
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let eid = c.alloc_eid();
        let p = Player::new(addr, eid, "P".into(), [1; 16], 0, false, vec![0; 8192], (0.5, 5.62, 0.5));
        c.players.insert(eid, p);
        c.by_addr.insert(addr, eid);

        c.tick = 5; // inside the grace period: nothing is sent
        assert!(c.tick_chunks().joined.is_empty());
        assert!(c.out.is_empty());

        c.tick = RADIUS_FALLBACK_TICKS; // grace period over: stream, then spawn
        let mut spawned = Vec::new();
        for _ in 0..200 {
            spawned.extend(c.tick_chunks().joined);
            if !spawned.is_empty() {
                break;
            }
        }
        assert_eq!(spawned, vec![eid]);
        assert_eq!(c.players[&eid].state, PlayerState::Spawned);
        assert!(c.out.len() > 10);
    }

    #[test]
    fn broadcasts_stay_inside_their_world() {
        let mut c = core();
        let w2 = c.fork_world();
        let a = add_in(&mut c, TEMPLATE, 1, 0.5);
        let _b = add_in(&mut c, w2, 2, 0.5);
        let _d = add_in(&mut c, w2, 3, 1.5);
        c.out.clear();
        c.broadcast_text(w2, text_type::RAW, "", "hi");
        assert_eq!(c.out.len(), 2);
        c.out.clear();
        c.broadcast_text(TEMPLATE, text_type::RAW, "", "hi");
        assert_eq!(c.out.len(), 1);
        // items and attacks do not cross worlds
        c.spawn_item(w2, Vec3 { x: 0.5, y: 4.0, z: 0.5 }, Slot::new(265, 1, 0));
        for _ in 0..12 {
            c.tick_items();
        }
        assert!(c.players[&a].inventory[0].is_empty());
        c.tick = 100;
        assert!(c.compute_attack(a, _b).is_none());
    }

    #[test]
    fn near_teleport_waits_for_destination_neighbour_chunks_and_excludes_self_broadcast() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        let b = add(&mut c, 2, 1.5);
        c.out.clear();

        // A short move can still cross a chunk edge; the 3x3 destination area
        // must be sent before the local-player correction is issued.
        c.teleport(a, 1.0, 4.0, 0.5);
        assert!(c.players[&a].tp_wait);
        assert!(c.players[&a].tp_target.is_none());

        // Neither the local correction nor the observer entity update is sent
        // until the destination chunks are ready.
        assert!(!c.out.iter().any(|o| o.data.first() == Some(&crate::protocol::ids::MOVE_PLAYER)));

        for _ in 0..4 {
            c.tick_chunks();
            if !c.players[&a].tp_wait { break; }
        }
        assert!(!c.players[&a].tp_wait);
        assert!(c.players[&a].tp_target.is_some());
        assert!(c.out.iter().any(|o| {
            o.addr == c.players[&a].addr
                && o.data.first() == Some(&crate::protocol::ids::MOVE_PLAYER)
                && o.data.get(1..9) == Some(&LOCAL_PLAYER_EID.to_be_bytes()[..])
        }));
        assert!(c.out.iter().any(|o| {
            o.addr == c.players[&b].addr
                && o.data.first() == Some(&crate::protocol::ids::MOVE_PLAYER)
                && o.data.get(1..9) == Some(&a.to_be_bytes()[..])
        }));
    }

    #[test]
    fn teleport_releases_after_destination_3x3_without_waiting_for_full_view() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.players.get_mut(&a).unwrap().view_radius = 4;
        c.teleport(a, 40.5, 8.0, 40.5);
        assert!(c.players[&a].tp_wait);

        // Only the destination 3x3 is required by the legacy GenisysPro
        // teleport check. The rest of the view-distance queue may continue.
        for _ in 0..4 {
            c.tick_chunks();
            if !c.players[&a].tp_wait {
                break;
            }
        }
        assert!(!c.players[&a].tp_wait);
        assert!(!c.players[&a].chunk_queue.is_empty(), "far view chunks should still be streaming");
        assert_eq!(c.players[&a].tp_target.map(|(v, _)| (v.x, v.y, v.z)), Some((40.5, 9.62, 40.5)));
    }

    #[test]
    fn teleport_requeues_destination_chunks_marked_sent_but_not_loaded() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.players.get_mut(&a).unwrap().view_radius = 4;

        // Simulate the stale bookkeeping state: destination chunks were marked
        // sent by an earlier plan, but the client never received them.
        for z in 2..=4 {
            for x in 2..=4 {
                c.players.get_mut(&a).unwrap().sent_chunks.insert((x, z));
            }
        }
        c.teleport(a, 40.5, 8.0, 40.5);
        assert!(c.players[&a].tp_wait);
        assert!(!c.players[&a].chunk_queue.is_empty());

        for _ in 0..20 {
            c.tick_chunks();
            if !c.players[&a].tp_wait {
                break;
            }
        }
        assert!(!c.players[&a].tp_wait, "destination chunk stream must release the pending teleport");
        assert_eq!(c.players[&a].tp_target.map(|(v, _)| (v.x, v.y, v.z)), Some((40.5, 9.62, 40.5)));
        assert!(c.out.iter().any(|o| {
            o.addr == c.players[&a].addr
                && o.data.first() == Some(&crate::protocol::ids::MOVE_PLAYER)
                && o.data.get(1..9) == Some(&LOCAL_PLAYER_EID.to_be_bytes()[..])
        }));
    }

    #[test]
    fn second_teleport_during_chunk_stream_uses_latest_destination() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.players.get_mut(&a).unwrap().view_radius = 4;

        // Chunk 3 is more than two chunks from the initial center, so this
        // teleport must wait for the destination area to stream.
        c.teleport(a, 48.5, 8.0, 0.5);
        assert!(c.players[&a].tp_wait);
        assert!(c.players[&a].tp_target.is_none());

        // Drain only one tick, leaving queued chunks, then retarget within the
        // new center. The second teleport must not bypass the pending stream.
        c.tick_chunks();
        assert!(!c.players[&a].chunk_queue.is_empty());
        // Retarget to a different chunk centre. A move within the already
        // loaded 3x3 area should teleport immediately; it must not be used
        // to test whether a second, still-unloaded destination stays pending.
        c.teleport(a, 16.5, 12.0, 0.5);
        assert!(c.players[&a].tp_wait);
        assert!(c.players[&a].tp_target.is_none());
        assert_eq!((c.players[&a].x, c.players[&a].y, c.players[&a].z), (16.5, 13.62, 0.5));

        for _ in 0..10 {
            c.tick_chunks();
            if !c.players[&a].tp_wait {
                break;
            }
        }
        assert!(!c.players[&a].tp_wait);
        assert_eq!(c.players[&a].tp_target.map(|(v, _)| (v.x, v.y, v.z)), Some((16.5, 13.62, 0.5)));
    }

    #[test]
    fn world_switch_freezes_streams_then_teleports() {
        let mut c = core();
        let w2 = c.fork_world();
        let a = add(&mut c, 1, 0.5);
        let b = add(&mut c, 2, 1.5); // stays behind
        c.players.get_mut(&a).unwrap().view_radius = 2;
        c.out.clear();
        assert!(c.move_to_world(a, w2, 10.5, 5.0, 10.5, 90.0, 0.0));
        assert!(c.players[&a].switching && c.players[&a].world == w2);
        assert!(!c.out.is_empty()); // b was told to remove a, a told to forget b
        assert!(c.out.iter().any(|o| o.addr == c.players[&b].addr));
        // while frozen the player receives chunks, then the teleport
        let mut switched = Vec::new();
        for _ in 0..30 {
            switched.extend(c.tick_chunks().switched);
            if !switched.is_empty() {
                break;
            }
        }
        assert_eq!(switched, vec![a]);
        assert!(!c.players[&a].switching);
        assert_eq!(c.players_in(w2), vec![a]);
        assert!(!c.drop_world(w2)); // occupied
        c.players.get_mut(&a).unwrap().world = TEMPLATE;
        assert!(c.drop_world(w2));
        assert!(!c.drop_world(TEMPLATE));
    }

    #[test]
    fn bots_are_players_without_a_socket() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        c.out.clear();
        let bot = c.add_bot(TEMPLATE, "Bot", vec![0; 8192], 3.5, 4.0, 0.5, 0.0);
        assert!(c.players[&bot].is_bot);
        assert_eq!(c.out.len(), 2); // list + add_player, only to the human
        c.out.clear();
        c.send_text(bot, text_type::RAW, "", "x");
        c.broadcast_text(TEMPLATE, text_type::RAW, "", "x");
        assert_eq!(c.out.len(), 1);
        c.tick = 100;
        assert!(c.compute_attack(a, bot).is_some() || c.players[&bot].gamemode == 0);
        c.remove_player(bot);
        assert!(!c.players.contains_key(&bot));
    }

    #[test]
    fn spectators_are_hidden_and_untouchable() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        let b = add(&mut c, 2, 1.5);
        c.set_gamemode(b, 3);
        assert!(c.players[&b].hidden);
        c.tick = 100;
        assert!(c.compute_attack(a, b).is_none());
        assert!(c.compute_attack(b, a).is_none());
        c.set_gamemode(b, 0);
        assert!(!c.players[&b].hidden);
    }

    #[test]
    fn emerald_holograms_are_invisible_and_update_their_name_tag() {
        let mut c = core();
        let viewer = add(&mut c, 1, 0.5);
        c.out.clear();
        let h = c.add_hologram(TEMPLATE, Vec3 { x: 2.5, y: 6.0, z: 2.5 }, "§aEMERALD §7| §eNext: 30s");
        assert!(c.npcs.get(&h).unwrap().hologram);
        assert!(c.out.iter().any(|o| o.addr == c.players[&viewer].addr && o.data.first() == Some(&crate::protocol::ids::ADD_PLAYER)));
        c.out.clear();
        c.set_npc_name(h, "§aEMERALD §7| §eNext: 29s");
        assert!(c.out.iter().any(|o| o.addr == c.players[&viewer].addr && o.data.first() == Some(&crate::protocol::ids::SET_ENTITY_DATA)));
    }

    #[test]
    fn npcs_are_shown_per_world() {
        let mut c = core();
        let w2 = c.fork_world();
        let a = add(&mut c, 1, 0.5);
        c.out.clear();
        let n = c.add_npc(TEMPLATE, Vec3 { x: 2.0, y: 4.0, z: 2.0 }, 0.0, "Shop", vec![0; 8192]);
        assert_eq!(c.out.len(), 2);
        c.add_npc(w2, Vec3 { x: 2.0, y: 4.0, z: 2.0 }, 0.0, "Shop", vec![0; 8192]);
        assert_eq!(c.out.len(), 2); // nobody in w2
        c.remove_npc(n);
        assert!(c.npcs.len() == 1 && c.players.contains_key(&a));
        assert!(c.drop_world(w2) && c.npcs.is_empty());
    }

    #[test]
    fn falling_more_than_three_blocks_hurts() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        assert_eq!(c.track_fall(a, 30.0, (0.5, 26.0, 0.5), false), None);
        assert_eq!(c.track_fall(a, 26.0, (0.5, 24.0, 0.5), false), None);
        assert_eq!(c.track_fall(a, 24.0, (0.5, 24.0, 0.5), true), Some(3.0));
        assert_eq!(c.players[&a].fall_distance, 0.0);
        // a short drop is free, and creative players never take fall damage
        assert_eq!(c.track_fall(a, 6.0, (0.5, 4.0, 0.5), false), None);
        assert_eq!(c.track_fall(a, 4.0, (0.5, 4.0, 0.5), true), None);
        c.set_gamemode(a, 1);
        assert_eq!(c.track_fall(a, 30.0, (0.5, 10.0, 0.5), false), None);
        assert_eq!(c.track_fall(a, 10.0, (0.5, 10.0, 0.5), true), None);
    }

    #[test]
    fn creative_players_keep_menu_picks() {
        let mut c = core();
        let a = add(&mut c, 1, 0.5);
        assert!(!c.creative_set_slot(a, 0, 9, Slot::new(1, 1, 0)), "survival: not accepted");
        c.out.clear();
        c.set_gamemode(a, 1);
        assert!(c.out.iter().any(|o| o.data[0] == crate::protocol::ids::CONTAINER_SET_CONTENT), "menu is sent");
        assert!(c.creative_set_slot(a, 0, 9, Slot::new(1, 64, 0)));
        assert_eq!(c.players[&a].inventory[0], Slot::new(1, 64, 0));
    }

    #[test]
    fn adventure_flag_layouts() {
        // creative = allow flight (+ auto jump in every mode)
        assert_eq!(adventure_flags(1, true), 0x80 | 0x40);
        assert_eq!(adventure_flags(1, false), 0x40 | 0x20);
        assert!(adventure_flags(3, true) & 0x100 != 0);
        assert!(adventure_flags(3, false) & 0x80 != 0);
        // survival / adventure: only auto jump (+ immutable world for adventure)
        assert_eq!(adventure_flags(0, true), 0x40);
        assert_eq!(adventure_flags(0, false), 0x20);
        assert_eq!(adventure_flags(2, true), 0x01 | 0x40);
        assert_eq!(play::client_gametype(3), 1);
        assert_eq!(play::client_gametype(2), 0);
    }
}
