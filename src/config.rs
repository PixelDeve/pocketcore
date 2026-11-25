use std::fs;
use std::io;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServerConfig {
    pub server: ServerSection,
    pub network: NetworkSection,
    pub world: WorldSection,
    pub protocol: ProtocolSection,
    pub pvp: PvpSection,
    pub scripting: ScriptingSection,
    pub bedwars: BedwarsSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSection {
    pub name: String,
    pub max_players: u32,
    /// Maximum chunk radius a client may request.
    pub view_distance: i32,
    /// 0 survival, 1 creative, 2 adventure.
    pub default_gamemode: i32,
    /// Movement/animation broadcasts reach players within this many blocks.
    pub broadcast_radius: f32,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self { name: "Bedwars Lobby".into(), max_players: 300, view_distance: 6, default_gamemode: 0, broadcast_radius: 96.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSection {
    pub bind: String,
    pub max_mtu: usize,
    pub session_timeout_secs: u64,
    pub max_connections_per_ip: usize,
}

impl Default for NetworkSection {
    fn default() -> Self {
        Self { bind: "0.0.0.0:19132".into(), max_mtu: 1492, session_timeout_secs: 10, max_connections_per_ip: 5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldSection {
    /// "flat" or "void".
    pub generator: String,
    /// Chunk radius around the origin that exists (0 = unbounded).
    pub limit_chunks: i32,
    pub spawn: [i32; 3],
}

impl Default for WorldSection {
    fn default() -> Self {
        Self { generator: "flat".into(), limit_chunks: 8, spawn: [0, 4, 0] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolSection {
    /// FullChunkData has an `order` byte before the length.
    pub chunk_order_byte: bool,
    /// Hex bytes appended to StartGame after the position floats.
    pub start_game_tail: String,
    /// Log every received game packet at debug level.
    pub trace_packets: bool,
    /// Placeholder slots before the real inventory in ContainerSetContent (9 in PM 0.14; 0 = plain).
    pub hotbar_offset: usize,
    /// Send the extra MovePlayer / Respawn packets during join (off = PocketMine order).
    pub join_extras: bool,
    /// AdventureSettings bit layout: "legacy" (0.14: 0x80 allow flight, 0x100 no clip) or "modern" (0.15+: 0x40 / 0x80).
    pub adventure_flags: String,
    /// Send Respawn (plus settings and time) right before PLAYER_SPAWN, like PocketMine.
    pub spawn_respawn: bool,
    /// Append the permission int (2) to AdventureSettings, like legacy PocketMine.
    pub adventure_permission: bool,
    /// Old behaviour: empty placeholder slots before the survival inventory.
    pub inventory_placeholders: bool,
    /// Survival break timing check (vanilla hardness and tool speeds).
    pub enforce_break_time: bool,
}

impl Default for ProtocolSection {
    fn default() -> Self {
        Self {
            chunk_order_byte: true,
            start_game_tail: "00".repeat(16),
            trace_packets: false,
            hotbar_offset: 9,
            join_extras: false,
            adventure_flags: "legacy".into(),
            spawn_respawn: true,
            adventure_permission: true,
            inventory_placeholders: false,
            enforce_break_time: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PvpSection {
    pub knockback_horizontal: f32,
    pub knockback_vertical: f32,
    pub attack_cooldown_ticks: u64,
    pub max_reach: f32,
}

impl Default for PvpSection {
    fn default() -> Self {
        Self { knockback_horizontal: 0.4, knockback_vertical: 0.4, attack_cooldown_ticks: 8, max_reach: 6.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptingSection {
    pub enabled: bool,
    pub dir: String,
    /// Check plugin files for changes every N ticks (20 ticks = 1 s; 0 = never).
    pub reload_check_ticks: u64,
}

impl Default for ScriptingSection {
    fn default() -> Self {
        Self { enabled: true, dir: "plugins".into(), reload_check_ticks: 40 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BedwarsSection {
    /// Players per team (4 teams). Bots fill the rest.
    pub team_size: usize,
    /// Seconds a lobby waits for humans before bots fill the free slots.
    pub bot_wait_secs: u64,
    pub countdown_secs: u64,
    pub respawn_secs: u64,
    pub end_delay_secs: u64,
    /// Generator intervals in ticks (20 ticks = 1 s).
    pub iron_ticks: u64,
    pub gold_ticks: u64,
    pub emerald_ticks: u64,
    /// Feet below this height kill the player.
    pub void_y: f32,
    pub build_limit_y: i32,
    /// Folder holding `maps/`, `shop.toml` and `stats.toml`.
    pub dir: String,
    /// World file placed on the server as the template (relative to `dir`/maps unless it has a slash).
    pub map: String,
    /// "gui" = chest shop (players can opt out with /shop chat), "chat" = chat shop only.
    pub shop: String,
}

impl Default for BedwarsSection {
    fn default() -> Self {
        Self {
            team_size: 2,
            bot_wait_secs: 40,
            countdown_secs: 5,
            respawn_secs: 5,
            end_delay_secs: 10,
            iron_ticks: 40,
            gold_ticks: 240,
            emerald_ticks: 1200,
            void_y: 0.0,
            build_limit_y: 120,
            dir: ".".into(),
            map: "original".into(),
            shop: "gui".into(),
        }
    }
}

impl BedwarsSection {
    pub fn to_bw(&self) -> crate::bedwars::BwConfig {
        let d = crate::bedwars::BwConfig::default();
        crate::bedwars::BwConfig {
            team_size: self.team_size.clamp(1, 16),
            bot_wait_ticks: self.bot_wait_secs * 20,
            countdown_ticks: self.countdown_secs * 20,
            end_delay_ticks: self.end_delay_secs * 20,
            respawn_ticks: self.respawn_secs * 20,
            iron_every: self.iron_ticks.max(1),
            gold_every: self.gold_ticks.max(1),
            emerald_every: self.emerald_ticks.max(1),
            void_y: self.void_y,
            build_limit_y: self.build_limit_y,
            dir: self.dir.clone().into(),
            shop_gui: !self.shop.eq_ignore_ascii_case("chat"),
            ..d
        }
    }
}

pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() % 2 != 0 {
        return Err("odd number of hex digits".into());
    }
    digits
        .chunks(2)
        .map(|pair| {
            let s: String = pair.iter().collect();
            u8::from_str_radix(&s, 16).map_err(|e| format!("bad hex {s:?}: {e}"))
        })
        .collect()
}

/// Loads `path`; writes a default file if it does not exist.
pub fn load(path: &str) -> io::Result<ServerConfig> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{path}: {e}"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let cfg = ServerConfig::default();
            let text = toml::to_string_pretty(&cfg).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            fs::write(path, text)?;
            log::info!("wrote default {path}");
            Ok(cfg)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex() {
        assert_eq!(parse_hex("00 ff 1A").unwrap(), vec![0, 255, 26]);
        assert!(parse_hex("abc").is_err());
        assert!(parse_hex("zz").is_err());
    }

    #[test]
    fn default_roundtrips() {
        let text = toml::to_string_pretty(&ServerConfig::default()).unwrap();
        let back: ServerConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.world.spawn, [0, 4, 0]);
    }
}
