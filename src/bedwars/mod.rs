//! Bedwars: 4 teams (red, green, yellow, blue), one cloned world per match.
//!
//! * `mod.rs`   - config, instance registry, assignment, lobby, match start / teardown
//! * `rules.rs` - block, damage, death, respawn, void and win rules
//! * `bots.rs`  - bot AI (defend own island, never hit teammates)
//! * `cmds.rs`  - setup commands, shop menu, bed placement
//! * `map.rs`   - persisted setup of the original map
//! * `shop.rs`  - `shop.toml` catalogue
//! * `stats.rs` - persistent stats / coin rewards

pub mod bots;
pub mod cmds;
pub mod map;
pub mod gui;
pub mod rules;
#[cfg(test)]
mod lifecycle;
pub mod shop;
pub mod stats;
pub mod toml_lite;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::binary::Vec3;
use crate::entity::skin;
use crate::protocol::play::text_type;
use crate::state::{Core, WorldId, TEMPLATE};
use crate::world::pcw;

use map::{team_label, MapConfig, Spawner, SpawnerKind, Spot, TEAM_COUNT};
use shop::Shop;
use stats::Stats;

/// Wooden sword + leather armour.
pub const KIT_SWORD: i16 = 268;
pub const BED_ITEM: i16 = 355;
pub const BED_BLOCK: u8 = 26;

const BOT_NAMES: [&str; 16] = [
    "Kai", "Mia", "Noah", "Zoe", "Liam", "Ava", "Finn", "Ivy", "Max", "Luna", "Leo", "Nova", "Jax", "Remy", "Skye", "Rex",
];

#[derive(Clone, Debug)]
pub struct BwConfig {
    /// Players per team (4 teams); bots fill what humans do not.
    pub team_size: usize,
    /// Ticks an instance waits for humans before bots fill the free slots (40 s).
    pub bot_wait_ticks: u64,
    pub countdown_ticks: u64,
    pub end_delay_ticks: u64,
    pub respawn_ticks: u64,
    pub iron_every: u64,
    pub gold_every: u64,
    pub emerald_every: u64,
    /// Maximum dropped resources allowed near each generator.
    pub iron_item_cap: u32,
    pub gold_item_cap: u32,
    pub emerald_item_cap: u32,
    /// Feet below this height = void death.
    pub void_y: f32,
    pub build_limit_y: i32,
    /// Where `maps/`, `shop.toml` and `stats.toml` live.
    pub dir: PathBuf,
    pub autosave_ticks: u64,
    /// Chest-GUI shop (false = chat shop only).
    pub shop_gui: bool,
}

impl Default for BwConfig {
    fn default() -> Self {
        Self {
            team_size: 2,
            bot_wait_ticks: 40 * 20,
            countdown_ticks: 5 * 20,
            end_delay_ticks: 10 * 20,
            respawn_ticks: 5 * 20,
            iron_every: 40,
            gold_every: 240,
            emerald_every: 1200,
            iron_item_cap: 20,
            gold_item_cap: 10,
            emerald_item_cap: 4,
            void_y: 0.0,
            build_limit_y: 120,
            dir: PathBuf::from("."),
            autosave_ticks: 600,
            shop_gui: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Countdown { start_at: u64 },
    Running,
    Ending { until: u64 },
}

#[derive(Clone, Debug)]
pub struct Member {
    pub team: usize,
    /// Still in the game (not eliminated).
    pub alive: bool,
    pub bot: bool,
    pub respawn_at: Option<u64>,
    /// Last attacker and when, for void-kill credit.
    pub last_hit: Option<(i64, u64)>,
}

pub struct Instance {
    pub world: WorldId,
    pub phase: Phase,
    pub created: u64,
    pub first_join: Option<u64>,
    pub started: u64,
    pub members: HashMap<i64, Member>,
    pub bed_alive: [bool; TEAM_COUNT],
    /// Blocks players placed (the only ones they may break).
    pub placed: HashSet<(i32, i32, i32)>,
    /// Snapshot of the template setup taken when the world was cloned.
    pub map: MapConfig,
    pub winner: Option<usize>,
    pub kills: HashMap<i64, u32>,
    /// Emerald-generator holograms paired with their map spawner coordinates.
    pub emerald_holograms: Vec<(i64, Spawner)>,
}

impl Instance {
    pub fn team_count(&self, team: usize) -> usize {
        self.members.values().filter(|m| m.team == team).count()
    }

    pub fn alive_in_team(&self, team: usize) -> usize {
        self.members.values().filter(|m| m.team == team && m.alive).count()
    }

    /// Teams that still have somebody playing.
    pub fn live_teams(&self) -> Vec<usize> {
        (0..TEAM_COUNT).filter(|&t| self.alive_in_team(t) > 0).collect()
    }
}

/// Chat-shop state of one player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    Categories,
    Items(usize),
}

pub struct Bedwars {
    pub cfg: BwConfig,
    /// Setup of the original map (what new instances copy).
    pub map: MapConfig,
    pub shop: Shop,
    pub stats: Stats,
    pub instances: HashMap<WorldId, Instance>,
    pub sessions: HashMap<i64, Session>,
    /// Open chest shops.
    pub guis: HashMap<i64, gui::Gui>,
    /// Players who chose `/shop chat`.
    pub chat_pref: HashSet<i64>,
    /// Operator -> team whose bed they are about to place.
    pub pending_bed: HashMap<i64, usize>,
    pub brains: HashMap<i64, bots::Brain>,
    /// NPC entities shown in the original map (kept in sync with `map.npcs`).
    pub template_npcs: Vec<i64>,
    /// Temporary slime safety platforms: (world, block position, expiry tick).
    pub temporary_blocks: Vec<(WorldId, (i32, i32, i32), u64)>,
    /// Automatically-fused TNT blocks: (world, position, detonation tick).
    pub primed_tnt: Vec<(WorldId, (i32, i32, i32), u64)>,
    pub(crate) shop_mtime: Option<std::time::SystemTime>,
    next_bot: usize,
    /// PNG skins from the `skins` folder (bots and shop keeper).
    skins: skin::SkinSet,
}

impl Bedwars {
    pub fn new(cfg: BwConfig, core: &mut Core) -> Bedwars {
        let map = MapConfig::load(&cfg.dir.join("maps").join("original.toml")).unwrap_or_else(|e| {
            log::error!("map setup: {e}; starting with an empty setup");
            MapConfig::default()
        });
        let shop_path = cfg.dir.join("shop.toml");
        let shop = Shop::load_or_create(&shop_path).unwrap_or_else(|e| {
            log::error!("shop: {e}; using the built-in shop");
            Shop::builtin()
        });
        let stats = Stats::load(&cfg.dir.join("stats.toml")).unwrap_or_else(|e| {
            log::error!("stats: {e}");
            Stats::default()
        });
        let shop_mtime = std::fs::metadata(&shop_path).and_then(|m| m.modified()).ok();
        let skins = skin::SkinSet::load(&cfg.dir.join("skins"));
        let mut bw = Bedwars {
            cfg,
            map,
            shop,
            stats,
            instances: HashMap::new(),
            sessions: HashMap::new(),
            guis: HashMap::new(),
            chat_pref: HashSet::new(),
            pending_bed: HashMap::new(),
            brains: HashMap::new(),
            template_npcs: Vec::new(),
            temporary_blocks: Vec::new(),
            primed_tnt: Vec::new(),
            shop_mtime,
            next_bot: 0,
            skins,
        };
        bw.sync_template_npcs(core);
        let problems = bw.map.problems();
        if problems.is_empty() {
            log::info!("map setup complete: matches can start");
        } else {
            log::warn!("map setup incomplete, matches cannot start yet. Missing: {}", problems.join("; "));
        }
        bw
    }

    pub fn capacity(&self) -> usize {
        self.cfg.team_size.max(1) * TEAM_COUNT
    }

    pub fn map_path(&self) -> PathBuf {
        self.cfg.dir.join("maps").join("original.toml")
    }

    pub fn world_path(&self) -> PathBuf {
        self.cfg.dir.join("maps").join("original.pcw")
    }

    // ---- instances --------------------------------------------------------------------------------

    /// Humans in the world plus registered members (covers players that are
    /// still logging in and have not been registered yet).
    pub fn occupancy(&self, core: &Core, wid: WorldId) -> usize {
        let registered = self.instances.get(&wid).map_or(0, |i| i.members.len());
        let unregistered = core
            .players
            .values()
            .filter(|p| p.world == wid && !p.is_bot && !self.instances.get(&wid).map_or(false, |i| i.members.contains_key(&p.eid)))
            .count();
        registered + unregistered
    }

    /// A waiting instance with a free slot (the fullest first, so matches
    /// fill up), or a brand-new clone. A running match never blocks anyone.
    pub fn pick_instance(&mut self, core: &mut Core) -> WorldId {
        let cap = self.capacity();
        let mut best: Option<(usize, WorldId)> = None;
        for (wid, inst) in &self.instances {
            if !matches!(inst.phase, Phase::Waiting | Phase::Countdown { .. }) {
                continue;
            }
            let occ = self.occupancy(core, *wid);
            if occ < cap && best.map_or(true, |(b, bw)| occ > b || (occ == b && *wid < bw)) {
                best = Some((occ, *wid));
            }
        }
        match best {
            Some((_, wid)) => wid,
            None => self.create_instance(core),
        }
    }

    /// Clones the template map into a fresh match world with its setup.
    pub fn create_instance(&mut self, core: &mut Core) -> WorldId {
        let wid = core.fork_world();
        let shop_skin = self.skins.shop();
        for n in &self.map.npcs {
            core.add_npc(wid, Vec3 { x: n.x, y: n.y, z: n.z }, n.yaw, SHOP_NAME, shop_skin.clone());
        }
        let inst = Instance {
            world: wid,
            phase: Phase::Waiting,
            created: core.tick,
            first_join: None,
            started: 0,
            members: HashMap::new(),
            bed_alive: [false; TEAM_COUNT],
            placed: HashSet::new(),
            map: self.map.clone(),
            winner: None,
            kills: HashMap::new(),
            emerald_holograms: Vec::new(),
        };
        self.instances.insert(wid, inst);
        log::info!("match world {wid} created ({} instance(s) active)", self.instances.len());
        wid
    }

    /// Where a player in the lobby of `wid` stands: the waiting room, else the world spawn.
    pub fn lobby_spot(&self, core: &Core, wid: WorldId) -> Spot {
        let wr = self.instances.get(&wid).map(|i| i.map.waiting_room).unwrap_or(self.map.waiting_room);
        wr.unwrap_or_else(|| {
            let s = core.spawn_of(wid);
            Spot { x: s.0 as f32 + 0.5, y: s.1 as f32, z: s.2 as f32 + 0.5, yaw: 0.0, pitch: 0.0 }
        })
    }

    /// Called at login, before the join sequence: which world and position a
    /// new connection gets.
    pub fn assign_login(&mut self, core: &mut Core) -> (WorldId, Spot) {
        let wid = self.pick_instance(core);
        (wid, self.lobby_spot(core, wid))
    }

    fn smallest_team(&self, inst: &Instance) -> usize {
        (0..TEAM_COUNT).min_by_key(|&t| (inst.team_count(t), t)).unwrap_or(0)
    }

    /// Adds a member to a waiting instance and puts the player into lobby state.
    pub fn register_member(&mut self, core: &mut Core, wid: WorldId, eid: i64, bot: bool) -> Option<usize> {
        let tick = core.tick;
        let team = {
            let inst = self.instances.get(&wid)?;
            self.smallest_team(inst)
        };
        let inst = self.instances.get_mut(&wid)?;
        inst.members.insert(eid, Member { team, alive: true, bot, respawn_at: None, last_hit: None });
        if !bot && inst.first_join.is_none() {
            inst.first_join = Some(tick);
        }
        if !bot {
            self.lobby_state(core, eid);
        }
        Some(team)
    }

    /// Adventure mode, empty inventory, full health, respawn in the waiting room.
    pub fn lobby_state(&mut self, core: &mut Core, eid: i64) {
        let wid = core.world_of(eid);
        let spot = self.lobby_spot(core, wid);
        if let Some(p) = core.players.get_mut(&eid) {
            p.spawn_override = Some((spot.x, spot.y, spot.z));
            p.kb = None;
            p.dead = false;
            p.health = 20.0;
        }
        let practice = self.practice(wid);
        core.set_gamemode(eid, if practice { 0 } else { 2 });
        core.clear_inventory(eid);
        core.set_armor(eid, [0; 4]);
        if practice {
            // the match can never start (map setup incomplete): give a small kit so building,
            // fighting, fall damage and the void can be tried out
            core.give_item(eid, KIT_SWORD, 1, 0);
            core.give_item(eid, 35, 64, 0);
        }
        core.send(eid, crate::protocol::play::set_health(20));
    }

    /// Practice mode: the map setup is incomplete, so no match can ever start. The waiting
    /// room then plays by the in-match rules (build own blocks, damage, fall damage, void death).
    pub fn practice(&self, wid: WorldId) -> bool {
        self.instances.get(&wid).map_or(false, |i| i.phase == Phase::Waiting && !i.map.problems().is_empty())
    }

    /// A player finished joining the server.
    pub fn on_join(&mut self, core: &mut Core, eid: i64) {
        let wid = core.world_of(eid);
        let Some(team) = self.register_member(core, wid, eid, false) else { return };
        let cap = self.capacity();
        let occ = self.instances.get(&wid).map_or(0, |i| i.members.len());
        core.send_text(eid, text_type::RAW, "", &format!("\u{a7}6[Bedwars] \u{a7}eYou are on the {} \u{a7}eteam. Players: {occ}/{cap}", team_label(team)));
        if let Some(sp) = core.players.get(&eid).map(|p| p.name.clone()) {
            log::info!("{sp} joined match world {wid} as {}", map::TEAM_NAMES[team]);
        }
    }

    /// After a world switch finished streaming.
    pub fn on_arrive(&mut self, core: &mut Core, eid: i64) {
        let wid = core.world_of(eid);
        if wid == TEMPLATE {
            core.set_gamemode(eid, 1);
            core.send_text(eid, text_type::RAW, "", "\u{a7}6[Bedwars] \u{a7}eYou are in the \u{a7}6original map\u{a7}e. Changes here are saved and copied to every new match. /lobby to leave.");
            return;
        }
        if let Some(team) = self.instances.get(&wid).and_then(|i| i.members.get(&eid)).map(|m| m.team) {
            self.lobby_state(core, eid);
            core.send_text(eid, text_type::RAW, "", &format!("\u{a7}6[Bedwars] \u{a7}eYou are on the {} \u{a7}eteam.", team_label(team)));
        }
    }

    // ---- lobby / leaving ------------------------------------------------------------------------

    /// Removes a player from whatever instance they belong to (disconnect or move).
    pub fn on_leave(&mut self, core: &mut Core, eid: i64) {
        self.sessions.remove(&eid);
        self.guis.remove(&eid);
        self.pending_bed.remove(&eid);
        let Some(wid) = self.instances.iter().find(|(_, i)| i.members.contains_key(&eid)).map(|(w, _)| *w) else { return };
        let running = self.instances.get(&wid).map_or(false, |i| i.phase == Phase::Running);
        if let Some(inst) = self.instances.get_mut(&wid) {
            inst.members.remove(&eid);
        }
        if running {
            self.check_winner(core, wid);
        }
    }

    /// Sends a player to a waiting match world (a new clone if all are busy).
    pub fn send_to_lobby(&mut self, core: &mut Core, eid: i64) {
        self.on_leave(core, eid);
        let target = self.pick_instance(core);
        self.register_member(core, target, eid, false);
        let s = self.lobby_spot(core, target);
        if let Some(p) = core.players.get_mut(&eid) {
            p.hidden = false;
        }
        core.move_to_world(eid, target, s.x, s.y, s.z, s.yaw, s.pitch);
    }

    /// Operator visit of the original template map.
    pub fn visit_original(&mut self, core: &mut Core, eid: i64) {
        self.on_leave(core, eid);
        let spot = self.map.waiting_room.unwrap_or_else(|| {
            let s = core.spawn_of(TEMPLATE);
            Spot { x: s.0 as f32 + 0.5, y: s.1 as f32, z: s.2 as f32 + 0.5, yaw: 0.0, pitch: 0.0 }
        });
        if let Some(p) = core.players.get_mut(&eid) {
            p.hidden = false;
            p.spawn_override = Some((spot.x, spot.y, spot.z));
        }
        core.move_to_world(eid, TEMPLATE, spot.x, spot.y, spot.z, spot.yaw, spot.pitch);
    }

    // ---- bots -----------------------------------------------------------------------------------------

    /// Fills every team up to `team_size` with bots.
    pub fn fill_bots(&mut self, core: &mut Core, wid: WorldId) -> usize {
        let spot = self.lobby_spot(core, wid);
        let size = self.cfg.team_size.max(1);
        let mut added = 0;
        for team in 0..TEAM_COUNT {
            loop {
                let have = self.instances.get(&wid).map_or(size, |i| i.team_count(team));
                if have >= size {
                    break;
                }
                let name = format!("{}{}", map::TEAM_COLORS[team], BOT_NAMES[self.next_bot % BOT_NAMES.len()]);
                let bot_skin = self.skins.bot(self.next_bot, team);
                self.next_bot += 1;
                let (jx, jz) = (((added % 5) as f32 - 2.0) * 0.8, ((added / 5) as f32) * 0.8);
                let eid = core.add_bot(wid, &name, bot_skin, spot.x + jx, spot.y, spot.z + jz, spot.yaw);
                if let Some(inst) = self.instances.get_mut(&wid) {
                    inst.members.insert(eid, Member { team, alive: true, bot: true, respawn_at: None, last_hit: None });
                }
                self.brains.insert(eid, bots::Brain::new(team));
                added += 1;
            }
        }
        added
    }

    // ---- match start / teardown ------------------------------------------------------------------------

    /// Wooden sword and leather armour.
    pub fn equip(&mut self, core: &mut Core, eid: i64) {
        core.clear_inventory(eid);
        core.give_item(eid, KIT_SWORD, 1, 0);
        core.set_armor(eid, shop::ArmorSet::Leather.ids());
        core.set_gamemode(eid, 0);
        if let Some(p) = core.players.get_mut(&eid) {
            p.health = 20.0;
            p.dead = false;
        }
        core.send(eid, crate::protocol::play::set_health(20));
    }

    pub fn start_match(&mut self, core: &mut Core, wid: WorldId) {
        let tick = core.tick;
        let Some(inst) = self.instances.get_mut(&wid) else { return };
        inst.phase = Phase::Running;
        inst.started = tick;
        let teams_used: Vec<bool> = (0..TEAM_COUNT).map(|t| inst.team_count(t) > 0).collect();
        inst.bed_alive = [teams_used[0], teams_used[1], teams_used[2], teams_used[3]];
        let map = inst.map.clone();
        inst.emerald_holograms.clear();
        for spawner in map.spawners.iter().copied().filter(|s| s.kind == SpawnerKind::Advanced) {
            let initial_seconds = (self.cfg.emerald_every.max(1) + 19) / 20;
            let label = format!("§aEMERALD §7| §eNext: {initial_seconds}s");
            let hologram = core.add_hologram(
                wid,
                Vec3 { x: spawner.x as f32 + 0.5, y: spawner.y as f32 + 1.0, z: spawner.z as f32 + 0.5 },
                &label,
            );
            inst.emerald_holograms.push((hologram, spawner));
        }
        let members: Vec<(i64, usize)> = inst.members.iter().map(|(e, m)| (*e, m.team)).collect();
        for t in 0..TEAM_COUNT {
            if !teams_used[t] {
                if let Some(b) = map.beds[t] {
                    core.set_block(wid, b.foot.x, b.foot.y, b.foot.z, 0, 0);
                    core.set_block(wid, b.head.x, b.head.y, b.head.z, 0, 0);
                }
            }
        }
        for (eid, team) in members {
            let s = match map.teams[team] {
                Some(s) => s,
                None => {
                    // no spawn set for this team: use its bed, else any other team spawn, never leave them behind
                    let fb = map
                        .beds[team]
                        .map(|b| Spot { x: b.foot.x as f32 + 0.5, y: b.foot.y as f32 + 1.0, z: b.foot.z as f32 + 0.5, yaw: 0.0, pitch: 0.0 })
                        .or_else(|| map.teams.iter().flatten().next().copied());
                    match fb {
                        Some(s) => {
                            log::warn!("team {} has no spawn point (set one with the team spawn command); using ({:.1}, {:.1}, {:.1})", team_label(team), s.x, s.y, s.z);
                            s
                        }
                        None => {
                            log::warn!("team {} has no spawn point and no bed: eid {eid} stays in the waiting area", team_label(team));
                            continue;
                        }
                    }
                }
            };
            if let Some(p) = core.players.get_mut(&eid) {
                p.spawn_override = Some((s.x, s.y, s.z));
            }
            self.equip(core, eid);
            log::info!("match start: teleporting eid={eid} to the {} team spawn ({:.1}, {:.1}, {:.1})", team_label(team), s.x, s.y, s.z);
            core.teleport_facing(eid, s.x, s.y, s.z, s.yaw, s.pitch);
            if let Some(b) = self.brains.get_mut(&eid) {
                b.home = Vec3 { x: s.x, y: s.y, z: s.z };
            }
            core.send_text(eid, text_type::RAW, "", &format!("\u{a7}6[Bedwars] \u{a7}eYou are on the {} \u{a7}eteam. Protect your bed, destroy theirs!", team_label(team)));
        }
        core.broadcast_text(wid, text_type::RAW, "", "\u{a7}6[Bedwars] \u{a7}aThe game has started! \u{a7}eProtect your bed and eliminate the other teams.");
        log::info!("match started in world {wid}");
    }

    /// Moves everyone to a new lobby, removes bots, drops the cloned world.
    pub fn teardown(&mut self, core: &mut Core, wid: WorldId) {
        let Some(inst) = self.instances.remove(&wid) else { return };
        for (eid, m) in &inst.members {
            if m.bot {
                self.brains.remove(eid);
                core.remove_player(*eid);
            }
        }
        let humans: Vec<i64> = core.players.values().filter(|p| p.world == wid && !p.is_bot).map(|p| p.eid).collect();
        for eid in humans {
            self.sessions.remove(&eid);
            self.send_to_lobby(core, eid);
        }
        core.drop_world(wid);
        log::info!("match world {wid} torn down ({} instance(s) active)", self.instances.len());
    }

    // ---- per-tick --------------------------------------------------------------------------------------

    pub fn on_tick(&mut self, core: &mut Core, tick: u64) {
        // Expire temporary slime platforms after 25 seconds.
        let mut keep = Vec::with_capacity(self.temporary_blocks.len());
        for (wid, (x, y, z), expiry) in self.temporary_blocks.drain(..) {
            if tick >= expiry {
                if core.get_block(wid, x, y, z).0 == 165 { core.set_block(wid, x, y, z, 0, 0); }
            } else { keep.push((wid, (x, y, z), expiry)); }
        }
        self.temporary_blocks = keep;
        self.tick_tnt(core, tick);
        let ids: Vec<WorldId> = {
            let mut v: Vec<WorldId> = self.instances.keys().copied().collect();
            v.sort_unstable();
            v
        };
        for wid in ids {
            self.tick_instance(core, wid, tick);
        }
        self.tick_gui(core, tick);
        self.gc(core);
        if tick % 40 == 0 {
            self.evict_non_ops(core);
        }
        if tick % 100 == 0 {
            self.reload_shop_if_changed();
        }
        if self.cfg.autosave_ticks > 0 && tick % self.cfg.autosave_ticks == 0 {
            self.save_all(core);
        }
    }

    fn tick_instance(&mut self, core: &mut Core, wid: WorldId, tick: u64) {
        let Some(inst) = self.instances.get(&wid) else { return };
        let (phase, first_join) = (inst.phase, inst.first_join);
        let cap = self.capacity();
        match phase {
            Phase::Waiting => {
                let Some(first) = first_join else { return };
                let (members, inst_problems) = (inst.members.len(), inst.map.problems());
                if !inst_problems.is_empty() {
                    if self.map.problems().is_empty() {
                        // the template was finished after this clone was made: move everyone to a fresh clone
                        let humans: Vec<i64> = core.players_in(wid).into_iter().filter(|e| core.players.get(e).map_or(false, |p| !p.is_bot)).collect();
                        for e in humans {
                            self.send_to_lobby(core, e);
                        }
                    } else if tick % 600 == 0 {
                        let msg = format!("\u{a7}c[Bedwars] Map not set up yet. Missing: {}", inst_problems.join(", "));
                        core.broadcast_text(wid, text_type::RAW, "", &msg);
                    }
                    return;
                }
                let waited = tick.saturating_sub(first);
                if members >= cap {
                    self.begin_countdown(core, wid, tick);
                } else if waited >= self.cfg.bot_wait_ticks {
                    let n = self.fill_bots(core, wid);
                    core.broadcast_text(wid, text_type::RAW, "", &format!("\u{a7}6[Bedwars] \u{a7}7Not enough players: {n} bot(s) joined to fill the teams."));
                    self.begin_countdown(core, wid, tick);
                } else if tick % 20 == 0 {
                    let left = (self.cfg.bot_wait_ticks - waited) / 20;
                    let msg = format!("\u{a7}eWaiting for players {members}/{cap}  \u{a7}7bots join in {left}s");
                    for e in core.players_in(wid) {
                        core.send_text(e, text_type::POPUP, "", &msg);
                    }
                }
            }
            Phase::Countdown { start_at } => {
                if tick >= start_at {
                    self.start_match(core, wid);
                } else if tick % 20 == 0 {
                    let msg = format!("\u{a7}aStarting in {}", (start_at - tick + 19) / 20);
                    for e in core.players_in(wid) {
                        core.send_text(e, text_type::POPUP, "", &msg);
                    }
                }
            }
            Phase::Running => {
                self.tick_running(core, wid, tick);
            }
            Phase::Ending { until } => {
                if tick >= until {
                    self.teardown(core, wid);
                }
            }
        }
    }

    fn begin_countdown(&mut self, core: &mut Core, wid: WorldId, tick: u64) {
        if let Some(inst) = self.instances.get_mut(&wid) {
            inst.phase = Phase::Countdown { start_at: tick + self.cfg.countdown_ticks };
        }
        core.broadcast_text(wid, text_type::RAW, "", "\u{a7}6[Bedwars] \u{a7}aThe match is full: starting soon!");
    }

    /// Drops instances nobody is in any more (empty lobbies, abandoned matches).
    fn gc(&mut self, core: &mut Core) {
        let dead: Vec<WorldId> = self
            .instances
            .keys()
            .copied()
            .filter(|w| !core.players.values().any(|p| p.world == *w && !p.is_bot))
            .collect();
        for wid in dead {
            // keep one empty waiting lobby around only if it is the sole instance? Not needed: clones are cheap.
            if let Some(inst) = self.instances.remove(&wid) {
                for (eid, m) in &inst.members {
                    if m.bot {
                        self.brains.remove(eid);
                        core.remove_player(*eid);
                    }
                }
            }
            core.drop_world(wid);
            log::info!("match world {wid} removed (empty)");
        }
    }

    /// Only operators may be in the original map.
    fn evict_non_ops(&mut self, core: &mut Core) {
        let intruders: Vec<i64> = core
            .players
            .values()
            .filter(|p| p.world == TEMPLATE && !p.is_bot && !p.switching && !core.ops.contains(&p.name.to_ascii_lowercase()))
            .map(|p| p.eid)
            .collect();
        for eid in intruders {
            self.send_to_lobby(core, eid);
        }
    }

    // ---- persistence ------------------------------------------------------------------------------------

    pub fn save_map(&self) {
        if let Err(e) = self.map.save(&self.map_path()) {
            log::error!("could not save map setup: {e}");
        }
    }

    /// Writes the template world, setup and stats if they changed.
    pub fn save_all(&mut self, core: &mut Core) {
        if let Some(w) = core.worlds.get_mut(&TEMPLATE) {
            if w.has_unsaved() {
                w.materialize_all();
                match pcw::save(w, &self.world_path()) {
                    Ok(()) => {
                        w.mark_saved();
                        log::info!("original map saved to {}", self.world_path().display());
                    }
                    Err(e) => log::error!("could not save the original map: {e}"),
                }
            }
        }
        if self.stats.dirty {
            if let Err(e) = self.stats.save(&self.cfg.dir.join("stats.toml")) {
                log::error!("could not save stats: {e}");
            }
        }
    }

    pub fn reload_shop(&mut self) -> Result<(), String> {
        let path = self.cfg.dir.join("shop.toml");
        let shop = Shop::load_or_create(&path)?;
        self.shop = shop;
        self.shop_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(())
    }

    fn reload_shop_if_changed(&mut self) {
        let path = self.cfg.dir.join("shop.toml");
        let m = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if m.is_some() && m != self.shop_mtime {
            match self.reload_shop() {
                Ok(()) => log::info!("shop.toml reloaded"),
                Err(e) => {
                    log::error!("shop.toml not reloaded: {e}");
                    self.shop_mtime = m; // do not retry until it changes again
                }
            }
        }
    }

    /// Makes the NPCs of the original map match `map.npcs`.
    pub fn sync_template_npcs(&mut self, core: &mut Core) {
        for id in self.template_npcs.drain(..) {
            core.remove_npc(id);
        }
        let shop_skin = self.skins.shop();
        for n in &self.map.npcs {
            let id = core.add_npc(TEMPLATE, Vec3 { x: n.x, y: n.y, z: n.z }, n.yaw, SHOP_NAME, shop_skin.clone());
            self.template_npcs.push(id);
        }
    }
}

pub const SHOP_NAME: &str = "\u{a7}6\u{a7}lITEM SHOP";
