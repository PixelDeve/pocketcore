//! Bedwars commands, the shop menu and bed placement.
//!
//! Operator setup (only inside the original map, see `/visit original`):
//!   /waitingroom create|delete     /team create <red|green|yellow|blue>
//!   /spawner basic|advance|remove  /shop npc|remove     /bed team <team>
//!   /bedwars status|reload|save    /visit original      /gamemode (in commands.rs)
//! Everyone: /lobby, /stats.  Shop: tap the NPC, then answer with a number.

use crate::binary::Vec3;
use crate::commands::Sender;
use crate::protocol::item::Slot;
use crate::protocol::play::text_type;
use crate::state::{Core, TEMPLATE};

use super::map::{team_index, team_label, BedSpot, BlockXyz, Spawner, SpawnerKind, Spot, TEAM_NAMES};
use super::shop::{ArmorSet, ShopItem};
use super::{Bedwars, Phase, Session, BED_BLOCK, BED_ITEM, SHOP_NAME};

const TAG: &str = "\u{a7}6[Bedwars] ";
const SHOP_REACH: f32 = 8.0;

/// Facing 0..3 from a yaw: 0 = +z, 1 = -x, 2 = -z, 3 = +x; the bed's head
/// lies in that direction.
pub fn bed_direction(yaw: f32) -> u8 {
    ((yaw / 90.0 + 0.5).floor() as i32).rem_euclid(4) as u8
}

pub fn direction_offset(d: u8) -> (i32, i32) {
    match d & 3 {
        0 => (0, 1),
        1 => (-1, 0),
        2 => (0, -1),
        _ => (1, 0),
    }
}

impl Bedwars {
    fn say(core: &mut Core, who: Sender, msg: &str) {
        match who {
            Sender::Player(e) => core.send_text(e, text_type::RAW, "", msg),
            Sender::Console => log::info!("{}", msg.replace('\u{a7}', "")),
        }
    }

    /// Feet position and facing of a player.
    fn spot_of(core: &Core, eid: i64) -> Option<Spot> {
        core.players.get(&eid).map(|p| Spot { x: p.x, y: p.y - crate::state::EYE_HEIGHT, z: p.z, yaw: p.yaw, pitch: p.pitch })
    }

    /// Handles a command line (no slash). True if it was one of ours.
    pub fn command(&mut self, core: &mut Core, who: Sender, line: &str) -> bool {
        let mut it = line.split_whitespace();
        let Some(cmd) = it.next().map(|c| c.to_ascii_lowercase()) else { return false };
        let args: Vec<&str> = it.collect();
        match cmd.as_str() {
            "lobby" | "leave" | "hub" => {
                let Sender::Player(e) = who else { return true };
                if core.world_of(e) == TEMPLATE && !core.can_use(who) {
                    return true;
                }
                self.send_to_lobby(core, e);
                Self::say(core, who, &format!("{TAG}\u{a7}eSending you to a lobby..."));
                true
            }
            "stats" => {
                let Sender::Player(e) = who else { return true };
                let name = core.players.get(&e).map(|p| p.name.clone()).unwrap_or_default();
                let s = self.stats.get(&name);
                Self::say(
                    core,
                    who,
                    &format!("{TAG}\u{a7}e{name}: \u{a7}f{} wins, {} games, {} kills, {} beds, \u{a7}6{} coins", s.wins, s.games, s.kills, s.beds, s.coins),
                );
                true
            }
            "waitingroom" | "team" | "spawner" | "bed" | "visit" | "map" | "bedwars" | "bw" => {
                if !core.can_use(who) {
                    Self::say(core, who, "\u{a7}cYou do not have permission to use this command.");
                    return true;
                }
                self.op_command(core, who, &cmd, &args);
                true
            }
            "shop" => {
                match args.first().map(|a| a.to_ascii_lowercase()).as_deref() {
                    None => {
                        if let Sender::Player(e) = who {
                            self.open_menu(core, e);
                        }
                    }
                    Some("chat") | Some("gui") => {
                        if let Sender::Player(e) = who {
                            let gui = args.first().map_or(false, |a| a.eq_ignore_ascii_case("gui"));
                            if gui {
                                self.chat_pref.remove(&e);
                            } else {
                                self.chat_pref.insert(e);
                                self.close_gui(core, e);
                            }
                            let m = if gui && self.cfg.shop_gui { "\u{a7}aShop mode: chest GUI." } else if gui { "\u{a7}cThe chest GUI is disabled on this server." } else { "\u{a7}aShop mode: chat." };
                            core.send_text(e, text_type::RAW, "", m);
                        }
                    }
                    Some(_) if !core.can_use(who) => Self::say(core, who, "\u{a7}cYou do not have permission to use this command."),
                    Some(sub) => self.shop_setup(core, who, sub),
                }
                true
            }
            _ => false,
        }
    }

    fn need_template(core: &mut Core, who: Sender) -> Option<i64> {
        let Sender::Player(e) = who else {
            Self::say(core, who, "\u{a7}cThis command must be run by a player standing in the original map.");
            return None;
        };
        if core.world_of(e) != TEMPLATE {
            Self::say(core, who, "\u{a7}cSetup only works in the original map. Use \u{a7}e/visit original\u{a7}c first.");
            return None;
        }
        Some(e)
    }

    fn op_command(&mut self, core: &mut Core, who: Sender, cmd: &str, args: &[&str]) {
        let sub = args.first().map(|a| a.to_ascii_lowercase());
        match (cmd, sub.as_deref()) {
            ("visit" | "map", Some("original")) => {
                let Sender::Player(e) = who else { return Self::say(core, who, "\u{a7}cPlayers only.") };
                self.visit_original(core, e);
                Self::say(core, who, &format!("{TAG}\u{a7}eTeleporting to the original map..."));
            }
            ("visit" | "map", _) => Self::say(core, who, "\u{a7}cUsage: /visit original"),
            ("waitingroom", Some("create")) => {
                let Some(e) = Self::need_template(core, who) else { return };
                self.map.waiting_room = Self::spot_of(core, e);
                self.map_changed(core);
                Self::say(core, who, &format!("{TAG}\u{a7}aWaiting room set here."));
            }
            ("waitingroom", Some("delete")) => {
                if Self::need_template(core, who).is_none() {
                    return;
                }
                let had = self.map.waiting_room.take().is_some();
                self.map_changed(core);
                Self::say(core, who, &if had { format!("{TAG}\u{a7}aWaiting room removed.") } else { "\u{a7}eThere is no waiting room.".to_owned() });
            }
            ("waitingroom", _) => Self::say(core, who, "\u{a7}cUsage: /waitingroom create|delete"),
            ("team", Some("create")) => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(t) = args.get(1).and_then(|n| team_index(n)) else {
                    return Self::say(core, who, "\u{a7}cUsage: /team create <red|green|yellow|blue>");
                };
                self.map.teams[t] = Self::spot_of(core, e);
                self.map_changed(core);
                Self::say(core, who, &format!("{TAG}\u{a7}aSpawn of the {} \u{a7}ateam set here.", team_label(t)));
            }
            ("team", _) => Self::say(core, who, "\u{a7}cUsage: /team create <red|green|yellow|blue>"),
            ("spawner", Some(k @ ("basic" | "advance" | "advanced"))) => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(s) = Self::spot_of(core, e) else { return };
                let kind = SpawnerKind::from_name(k).unwrap_or(SpawnerKind::Basic);
                self.map.add_spawner(Spawner { kind, x: s.x.floor() as i32, y: s.y.floor() as i32, z: s.z.floor() as i32 });
                self.map_changed(core);
                let what = if kind == SpawnerKind::Basic { "iron + gold" } else { "emerald" };
                Self::say(core, who, &format!("{TAG}\u{a7}a{} generator placed ({what})\u{a7}a.", kind.name()));
            }
            ("spawner", Some("remove")) => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(s) = Self::spot_of(core, e) else { return };
                let gone = self.map.remove_spawner_near(s.x, s.y, s.z, 3.0).is_some();
                self.map_changed(core);
                Self::say(core, who, &if gone { format!("{TAG}\u{a7}aGenerator removed.") } else { "\u{a7}eNo generator within 3 blocks.".to_owned() });
            }
            ("spawner", _) => Self::say(core, who, "\u{a7}cUsage: /spawner basic|advance|remove"),
            ("bed", Some("team")) => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(t) = args.get(1).and_then(|n| team_index(n)) else {
                    return Self::say(core, who, "\u{a7}cUsage: /bed team <red|green|yellow|blue>");
                };
                core.give_item(e, BED_ITEM, 1, 0);
                self.pending_bed.insert(e, t);
                Self::say(core, who, &format!("{TAG}\u{a7}eYou got a bed for the {} \u{a7}eteam. Place it on that team's island.", team_label(t)));
            }
            ("bed", _) => Self::say(core, who, "\u{a7}cUsage: /bed team <red|green|yellow|blue>"),
            ("bedwars" | "bw", Some("status")) => self.status(core, who),
            ("bedwars" | "bw", Some("reload")) => {
                let msg = match self.reload_shop() {
                    Ok(()) => format!("{TAG}\u{a7}ashop.toml reloaded ({} categories).", self.shop.categories.len()),
                    Err(e) => format!("\u{a7}cshop.toml not reloaded: {e}"),
                };
                Self::say(core, who, &msg);
            }
            ("bedwars" | "bw", Some("save")) => {
                self.save_map();
                self.save_all(core);
                Self::say(core, who, &format!("{TAG}\u{a7}aOriginal map, setup and stats saved."));
            }
            ("bedwars" | "bw", _) => Self::say(core, who, "\u{a7}cUsage: /bedwars status|reload|save"),
            _ => {}
        }
    }

    fn shop_setup(&mut self, core: &mut Core, who: Sender, sub: &str) {
        match sub {
            "npc" => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(s) = Self::spot_of(core, e) else { return };
                self.map.add_npc(s);
                self.map_changed(core);
                Self::say(core, who, &format!("{TAG}\u{a7}aShop NPC placed. Edit items and prices in shop.toml, then /bedwars reload."));
            }
            "remove" => {
                let Some(e) = Self::need_template(core, who) else { return };
                let Some(s) = Self::spot_of(core, e) else { return };
                let gone = self.map.remove_npc_near(s.x, s.y, s.z, 3.0).is_some();
                self.map_changed(core);
                Self::say(core, who, &if gone { format!("{TAG}\u{a7}aShop NPC removed.") } else { "\u{a7}eNo shop NPC within 3 blocks.".to_owned() });
            }
            _ => Self::say(core, who, "\u{a7}cUsage: /shop npc | /shop remove | /shop"),
        }
    }

    /// Persist the setup and refresh what the original map shows.
    fn map_changed(&mut self, core: &mut Core) {
        self.save_map();
        self.sync_template_npcs(core);
        let problems = self.map.problems();
        if problems.is_empty() {
            log::info!("map setup complete");
        }
    }

    fn status(&self, core: &mut Core, who: Sender) {
        let problems = self.map.problems();
        let head = if problems.is_empty() { "\u{a7}aMap setup complete".to_owned() } else { format!("\u{a7}cMissing: {}", problems.join(", ")) };
        Self::say(core, who, &format!("{TAG}{head}"));
        Self::say(
            core,
            who,
            &format!(
                "\u{a7}7{} generator(s), {} shop NPC(s), team size {} ({} slots)",
                self.map.spawners.len(),
                self.map.npcs.len(),
                self.cfg.team_size,
                self.capacity()
            ),
        );
        let mut ids: Vec<_> = self.instances.keys().copied().collect();
        ids.sort_unstable();
        for w in ids {
            let i = &self.instances[&w];
            let humans = i.members.values().filter(|m| !m.bot).count();
            let phase = match i.phase {
                Phase::Waiting => "waiting".to_owned(),
                Phase::Countdown { .. } => "starting".to_owned(),
                Phase::Running => "RUNNING".to_owned(),
                Phase::Ending { .. } => "ending".to_owned(),
            };
            Self::say(core, who, &format!("\u{a7}7world {w}: {phase}, {humans} human(s), {} member(s)", i.members.len()));
        }
    }

    // ---- bed placement ---------------------------------------------------------------------------------

    /// An operator placed the bed item at `(x, y, z)`. Returns true: the
    /// placement was handled here (placed or refused).
    pub fn try_place_bed(&mut self, core: &mut Core, eid: i64, x: i32, y: i32, z: i32) -> bool {
        let who = Sender::Player(eid);
        if core.world_of(eid) != TEMPLATE || !core.can_use(who) {
            core.resync_block(eid, x, y, z);
            return true;
        }
        let Some(team) = self.pending_bed.get(&eid).copied() else {
            Self::say(core, who, "\u{a7}cUse /bed team <team> first to choose which team this bed belongs to.");
            core.resync_block(eid, x, y, z);
            return true;
        };
        let yaw = core.players.get(&eid).map_or(0.0, |p| p.yaw);
        let d = bed_direction(yaw);
        let (dx, dz) = direction_offset(d);
        let (hx, hz) = (x + dx, z + dz);
        if !core.can_place_at(TEMPLATE, x, y, z) || !core.can_place_at(TEMPLATE, hx, y, hz) {
            Self::say(core, who, "\u{a7}cNot enough room for a bed there (it needs two free blocks).");
            core.resync_block(eid, x, y, z);
            core.resync_block(eid, hx, y, hz);
            return true;
        }
        if let Some(old) = self.map.beds[team] {
            core.set_block(TEMPLATE, old.foot.x, old.foot.y, old.foot.z, 0, 0);
            core.set_block(TEMPLATE, old.head.x, old.head.y, old.head.z, 0, 0);
        }
        core.set_block(TEMPLATE, x, y, z, BED_BLOCK, d);
        core.set_block(TEMPLATE, hx, y, hz, BED_BLOCK, d | 8);
        self.map.beds[team] = Some(BedSpot { foot: BlockXyz { x, y, z }, head: BlockXyz { x: hx, y, z: hz } });
        self.pending_bed.remove(&eid);
        if let Some(p) = core.players.get_mut(&eid) {
            p.consume_one(BED_ITEM);
        }
        core.send_inventory(eid);
        self.map_changed(core);
        Self::say(core, who, &format!("{TAG}\u{a7}aBed of the {} \u{a7}ateam registered at {x}, {y}, {z}.", team_label(team)));
        true
    }

    // ---- shop ----------------------------------------------------------------------------------------------

    pub(super) fn near_shop(core: &Core, eid: i64) -> bool {
        let Some(p) = core.players.get(&eid) else { return false };
        core.npcs.values().any(|n| {
            n.world == p.world && (n.pos.x - p.x).powi(2) + (n.pos.z - p.z).powi(2) + (n.pos.y - (p.y - crate::state::EYE_HEIGHT)).powi(2) <= SHOP_REACH * SHOP_REACH
        })
    }

    /// A player tapped an entity. True if it was a shop NPC (never damaged).
    pub fn on_interact(&mut self, core: &mut Core, eid: i64, target: i64) -> bool {
        if !core.npcs.contains_key(&target) {
            return false;
        }
        self.open_menu(core, eid);
        true
    }

    pub(super) fn team_of(&self, core: &Core, eid: i64) -> Option<usize> {
        let wid = core.world_of(eid);
        let inst = self.instances.get(&wid)?;
        let m = inst.members.get(&eid)?;
        (inst.phase == Phase::Running && m.alive).then_some(m.team)
    }

    pub fn open_menu(&mut self, core: &mut Core, eid: i64) {
        if !Self::near_shop(core, eid) {
            core.send_text(eid, text_type::RAW, "", "\u{a7}cStand next to a shop NPC.");
            return;
        }
        if core.world_of(eid) != TEMPLATE && self.team_of(core, eid).is_none() {
            core.send_text(eid, text_type::RAW, "", "\u{a7}cThe shop is only open to players during a match.");
            return;
        }
        if self.gui_enabled_for(eid) {
            self.sessions.remove(&eid);
            self.open_gui(core, eid);
            return;
        }
        self.sessions.insert(eid, Session::Categories);
        self.show(core, eid);
    }

    fn show(&mut self, core: &mut Core, eid: i64) {
        let Some(s) = self.sessions.get(&eid).copied() else { return };
        let mut lines = vec![format!("\u{a7}8-------- {} \u{a7}8--------", self.shop.title)];
        match s {
            Session::Categories => {
                for (i, c) in self.shop.categories.iter().enumerate() {
                    lines.push(format!("\u{a7}e{}. \u{a7}f{} \u{a7}7({} items)", i + 1, c.name, c.items.len()));
                }
                lines.push("\u{a7}7Type a number in chat to choose, 0 to close.".to_owned());
            }
            Session::Items(c) => {
                let Some(cat) = self.shop.categories.get(c) else { return };
                lines.push(format!("\u{a7}6{}", cat.name));
                for (i, it) in cat.items.iter().enumerate() {
                    lines.push(format!("\u{a7}e{}. {}", i + 1, it.describe()));
                }
                lines.push("\u{a7}7Type a number to buy, 0 to go back.".to_owned());
            }
        }
        for l in lines {
            core.send_text(eid, text_type::RAW, "", &l);
        }
    }

    /// Shop answer typed in chat. True if the message was consumed.
    pub fn on_chat_reply(&mut self, core: &mut Core, eid: i64, msg: &str) -> bool {
        let Some(session) = self.sessions.get(&eid).copied() else { return false };
        let Ok(n) = msg.trim().parse::<usize>() else { return false };
        if !Self::near_shop(core, eid) || (core.world_of(eid) != TEMPLATE && self.team_of(core, eid).is_none()) {
            self.sessions.remove(&eid);
            core.send_text(eid, text_type::RAW, "", "\u{a7}7Shop closed.");
            return true;
        }
        match session {
            Session::Categories => {
                if n == 0 {
                    self.sessions.remove(&eid);
                    core.send_text(eid, text_type::RAW, "", "\u{a7}7Shop closed.");
                } else if n <= self.shop.categories.len() {
                    self.sessions.insert(eid, Session::Items(n - 1));
                    self.show(core, eid);
                } else {
                    core.send_text(eid, text_type::RAW, "", "\u{a7}cNo such category.");
                }
            }
            Session::Items(c) => {
                if n == 0 {
                    self.sessions.insert(eid, Session::Categories);
                    self.show(core, eid);
                } else if let Some(item) = self.shop.categories.get(c).and_then(|cat| cat.items.get(n - 1)).cloned() {
                    match self.team_of(core, eid) {
                        Some(team) => {
                            Self::purchase(core, eid, &item, team);
                        }
                        None => core.send_text(eid, text_type::RAW, "", "\u{a7}7Preview only: purchases work during a match."),
                    }
                } else {
                    core.send_text(eid, text_type::RAW, "", "\u{a7}cNo such item.");
                }
            }
        }
        true
    }

    /// Takes the price from the inventory and delivers the item. All or nothing.
    pub fn purchase(core: &mut Core, eid: i64, item: &ShopItem, team: usize) -> bool {
        let cur = item.currency;
        let Some(p) = core.players.get_mut(&eid) else { return false };
        let have = p.count_item(cur.item_id());
        if have < item.price {
            let missing = item.price - have;
            core.send_text(eid, text_type::RAW, "", &format!("\u{a7}cYou need {missing} more {}\u{a7}c.", cur.label()));
            return false;
        }
        if let Some(set) = item.armor_set {
            let owned = ArmorSet::from_chestplate(p.armor[1].id).map(|s| s.tier());
            if owned.map_or(false, |t| t >= set.tier()) {
                core.send_text(eid, text_type::RAW, "", "\u{a7}cYou already wear equal or better armor.");
                return false;
            }
            p.take_items(cur.item_id(), item.price);
            core.send_inventory(eid);
            core.set_armor(eid, set.ids());
        } else {
            let snapshot = p.inventory;
            p.take_items(cur.item_id(), item.price);
            let left = p.give(Slot::new(item.id, item.amount, item.resolved_meta(team)));
            if left > 0 {
                p.inventory = snapshot;
                core.send_text(eid, text_type::RAW, "", "\u{a7}cYour inventory is full.");
                return false;
            }
            core.send_inventory(eid);
        }
        core.send_text(eid, text_type::POPUP, "", &format!("\u{a7}aPurchased \u{a7}f{}", item.name));
        true
    }

    /// Shop NPC name shown above the entity.
    pub fn npc_name() -> &'static str {
        SHOP_NAME
    }
}

#[allow(dead_code)]
fn unused(_: Vec3) {}

pub const TEAM_LIST: [&str; 4] = TEAM_NAMES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bed_faces_where_the_player_looks() {
        assert_eq!(bed_direction(0.0), 0);
        assert_eq!(bed_direction(90.0), 1);
        assert_eq!(bed_direction(180.0), 2);
        assert_eq!(bed_direction(270.0), 3);
        assert_eq!(bed_direction(-90.0), 3);
        assert_eq!(bed_direction(359.0), 0);
        assert_eq!(direction_offset(0), (0, 1));
        assert_eq!(direction_offset(1), (-1, 0));
        assert_eq!(direction_offset(2), (0, -1));
        assert_eq!(direction_offset(3), (1, 0));
    }
}
