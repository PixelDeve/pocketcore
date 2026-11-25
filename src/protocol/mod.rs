//! MCPE 0.14.3 protocol layer (protocol 70).

pub mod batch;
pub mod ids;
pub mod item;
pub mod packets;
pub mod play;

pub const PROTOCOL_VERSION: u32 = 70;
/// Every 0.14 game packet travels inside a RakNet frame as `0x8e` + packet id
/// + body (PocketMine: MCPE_RAKNET_PACKET_ID). Without it the client ignores us.
pub const GAME_PACKET_WRAPPER: u8 = 0x8e;
pub const GAME_VERSION: &str = "0.14.3";
/// MOTD protocol field is "<minimum> <current>" (wiki.vg: `MCPE;name;70 70;0.14.3;0;20`).
pub const MOTD_PROTOCOL: &str = "70 70";

/// Wire details that could not be verified offline. They are configurable in
/// `server.toml` ([protocol]) so they can be tuned against a real client
/// without recompiling.
#[derive(Clone, Debug)]
pub struct Quirks {
    /// FullChunkData carries an `order` byte (0 = columns) before the length.
    pub chunk_order_byte: bool,
    /// Bytes appended to StartGame after the spawn position floats.
    pub start_game_tail: Vec<u8>,
    /// Also send the extra MovePlayer reset packets and a Respawn during the
    /// join sequence (older experimental behaviour; off = PocketMine order).
    pub join_extras: bool,
    /// AdventureSettings bit layout: true = 0.14 table (0x40 auto jump, 0x80 allow flight,
    /// 0x100 no clip), false = 0.15+ table (0x40 allow flight, 0x80 no clip, 0x200 flying).
    pub legacy_adventure_flags: bool,
    /// PocketMine `doFirstSpawn` order: AdventureSettings, time and a Respawn
    /// packet are sent right before PLAYER_SPAWN.
    pub spawn_respawn: bool,
    /// Append the user-permission int (2) that legacy PocketMine sets in AdventureSettings.
    pub adventure_permission: bool,
    /// Prepend `hotbar_offset` empty placeholder slots to the survival inventory
    /// contents (old behaviour: shows 9 empty slots before the items).
    pub inventory_placeholders: bool,
    /// Survival: reject a RemoveBlock that arrives long before the block's break time.
    pub enforce_break_time: bool,
}

impl Default for Quirks {
    fn default() -> Self {
        Self { chunk_order_byte: true, start_game_tail: vec![0; 16], join_extras: false, legacy_adventure_flags: true, spawn_respawn: false, adventure_permission: false, inventory_placeholders: true, enforce_break_time: false }
    }
}
