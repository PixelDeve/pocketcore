//! Match rules: building, beds, damage, death, respawn, void, winning, generators.

use crate::binary::Vec3;
use crate::protocol::play::{self, text_type};
use crate::state::{Core, Hit, WorldId, TEMPLATE};

use super::map::{team_label, SpawnerKind, TEAM_COLORS, TEAM_COUNT, TEAM_NAMES};
use super::shop::{EMERALD, GOLD, IRON};
use super::{Bedwars, Phase, BED_BLOCK};

/// Legacy LevelEvent sound: click. 1007 is the GHAST sound, not a generic alert.
const SOUND_ALERT: i16 = 1000;
/// Attacker credit for void deaths lasts this long (ticks).
const CREDIT_TICKS: u64 = 200;

impl Bedwars {
    /// Team colour + name for chat (bots already carry their colour).
    pub fn display(&self, core: &Core, wid: WorldId, eid: i64) -> String {
        let name = core.players.get(&eid).map(|p| p.name.clone()).unwrap_or_else(|| "?".into());
        match self.instances.get(&wid).and_then(|i| i.members.get(&eid)) {
            Some(m) if !m.bot => format!("{}{}\u{a7}r", TEAM_COLORS[m.team], name),
            _ => format!("{name}\u{a7}r"),
        }
    }

    fn sound(core: &mut Core, wid: WorldId, at: Vec3) {
        core.broadcast(wid, &play::level_event(SOUND_ALERT, at, 0));
    }

    // ---- building -------------------------------------------------------------------------------

    /// Operators in creative mode may build and break anywhere (match rules do not apply).
    fn creative_op(core: &Core, eid: i64) -> bool {
        core.players.get(&eid).map_or(false, |p| p.gamemode == 1 && core.ops.contains(&p.name.to_ascii_lowercase()))
    }

    fn is_op(core: &Core, eid: i64) -> bool {
        core.players.get(&eid).map_or(false, |p| core.ops.contains(&p.name.to_ascii_lowercase()))
    }

    /// `None` = allowed. `Some(msg)` = refuse (msg shown as a popup if not empty).
    pub fn on_block_place(&mut self, core: &Core, eid: i64, x: i32, y: i32, z: i32, id: u8) -> Option<&'static str> {
        let wid = core.world_of(eid);
        if wid == TEMPLATE {
            // only operators edit the original map (normal players are never meant to be in it)
            return if Self::is_op(core, eid) { None } else { Some("\u{a7}cOnly operators can edit the original map") };
        }
        if Self::creative_op(core, eid) {
            return None; // operators in creative build anywhere
        }
        let limit = self.cfg.build_limit_y;
        let Some(inst) = self.instances.get_mut(&wid) else { return Some("") };
        let Some(m) = inst.members.get(&eid) else { return Some("") };
        let open = inst.phase == Phase::Running || (inst.phase == Phase::Waiting && !inst.map.problems().is_empty());
        if !open || !m.alive {
            return Some("\u{a7}cYou cannot build right now");
        }
        if id == BED_BLOCK {
            return Some("\u{a7}cYou cannot place beds");
        }
        if y > limit {
            return Some("\u{a7}cBuild height limit reached");
        }
        for s in inst.map.teams.iter().flatten() {
            if x == s.x.floor() as i32 && z == s.z.floor() as i32 && (y == s.y.floor() as i32 || y == s.y.floor() as i32 + 1) {
                return Some("\u{a7}cYou cannot build here");
            }
        }
        if inst.map.spawners.iter().any(|g| g.x == x && g.z == z && (g.y == y || g.y + 1 == y)) {
            return Some("\u{a7}cYou cannot build on a generator");
        }
        inst.placed.insert((x, y, z));
        None
    }

    /// `None` = allowed. Enemy beds and player-placed blocks only.
    pub fn on_block_break(&mut self, core: &mut Core, eid: i64, x: i32, y: i32, z: i32) -> Option<&'static str> {
        let wid = core.world_of(eid);
        if wid == TEMPLATE {
            if !Self::is_op(core, eid) {
                return Some("\u{a7}cOnly operators can edit the original map");
            }
            // keep the registry honest when an operator removes a registered bed
            if let Some(t) = self.map.bed_at(x, y, z) {
                if let Some(b) = self.map.beds[t].take() {
                    core.set_block(TEMPLATE, b.foot.x, b.foot.y, b.foot.z, 0, 0);
                    core.set_block(TEMPLATE, b.head.x, b.head.y, b.head.z, 0, 0);
                    self.save_map();
                    core.send_text(eid, text_type::RAW, "", &format!("\u{a7}eUnregistered the {} bed.", TEAM_NAMES[t]));
                }
            }
            return None;
        }
        if Self::creative_op(core, eid) {
            // Creative operators may break anything, but a registered bed of a running match
            // still counts as destroyed (announcements, no more respawns for that team).
            let hit = self.instances.get(&wid).filter(|i| i.phase == Phase::Running).and_then(|i| {
                let t = i.map.bed_at(x, y, z)?;
                i.bed_alive[t].then_some(t)
            });
            if let Some(team) = hit {
                self.bed_destroyed(core, wid, eid, team, (x, y, z));
            }
            return None;
        }
        let Some(inst) = self.instances.get_mut(&wid) else { return Some("") };
        let Some(m) = inst.members.get(&eid) else { return Some("") };
        let open = inst.phase == Phase::Running || (inst.phase == Phase::Waiting && !inst.map.problems().is_empty());
        if !open || !m.alive {
            return Some("\u{a7}cYou cannot break blocks right now");
        }
        let my_team = m.team;
        if let Some(team) = inst.map.bed_at(x, y, z) {
            if team == my_team {
                return Some("\u{a7}cYou cannot break your own bed!");
            }
            if !inst.bed_alive[team] {
                return None;
            }
            self.bed_destroyed(core, wid, eid, team, (x, y, z));
            return None;
        }
        if inst.placed.remove(&(x, y, z)) {
            return None;
        }
        Some("\u{a7}cYou can only break blocks placed by players")
    }

    /// A team's bed was broken: both halves vanish, everyone is told, the team stops respawning.
    fn bed_destroyed(&mut self, core: &mut Core, wid: WorldId, eid: i64, team: usize, at: (i32, i32, i32)) {
        let Some(inst) = self.instances.get_mut(&wid) else { return };
        inst.bed_alive[team] = false;
        let bed = inst.map.beds[team];
        let victims: Vec<i64> = inst.members.iter().filter(|(_, mm)| mm.team == team && !mm.bot).map(|(e, _)| *e).collect();
        let breaker_human = !inst.members.get(&eid).map_or(true, |mm| mm.bot);
        if let Some(b) = bed {
            core.set_block(wid, b.foot.x, b.foot.y, b.foot.z, 0, 0);
            core.set_block(wid, b.head.x, b.head.y, b.head.z, 0, 0);
        }
        let who = self.display(core, wid, eid);
        log::info!("BED DESTROYED world={wid} team={} by eid={eid} at {at:?}, notifying {} team member(s)", TEAM_NAMES[team], victims.len());
        core.broadcast_text(
            wid,
            text_type::RAW,
            "",
            &format!("\n\u{a7}6\u{a7}lBED DESTROYED \u{a7}r\u{a7}7> {} \u{a7}7bed was destroyed by {who}\u{a7}7!\n", team_label(team)),
        );
        Self::sound(core, wid, Vec3 { x: at.0 as f32, y: at.1 as f32, z: at.2 as f32 });
        for v in victims {
            core.send_text(v, text_type::RAW, "", "\u{a7}c\u{a7}lYour bed was destroyed! \u{a7}r\u{a7}cYou will no longer respawn!");
            core.send_text(v, text_type::POPUP, "", "\u{a7}c\u{a7}lYour bed was destroyed!");
            core.send_text(v, text_type::TIP, "", "\u{a7}cYou will no longer respawn!");
        }
        if breaker_human {
            if let Some(n) = core.players.get(&eid).map(|p| p.name.clone()) {
                self.stats.add_bed(&n);
            }
        }
    }



    /// Light a placed TNT block: it explodes after the classic 4 seconds (fuse 80 ticks).
    /// Only sounds and particles the client is already known to handle are used: unknown level
    /// events (the old 1008 / 2008) can crash the 0.14 client.
    pub fn prime_tnt(&mut self, core: &mut Core, wid: WorldId, pos: (i32, i32, i32)) {
        if self.primed_tnt.iter().any(|(w, p, _)| *w == wid && *p == pos) {
            return;
        }
        self.primed_tnt.push((wid, pos, core.tick + 80));
        let at = Vec3 { x: pos.0 as f32 + 0.5, y: pos.1 as f32 + 0.5, z: pos.2 as f32 + 0.5 };
        Self::sound(core, wid, at);
        core.broadcast_text(wid, text_type::TIP, "", "\u{a7}c\u{a7}lTNT ignited! \u{a7}r\u{a7}7Explodes in 4 seconds");
        log::info!("TNT primed in world {wid} at {pos:?}");
    }

    /// Explode TNT whose fuse ran out: destroys nearby player-placed blocks (never the map or beds),
    /// sets off neighbouring TNT and hurts players within 4 blocks.
    pub fn tick_tnt(&mut self, core: &mut Core, tick: u64) {
        if self.primed_tnt.is_empty() {
            return;
        }
        let fuses = std::mem::take(&mut self.primed_tnt);
        let mut pending: Vec<(WorldId, (i32, i32, i32), u64)> = Vec::with_capacity(fuses.len());
        for (wid, pos, due) in fuses {
            if tick < due {
                pending.push((wid, pos, due));
                continue;
            }
            let (x, y, z) = pos;
            if core.get_block(wid, x, y, z).0 != 46 {
                continue; // the block was broken before the fuse ran out
            }
            let at = Vec3 { x: x as f32 + 0.5, y: y as f32 + 0.5, z: z as f32 + 0.5 };
            core.set_block(wid, x, y, z, 0, 0);
            if let Some(inst) = self.instances.get_mut(&wid) {
                inst.placed.remove(&pos);
            }
            Self::sound(core, wid, at);
            core.broadcast(wid, &play::level_event(play::EVENT_PARTICLE_DESTROY, at, 46));
            let mut removed = 0u32;
            if self.instances.contains_key(&wid) {
                for dx in -3i32..=3 {
                    for dy in -3i32..=3 {
                        for dz in -3i32..=3 {
                            if dx * dx + dy * dy + dz * dz > 9 || (dx, dy, dz) == (0, 0, 0) {
                                continue;
                            }
                            let cell = (x + dx, y + dy, z + dz);
                            let (bid, bmeta) = core.get_block(wid, cell.0, cell.1, cell.2);
                            if bid == 0 {
                                continue;
                            }
                            if bid == 46 {
                                // chain reaction with a short fuse
                                let delay = 10 + ((dx.abs() + dy.abs() + dz.abs()) as u64 * 4) % 20;
                                if !pending.iter().any(|(w, p, _)| *w == wid && *p == cell) {
                                    pending.push((wid, cell, tick + delay));
                                }
                                continue;
                            }
                            let placed = self.instances.get_mut(&wid).map_or(false, |i| i.placed.remove(&cell));
                            if placed && core.set_block(wid, cell.0, cell.1, cell.2, 0, 0) {
                                removed += 1;
                                if removed % 3 == 1 {
                                    let c = Vec3 { x: cell.0 as f32 + 0.5, y: cell.1 as f32 + 0.5, z: cell.2 as f32 + 0.5 };
                                    core.broadcast(wid, &play::level_event(play::EVENT_PARTICLE_DESTROY, c, bid as i32 | ((bmeta as i32) << 8)));
                                }
                            }
                        }
                    }
                }
            }
            let victims = core.players_in(wid);
            for victim in victims {
                let Some(p) = core.players.get(&victim) else { continue };
                if p.dead || p.hidden || p.gamemode != 0 {
                    continue;
                }
                let dist = ((p.x - at.x).powi(2) + (p.y - at.y).powi(2) + (p.z - at.z).powi(2)).sqrt();
                if dist >= 4.0 {
                    continue;
                }
                let damage = 8.0 * (1.0 - dist / 4.0);
                if core.apply_damage(victim, None, damage) == Hit::Died {
                    self.on_death(core, victim, None);
                }
            }
            log::info!("TNT exploded at {pos:?} in world {wid}: {removed} player-placed block(s) destroyed");
        }
        pending.extend(std::mem::take(&mut self.primed_tnt));
        self.primed_tnt = pending;
    }

    /// Deploy a short-lived 3x3 slime safety platform below a falling player.
    /// The platform is placed ahead of the fall (two ticks of speed below the feet) and never
    /// overlaps the player's own body, so the client lands on it instead of falling through.
    pub fn activate_slime_platform(&mut self, core: &mut Core, eid: i64) -> bool {
        let Some((wid, fx, fy, fz, falling, held, alive, gm, match_ok)) = core.players.get(&eid).map(|p| {
            let match_ok = self.instances.get(&p.world).map_or(false, |i| {
                i.phase == Phase::Running || (i.phase == Phase::Waiting && !i.map.problems().is_empty())
            });
            let feet = p.y - crate::state::EYE_HEIGHT;
            (p.world, p.x, feet - p.fall_speed * 2.0, p.z, p.fall_distance >= 1.5 || p.fall_speed >= 0.6, p.held_item(), !p.dead, p.gamemode, match_ok)
        }) else {
            return false;
        };
        if held.id != 165 {
            return false;
        }
        if !alive || gm != 0 || !match_ok || !falling {
            log::info!("slime platform refused for eid={eid}: alive={alive} survival={} in_match={match_ok} falling={falling}", gm == 0);
            return false;
        }
        let platform_y = fy.floor() as i32 - 1;
        if platform_y < 1 {
            return false;
        }
        let (cx, cz) = (fx.floor() as i32, fz.floor() as i32);
        let mut positions = Vec::with_capacity(9);
        for dx in -1..=1 {
            for dz in -1..=1 {
                let pos = (cx + dx, platform_y, cz + dz);
                if core.get_block(wid, pos.0, pos.1, pos.2).0 == 0 && !core.player_in_cell(wid, pos.0, pos.1, pos.2) {
                    positions.push(pos);
                }
            }
        }
        if positions.is_empty() {
            log::info!("slime platform refused for eid={eid}: no free block under the player at y={platform_y}");
            return false;
        }
        if !core.players.get_mut(&eid).map_or(false, |p| p.consume_one_matching(165, held.damage)) {
            return false;
        }
        let n = positions.len();
        for pos in positions {
            if core.set_block(wid, pos.0, pos.1, pos.2, 165, 0) {
                self.temporary_blocks.push((wid, pos, core.tick + 25 * 20));
            }
        }
        core.send_inventory(eid);
        core.send_text(eid, text_type::TIP, "", "\u{a7}aSlime safety platform deployed!");
        log::info!("slime platform deployed for eid={eid} at ({cx}, {platform_y}, {cz}), {n} block(s), gone in 25 s");
        true
    }

    // ---- damage / death -----------------------------------------------------------------------------

    /// Final damage, or `None` to cancel (lobby, friendly fire, spectators...).
    pub fn on_damage(&mut self, core: &Core, victim: i64, attacker: Option<i64>, dmg: f32) -> Option<f32> {
        let wid = core.world_of(victim);
        if wid == TEMPLATE {
            return None;
        }
        let tick = core.tick;
        let inst = self.instances.get_mut(&wid)?;
        let open = inst.phase == Phase::Running || (inst.phase == Phase::Waiting && !inst.map.problems().is_empty());
        if !open {
            return None;
        }
        let v_team = {
            let v = inst.members.get(&victim)?;
            if !v.alive {
                return None;
            }
            v.team
        };
        if let Some(a) = attacker {
            let am = inst.members.get(&a)?;
            if !am.alive || am.team == v_team {
                return None; // never hurt a teammate
            }
            if let Some(v) = inst.members.get_mut(&victim) {
                v.last_hit = Some((a, tick));
            }
        }
        Some(dmg)
    }

    /// Attack by any member (used by bots; humans go through game.rs so
    /// plugins see the damage event). True if it landed.
    pub fn attack(&mut self, core: &mut Core, attacker: i64, victim: i64) -> bool {
        let Some(base) = core.compute_attack(attacker, victim) else { return false };
        let Some(dmg) = self.on_damage(core, victim, Some(attacker), base) else { return false };
        match core.apply_damage(victim, Some(attacker), dmg) {
            Hit::Died => {
                self.on_death(core, victim, Some(attacker));
                true
            }
            Hit::Alive => true,
            Hit::Ignored => false,
        }
    }

    pub fn on_death(&mut self, core: &mut Core, victim: i64, killer: Option<i64>) {
        let wid = core.world_of(victim);
        let tick = core.tick;
        let (respawn_ticks, grace) = (self.cfg.respawn_ticks, 60);
        let (v_team, credit) = {
            let Some(inst) = self.instances.get_mut(&wid) else { return };
            let open = inst.phase == Phase::Running || (inst.phase == Phase::Waiting && !inst.map.problems().is_empty());
            if !open {
                return;
            }
            let Some(m) = inst.members.get_mut(&victim) else { return };
            let credit = killer.or_else(|| m.last_hit.filter(|(_, t)| tick.saturating_sub(*t) <= CREDIT_TICKS).map(|(a, _)| a));
            (m.team, credit)
        };
        let killer = credit.filter(|k| *k != victim && self.instances.get(&wid).map_or(false, |i| i.members.contains_key(k)));
        let vname = self.display(core, wid, victim);
        let bed = self.instances.get(&wid).map_or(false, |i| i.bed_alive[v_team]);
        let final_kill = !bed;
        let tag = if final_kill { " \u{a7}6\u{a7}lFINAL KILL!" } else { "" };
        let msg = match killer {
            Some(k) => format!("\u{a7}7{vname} \u{a7}7was slain by {}\u{a7}7.{tag}", self.display(core, wid, k)),
            None => format!("\u{a7}7{vname} \u{a7}7died.{tag}"),
        };
        core.broadcast_text(wid, text_type::RAW, "", &msg);
        if let Some(k) = killer {
            let human = self.instances.get(&wid).and_then(|i| i.members.get(&k)).map_or(false, |m| !m.bot);
            if human {
                if let Some(n) = core.players.get(&k).map(|p| p.name.clone()) {
                    self.stats.add_kill(&n);
                }
            }
            if let Some(inst) = self.instances.get_mut(&wid) {
                *inst.kills.entry(k).or_insert(0) += 1;
            }
        }
        if let Some(inst) = self.instances.get_mut(&wid) {
            if let Some(m) = inst.members.get_mut(&victim) {
                m.last_hit = None;
                if final_kill {
                    m.alive = false;
                    m.respawn_at = Some(tick + grace);
                } else {
                    m.respawn_at = Some(tick + respawn_ticks);
                }
            }
        }
        if final_kill {
            core.send_text(victim, text_type::RAW, "", "\u{a7}cYour bed is gone: you are eliminated.");
            self.check_winner(core, wid);
        } else {
            core.send_text(victim, text_type::POPUP, "", &format!("\u{a7}eRespawning in {}s", respawn_ticks / 20));
        }
    }

    /// The client pressed Respawn (or the timer forced it): re-equip or spectate.
    pub fn on_respawn(&mut self, core: &mut Core, eid: i64) {
        let wid = core.world_of(eid);
        if wid == TEMPLATE {
            return;
        }
        let state = self.instances.get_mut(&wid).and_then(|i| {
            let m = i.members.get_mut(&eid)?;
            m.respawn_at = None;
            Some((i.phase, m.alive, m.bot))
        });
        match state {
            Some((Phase::Running, true, _)) => self.equip(core, eid),
            Some((Phase::Running, false, true)) => {
                // eliminated bots simply leave
                self.brains.remove(&eid);
                core.remove_player(eid);
                if let Some(i) = self.instances.get_mut(&wid) {
                    i.members.remove(&eid);
                }
            }
            Some((Phase::Running, false, false)) => self.make_spectator(core, eid),
            Some(_) => self.lobby_state(core, eid),
            None => {}
        }
    }

    /// Eliminated players watch from above a surviving island.
    pub fn make_spectator(&mut self, core: &mut Core, eid: i64) {
        let wid = core.world_of(eid);
        let view = self.instances.get(&wid).and_then(|i| {
            i.live_teams().first().and_then(|t| i.map.teams[*t]).or(i.map.waiting_room)
        });
        core.clear_inventory(eid);
        core.set_armor(eid, [0; 4]);
        core.set_gamemode(eid, 3);
        if let Some(p) = core.players.get_mut(&eid) {
            p.dead = false;
            p.health = 20.0;
        }
        core.send(eid, play::set_health(20));
        if let Some(s) = view {
            core.teleport_facing(eid, s.x, s.y + 12.0, s.z, s.yaw, 30.0);
        }
        core.send_text(eid, text_type::RAW, "", "\u{a7}7You are a spectator. \u{a7}e/lobby\u{a7}7 to leave to the next game.");
    }

    // ---- movement: void ---------------------------------------------------------------------------------

    pub fn on_move(&mut self, core: &mut Core, eid: i64, feet_y: f32) {
        if feet_y >= self.cfg.void_y {
            return;
        }
        let wid = core.world_of(eid);
        if wid == TEMPLATE {
            return;
        }
        let Some(inst) = self.instances.get(&wid) else { return };
        let Some(m) = inst.members.get(&eid) else { return };
        let (phase, alive) = (inst.phase, m.alive);
        let practice = phase == Phase::Waiting && !inst.map.problems().is_empty();
        match phase {
            Phase::Running | Phase::Waiting if alive && (phase == Phase::Running || practice) => {
                if core.players.get(&eid).map_or(true, |p| p.dead) {
                    return;
                }
                if core.set_health(eid, 0.0) == Hit::Died {
                    self.on_death(core, eid, None);
                }
            }
            Phase::Running => {
                // spectator slipped below the map: back above an island
                self.make_spectator(core, eid);
            }
            _ => {
                let s = self.lobby_spot(core, wid);
                core.teleport_facing(eid, s.x, s.y, s.z, s.yaw, s.pitch);
            }
        }
    }

    // ---- winning ------------------------------------------------------------------------------------------

    pub fn check_winner(&mut self, core: &mut Core, wid: WorldId) {
        let tick = core.tick;
        let end_delay = self.cfg.end_delay_ticks;
        let Some(inst) = self.instances.get_mut(&wid) else { return };
        if inst.phase != Phase::Running {
            return;
        }
        let live = inst.live_teams();
        if live.len() > 1 {
            return;
        }
        let winner = live.first().copied();
        inst.phase = Phase::Ending { until: tick + end_delay };
        inst.winner = winner;
        let results: Vec<(i64, usize, bool)> = inst.members.iter().map(|(e, m)| (*e, m.team, m.bot)).collect();
        let mut winners = Vec::new();
        let mut lines = Vec::new();
        for (eid, team, bot) in results {
            if bot {
                continue;
            }
            let Some(name) = core.players.get(&eid).map(|p| p.name.clone()) else { continue };
            let won = Some(team) == winner;
            let paid = self.stats.finish_game(&name, won);
            if won {
                winners.push(name.clone());
            }
            lines.push((eid, won, paid));
        }
        let title = match winner {
            Some(t) => format!("{} \u{a7}6\u{a7}lTEAM WINS!", team_label(t)),
            None => "\u{a7}7\u{a7}lGAME OVER \u{a7}r\u{a7}7- nobody won".to_owned(),
        };
        let who = if winners.is_empty() { String::new() } else { format!("\n\u{a7}eWinners: \u{a7}f{}", winners.join(", ")) };
        core.broadcast_text(wid, text_type::RAW, "", &format!("\n\u{a7}6\u{a7}m=====\u{a7}r {title} \u{a7}6\u{a7}m=====\u{a7}r{who}\n\u{a7}7Returning to the lobby in {}s", end_delay / 20));
        for (eid, won, paid) in lines {
            let extra = if won { " \u{a7}6(victory bonus!)" } else { "" };
            core.send_text(eid, text_type::RAW, "", &format!("\u{a7}6+{paid} coins{extra}"));
            core.send_text(eid, text_type::POPUP, "", &title);
        }
        if let Err(e) = self.stats.save(&self.cfg.dir.join("stats.toml")) {
            log::error!("could not save stats: {e}");
        }
        log::info!("match in world {wid} ended, winner: {:?}", winner.map(|t| TEAM_NAMES[t]));
    }

    // ---- running-phase tick ---------------------------------------------------------------------------------

    pub fn tick_running(&mut self, core: &mut Core, wid: WorldId, tick: u64) {
        self.tick_generators(core, wid, tick);
        // respawn timers
        let due: Vec<i64> = self
            .instances
            .get(&wid)
            .map(|i| i.members.iter().filter(|(_, m)| m.respawn_at.map_or(false, |t| tick >= t)).map(|(e, _)| *e).collect())
            .unwrap_or_default();
        for eid in due {
            if let Some(m) = self.instances.get_mut(&wid).and_then(|i| i.members.get_mut(&eid)) {
                m.respawn_at = None;
            }
            if core.players.get(&eid).map_or(false, |p| p.dead) {
                core.respawn(eid);
                self.on_respawn(core, eid);
            }
        }
        self.tick_bots(core, wid, tick);
        // a team whose members all left cannot win; re-check periodically
        if tick % 40 == 0 {
            self.check_winner(core, wid);
        }
    }

    fn tick_generators(&mut self, core: &mut Core, wid: WorldId, tick: u64) {
        let Some(inst) = self.instances.get(&wid) else { return };
        let rel = tick.saturating_sub(inst.started);
        if rel == 0 {
            return;
        }

        let cfg = &self.cfg;
        let mut drops: Vec<(Vec3, i16, u32)> = Vec::new();
        for g in &inst.map.spawners {
            let at = Vec3 { x: g.x as f32 + 0.5, y: g.y as f32 + 0.3, z: g.z as f32 + 0.5 };
            match g.kind {
                SpawnerKind::Basic => {
                    if cfg.iron_every > 0 && rel % cfg.iron_every == 0 {
                        drops.push((at, IRON, cfg.iron_item_cap));
                    }
                    if cfg.gold_every > 0 && rel % cfg.gold_every == 0 {
                        drops.push((at, GOLD, cfg.gold_item_cap));
                    }
                }
                SpawnerKind::Advanced => {
                    if cfg.emerald_every > 0 && rel % cfg.emerald_every == 0 {
                        drops.push((at, EMERALD, cfg.emerald_item_cap));
                    }
                }
            }
        }

        // Re-check immediately before each spawn. This also keeps overlapping
        // generators from exceeding the configured cap in the same server tick.
        for (at, id, cap) in drops {
            if core.count_items_near(wid, at, 1.5, id) < cap {
                core.spawn_item(wid, at, crate::protocol::item::Slot::new(id, 1, 0));
            }
        }

        // Update emerald holograms once per second, without despawning entities.
        if tick % 20 == 0 {
            let interval = cfg.emerald_every.max(1);
            let remaining_ticks = interval - (rel % interval);
            let seconds = (remaining_ticks + 19) / 20;
            let label = format!("§aEMERALD §7| §eNext: {seconds}s");
            for (eid, _) in &inst.emerald_holograms {
                core.set_npc_name(*eid, &label);
            }
        }
    }
}

/// Number of teams (re-exported for tests).
pub const TEAMS: usize = TEAM_COUNT;
