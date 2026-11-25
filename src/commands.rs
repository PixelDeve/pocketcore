//! Built-in commands, usable from in-game chat ("/op Steve") and from the
//! server console ("op Steve").
//!
//! /help /op /deop /kick /ban /unban /mute /unmute /tp /fill
//!
//! Operators, bans and mutes are stored by lower-case player name in
//! `ops.txt`, `banned.txt` and `muted.txt` (one name per line, `#` comments
//! allowed) so they survive restarts. Everything except /help needs operator
//! status; the console always has it, which is how the first op is created.

use std::collections::{HashMap, HashSet};
use std::fs;

use crate::protocol::play::{self, text_type, BlockRecord};
use crate::state::{Core, WorldId, EYE_HEIGHT, TEMPLATE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sender {
    Console,
    Player(i64),
}

const OPS_FILE: &str = "ops.txt";
const BANNED_FILE: &str = "banned.txt";
const MUTED_FILE: &str = "muted.txt";
const MAX_FILL_BLOCKS: f64 = 65536.0;

/// (name, usage line, needs operator)
const COMMANDS: &[(&str, &str, bool)] = &[
    ("help", "/help [command] - list commands", false),
    ("op", "/op <player> - give operator status", true),
    ("deop", "/deop <player> - remove operator status", true),
    ("kick", "/kick <player> [reason] - remove a player", true),
    ("ban", "/ban <player> [reason] - ban a name (works while offline)", true),
    ("unban", "/unban <player> - lift a ban", true),
    ("mute", "/mute <player> - block a player's chat", true),
    ("unmute", "/unmute <player> - let a player chat again", true),
    ("tp", "/tp <player> | <player> <player> | <x> <y> <z> | <player> <x> <y> <z>  (~ = relative)", true),
    ("tpall", "/tpall <player> - teleport all online human players to a player", true),
    ("lobby", "/lobby - leave to the next game lobby", false),
    ("stats", "/stats - your wins, kills, beds and coins", false),
    ("shop", "/shop - open the item shop (next to a shop NPC); /shop npc|remove (op, original map)", false),
    ("visit", "/visit original (alias /map original) - go to the original template map", true),
    ("waitingroom", "/waitingroom create|delete - set the waiting lobby here (original map)", true),
    ("team", "/team create <red|green|yellow|blue> - set that team's island spawn here", true),
    ("spawner", "/spawner basic|advance|remove - iron+gold / emerald generator here", true),
    ("bed", "/bed team <red|green|yellow|blue> - get that team's bed to place", true),
    ("bedwars", "/bedwars status|reload|save - status, reload shop.toml, save the original map", true),
    ("whoami", "/whoami - what the server thinks you are (op, game mode, held item)", false),
    ("list", "/list - list online players", false),
    ("say", "/say <message> - broadcast a server announcement", true),
    ("heal", "/heal [player] - restore health", true),
    ("clear", "/clear <player> - clear a player's inventory", true),
    ("give", "/give <player> <item-id> [count] [meta] - give an item", true),
    ("gamemode", "/gamemode <0|s|1|c|2|a|3|sp> [player] - set game mode (alias /gm)", true),
    ("fill", "/fill <x1> <y1> <z1> <x2> <y2> <z2> <block[:meta]> [meta]  (max 65536 blocks)", true),
];

fn load_list(path: &str) -> HashSet<String> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(|l| l.trim().to_ascii_lowercase())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect(),
        Err(_) => HashSet::new(),
    }
}

fn save_list(path: &str, set: &HashSet<String>) {
    let mut names: Vec<&String> = set.iter().collect();
    names.sort();
    let mut text = String::new();
    for n in names {
        text.push_str(n);
        text.push('\n');
    }
    if let Err(e) = fs::write(path, text) {
        log::warn!("could not write {path}: {e}");
    }
}

fn strip_colors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn block_by_name(name: &str) -> Option<u8> {
    Some(match name {
        "air" => 0,
        "stone" => 1,
        "grass" => 2,
        "dirt" => 3,
        "cobblestone" | "cobble" => 4,
        "planks" | "wood_planks" => 5,
        "bedrock" => 7,
        "water" => 9,
        "lava" => 11,
        "sand" => 12,
        "gravel" => 13,
        "gold_ore" => 14,
        "iron_ore" => 15,
        "coal_ore" => 16,
        "log" | "wood" => 17,
        "leaves" => 18,
        "sponge" => 19,
        "glass" => 20,
        "lapis_block" => 22,
        "sandstone" => 24,
        "wool" => 35,
        "gold_block" => 41,
        "iron_block" => 42,
        "brick" | "bricks" => 45,
        "tnt" => 46,
        "bookshelf" => 47,
        "obsidian" => 49,
        "diamond_block" => 57,
        "ice" => 79,
        "snow" => 80,
        "clay" => 82,
        "netherrack" => 87,
        "glowstone" => 89,
        _ => return None,
    })
}

/// `stone`, `35`, `wool:14` or `35:14`, plus an optional separate meta argument.
fn parse_block(spec: &str, meta: Option<&str>) -> Option<(u8, u8)> {
    let spec = spec.to_ascii_lowercase();
    let (name, inline_meta) = match spec.split_once(':') {
        Some((n, m)) => (n.to_owned(), Some(m.to_owned())),
        None => (spec.clone(), None),
    };
    let id: u8 = match name.parse::<u16>() {
        Ok(n) if n <= 255 => n as u8,
        Ok(_) => return None,
        Err(_) => block_by_name(&name)?,
    };
    let meta_text = inline_meta.or_else(|| meta.map(str::to_owned));
    let data: u8 = match meta_text {
        Some(m) => match m.parse::<u8>() {
            Ok(v) if v <= 15 => v,
            _ => return None,
        },
        None => 0,
    };
    Some((id, data))
}

/// `0|s|survival`, `1|c|creative`, `2|a|adventure`, `3|sp|spectator`.
pub fn parse_gamemode(s: &str) -> Option<i32> {
    match s.to_ascii_lowercase().as_str() {
        "0" | "s" | "survival" => Some(0),
        "1" | "c" | "creative" => Some(1),
        "2" | "a" | "adventure" => Some(2),
        "3" | "sp" | "spectator" => Some(3),
        _ => None,
    }
}

/// A plain number, or `~` / `~offset` relative to `base`.
fn parse_coord(s: &str, base: f32) -> Option<f32> {
    let v = if let Some(rest) = s.strip_prefix('~') {
        if rest.is_empty() {
            base
        } else {
            base + rest.parse::<f32>().ok()?
        }
    } else {
        s.parse::<f32>().ok()?
    };
    v.is_finite().then_some(v)
}

impl Core {
    /// Loads ops.txt / banned.txt / muted.txt (missing files = empty lists).
    pub fn load_lists(&mut self) {
        self.ops = load_list(OPS_FILE);
        self.banned = load_list(BANNED_FILE);
        self.muted = load_list(MUTED_FILE);
        log::info!("{} op(s), {} ban(s), {} mute(s) loaded", self.ops.len(), self.banned.len(), self.muted.len());
    }

    /// Runs one command line (without the leading slash).
    pub fn run_command(&mut self, who: Sender, line: &str) {
        let mut parts = line.split_whitespace();
        let Some(first) = parts.next() else { return };
        let cmd = first.to_ascii_lowercase();
        let args: Vec<&str> = parts.collect();

        if cmd == "help" || cmd == "?" {
            self.cmd_help(who, &args);
            return;
        }
        if cmd == "whoami" {
            self.cmd_whoami(who);
            return;
        }
        if cmd == "list" {
            self.cmd_list(who);
            return;
        }
        let known = matches!(
            cmd.as_str(),
            "op" | "deop" | "kick" | "ban" | "unban" | "pardon" | "mute" | "unmute" | "tp" | "teleport" | "tpall" | "fill" | "gamemode" | "gm" | "say" | "heal" | "clear" | "give"
        );
        if !known {
            self.reply(who, "\u{a7}cUnknown command. Type /help for a list.");
            return;
        }
        if !self.can_use(who) {
            self.reply(who, "\u{a7}cYou do not have permission to use this command.");
            return;
        }
        match cmd.as_str() {
            "op" => self.cmd_op(who, &args, true),
            "deop" => self.cmd_op(who, &args, false),
            "kick" => self.cmd_kick(who, &args),
            "ban" => self.cmd_ban(who, &args, true),
            "unban" | "pardon" => self.cmd_ban(who, &args, false),
            "mute" => self.cmd_mute(who, &args, true),
            "unmute" => self.cmd_mute(who, &args, false),
            "tp" | "teleport" => self.cmd_tp(who, &args),
            "tpall" => self.cmd_tpall(who, &args),
            "fill" => self.cmd_fill(who, &args),
            "gamemode" | "gm" => self.cmd_gamemode(who, &args),
            "say" => self.cmd_say(who, &args),
            "heal" => self.cmd_heal(who, &args),
            "clear" => self.cmd_clear(who, &args),
            "give" => self.cmd_give(who, &args),
            _ => {}
        }
    }

    // ---- helpers ------------------------------------------------------------------------

    pub(crate) fn reply(&mut self, who: Sender, msg: &str) {
        match who {
            Sender::Console => log::info!("{}", strip_colors(msg)),
            Sender::Player(eid) => self.send_text(eid, text_type::RAW, "", msg),
        }
    }

    pub(crate) fn can_use(&self, who: Sender) -> bool {
        match who {
            Sender::Console => true,
            Sender::Player(eid) => {
                self.players.get(&eid).map_or(false, |p| self.ops.contains(&p.name.to_ascii_lowercase()))
            }
        }
    }

    /// Exact (case-insensitive) name first, then a unique name prefix.
    pub(crate) fn find_player(&self, name: &str) -> Option<i64> {
        let lower = name.to_ascii_lowercase();
        if lower.is_empty() {
            return None;
        }
        let mut prefix: Option<i64> = None;
        let mut ambiguous = false;
        for p in self.players.values() {
            let n = p.name.to_ascii_lowercase();
            if n == lower {
                return Some(p.eid);
            }
            if n.starts_with(&lower) {
                if prefix.is_some() {
                    ambiguous = true;
                }
                prefix = Some(p.eid);
            }
        }
        if ambiguous {
            None
        } else {
            prefix
        }
    }

    /// The online player a typed name refers to (if any) and the name to use
    /// for list entries: the player's real name, or what was typed.
    pub(crate) fn resolve(&self, typed: &str) -> (Option<i64>, String) {
        let eid = self.find_player(typed);
        let name = eid
            .and_then(|e| self.players.get(&e))
            .map(|p| p.name.clone())
            .unwrap_or_else(|| typed.to_owned());
        (eid, name)
    }

    pub(crate) fn feet_pos(&self, eid: i64) -> Option<(f32, f32, f32)> {
        self.players.get(&eid).map(|p| (p.x, p.y - EYE_HEIGHT, p.z))
    }

    // ---- commands -----------------------------------------------------------------------

    fn cmd_help(&mut self, who: Sender, args: &[&str]) {
        let is_op = self.can_use(who);
        if let Some(arg) = args.first() {
            let name = arg.trim_start_matches('/').to_ascii_lowercase();
            match COMMANDS.iter().find(|c| c.0 == name) {
                Some(c) if !c.2 || is_op => self.reply(who, &format!("\u{a7}e{}", c.1)),
                _ => self.reply(who, "\u{a7}cUnknown command."),
            }
            return;
        }
        self.reply(who, "\u{a7}6--- Commands ---");
        for c in COMMANDS {
            if !c.2 || is_op {
                self.reply(who, &format!("\u{a7}e{}", c.1));
            }
        }
    }

    fn cmd_op(&mut self, who: Sender, args: &[&str], grant: bool) {
        let Some(typed) = args.first() else {
            return self.reply(who, if grant { "\u{a7}cUsage: /op <player>" } else { "\u{a7}cUsage: /deop <player>" });
        };
        let (eid, name) = self.resolve(typed);
        let key = name.to_ascii_lowercase();
        if grant {
            if !self.ops.insert(key) {
                return self.reply(who, &format!("\u{a7}e{} is already an operator.", name));
            }
            save_list(OPS_FILE, &self.ops);
            self.reply(who, &format!("\u{a7}aMade {} an operator.", name));
            if let Some(e) = eid {
                self.send_text(e, text_type::RAW, "", "\u{a7}eYou are now an operator.");
                self.send_adventure(e);
            }
        } else {
            if !self.ops.remove(&key) {
                return self.reply(who, &format!("\u{a7}e{} is not an operator.", name));
            }
            save_list(OPS_FILE, &self.ops);
            self.reply(who, &format!("\u{a7}aRemoved operator status from {}.", name));
            if let Some(e) = eid {
                self.send_text(e, text_type::RAW, "", "\u{a7}eYou are no longer an operator.");
                self.send_adventure(e);
            }
        }
    }

    fn cmd_kick(&mut self, who: Sender, args: &[&str]) {
        let Some(typed) = args.first() else {
            return self.reply(who, "\u{a7}cUsage: /kick <player> [reason]");
        };
        let (eid, name) = self.resolve(typed);
        let Some(eid) = eid else {
            return self.reply(who, &format!("\u{a7}cPlayer not found: {}", typed));
        };
        let reason = if args.len() > 1 { args[1..].join(" ") } else { "Kicked by an operator".to_owned() };
        self.kick(eid, &reason);
        self.reply(who, &format!("\u{a7}aKicked {}.", name));
    }

    fn cmd_ban(&mut self, who: Sender, args: &[&str], ban: bool) {
        let Some(typed) = args.first() else {
            return self.reply(who, if ban { "\u{a7}cUsage: /ban <player> [reason]" } else { "\u{a7}cUsage: /unban <player>" });
        };
        let (eid, name) = self.resolve(typed);
        let key = name.to_ascii_lowercase();
        if ban {
            if !self.banned.insert(key) {
                return self.reply(who, &format!("\u{a7}e{} is already banned.", name));
            }
            save_list(BANNED_FILE, &self.banned);
            let reason = if args.len() > 1 { args[1..].join(" ") } else { "Banned by an operator".to_owned() };
            if let Some(e) = eid {
                self.kick(e, &format!("Banned: {}", reason));
            }
            self.reply(who, &format!("\u{a7}aBanned {}.", name));
        } else {
            if !self.banned.remove(&key) {
                return self.reply(who, &format!("\u{a7}e{} is not banned.", name));
            }
            save_list(BANNED_FILE, &self.banned);
            self.reply(who, &format!("\u{a7}aUnbanned {}.", name));
        }
    }

    fn cmd_mute(&mut self, who: Sender, args: &[&str], mute: bool) {
        let Some(typed) = args.first() else {
            return self.reply(who, if mute { "\u{a7}cUsage: /mute <player>" } else { "\u{a7}cUsage: /unmute <player>" });
        };
        let (eid, name) = self.resolve(typed);
        let key = name.to_ascii_lowercase();
        if mute {
            if !self.muted.insert(key) {
                return self.reply(who, &format!("\u{a7}e{} is already muted.", name));
            }
            save_list(MUTED_FILE, &self.muted);
            self.reply(who, &format!("\u{a7}aMuted {}.", name));
            if let Some(e) = eid {
                self.send_text(e, text_type::RAW, "", "\u{a7}cYou have been muted.");
            }
        } else {
            if !self.muted.remove(&key) {
                return self.reply(who, &format!("\u{a7}e{} is not muted.", name));
            }
            save_list(MUTED_FILE, &self.muted);
            self.reply(who, &format!("\u{a7}aUnmuted {}.", name));
            if let Some(e) = eid {
                self.send_text(e, text_type::RAW, "", "\u{a7}aYou can chat again.");
            }
        }
    }

    fn cmd_tp(&mut self, who: Sender, args: &[&str]) {
        const USAGE: &str = "\u{a7}cUsage: /tp <player> | /tp <player> <player> | /tp <x> <y> <z> | /tp <player> <x> <y> <z>";
        let sender_eid = match who {
            Sender::Player(e) => Some(e),
            Sender::Console => None,
        };

        let mut dest_world: Option<WorldId> = None;
        let (mover, target): (i64, (f32, f32, f32)) = match args.len() {
            1 | 2 => {
                let dest_typed = args[args.len() - 1];
                let mover = if args.len() == 2 {
                    match self.find_player(args[0]) {
                        Some(e) => e,
                        None => return self.reply(who, &format!("\u{a7}cPlayer not found: {}", args[0])),
                    }
                } else {
                    match sender_eid {
                        Some(e) => e,
                        None => return self.reply(who, USAGE),
                    }
                };
                let Some(dest) = self.find_player(dest_typed) else {
                    return self.reply(who, &format!("\u{a7}cPlayer not found: {}", dest_typed));
                };
                let Some(pos) = self.feet_pos(dest) else {
                    return self.reply(who, "\u{a7}cPlayer not found.");
                };
                dest_world = Some(self.world_of(dest));
                (mover, pos)
            }
            3 | 4 => {
                let (mover, coords) = if args.len() == 3 {
                    match sender_eid {
                        Some(e) => (e, &args[0..3]),
                        None => return self.reply(who, USAGE),
                    }
                } else {
                    match self.find_player(args[0]) {
                        Some(e) => (e, &args[1..4]),
                        None => return self.reply(who, &format!("\u{a7}cPlayer not found: {}", args[0])),
                    }
                };
                let Some(base) = self.feet_pos(mover) else {
                    return self.reply(who, "\u{a7}cPlayer not found.");
                };
                let (Some(x), Some(y), Some(z)) =
                    (parse_coord(coords[0], base.0), parse_coord(coords[1], base.1), parse_coord(coords[2], base.2))
                else {
                    return self.reply(who, "\u{a7}cInvalid coordinates.");
                };
                (mover, (x, y, z))
            }
            _ => return self.reply(who, USAGE),
        };

        let (x, y, z) = (target.0.clamp(-3.0e7, 3.0e7), target.1.clamp(-64.0, 1000.0), target.2.clamp(-3.0e7, 3.0e7));
        match dest_world {
            Some(w) if w != self.world_of(mover) => {
                self.move_to_world(mover, w, x, y, z, 0.0, 0.0);
            }
            _ => self.teleport(mover, x, y, z),
        }
        let name = self.players.get(&mover).map(|p| p.name.clone()).unwrap_or_default();
        self.reply(who, &format!("\u{a7}aTeleported {} to {:.1}, {:.1}, {:.1}", name, x, y, z));
    }

    fn cmd_tpall(&mut self, who: Sender, args: &[&str]) {
        let Some(name) = args.first() else {
            return self.reply(who, "\u{a7}cUsage: /tpall <player>");
        };
        let Some(target) = self.find_player(name) else {
            return self.reply(who, &format!("\u{a7}cPlayer not found: {}", name));
        };
        let Some((x, y, z)) = self.feet_pos(target) else {
            return self.reply(who, "\u{a7}cTarget is not available.");
        };
        let world = self.world_of(target);
        let movers: Vec<i64> = self.players.values()
            .filter(|p| !p.is_bot && p.eid != target && !p.dead && !p.switching && p.state == crate::entity::player::PlayerState::Spawned)
            .map(|p| p.eid)
            .collect();
        let mut moved = 0usize;
        for eid in movers {
            let ok = if self.world_of(eid) == world {
                self.teleport(eid, x, y, z);
                true
            } else {
                self.move_to_world(eid, world, x, y, z, 0.0, 0.0)
            };
            if ok { moved += 1; }
        }
        self.reply(who, &format!("\u{a7}aTeleported {} player(s) to {}.", moved, name));
    }

    /// Diagnostic: the server's view of the player (is the op list / game mode what the client shows?).
    fn cmd_whoami(&mut self, who: Sender) {
        let Sender::Player(eid) = who else {
            return self.reply(who, "\u{a7}cThis command is for players.");
        };
        let Some(p) = self.players.get(&eid) else { return };
        let op = self.ops.contains(&p.name.to_ascii_lowercase());
        let held = p.held_item();
        let flags = crate::state::adventure_flags(p.gamemode, self.cfg.quirks.legacy_adventure_flags);
        let msg = format!(
            "\u{a7}e{}: op={} gamemode={} world={} hotbar slot={} item={}x{} adventure flags=0x{:x} ({} ops loaded)",
            p.name,
            op,
            p.gamemode,
            p.world,
            p.held,
            held.id,
            held.count,
            flags,
            self.ops.len()
        );
        self.reply(who, &msg);
    }

    fn cmd_list(&mut self, who: Sender) {
        let mut names: Vec<String> = self.players.values()
            .filter(|p| !p.is_bot && p.state == crate::entity::player::PlayerState::Spawned)
            .map(|p| p.name.clone()).collect();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        self.reply(who, &format!("\u{a7}eOnline players ({}): {}", names.len(), if names.is_empty() { "none".into() } else { names.join(", ") }));
    }

    fn cmd_say(&mut self, who: Sender, args: &[&str]) {
        if args.is_empty() { return self.reply(who, "\u{a7}cUsage: /say <message>"); }
        let message = format!("\u{a7}6[Server] \u{a7}f{}", args.join(" "));
        self.broadcast_all_text(text_type::RAW, &message);
        self.reply(who, "\u{a7}aAnnouncement sent.");
    }

    fn cmd_heal(&mut self, who: Sender, args: &[&str]) {
        let target = match args.first() {
            Some(name) => match self.find_player(name) {
                Some(e) => e,
                None => return self.reply(who, &format!("\u{a7}cPlayer not found: {}", name)),
            },
            None => match who { Sender::Player(e) => e, Sender::Console => return self.reply(who, "\u{a7}cUsage: /heal <player>") },
        };
        if self.set_health(target, crate::state::MAX_HEALTH) == crate::state::Hit::Ignored {
            return self.reply(who, "\u{a7}cCould not heal that player (they may be dead).");
        }
        let name = self.players.get(&target).map(|p| p.name.clone()).unwrap_or_default();
        self.reply(who, &format!("\u{a7}aHealed {}.", name));
    }

    fn cmd_clear(&mut self, who: Sender, args: &[&str]) {
        let Some(name) = args.first() else { return self.reply(who, "\u{a7}cUsage: /clear <player>"); };
        let Some(target) = self.find_player(name) else { return self.reply(who, &format!("\u{a7}cPlayer not found: {}", name)); };
        self.clear_inventory(target);
        self.reply(who, &format!("\u{a7}aCleared {}'s inventory.", name));
    }

    fn cmd_give(&mut self, who: Sender, args: &[&str]) {
        if args.len() < 2 || args.len() > 4 { return self.reply(who, "\u{a7}cUsage: /give <player> <item-id> [count] [meta]"); }
        let Some(target) = self.find_player(args[0]) else { return self.reply(who, &format!("\u{a7}cPlayer not found: {}", args[0])); };
        let (Ok(id), Ok(count), Ok(meta)) = (
            args[1].parse::<i16>(),
            args.get(2).copied().unwrap_or("1").parse::<u8>(),
            args.get(3).copied().unwrap_or("0").parse::<i16>(),
        ) else { return self.reply(who, "\u{a7}cItem id, count or meta is invalid."); };
        if id <= 0 || count == 0 || count > 64 || meta < 0 { return self.reply(who, "\u{a7}cUse item id > 0, count 1-64 and meta >= 0."); }
        let left = self.give_item(target, id, count, meta);
        let delivered = count - left;
        self.reply(who, &format!("\u{a7}aGave {} {}x{} ({} did not fit).", args[0], delivered, id, left));
    }

    fn cmd_gamemode(&mut self, who: Sender, args: &[&str]) {
        const USAGE: &str = "\u{a7}cUsage: /gamemode <0|s|1|c|2|a|3|sp> [player]";
        let Some(mode) = args.first().and_then(|m| parse_gamemode(m)) else {
            return self.reply(who, USAGE);
        };
        let target = match (args.get(1), who) {
            (Some(name), _) => match self.find_player(name) {
                Some(e) => e,
                None => return self.reply(who, &format!("\u{a7}cPlayer not found: {}", name)),
            },
            (None, Sender::Player(e)) => e,
            (None, Sender::Console) => return self.reply(who, USAGE),
        };
        self.set_gamemode(target, mode);
        let name = self.players.get(&target).map(|p| p.name.clone()).unwrap_or_default();
        let label = ["Survival", "Creative", "Adventure", "Spectator"][mode as usize];
        self.reply(who, &format!("\u{a7}aSet {}'s game mode to {}.", name, label));
        if Sender::Player(target) != who {
            self.send_text(target, text_type::RAW, "", &format!("\u{a7}eYour game mode is now {}.", label));
        }
    }

    fn cmd_fill(&mut self, who: Sender, args: &[&str]) {
        const USAGE: &str = "\u{a7}cUsage: /fill <x1> <y1> <z1> <x2> <y2> <z2> <block[:meta]> [meta]";
        if args.len() < 7 || args.len() > 8 {
            return self.reply(who, USAGE);
        }
        let base = match who {
            Sender::Player(e) => self.feet_pos(e).unwrap_or((0.0, 0.0, 0.0)),
            Sender::Console => (0.0, 0.0, 0.0),
        };
        let bases = [base.0, base.1, base.2];
        let mut c = [0i64; 6];
        for i in 0..6 {
            match parse_coord(args[i], bases[i % 3]) {
                Some(v) => c[i] = v.clamp(-3.0e7, 3.0e7).floor() as i64,
                None => return self.reply(who, "\u{a7}cInvalid coordinates."),
            }
        }
        let Some((id, meta)) = parse_block(args[6], args.get(7).copied()) else {
            return self.reply(who, "\u{a7}cUnknown block. Use a name (stone, wool, glass ...) or a numeric id.");
        };

        let (x1, x2) = (c[0].min(c[3]), c[0].max(c[3]));
        let (y1, y2) = (c[1].min(c[4]), c[1].max(c[4]));
        let (z1, z2) = (c[2].min(c[5]), c[2].max(c[5]));
        if y1 < 0 || y2 > 127 {
            return self.reply(who, "\u{a7}cY must be between 0 and 127.");
        }
        let volume = ((x2 - x1 + 1) as f64) * ((y2 - y1 + 1) as f64) * ((z2 - z1 + 1) as f64);
        if volume > MAX_FILL_BLOCKS {
            return self.reply(who, &format!("\u{a7}cThat region is too large (max {} blocks).", MAX_FILL_BLOCKS as u32));
        }

        let wid: WorldId = match who {
            Sender::Player(e) => self.world_of(e),
            Sender::Console => TEMPLATE,
        };
        let mut records: Vec<BlockRecord> = Vec::new();
        let mut outside = 0usize;
        for x in x1..=x2 {
            for z in z1..=z2 {
                for y in y1..=y2 {
                    let (xi, yi, zi) = (x as i32, y as i32, z as i32);
                    if self.get_block(wid, xi, yi, zi) == (id, meta) {
                        continue;
                    }
                    if self.worlds.get_mut(&wid).map_or(false, |w| w.set_block(xi, yi, zi, id, meta)) {
                        records.push(BlockRecord { x: xi, y: yi, z: zi, id, meta });
                    } else {
                        outside += 1;
                    }
                }
            }
        }
        let changed = records.len();

        let mut by_chunk: HashMap<(i32, i32), Vec<BlockRecord>> = HashMap::new();
        for r in records {
            by_chunk.entry((r.x >> 4, r.z >> 4)).or_default().push(r);
        }
        for ((cx, cz), recs) in by_chunk {
            for part in recs.chunks(256) {
                self.send_to_chunk_viewers(wid, cx, cz, play::update_block(part));
            }
        }

        let mut msg = format!("\u{a7}aFilled {} block(s) with {}:{}.", changed, id, meta);
        if outside > 0 {
            msg.push_str(&format!(" \u{a7}e{} outside the world were skipped.", outside));
        }
        self.reply(who, &msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_specs() {
        assert_eq!(parse_block("stone", None), Some((1, 0)));
        assert_eq!(parse_block("WOOL", Some("14")), Some((35, 14)));
        assert_eq!(parse_block("35:11", None), Some((35, 11)));
        assert_eq!(parse_block("wool:14", None), Some((35, 14)));
        assert_eq!(parse_block("256", None), None);
        assert_eq!(parse_block("stone", Some("16")), None);
        assert_eq!(parse_block("nonsense", None), None);
    }

    #[test]
    fn gamemodes() {
        for (t, m) in [("0", 0), ("S", 0), ("survival", 0), ("1", 1), ("c", 1), ("2", 2), ("a", 2), ("3", 3), ("SP", 3)] {
            assert_eq!(parse_gamemode(t), Some(m), "{t}");
        }
        assert_eq!(parse_gamemode("4"), None);
        assert_eq!(parse_gamemode("x"), None);
    }

    #[test]
    fn coordinates() {
        assert_eq!(parse_coord("12", 5.0), Some(12.0));
        assert_eq!(parse_coord("~", 5.0), Some(5.0));
        assert_eq!(parse_coord("~-2", 5.0), Some(3.0));
        assert_eq!(parse_coord("abc", 5.0), None);
    }

    #[test]
    fn colors_are_stripped() {
        assert_eq!(strip_colors("\u{a7}aHello \u{a7}cworld"), "Hello world");
    }
}
