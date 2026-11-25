//! Game layer: packet handling on top of the shared [`Core`], with Luau
//! events fired around every gameplay action.
//!
//! Rule: never hold a `core` borrow while calling into `scripts` (plugins
//! borrow the core themselves).

use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use crate::bedwars::{Bedwars, BwConfig};
use crate::binary::Vec3;
use crate::commands::Sender;
use crate::state::{Core, GameConfig, Hit, EYE_HEIGHT};
use crate::entity::player::{Player, PlayerState};
use crate::protocol::packets::{self, status, LoginExtra, LoginPacket, MovePlayer, StartGame, LOCAL_PLAYER_EID, MOVE_NORMAL};
use crate::protocol::play::{self, action, text_type, EVENT_PARTICLE_DESTROY};
use crate::protocol::{self, batch, ids};
use crate::raknet::handler::{GameHandler, Outbox};
use crate::raknet::session::DisconnectReason;
use crate::scripting::{ScriptConfig, ScriptHost};
use crate::world::World;

/// Stale client moves tolerated after a teleport before its own position is trusted again (~1.5 s).
const TP_STALE_LIMIT: u32 = 30;

const NOON: i32 = 6000;

pub struct GameServer {
    core: Rc<RefCell<Core>>,
    scripts: ScriptHost,
    bw: Bedwars,
    /// Lines typed into the server console (stdin), polled every tick.
    console: Option<Receiver<String>>,
}

fn valid_name(n: &str) -> bool {
    (1..=16).contains(&n.len())
        && !n.starts_with(' ')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ' ')
}

fn kick(addr: SocketAddr, out: &mut Outbox, message: &str) {
    out.send(addr, packets::disconnect(message));
    out.disconnect(addr);
}

impl GameServer {
    pub fn new(cfg: GameConfig, world: World, scripts: ScriptConfig, bw_cfg: BwConfig) -> Self {
        let core = Rc::new(RefCell::new(Core::new(cfg, world)));
        core.borrow_mut().load_lists();
        let bw = Bedwars::new(bw_cfg, &mut core.borrow_mut());
        let scripts = ScriptHost::new(Rc::clone(&core), scripts);
        Self { core, scripts, bw, console: None }
    }

    /// Console commands (`op Name`, `ban Name`, ...) arrive through this channel.
    pub fn set_console(&mut self, rx: Receiver<String>) {
        self.console = Some(rx);
    }

    /// Loads plugins and fires `on_server_start`.
    pub fn start(&mut self) {
        self.scripts.load_all();
        self.scripts.server_start();
    }

    fn flush(&self, out: &mut Outbox) {
        let mut c = self.core.borrow_mut();
        for p in c.out.drain(..) {
            out.send(p.addr, p.data);
        }
        for a in c.disconnects.drain(..) {
            out.disconnect(a);
        }
    }

    /// Entity id of a fully spawned player.
    fn spawned(&self, addr: SocketAddr) -> Option<i64> {
        let c = self.core.borrow();
        let eid = *c.by_addr.get(&addr)?;
        (c.players.get(&eid)?.state == PlayerState::Spawned).then_some(eid)
    }

    fn handle_packet(&mut self, addr: SocketAddr, data: &[u8], out: &mut Outbox, depth: u8) {
        if data.is_empty() {
            log::warn!("{addr}: empty game packet");
            return;
        }
        if matches!(data[0], ids::USE_ITEM | ids::REMOVE_BLOCK | ids::PLAYER_ACTION) {
            let c = self.core.borrow();
            let who = c.by_addr.get(&addr).and_then(|eid| c.players.get(eid).map(|p| {
                (p.name.clone(), *eid, p.gamemode, p.world, p.held_item())
            }));
            match who {
                Some((name, eid, gm, wid, held)) => log::info!(
                    "BLOCK_PACKET rx player={name:?} eid={eid} pid=0x{:02x} len={} depth={} gm={} world={} held={held:?} body={:02x?}",
                    data[0], data.len(), depth, gm, wid, &data[1..]
                ),
                None => log::info!(
                    "BLOCK_PACKET rx player=<unbound> addr={addr} pid=0x{:02x} len={} depth={} body={:02x?}",
                    data[0], data.len(), depth, &data[1..]
                ),
            }
        }
        if self.core.borrow().cfg.trace {
            let n = data.len().min(48);
            log::debug!("{addr} -> id=0x{:02x} len={} {:02x?}", data[0], data.len(), &data[..n]);
        }
        let body = &data[1..];
        match data[0] {
            ids::LOGIN => self.on_login(addr, body, out),
            ids::BATCH if depth == 0 => self.on_batch(addr, body, out),
            ids::REQUEST_CHUNK_RADIUS => self.on_chunk_radius(addr, body),
            ids::MOVE_PLAYER => self.on_move(addr, body),
            ids::TEXT => self.on_text(addr, body),
            ids::USE_ITEM => self.on_use_item(addr, body),
            ids::REMOVE_BLOCK => self.on_remove_block(addr, body),
            ids::INTERACT => self.on_interact(addr, body),
            ids::PLAYER_ACTION => self.on_player_action(addr, body),
            ids::ANIMATE => self.on_animate(addr, body),
            ids::MOB_EQUIPMENT => self.on_mob_equipment(addr, body),
            // Dropping / rearranging items client-side: re-sync instead of trusting it.
            ids::CONTAINER_SET_SLOT => {
                if let Some(eid) = self.spawned(addr) {
                    let mut c = self.core.borrow_mut();
                    let handled = match play::decode_container_set_slot(body) {
                        // creative players pick items out of the menu: keep what they chose
                        Ok((win, slot, item)) => {
                            self.bw.on_container_slot(&mut c, eid, win, slot) || c.creative_set_slot(eid, win, slot, item)
                        }
                        Err(_) => false,
                    };
                    if !handled {
                        c.send_inventory(eid);
                    }
                }
            }
            ids::CONTAINER_CLOSE => {
                if let Some(eid) = self.spawned(addr) {
                    let mut c = self.core.borrow_mut();
                    let handled = play::decode_container_close(body).map_or(false, |w| self.bw.on_container_close(&mut c, eid, w));
                    if !handled {
                        c.send_inventory(eid);
                    }
                }
            }
            // Dropping / rearranging items client-side: re-sync instead of trusting it.
            ids::DROP_ITEM => {
                if let Some(eid) = self.spawned(addr) {
                    self.core.borrow_mut().send_inventory(eid);
                }
            }
            other => {
                // Helps to see what a client is waiting for / sending while it loads.
                if self.spawned(addr).is_none() {
                    log::info!("{addr}: unhandled packet 0x{other:02x} ({} bytes) before spawn", data.len());
                } else {
                    log::debug!("{addr}: unhandled packet 0x{other:02x} ({} bytes)", data.len());
                }
            }
        }
    }

    fn on_batch(&mut self, addr: SocketAddr, body: &[u8], out: &mut Outbox) {
        let raw = match batch::decode_batch(body) {
            Ok(r) => r,
            Err(e) => {
                log::debug!("{addr}: bad batch: {e}");
                return;
            }
        };
        batch::for_each_packet(&raw, |p| self.handle_packet(addr, p, out, 1));
    }

    // ---- login --------------------------------------------------------------------------

    fn on_login(&mut self, addr: SocketAddr, body: &[u8], out: &mut Outbox) {
        let login = match LoginPacket::decode(body) {
            Ok(l) => l,
            Err(e) => {
                log::debug!("{addr}: bad login: {e}");
                out.disconnect(addr);
                return;
            }
        };
        let ours = protocol::PROTOCOL_VERSION as i32;
        if login.protocol1 != ours {
            let st = if login.protocol1 < ours { status::FAILED_CLIENT } else { status::FAILED_SERVER };
            log::info!("{addr}: protocol {} rejected (server speaks {ours})", login.protocol1);
            out.send(addr, packets::play_status(st));
            out.disconnect(addr);
            return;
        }
        let extra = login.extra.unwrap_or_else(|| {
            log::debug!("{addr}: login without client data, using fallback data");
            LoginExtra {
                client_id: 0,
                uuid: [0u8; 16],
                server_address: String::new(),
                client_secret: String::new(),
                skin_slim: false,
                skin: vec![0u8; 8192],
            }
        });

        let mut c = self.core.borrow_mut();
        if c.by_addr.contains_key(&addr) {
            return;
        }
        if !valid_name(&login.username) {
            return kick(addr, out, "Invalid username");
        }
        if c.banned.contains(&login.username.to_ascii_lowercase()) {
            log::info!("{addr}: banned player {} tried to join", login.username);
            return kick(addr, out, "You are banned from this server");
        }
        if c.players.len() >= c.cfg.max_players {
            return kick(addr, out, "Server is full");
        }
        if c.players.values().any(|p| p.name.eq_ignore_ascii_case(&login.username)) {
            return kick(addr, out, "That name is already in use");
        }
        if extra.skin.len() != 8192 && extra.skin.len() != 16384 {
            return kick(addr, out, "Invalid skin");
        }

        let eid = c.alloc_eid();
        // Bedwars picks the match world (a fresh clone if every lobby is busy)
        // and the waiting-room position for the new connection.
        let (wid, spot) = self.bw.assign_login(&mut c);
        let spawn = (spot.x.floor() as i32, spot.y.floor() as i32, spot.z.floor() as i32);
        let pos = Vec3 { x: spot.x, y: spot.y + EYE_HEIGHT, z: spot.z };
        log::info!("{addr}: {} logged in (eid {eid})", login.username);
        let mut player = Player::new(
            addr,
            eid,
            login.username,
            extra.uuid,
            extra.client_id,
            extra.skin_slim,
            extra.skin,
            (pos.x, pos.y, pos.z),
        );
        player.gamemode = c.cfg.gamemode;
        player.login_tick = c.tick;
        player.world = wid;
        player.yaw = spot.yaw;
        player.pitch = spot.pitch;
        c.players.insert(eid, player);
        c.by_addr.insert(addr, eid);
        Self::join_sequence(&mut c, addr, pos, spawn);
    }

    /// Same order PocketMine uses for 0.14: PlayStatus, StartGame, time, spawn,
    /// health, difficulty, adventure settings. Chunks follow once the client
    /// asks for a radius (or after a short fallback delay, see `tick_chunks`).
    fn join_sequence(c: &mut Core, addr: SocketAddr, pos: Vec3, spawn: (i32, i32, i32)) {
        let tail = c.cfg.quirks.start_game_tail.clone();
        let extras = c.cfg.quirks.join_extras;
        let gamemode = c.cfg.gamemode;
        c.push(addr, packets::play_status(status::LOGIN_SUCCESS));
        if extras {
            c.push(addr, packets::move_player(LOCAL_PLAYER_EID, pos, 0.0, 0.0, 0.0, packets::MOVE_RESET, true));
        }
        let legacy = c.cfg.quirks.legacy_adventure_flags;
        let flags = crate::state::adventure_flags(gamemode, legacy);
        let sg = StartGame { seed: 0, dimension: 0, generator: 1, gamemode: play::client_gametype(gamemode), eid: LOCAL_PLAYER_EID, spawn, pos };
        c.push(addr, packets::start_game(&sg, &tail));
        if extras {
            c.push(addr, packets::set_spawn_position(spawn.0, spawn.1, spawn.2));
            c.push(addr, packets::move_player(LOCAL_PLAYER_EID, pos, 0.0, 0.0, 0.0, packets::MOVE_RESET, true));
            c.push(addr, packets::set_time(NOON, false));
            c.push(addr, packets::adventure_settings_ext(flags, c.cfg.quirks.adventure_permission.then_some(2)));
            c.push(addr, packets::respawn(pos));
        } else {
            c.push(addr, packets::set_time(NOON, false));
            c.push(addr, packets::set_spawn_position(spawn.0, spawn.1, spawn.2));
            c.push(addr, play::set_health(20));
            c.push(addr, packets::set_difficulty(1));
            c.push(addr, packets::adventure_settings_ext(flags, c.cfg.quirks.adventure_permission.then_some(2)));
        }
        log::info!("{addr}: PlayStatus + StartGame sent, waiting for the client's chunk radius request");
    }

    fn on_chunk_radius(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(req) = packets::decode_request_chunk_radius(body) else { return };
        let mut c = self.core.borrow_mut();
        let c = &mut *c;
        let Some(&eid) = c.by_addr.get(&addr) else { return };
        let r = req.clamp(1, c.cfg.max_view);
        let Some(p) = c.players.get_mut(&eid) else { return };
        log::info!("{}: chunk radius request {} (using {})", p.name, req, r);
        p.view_radius = r;
        if p.state == PlayerState::LoggedIn {
            p.state = PlayerState::Loading;
        }
        let Some(world) = c.worlds.get(&p.world) else { return };
        p.plan_chunks(&|x, z| world.in_bounds(x, z));
        p.initial_chunks_sent = 0;
        p.initial_chunks_needed = p.chunk_queue.len().min(56);
        c.out.push(crate::state::OutPacket { addr, data: packets::chunk_radius_update(r) });
    }


    // ---- movement / chat ------------------------------------------------------------------------

    fn on_move(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(m) = MovePlayer::decode(body) else { return };
        let ok = |v: f32, lim: f32| v.is_finite() && v.abs() < lim;
        if !(ok(m.pos.x, 3.2e7) && ok(m.pos.z, 3.2e7) && ok(m.pos.y, 1024.0) && ok(m.yaw, 1e5) && ok(m.pitch, 1e5)) {
            return;
        }
        let (eid, feet, prev_feet_y) = {
            let mut c = self.core.borrow_mut();
            let c = &mut *c;
            let Some(&eid) = c.by_addr.get(&addr) else { return };
            let Some(p) = c.players.get_mut(&eid) else { return };
            // Teleports must address the entity id the client really uses for itself.
            if p.local_eid != m.eid {
                log::info!("{}: client entity id is {:#x} (server assumed {:#x}); teleports are addressed to it", p.name, m.eid, p.local_eid);
                p.local_eid = m.eid;
            }
            // while a world switch streams, the client still reports its old position
            if p.dead || p.switching || p.tp_wait {
                return;
            }
            // PocketMine `isTeleporting`: until the client reports a position near the teleport target,
            // its moves are stale; re-send the teleport (and after a few misses a Respawn, which carries
            // a position the client always applies).
            if let Some((t, n)) = p.tp_target {
                let d2 = (m.pos.x - t.x).powi(2) + (m.pos.y - t.y).powi(2) + (m.pos.z - t.z).powi(2);
                if d2 > 4.0 && n < TP_STALE_LIMIT {
                    let n = n + 1;
                    p.tp_target = Some((t, n));
                    let (addr, yaw, pitch, health) = (p.addr, p.yaw, p.pitch, p.health);
                    // alternate between the learned id and the default -1 in case they differ
                    let id = if n % 2 == 1 { p.local_eid } else { LOCAL_PLAYER_EID };
                    c.out.push(crate::state::OutPacket { addr, data: packets::move_player(id, t, yaw, yaw, pitch, packets::MOVE_RESET, true) });
                    if n == 3 {
                        log::info!("client ignored the teleport, sending Respawn to ({:.1}, {:.1}, {:.1})", t.x, t.y, t.z);
                        c.out.push(crate::state::OutPacket { addr, data: packets::respawn(t) });
                        c.out.push(crate::state::OutPacket { addr, data: play::set_health(health.ceil() as i32) });
                    }
                    return;
                }
                if d2 > 4.0 {
                    // The client is the authority on where it stands. Rejecting its moves forever leaves the
                    // server position at the teleport target, which breaks reach checks, bot targeting,
                    // item pickup and the shop for a player who really stands elsewhere.
                    log::warn!("{}: client never confirmed the teleport to ({:.1}, {:.1}, {:.1}); trusting its own position", p.name, t.x, t.y, t.z);
                }
                p.tp_target = None;
            }
            let prev_feet_y = p.y - EYE_HEIGHT;
            let changed = p.x != m.pos.x || p.y != m.pos.y || p.z != m.pos.z || p.yaw != m.yaw || p.pitch != m.pitch;
            p.x = m.pos.x;
            p.y = m.pos.y;
            p.z = m.pos.z;
            p.yaw = m.yaw;
            p.pitch = m.pitch;
            let (wid, hidden, state) = (p.world, p.hidden, p.state);
            if state != PlayerState::LoggedIn && p.chunk_pos() != p.center {
                let Some(world) = c.worlds.get(&wid) else { return };
                p.plan_chunks(&|x, z| world.in_bounds(x, z));
            }
            if state != PlayerState::Spawned || !changed {
                return;
            }
            if !hidden {
                let pkt = packets::move_player(eid, m.pos, m.yaw, m.body_yaw, m.pitch, MOVE_NORMAL, m.on_ground);
                c.broadcast_near(wid, m.pos, eid, &pkt);
            }
            (eid, (m.pos.x, m.pos.y - EYE_HEIGHT, m.pos.z), prev_feet_y)
        };
        // fall damage (survival only; water, ladders and cobwebs cancel the fall)
        let fall = self.core.borrow_mut().track_fall(eid, prev_feet_y, feet, m.on_ground);
        if let Some(dmg) = fall {
            self.fall_damage(eid, dmg);
        }
        {
            // void check / lobby bounds
            let mut c = self.core.borrow_mut();
            self.bw.on_move(&mut c, eid, feet.1);
        }
        if self.scripts.wants_moves() {
            self.scripts.moved(eid, feet.0, feet.1, feet.2);
        }
    }

    /// Landing damage: match rules first, then plugins, like any other damage.
    fn fall_damage(&mut self, eid: i64, base: f32) {
        let ruled = {
            let c = self.core.borrow();
            self.bw.on_damage(&c, eid, None, base)
        };
        let Some(base) = ruled else { return };
        let Some(dmg) = self.scripts.damage(eid, None, base) else { return };
        let hit = self.core.borrow_mut().apply_damage(eid, None, dmg);
        if hit == Hit::Died {
            {
                let mut c = self.core.borrow_mut();
                self.bw.on_death(&mut c, eid, None);
            }
            self.scripts.player_death(eid, None);
        }
    }

    fn on_text(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(t) = play::decode_text(body) else { return };
        if t.kind != text_type::CHAT {
            return;
        }
        let msg: String = t.message.chars().filter(|c| !c.is_control()).take(256).collect();
        let msg = msg.trim();
        let Some(eid) = self.spawned(addr) else { return };
        if msg.is_empty() || self.scripts.chat(eid, msg) {
            return;
        }
        let mut c = self.core.borrow_mut();
        if let Some(cmd) = msg.strip_prefix('/') {
            if self.bw.command(&mut c, Sender::Player(eid), cmd) {
                return;
            }
            c.run_command(Sender::Player(eid), cmd);
        } else if self.bw.on_chat_reply(&mut c, eid, msg) {
            // a number typed while the shop menu is open
        } else if let Some((name, wid)) = c.players.get(&eid).map(|p| (p.name.clone(), p.world)) {
            if c.muted.contains(&name.to_ascii_lowercase()) {
                c.send_text(eid, text_type::RAW, "", "\u{a7}cYou are muted.");
            } else {
                c.broadcast_text(wid, text_type::CHAT, &name, msg);
            }
        }
    }

    fn on_animate(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(a) = play::decode_animate(body) else { return };
        let Some(eid) = self.spawned(addr) else { return };
        let mut c = self.core.borrow_mut();
        let Some((pos, wid, hidden)) = c.players.get(&eid).map(|p| (Vec3 { x: p.x, y: p.y, z: p.z }, p.world, p.hidden)) else { return };
        if !hidden {
            c.broadcast_near(wid, pos, eid, &play::animate(a.action, eid));
        }
    }

    fn on_mob_equipment(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(m) = play::decode_mob_equipment(body) else { return };
        let Some(eid) = self.spawned(addr) else { return };
        let mut c = self.core.borrow_mut();
        let off = c.cfg.hotbar_offset;
        let sel = m.selected as usize;
        let idx = (if off > 0 && sel >= off { sel - off } else { sel }).min(8);
        if let Some(p) = c.players.get_mut(&eid) {
            p.held = idx as u8;
            // Like PocketMine: remember which inventory slot the client linked to this hotbar slot
            // (0, 0x28 and 255 mean "nothing"), so the hotbar is not reset behind the player's back.
            let raw = m.slot as usize;
            if raw != 0 && raw != 0x28 && raw != 255 && raw >= off {
                let real = raw - off;
                if real < 36 {
                    p.hotbar[idx] = real as u8;
                }
            }
        }
        c.send_equipment(eid);
    }

    // ---- blocks -----------------------------------------------------------------------------------

    fn on_use_item(&mut self, addr: SocketAddr, body: &[u8]) {
        let u = match play::decode_use_item(body) {
            Ok(u) => u,
            Err(e) => {
                log::warn!("{addr}: BLOCK_PACKET decode failed type=USE_ITEM len={} error={e} raw={body:02x?}", body.len());
                return;
            }
        };
        let Some(eid) = self.spawned(addr) else {
            log::warn!("{addr}: decoded USE_ITEM but player is not spawned");
            return;
        };
        if u.face == 0xff {
            let activated = { let mut c = self.core.borrow_mut(); self.bw.activate_slime_platform(&mut c, eid) };
            if activated { return; }
            let c = self.core.borrow();
            if let Some(p) = c.players.get(&eid) {
                log::info!(
                    "{}: USE_ITEM air-use (face=255), no block target; clicked={:?} client_pos={:?} server_pos=({:.3},{:.3},{:.3}) click={:?} item={:?} held={:?} gm={} world={} trailer={:?}",
                    p.name, u.pos, u.player_pos, p.x, p.y, p.z, u.click_offset,
                    u.item, p.held_item(), p.gamemode, p.world, u.trailing_i32
                );
            }
            return;
        }
        let Some((dx, dy, dz)) = play::face_offset(u.face) else {
            log::warn!("{addr}: BLOCK_PACKET invalid USE_ITEM face={} pos={:?}", u.face, u.pos);
            return;
        };
        {
            let c = self.core.borrow();
            if let Some(p) = c.players.get(&eid) {
                log::info!(
                    "{}: USE_ITEM decoded pos={:?} face={} click={:?} client_pos={:?} client_item={:?} server_item={:?} trailer={:?} gm={} world={}",
                    p.name, u.pos, u.face, u.click_offset, u.player_pos, u.item,
                    p.held_item(), u.trailing_i32, p.gamemode, p.world
                );
            }
        }
        let place = (u.pos.x + dx, u.pos.y + dy, u.pos.z + dz);

        // (item id, meta, game mode, world)
        let checked: Option<(i16, u8, i32, u32)> = {
            let mut c = self.core.borrow_mut();
            let Some((gm, dead, held, claims, wid, switching)) = c
                .players
                .get(&eid)
                .map(|p| (p.gamemode, p.dead, p.held_item(), p.has_item_matching(u.item.id, u.item.damage), p.world, p.switching))
            else {
                return;
            };
            // Operators in creative are trusted: they may place whatever block the client reports.
            let creative_op = gm == 1 && c.can_use(Sender::Player(eid));
            if creative_op {
                let name = c.players.get(&eid).map(|p| p.name.clone()).unwrap_or_default();
                log::info!("{name}: UseItem at {:?} face {} client item {:?}, server hand item {:?}", u.pos, u.face, u.item, held);
            }
            // Prefer the item the client says it holds, but only if we gave it to them.
            let trusted = claims || (gm == 1 && (creative_op || crate::creative::contains(u.item.id)));
            let item = if !u.item.is_empty() && trusted { u.item } else { held };
            let placeable = (1..=255).contains(&item.id) || item.id == crate::bedwars::BED_ITEM;
            let refusal: Option<&str> = if dead {
                Some("player is dead")
            } else if switching {
                Some("world switch in progress")
            } else if !(gm == 0 || gm == 1) {
                Some("game mode cannot build")
            } else if item.is_empty() {
                Some("no item in hand (nothing picked from the creative menu, or the held slot is out of sync)")
            } else if !placeable {
                Some("held item is not a block")
            } else if !creative_op && !c.in_reach(eid, u.pos.x, u.pos.y, u.pos.z) {
                Some("target out of reach")
            } else if c.player_in_cell(wid, place.0, place.1, place.2) {
                Some("a player is standing there")
            } else if c.get_block(wid, u.pos.x, u.pos.y, u.pos.z).0 == 0 {
                Some("clicked block is air or outside the loaded world")
            } else if !c.can_place_at(wid, place.0, place.1, place.2) {
                Some("target position is not replaceable or outside the world")
            } else {
                None
            };
            match refusal {
                None => {
                    log::info!("BLOCK_PLACE validation passed eid={eid} world={wid} clicked={:?} target={place:?} face={} item={item:?} creative_op={creative_op}", u.pos, u.face);
                    Some((item.id, (item.damage & 0x0f) as u8, gm, wid))
                }
                Some(why) => {
                    log::warn!("BLOCK_PLACE validation rejected eid={eid} world={wid} clicked={:?} target={place:?} face={} item={item:?} creative_op={creative_op}: {why}", u.pos, u.face);
                    c.resync_block(eid, place.0, place.1, place.2);
                    c.send_inventory(eid);
                    None
                }
            }
        };
        let Some((item_id, meta, gm, wid)) = checked else { return };

        if item_id == crate::bedwars::BED_ITEM {
            // operators registering a team bed in the original map
            let mut c = self.core.borrow_mut();
            self.bw.try_place_bed(&mut c, eid, place.0, place.1, place.2);
            return;
        }
        let id = item_id as u8;

        let denied = {
            let c = self.core.borrow();
            self.bw.on_block_place(&c, eid, place.0, place.1, place.2, id)
        };
        if let Some(msg) = denied {
            log::info!("BLOCK_PLACE Bedwars rejected eid={eid} world={wid} target={place:?}: {msg}");
            let mut c = self.core.borrow_mut();
            if !msg.is_empty() {
                c.send_text(eid, text_type::POPUP, "", msg);
            }
            c.resync_block(eid, place.0, place.1, place.2);
            c.send_inventory(eid);
            return;
        }
        let creative_op = {
            let c = self.core.borrow();
            c.players.get(&eid).map_or(false, |p| p.gamemode == 1) && c.can_use(Sender::Player(eid))
        };
        if !creative_op && self.scripts.block_place(eid, place.0, place.1, place.2, id, meta) {
            log::info!("BLOCK_PLACE plugin rejected eid={eid} target={place:?}");
            let mut c = self.core.borrow_mut();
            c.resync_block(eid, place.0, place.1, place.2);
            c.send_inventory(eid);
            return;
        }
        let mut c = self.core.borrow_mut();
        let updated = c.set_block(wid, place.0, place.1, place.2, id, meta);
        log::info!("BLOCK_PLACE set_block eid={eid} world={wid} target={place:?} id={id} meta={meta} success={updated} broadcast={updated}");
        if updated && id == 46 {
            self.bw.prime_tnt(&mut c, wid, place);
        }
        if !updated {
            // Never charge the player for a placement the world did not accept.
            c.resync_block(eid, place.0, place.1, place.2);
            c.send_inventory(eid);
            return;
        }
        if gm == 0 {
            let consumed = c.players.get_mut(&eid)
                .map_or(false, |p| p.consume_one_matching(item_id, meta as i16));
            if consumed {
                // ContainerSetContent is required: otherwise the client keeps
                // displaying the pre-placement stack until another inventory update.
                c.send_inventory(eid);
            } else {
                // This should be unreachable after the validation above, but keep
                // the client/server inventory view aligned if a plugin changed it.
                log::warn!("BLOCK_PLACE inventory changed before consumption eid={eid} item={item_id}:{meta}");
                c.send_inventory(eid);
            }
        }
    }

    fn on_remove_block(&mut self, addr: SocketAddr, body: &[u8]) {
        let r = match play::decode_remove_block(body) {
            Ok(r) => r,
            Err(e) => {
                log::warn!("{addr}: BLOCK_PACKET decode failed type=REMOVE_BLOCK len={} error={e} raw={body:02x?}", body.len());
                return;
            }
        };
        let Some(eid) = self.spawned(addr) else {
            log::warn!("{addr}: decoded REMOVE_BLOCK but player is not spawned");
            return;
        };
        log::info!("{addr}: REMOVE_BLOCK decoded eid={} pos={:?}", r.eid, r.pos);
        self.break_block(eid, r.pos.x, r.pos.y, r.pos.z);
    }

    /// Shared by RemoveBlock and (for creative operators) PlayerAction START_BREAK.
    fn break_block(&mut self, eid: i64, x: i32, y: i32, z: i32) {
        let mut bed_before: Option<(u8, u8)> = None;
        let allowed = {
            let mut c = self.core.borrow_mut();
            let Some((gm, dead, wid, switching)) = c.players.get(&eid).map(|p| (p.gamemode, p.dead, p.world, p.switching)) else { return };
            let creative_op = gm == 1 && c.can_use(Sender::Player(eid));
            let id = c.get_block(wid, x, y, z).0;
            bed_before = Some(c.get_block(wid, x, y, z));
            if creative_op {
                log::info!("creative op break request at ({x}, {y}, {z}), block id {id}");
            }
            let refusal: Option<&str> = if dead {
                Some("player is dead")
            } else if switching {
                Some("world switch in progress")
            } else if !(gm == 0 || gm == 1) {
                Some("game mode cannot break blocks")
            } else if id == 0 {
                Some("block is air or outside the loaded world")
            } else if id == 7 && gm != 1 {
                Some("bedrock")
            } else if !creative_op && !c.in_reach(eid, x, y, z) {
                Some("target out of reach")
            } else {
                None
            };
            if let Some(why) = refusal {
                // Air is expected when a client reports the same break twice.
                log::warn!("BLOCK_BREAK validation rejected eid={eid} world={wid} pos=({x}, {y}, {z}) block_id={id} creative_op={creative_op}: {why}");
                c.resync_block(eid, x, y, z);
                false
            } else {
                log::info!("BLOCK_BREAK validation passed eid={eid} world={wid} pos=({x}, {y}, {z}) block_id={id} creative_op={creative_op}");
                true
            }
        };
        if !allowed {
            return;
        }
        // Survival break delay, like PocketMine/Genisys Level::useBreakOn: the time since the last
        // break (or START_BREAK) must cover the block's break time (minus one tick of compensation).
        {
            let mut c = self.core.borrow_mut();
            let (enforce, tick) = (c.cfg.quirks.enforce_break_time, c.tick);
            let info = c.players.get(&eid).map(|p| (p.gamemode, p.break_start, p.last_break, p.held_item().id));
            if let Some((0, start, last, tool)) = info {
                let block = bed_before.map_or(0, |b| b.0);
                let need = crate::blocks::break_ticks(block, tool);
                let start_ref = match start {
                    Some((sx, sy, sz, t0)) if (sx, sy, sz) == (x, y, z) => Some(t0),
                    _ => None,
                };
                let reference = [start_ref, last].into_iter().flatten().max();
                if let (Some(need), Some(reference)) = (need, reference) {
                    let elapsed = tick.saturating_sub(reference);
                    // 25% leeway so a slightly different client hardness never blocks a legit break
                    let min = ((need as f32) * 0.75) as u64;
                    if enforce && elapsed + 1 < min {
                        log::warn!("BLOCK_BREAK too fast eid={eid} pos=({x}, {y}, {z}) block={block} tool={tool}: {elapsed} ticks since last break/start, needs {need}");
                        c.resync_block(eid, x, y, z);
                        return;
                    }
                    log::info!("BLOCK_BREAK timing ok eid={eid} block={block} tool={tool}: {elapsed} ticks, break time {need}");
                } else if need.is_some() {
                    log::info!("BLOCK_BREAK first break, no reference tick yet eid={eid} block={block} (not timed)");
                }
                if let Some(p) = c.players.get_mut(&eid) {
                    p.break_start = None;
                    p.last_break = Some(tick);
                }
            }
        }
        // Bedwars: only enemy beds and player-placed blocks may be broken.
        let denied = {
            let mut c = self.core.borrow_mut();
            self.bw.on_block_break(&mut c, eid, x, y, z)
        };
        if let Some(msg) = denied {
            log::info!("BLOCK_BREAK Bedwars rejected eid={eid} world={} pos=({x}, {y}, {z}): {msg}", self.core.borrow().world_of(eid));
            let mut c = self.core.borrow_mut();
            if !msg.is_empty() {
                c.send_text(eid, text_type::POPUP, "", msg);
            }
            c.resync_block(eid, x, y, z);
            return;
        }
        let creative_op = {
            let c = self.core.borrow();
            c.players.get(&eid).map_or(false, |p| p.gamemode == 1) && c.can_use(Sender::Player(eid))
        };
        if !creative_op && self.scripts.block_break(eid, x, y, z) {
            log::info!("BLOCK_BREAK plugin rejected eid={eid} pos=({x}, {y}, {z})");
            self.core.borrow_mut().resync_block(eid, x, y, z);
            return;
        }
        let mut c = self.core.borrow_mut();
        let wid = c.world_of(eid);
        let updated = c.set_block(wid, x, y, z, 0, 0);
        log::info!("BLOCK_BREAK set_block eid={eid} world={wid} pos=({x}, {y}, {z}) success={updated} broadcast={updated}");
        // PocketMine: Level::addParticle(new DestroyBlockParticle(block centre, block)) -> LevelEvent 2001
        // with data = id | meta << 8, sent to everyone in the world (shows crumbs and plays the sound).
        if updated {
            if let Some((bid, bmeta)) = bed_before {
                if bid != 0 {
                    let centre = Vec3 { x: x as f32 + 0.5, y: y as f32 + 0.5, z: z as f32 + 0.5 };
                    c.broadcast(wid, &play::level_event(EVENT_PARTICLE_DESTROY, centre, bid as i32 | ((bmeta as i32) << 8)));
                }
            }
        }
        // A bed is two blocks: breaking one half removes the other half too.
        if let Some((26, meta)) = bed_before {
            let (dx, dz) = match meta & 3 {
                0 => (0, 1),
                1 => (-1, 0),
                2 => (0, -1),
                _ => (1, 0),
            };
            let (px, pz) = if meta & 8 == 0 { (x + dx, z + dz) } else { (x - dx, z - dz) };
            if c.get_block(wid, px, y, pz).0 == 26 {
                c.set_block(wid, px, y, pz, 0, 0);
            }
        }
    }

    // ---- combat -----------------------------------------------------------------------------------

    fn on_interact(&mut self, addr: SocketAddr, body: &[u8]) {
        let Ok(i) = play::decode_interact(body) else { return };
        let Some(eid) = self.spawned(addr) else { return };
        if i.action == 1 || i.action == 2 {
            // tapping a shop NPC (either click) opens the shop and never hurts it
            let handled = {
                let mut c = self.core.borrow_mut();
                self.bw.on_interact(&mut c, eid, i.target)
            };
            if handled {
                return;
            }
        }
        match i.action {
            2 => self.attack(eid, i.target),
            1 => self.scripts.entity_interact(eid, i.target),
            _ => {}
        }
    }

    fn attack(&mut self, attacker: i64, victim: i64) {
        let base = self.core.borrow_mut().compute_attack(attacker, victim);
        let Some(base) = base else { return };
        // match rules first (lobby peace, friendly fire, spectators), then plugins
        let ruled = {
            let c = self.core.borrow();
            self.bw.on_damage(&c, victim, Some(attacker), base)
        };
        let Some(base) = ruled else { return };
        let Some(dmg) = self.scripts.damage(victim, Some(attacker), base) else { return };
        let hit = self.core.borrow_mut().apply_damage(victim, Some(attacker), dmg);
        if hit == Hit::Died {
            {
                let mut c = self.core.borrow_mut();
                self.bw.on_death(&mut c, victim, Some(attacker));
            }
            self.scripts.player_death(victim, Some(attacker));
        }
    }

    fn on_player_action(&mut self, addr: SocketAddr, body: &[u8]) {
        let a = match play::decode_player_action(body) {
            Ok(a) => a,
            Err(e) => {
                log::warn!("{addr}: BLOCK_PACKET decode failed type=PLAYER_ACTION len={} error={e} raw={body:02x?}", body.len());
                return;
            }
        };
        let Some(eid) = self.spawned(addr) else {
            log::warn!("{addr}: decoded PLAYER_ACTION but player is not spawned");
            return;
        };
        log::info!("{addr}: PLAYER_ACTION decoded action={} pos={:?} face={}", a.action, a.pos, a.face);
        {
            let mut c = self.core.borrow_mut();
            let tick = c.tick;
            if let Some(p) = c.players.get_mut(&eid) {
                if a.action == action::START_BREAK && p.gamemode == 0 {
                    p.break_start = Some((a.pos.x, a.pos.y, a.pos.z, tick));
                    log::info!("{}: started mining ({}, {}, {})", p.name, a.pos.x, a.pos.y, a.pos.z);
                } else if a.action == action::ABORT_BREAK {
                    p.break_start = None;
                }
            }
        }
        if a.action == action::START_BREAK {
            // Creative breaking is instant. Some clients report it as START_BREAK instead of (or
            // before) RemoveBlock; a second report finds air and is ignored by `break_block`.
            let creative_op = {
                let c = self.core.borrow();
                c.players.get(&eid).map_or(false, |p| p.gamemode == 1) && c.can_use(Sender::Player(eid))
            };
            if creative_op {
                self.break_block(eid, a.pos.x, a.pos.y, a.pos.z);
            }
        }
        if a.action == action::RESPAWN {
            let respawned = self.core.borrow_mut().respawn(eid);
            if respawned {
                {
                    let mut c = self.core.borrow_mut();
                    self.bw.on_respawn(&mut c, eid);
                }
                self.scripts.player_respawn(eid);
            }
        }
    }
}

impl GameHandler for GameServer {
    fn on_connect(&mut self, addr: SocketAddr, _out: &mut Outbox) {
        log::debug!("{addr}: RakNet connected");
    }

    fn on_packet(&mut self, addr: SocketAddr, data: &[u8], out: &mut Outbox) {
        // 0.14 clients wrap every game packet in a 0x8e byte; strip it.
        let inner = if data.first() == Some(&protocol::GAME_PACKET_WRAPPER) { &data[1..] } else { data };
        if !inner.is_empty() {
            self.handle_packet(addr, inner, out, 0);
        }
        self.flush(out);
    }

    fn on_disconnect(&mut self, addr: SocketAddr, reason: DisconnectReason, out: &mut Outbox) {
        let found = {
            let c = self.core.borrow();
            c.by_addr.get(&addr).and_then(|e| c.players.get(e).map(|p| (*e, p.state == PlayerState::Spawned, p.name.clone())))
        };
        let Some((eid, spawned, name)) = found else { return };
        if spawned {
            self.scripts.player_quit(eid);
        }
        {
            let mut c = self.core.borrow_mut();
            self.bw.on_leave(&mut c, eid);
            self.bw.chat_pref.remove(&eid);
            c.remove_player(eid);
        }
        log::info!("{name} left ({reason:?})");
        self.flush(out);
    }

    fn on_tick(&mut self, tick: u64, out: &mut Outbox) {
        if let Some(rx) = &self.console {
            while let Ok(raw) = rx.try_recv() {
                let line = raw.trim();
                let line = line.strip_prefix('/').unwrap_or(line);
                if !line.is_empty() {
                    let mut c = self.core.borrow_mut();
                    if !self.bw.command(&mut c, Sender::Console, line) {
                        c.run_command(Sender::Console, line);
                    }
                }
            }
        }
        let chunks = {
            let mut c = self.core.borrow_mut();
            c.tick = tick;
            let chunks = c.tick_chunks();
            c.tick_items();
            c.tick_time(); // permanent daytime
            chunks
        };
        for eid in chunks.joined {
            {
                let mut c = self.core.borrow_mut();
                c.finish_join(eid);
                self.bw.on_join(&mut c, eid);
            }
            self.scripts.player_join(eid);
        }
        for eid in chunks.switched {
            let mut c = self.core.borrow_mut();
            c.enter_world_view(eid);
            self.bw.on_arrive(&mut c, eid);
        }
        if tick % 6 == 0 {
            // others see the arm swing while someone mines (the miner's own client draws the cracks)
            let mut c = self.core.borrow_mut();
            let mut swinging: Vec<(i64, u32, Vec3)> = Vec::new();
            for p in c.players.values_mut() {
                let Some((_, _, _, t0)) = p.break_start else { continue };
                if tick.saturating_sub(t0) > 400 {
                    p.break_start = None;
                } else if !p.hidden && p.state == PlayerState::Spawned {
                    swinging.push((p.eid, p.world, Vec3 { x: p.x, y: p.y, z: p.z }));
                }
            }
            for (eid, wid, pos) in swinging {
                c.broadcast_near(wid, pos, eid, &play::animate(1, eid));
            }
        }
        {
            let mut c = self.core.borrow_mut();
            self.bw.on_tick(&mut c, tick);
        }
        self.scripts.tick(tick);
        self.scripts.poll_reload(tick);
        self.flush(out);
    }
}
