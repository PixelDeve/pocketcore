//! Packet encoders / decoders for MCPE 0.14.3 (protocol 70).
//!
//! Every encoder returns a complete packet (id byte first). Numeric fields
//! are big-endian. Layouts follow the PocketMine 0.14 implementation; the
//! two details that could not be checked offline (StartGame tail, chunk
//! `order` byte) are driven by [`super::Quirks`].

use super::ids;
use crate::binary::{BinResult, BinaryError, ByteReader, ByteWriter, Vec3};

/// The local player is addressed as entity id -1 in 0.14 (wiki.vg login notes).
pub const LOCAL_PLAYER_EID: i64 = -1;

pub mod status {
    pub const LOGIN_SUCCESS: u32 = 0;
    pub const FAILED_CLIENT: u32 = 1;
    pub const FAILED_SERVER: u32 = 2;
    pub const PLAYER_SPAWN: u32 = 3;
}

pub mod adventure {
    pub const IMMUTABLE_WORLD: u32 = 0x01;
    pub const NO_PVP: u32 = 0x02;
    pub const NO_PVM: u32 = 0x04;
    pub const NO_MVP: u32 = 0x08;
    pub const AUTO_JUMP: u32 = 0x20;
    pub const ALLOW_FLIGHT: u32 = 0x40;
    pub const NO_CLIP: u32 = 0x80;
    pub const FLYING: u32 = 0x200;

    /// 0.14 table (PocketMine of the time): 0x20 nametags, 0x40 auto jump, 0x80 allow flight, 0x100 no clip.
    pub const LEGACY_AUTO_JUMP: u32 = 0x40;
    pub const LEGACY_ALLOW_FLIGHT: u32 = 0x80;
    pub const LEGACY_NO_CLIP: u32 = 0x100;
}

pub const MOVE_NORMAL: u8 = 0;
pub const MOVE_RESET: u8 = 1;
pub const MOVE_ROTATION: u8 = 2;

// ---------------------------------------------------------------------------
// Login (client -> server)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct LoginExtra {
    pub client_id: i64,
    pub uuid: [u8; 16],
    pub server_address: String,
    pub client_secret: String,
    pub skin_slim: bool,
    pub skin: Vec<u8>,
}

#[derive(Debug)]
pub struct LoginPacket {
    pub username: String,
    pub protocol1: i32,
    pub protocol2: i32,
    /// `None` if the remainder could not be parsed (e.g. a client speaking a
    /// different protocol); the version check happens first.
    pub extra: Option<LoginExtra>,
}

impl LoginPacket {
    /// `body` excludes the id byte.
    pub fn decode(body: &[u8]) -> BinResult<Self> {
        let mut r = ByteReader::new(body);
        let username = r.string_be()?;
        let protocol1 = r.i32_be()?;
        let protocol2 = r.i32_be()?;
        let extra = Self::decode_extra(&mut r).ok();
        Ok(Self { username, protocol1, protocol2, extra })
    }

    fn decode_extra(r: &mut ByteReader<'_>) -> BinResult<LoginExtra> {
        let client_id = r.i64_be()?;
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(r.bytes(16)?);
        let server_address = r.string_be()?;
        // The skin (u16 length + 8192/16384 bytes) is the last field. Locate it
        // from the end so the fields in front of it (secret, slim flag, skin
        // name, whatever this client version sends) cannot throw the parse off.
        let tail = r.rest();
        let (pre, skin) = split_skin(tail).ok_or(BinaryError::Invalid("skin"))?;
        let mut pr = ByteReader::new(pre);
        let client_secret = pr.string_be().unwrap_or_default();
        let skin_slim = match pr.remaining() {
            0 => false,
            1 => pr.u8()? != 0,
            _ => pr.string_be().map(|n| n.contains("Slim")).unwrap_or(false),
        };
        Ok(LoginExtra { client_id, uuid, server_address, client_secret, skin_slim, skin: skin.to_vec() })
    }
}

fn split_skin(tail: &[u8]) -> Option<(&[u8], &[u8])> {
    for skin_len in [16384usize, 8192] {
        let Some(start) = tail.len().checked_sub(skin_len + 2) else { continue };
        if tail[start..start + 2] == (skin_len as u16).to_be_bytes() {
            return Some((&tail[..start], &tail[start + 2..]));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Movement (both directions)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct MovePlayer {
    pub eid: i64,
    pub pos: Vec3,
    pub yaw: f32,
    pub body_yaw: f32,
    pub pitch: f32,
    pub mode: u8,
    pub on_ground: bool,
}

impl MovePlayer {
    /// `body` excludes the id byte.
    pub fn decode(body: &[u8]) -> BinResult<Self> {
        let mut r = ByteReader::new(body);
        Ok(Self {
            eid: r.i64_be()?,
            pos: r.vec3()?,
            yaw: r.f32_be()?,
            body_yaw: r.f32_be()?,
            pitch: r.f32_be()?,
            mode: r.u8()?,
            on_ground: r.bool()?,
        })
    }
}

pub fn move_player(eid: i64, pos: Vec3, yaw: f32, body_yaw: f32, pitch: f32, mode: u8, on_ground: bool) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(43);
    w.put_u8(ids::MOVE_PLAYER);
    w.put_i64_be(eid);
    w.put_vec3(pos);
    w.put_f32_be(yaw);
    w.put_f32_be(body_yaw);
    w.put_f32_be(pitch);
    w.put_u8(mode);
    w.put_bool(on_ground);
    w.into_inner()
}

pub fn decode_request_chunk_radius(body: &[u8]) -> BinResult<i32> {
    ByteReader::new(body).i32_be()
}

// ---------------------------------------------------------------------------
// Server -> client
// ---------------------------------------------------------------------------

pub fn play_status(status: u32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::PLAY_STATUS);
    w.put_u32_be(status);
    w.into_inner()
}

pub fn disconnect(message: &str) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(3 + message.len());
    w.put_u8(ids::DISCONNECT);
    w.put_string_be(message);
    w.into_inner()
}

pub struct StartGame {
    pub seed: i32,
    pub dimension: u8,
    /// 0 = old (limited), 1 = infinite, 2 = flat.
    pub generator: i32,
    /// 0 survival, 1 creative, 2 adventure.
    pub gamemode: i32,
    pub eid: i64,
    pub spawn: (i32, i32, i32),
    /// Player position (y includes the 1.62 eye offset).
    pub pos: Vec3,
}

pub fn start_game(sg: &StartGame, tail: &[u8]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(64 + tail.len());
    w.put_u8(ids::START_GAME);
    w.put_i32_be(sg.seed);
    w.put_u8(sg.dimension);
    w.put_i32_be(sg.generator);
    w.put_i32_be(sg.gamemode);
    w.put_i64_be(sg.eid);
    w.put_i32_be(sg.spawn.0);
    w.put_i32_be(sg.spawn.1);
    w.put_i32_be(sg.spawn.2);
    w.put_vec3(sg.pos);
    w.put_bytes(tail);
    w.into_inner()
}

pub fn set_difficulty(difficulty: u32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::SET_DIFFICULTY);
    w.put_u32_be(difficulty);
    w.into_inner()
}

pub fn set_time(time: i32, started: bool) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(6);
    w.put_u8(ids::SET_TIME);
    w.put_i32_be(time);
    w.put_bool(started);
    w.into_inner()
}

/// Layout x, z, y (matches the RemoveBlock/UpdateBlock coordinate order).
pub fn set_spawn_position(x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(13);
    w.put_u8(ids::SET_SPAWN_POSITION);
    w.put_i32_be(x);
    w.put_i32_be(z);
    w.put_i32_be(y);
    w.into_inner()
}

/// Flags plus a trailing user-permission int (extra trailing bytes are
/// ignored by clients that do not read it).
pub fn adventure_settings(flags: u32) -> Vec<u8> {
    // MCPE 0.14.x AdventureSettingsPacket contains only the 32-bit flags field.
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::ADVENTURE_SETTINGS);
    w.put_u32_be(flags);
    w.into_inner()
}

/// Flags, optionally followed by BOTH permission ints legacy PocketMine fills
/// in (`userPermission` and `globalPermission`, both 2). Sending the longer form
/// is the safe superset: a client that reads fewer ints ignores the rest, while
/// a client that reads more would drop a short packet and ignore every flag.
pub fn adventure_settings_ext(flags: u32, permission: Option<u32>) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(13);
    w.put_u8(ids::ADVENTURE_SETTINGS);
    w.put_u32_be(flags);
    if let Some(p) = permission {
        w.put_u32_be(p); // userPermission
        w.put_u32_be(p); // globalPermission
    }
    w.into_inner()
}

pub fn respawn(pos: Vec3) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(13);
    w.put_u8(ids::RESPAWN);
    w.put_vec3(pos);
    w.into_inner()
}

pub fn chunk_radius_update(radius: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::CHUNK_RADIUS_UPDATE);
    w.put_i32_be(radius);
    w.into_inner()
}

/// FullChunkData: x, z, [order byte], data length, data.
pub fn full_chunk_data(x: i32, z: i32, data: &[u8], order_byte: bool) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(14 + data.len());
    w.put_u8(ids::FULL_CHUNK_DATA);
    w.put_i32_be(x);
    w.put_i32_be(z);
    if order_byte {
        w.put_u8(0); // ORDER_COLUMNS
    }
    w.put_i32_be(data.len() as i32);
    w.put_bytes(data);
    w.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_decode() {
        let mut w = ByteWriter::new();
        w.put_string_be("Steve");
        w.put_i32_be(70);
        w.put_i32_be(70);
        w.put_i64_be(1234);
        w.put_bytes(&[7u8; 16]);
        w.put_string_be("127.0.0.1:19132");
        w.put_string_be("secret");
        w.put_bool(false);
        w.put_u16_be(8192);
        w.put_bytes(&vec![1u8; 8192]);
        let p = LoginPacket::decode(w.as_slice()).unwrap();
        assert_eq!(p.username, "Steve");
        assert_eq!(p.protocol1, 70);
        let e = p.extra.unwrap();
        assert_eq!(e.client_id, 1234);
        assert_eq!(e.uuid, [7u8; 16]);
        assert_eq!(e.skin.len(), 8192);
    }

    #[test]
    fn login_with_skin_name_layout() {
        let mut w = ByteWriter::new();
        w.put_string_be("Alex");
        w.put_i32_be(70);
        w.put_i32_be(70);
        w.put_i64_be(5);
        w.put_bytes(&[9u8; 16]);
        w.put_string_be("1.2.3.4:19132");
        w.put_string_be("0123456789abcd");
        w.put_string_be("Standard_CustomSlim");
        w.put_u16_be(16384);
        w.put_bytes(&vec![2u8; 16384]);
        let e = LoginPacket::decode(w.as_slice()).unwrap().extra.unwrap();
        assert_eq!(e.skin.len(), 16384);
        assert!(e.skin_slim);
        assert_eq!(e.uuid, [9u8; 16]);
    }

    #[test]
    fn login_wrong_protocol_still_reports_version() {
        let mut w = ByteWriter::new();
        w.put_string_be("Old");
        w.put_i32_be(60);
        w.put_i32_be(60);
        w.put_bytes(&[1, 2, 3]); // layout we do not understand
        let p = LoginPacket::decode(w.as_slice()).unwrap();
        assert_eq!(p.protocol1, 60);
        assert!(p.extra.is_none());
    }

    #[test]
    fn move_player_roundtrip() {
        let pkt = move_player(-1, Vec3 { x: 1.0, y: 5.62, z: -3.5 }, 90.0, 90.0, 10.0, MOVE_RESET, true);
        let m = MovePlayer::decode(&pkt[1..]).unwrap();
        assert_eq!(m.eid, -1);
        assert_eq!(m.pos, Vec3 { x: 1.0, y: 5.62, z: -3.5 });
        assert_eq!(m.mode, MOVE_RESET);
        assert!(m.on_ground);
    }
}