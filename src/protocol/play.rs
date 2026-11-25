//! Gameplay packets (Step 4): chat, entities, blocks, items, inventory.
//! Layouts follow PocketMine 0.14; entity ids are i64.

use super::ids;
use super::item::{read_slot, write_slot, Slot};
use crate::binary::{BinResult, BlockPos, ByteReader, ByteWriter, MetaValue, Vec3};

pub mod text_type {
    pub const RAW: u8 = 0;
    pub const CHAT: u8 = 1;
    pub const POPUP: u8 = 3;
    pub const TIP: u8 = 4;
    pub const SYSTEM: u8 = 5;
}

pub mod action {
    pub const START_BREAK: i32 = 0;
    pub const ABORT_BREAK: i32 = 1;
    pub const STOP_BREAK: i32 = 2;
    pub const RELEASE_ITEM: i32 = 5;
    pub const RESPAWN: i32 = 7;
}

pub mod event {
    pub const HURT: u8 = 2;
    pub const DEATH: u8 = 3;
}

pub const WINDOW_INVENTORY: u8 = 0;
pub const WINDOW_ARMOR: u8 = 0x78;
/// The creative menu is a pseudo container the server fills (PocketMine: SPECIAL_CREATIVE).
pub const WINDOW_CREATIVE: u8 = 0x79;
/// UpdateBlock flags: neighbours | network | priority.
pub const BLOCK_FLAGS: u8 = 0x0b;

/// LevelEvent: block break particles + sound (PocketMine `LevelEventPacket::EVENT_PARTICLE_DESTROY`).
pub const EVENT_PARTICLE_DESTROY: i16 = 2001;

/// Writes the unsigned VarInt representation used by legacy Bedrock LevelEvent.
/// Event IDs and the event data used by this server are non-negative.
#[inline]
fn put_level_event_varint(out: &mut Vec<u8>, value: u32) {
    let mut value = value;
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// LevelEvent: VarInt event id, big-endian position vector, VarInt event data.
/// This is used for legacy sounds and block-destruction particles.
pub fn level_event(evid: i16, pos: Vec3, data: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(20);
    w.put_u8(ids::LEVEL_EVENT);
    let mut out = Vec::with_capacity(19);
    put_level_event_varint(&mut out, evid.max(0) as u32);
    w.put_bytes(&out);
    w.put_vec3(pos);
    out.clear();
    put_level_event_varint(&mut out, data.max(0) as u32);
    w.put_bytes(&out);
    w.into_inner()
}

// ---- chat -----------------------------------------------------------------

pub fn text(kind: u8, source: &str, message: &str) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(8 + source.len() + message.len());
    w.put_u8(ids::TEXT);
    w.put_u8(kind);
    if kind == text_type::CHAT || kind == text_type::POPUP {
        w.put_string_be(source);
    }
    w.put_string_be(message);
    w.into_inner()
}

#[derive(Debug)]
pub struct TextIn {
    pub kind: u8,
    pub source: String,
    pub message: String,
}

pub fn decode_text(body: &[u8]) -> BinResult<TextIn> {
    let mut r = ByteReader::new(body);
    let kind = r.u8()?;
    let source = if kind == text_type::CHAT || kind == text_type::POPUP { r.string_be()? } else { String::new() };
    let message = r.string_be()?;
    Ok(TextIn { kind, source, message })
}

// ---- players / entities -------------------------------------------------------

pub fn add_player(uuid: &[u8; 16], name: &str, eid: i64, pos: Vec3, yaw: f32, pitch: f32, held: &Slot) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(128 + name.len() * 2);
    w.put_u8(ids::ADD_PLAYER);
    w.put_bytes(uuid);
    w.put_string_be(name);
    w.put_i64_be(eid);
    w.put_vec3(pos);
    w.put_vec3(Vec3::default()); // speed
    w.put_f32_be(yaw);
    w.put_f32_be(yaw); // head yaw
    w.put_f32_be(pitch);
    write_slot(&mut w, held);
    w.put_metadata(&[
        (0, MetaValue::Byte(0)),
        (1, MetaValue::Short(300)),
        (2, MetaValue::String(name.to_owned())),
        (3, MetaValue::Byte(1)),
        (4, MetaValue::Byte(0)),
    ]);
    w.into_inner()
}


/// Spawns an invisible fake player whose always-visible name tag acts as a hologram.
pub fn add_hologram(uuid: &[u8; 16], name: &str, eid: i64, pos: Vec3) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(128 + name.len() * 2);
    w.put_u8(ids::ADD_PLAYER);
    w.put_bytes(uuid);
    w.put_string_be(name);
    w.put_i64_be(eid);
    w.put_vec3(pos);
    w.put_vec3(Vec3::default());
    w.put_f32_be(0.0);
    w.put_f32_be(0.0);
    w.put_f32_be(0.0);
    write_slot(&mut w, &Slot::EMPTY);
    w.put_metadata(&[
        (0, MetaValue::Byte(0x20)),
        (1, MetaValue::Short(300)),
        (2, MetaValue::String(name.to_owned())),
        (3, MetaValue::Byte(1)),
        (4, MetaValue::Byte(0)),
    ]);
    w.into_inner()
}

/// Changes the visible name tag of an existing fake-player hologram.
pub fn set_entity_name_tag(eid: i64, name: &str) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(32 + name.len());
    w.put_u8(ids::SET_ENTITY_DATA);
    w.put_i64_be(eid);
    w.put_metadata(&[
        (2, MetaValue::String(name.to_owned())),
        (3, MetaValue::Byte(1)),
    ]);
    w.into_inner()
}

pub fn remove_player(eid: i64, uuid: &[u8; 16]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(25);
    w.put_u8(ids::REMOVE_PLAYER);
    w.put_i64_be(eid);
    w.put_bytes(uuid);
    w.into_inner()
}

pub struct PlayerListEntry<'a> {
    pub uuid: &'a [u8; 16],
    pub eid: i64,
    pub name: &'a str,
    pub slim: bool,
    pub skin: &'a [u8],
}

pub fn player_list_add(entries: &[PlayerListEntry<'_>]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(16 + entries.len() * 8300);
    w.put_u8(ids::PLAYER_LIST);
    w.put_u8(0);
    w.put_i32_be(entries.len() as i32);
    for e in entries {
        w.put_bytes(e.uuid);
        w.put_i64_be(e.eid);
        w.put_string_be(e.name);
        w.put_string_be(if e.slim { "Standard_CustomSlim" } else { "Standard_Custom" });
        w.put_u16_be(e.skin.len() as u16);
        w.put_bytes(e.skin);
    }
    w.into_inner()
}

pub fn player_list_remove(uuids: &[[u8; 16]]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(8 + uuids.len() * 16);
    w.put_u8(ids::PLAYER_LIST);
    w.put_u8(1);
    w.put_i32_be(uuids.len() as i32);
    for u in uuids {
        w.put_bytes(u);
    }
    w.into_inner()
}

pub fn remove_entity(eid: i64) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(9);
    w.put_u8(ids::REMOVE_ENTITY);
    w.put_i64_be(eid);
    w.into_inner()
}

pub fn add_item_entity(eid: i64, item: &Slot, pos: Vec3, speed: Vec3) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(48);
    w.put_u8(ids::ADD_ITEM_ENTITY);
    w.put_i64_be(eid);
    write_slot(&mut w, item);
    w.put_vec3(pos);
    w.put_vec3(speed);
    w.into_inner()
}

pub fn take_item_entity(item_eid: i64, taker_eid: i64) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(17);
    w.put_u8(ids::TAKE_ITEM_ENTITY);
    w.put_i64_be(item_eid);
    w.put_i64_be(taker_eid);
    w.into_inner()
}

pub fn set_health(health: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::SET_HEALTH);
    w.put_i32_be(health);
    w.into_inner()
}

/// eid, event, plus a trailing i32 of event data (ignored if unread).
pub fn entity_event(eid: i64, event: u8) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(14);
    w.put_u8(ids::ENTITY_EVENT);
    w.put_i64_be(eid);
    w.put_u8(event);
    w.put_i32_be(0);
    w.into_inner()
}

pub fn set_entity_motion(entries: &[(i64, Vec3)]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5 + entries.len() * 20);
    w.put_u8(ids::SET_ENTITY_MOTION);
    w.put_i32_be(entries.len() as i32);
    for (eid, m) in entries {
        w.put_i64_be(*eid);
        w.put_vec3(*m);
    }
    w.into_inner()
}

pub fn animate(action: u8, eid: i64) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(10);
    w.put_u8(ids::ANIMATE);
    w.put_u8(action);
    w.put_i64_be(eid);
    w.into_inner()
}

/// The 0.14 client only knows survival (0) and creative (1), like PocketMine's `mode & 1`.
/// Adventure is survival plus the "immutable world" adventure flag; spectator is shown as
/// creative (flight) and made a spectator with the adventure flags.
pub fn client_gametype(mode: i32) -> i32 {
    match mode {
        1 | 3 => 1,
        _ => 0,
    }
}

pub fn set_player_gametype(mode: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5);
    w.put_u8(ids::SET_PLAYER_GAMETYPE);
    w.put_i32_be(client_gametype(mode));
    w.into_inner()
}

// ---- blocks -----------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct BlockRecord {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub id: u8,
    pub meta: u8,
}

pub fn update_block(records: &[BlockRecord]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(5 + records.len() * 11);
    w.put_u8(ids::UPDATE_BLOCK);
    w.put_i32_be(records.len() as i32);
    for r in records {
        w.put_i32_be(r.x);
        w.put_i32_be(r.z);
        w.put_u8(r.y as u8);
        w.put_u8(r.id);
        w.put_u8(BLOCK_FLAGS << 4 | (r.meta & 0x0f));
    }
    w.into_inner()
}

#[derive(Debug)]
pub struct RemoveBlockIn {
    pub eid: i64,
    pub pos: BlockPos,
}

pub fn decode_remove_block(body: &[u8]) -> BlockResult<RemoveBlockIn> {
    let mut r = ByteReader::new(body);
    let eid = r.i64_be()?;
    let pos = r.block_pos_xzy()?;
    Ok(RemoveBlockIn { eid, pos })
}

type BlockResult<T> = BinResult<T>;

#[derive(Debug)]
pub struct UseItemIn {
    pub pos: BlockPos,
    /// 0..=5 when a block face was clicked; 0xff is an air-use action.
    pub face: u8,
    /// Relative click position inside the clicked block.
    pub click_offset: Vec3,
    /// Client-reported player position (diagnostic only; server position is authoritative).
    pub player_pos: Vec3,
    /// Item stack reported by the client.
    pub item: Slot,
    /// Optional 32-bit field found between the player position and the item
    /// in the live 0.14.3 trace (value 0 so far; probably the hotbar slot).
    pub trailing_i32: Option<i32>,
}

/// Legacy Pocket Edition UseItem layout: xyz:i32be, face:u8, click vector:f32be*3,
/// player position:f32be*3, and legacy Item slot. Some 0.14.3 clients append a
/// 32-bit extension after the slot; accept that observed extension
/// without shifting or corrupting the Item slot.
pub fn decode_use_item(body: &[u8]) -> BinResult<UseItemIn> {
    let mut r = ByteReader::new(body);
    let pos = r.block_pos_xyz()?;
    let face = r.u8()?;
    let click_offset = r.vec3_le()?;
    let player_pos = r.vec3_le()?;
    // Plain legacy layout first: the item follows the player position and
    // consumes the rest of the packet exactly.
    let rest = &body[r.position()..];
    {
        let mut ir = ByteReader::new(rest);
        if let Ok(item) = read_slot(&mut ir) {
            if ir.remaining() == 0 {
                return Ok(UseItemIn { pos, face, click_offset, player_pos, item, trailing_i32: None });
            }
        }
    }
    // The live 0.14.3 trace instead has a 4-byte int BETWEEN the player
    // position and the item slot (item bytes sit at the very end).
    if rest.len() > 4 {
        let mut ir = ByteReader::new(&rest[4..]);
        let item = read_slot(&mut ir)?;
        if ir.remaining() == 0 {
            let pre = i32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
            return Ok(UseItemIn { pos, face, click_offset, player_pos, item, trailing_i32: Some(pre) });
        }
    }
    Err(crate::binary::BinaryError::Invalid("unexpected bytes in 0.14 UseItem packet"))
}

/// Offset of the placed block for a clicked face.
pub fn face_offset(face: u8) -> Option<(i32, i32, i32)> {
    Some(match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        5 => (1, 0, 0),
        _ => return None,
    })
}

// ---- interaction ---------------------------------------------------------------

#[derive(Debug)]
pub struct InteractIn {
    /// 1 = right click, 2 = attack.
    pub action: u8,
    pub target: i64,
}

pub fn decode_interact(body: &[u8]) -> BinResult<InteractIn> {
    let mut r = ByteReader::new(body);
    Ok(InteractIn { action: r.u8()?, target: r.i64_be()? })
}

#[derive(Debug)]
pub struct AnimateIn {
    pub action: u8,
}

pub fn decode_animate(body: &[u8]) -> BinResult<AnimateIn> {
    Ok(AnimateIn { action: ByteReader::new(body).u8()? })
}

#[derive(Debug)]
pub struct PlayerActionIn {
    pub action: i32,
    pub pos: BlockPos,
    pub face: i32,
}

pub fn decode_player_action(body: &[u8]) -> BinResult<PlayerActionIn> {
    let mut r = ByteReader::new(body);
    // Legacy Pocket Edition PlayerAction begins with an 8-byte entity ID,
    // followed by fixed-width i32 action, x, y, z and face fields.
    // The entity ID is informational for this server and is not trusted.
    let _eid = r.i64_be()?;
    let action = r.i32_be()?;
    let pos = r.block_pos_xyz()?;
    let face = r.i32_be()?;
    Ok(PlayerActionIn { action, pos, face })
}

// ---- inventory -------------------------------------------------------------------

pub fn container_set_content(window: u8, items: &[Slot], hotbar: &[i32]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(8 + items.len() * 9 + hotbar.len() * 4);
    w.put_u8(ids::CONTAINER_SET_CONTENT);
    w.put_u8(window);
    w.put_i16_be(items.len() as i16);
    for s in items {
        write_slot(&mut w, s);
    }
    if window == WINDOW_INVENTORY {
        w.put_i16_be(hotbar.len() as i16);
        for h in hotbar {
            w.put_i32_be(*h);
        }
    }
    w.into_inner()
}

pub fn container_set_slot(window: u8, slot: i16, hotbar: i16, item: &Slot) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(16);
    w.put_u8(ids::CONTAINER_SET_SLOT);
    w.put_u8(window);
    w.put_i16_be(slot);
    w.put_i16_be(hotbar);
    write_slot(&mut w, item);
    w.into_inner()
}

// ---- chest GUI (shop) -----------------------------------------------------------------
//
// Layout cross-checked against PocketMine-era fake-chest menus (pmmp forum "inventory menu
// problem"): a chest block is faked with UpdateBlock, a BlockEntityData carries the title
// (CustomName), then ContainerOpen { windowid, type 0 = chest, slots, x, y, z } and a
// ContainerSetContent for that window id. Container ids (0xb5/0xb6/0xb7/0xb9) come from the
// contiguous 0.14 table in `ids.rs`; verify with `trace_packets` if the window never opens.

/// First custom window ID, matching PocketMine's legacy window counter (starts at 2).
pub const WINDOW_SHOP: u8 = 2;
pub const CHEST_SLOTS: usize = 27;
pub const CHEST_BLOCK: u8 = 54;

pub fn container_open(window: u8, kind: u8, slots: i16, x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(20);
    w.put_u8(ids::CONTAINER_OPEN);
    w.put_u8(window);
    w.put_u8(kind);
    w.put_i16_be(slots);
    w.put_i32_be(x);
    w.put_i32_be(y);
    w.put_i32_be(z);
    w.into_inner()
}

pub fn container_close(window: u8) -> Vec<u8> {
    vec![ids::CONTAINER_CLOSE, window]
}

fn nbt_name(w: &mut ByteWriter, tag: u8, name: &str) {
    w.put_u8(tag);
    w.put_i16_le(name.len() as i16);
    w.put_bytes(name.as_bytes());
}

/// BlockEntityData for an empty named chest (little-endian network NBT).
pub fn chest_block_entity(x: i32, y: i32, z: i32, title: &str) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(96 + title.len());
    w.put_u8(ids::BLOCK_ENTITY_DATA);
    w.put_i32_be(x);
    w.put_i32_be(y);
    w.put_i32_be(z);
    w.put_u8(10); // root compound, empty name
    w.put_i16_le(0);
    for (name, val) in [("id", "Chest"), ("CustomName", title)] {
        nbt_name(&mut w, 8, name);
        w.put_i16_le(val.len() as i16);
        w.put_bytes(val.as_bytes());
    }
    for (name, val) in [("x", x), ("y", y), ("z", z)] {
        nbt_name(&mut w, 3, name);
        w.put_i32_le(val);
    }
    nbt_name(&mut w, 9, "Items"); // empty list of compounds
    w.put_u8(10);
    w.put_i32_le(0);
    w.put_u8(0); // end of root
    w.into_inner()
}

/// Client ContainerClose: the window id.
pub fn decode_container_close(body: &[u8]) -> BinResult<u8> {
    ByteReader::new(body).u8()
}

/// Client ContainerSetSlot: (window, slot, item now in that slot).
pub fn decode_container_set_slot(body: &[u8]) -> BinResult<(u8, i16, Slot)> {
    let mut r = ByteReader::new(body);
    let window = r.u8()?;
    let slot = r.i16_be()?;
    let _hotbar = r.i16_be()?;
    let item = read_slot(&mut r)?;
    Ok((window, slot, item))
}

pub fn mob_equipment(eid: i64, item: &Slot, slot: u8, selected: u8) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(24);
    w.put_u8(ids::MOB_EQUIPMENT);
    w.put_i64_be(eid);
    write_slot(&mut w, item);
    w.put_u8(slot);
    w.put_u8(selected);
    w.into_inner()
}

pub fn mob_armor_equipment(eid: i64, armor: &[Slot; 4]) -> Vec<u8> {
    let mut w = ByteWriter::with_capacity(48);
    w.put_u8(ids::MOB_ARMOR_EQUIPMENT);
    w.put_i64_be(eid);
    for s in armor {
        write_slot(&mut w, s);
    }
    w.into_inner()
}

#[derive(Debug)]
pub struct MobEquipmentIn {
    pub item: Slot,
    pub slot: u8,
    pub selected: u8,
}

pub fn decode_mob_equipment(body: &[u8]) -> BinResult<MobEquipmentIn> {
    let mut r = ByteReader::new(body);
    let _eid = r.i64_be()?;
    let item = read_slot(&mut r)?;
    Ok(MobEquipmentIn { item, slot: r.u8()?, selected: r.u8()? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_roundtrip() {
        let pkt = text(text_type::CHAT, "Steve", "hello");
        let t = decode_text(&pkt[1..]).unwrap();
        assert_eq!((t.kind, t.source.as_str(), t.message.as_str()), (1, "Steve", "hello"));
        let raw = text(text_type::RAW, "ignored", "msg");
        assert_eq!(decode_text(&raw[1..]).unwrap().message, "msg");
    }

    #[test]
    fn use_item_protocol_70_field_order_and_item() {
        let mut w = ByteWriter::new();
        w.put_i32_be(3);
        w.put_i32_be(3);
        w.put_i32_be(-7);
        w.put_u8(1);
        for v in [0.25, 0.5, 0.75, 10.0, 64.0, -12.0] {
            w.put_f32_le(v);
        }
        // Protocol 70 UseItem has no selected-hotbar-slot field.
        write_slot(&mut w, &Slot::new(35, 12, 14));

        let u = decode_use_item(w.as_slice()).unwrap();
        assert_eq!((u.pos.x, u.pos.y, u.pos.z, u.face), (3, 3, -7, 1));
        assert_eq!(u.click_offset, Vec3 { x: 0.25, y: 0.5, z: 0.75 });
        assert_eq!(u.player_pos, Vec3 { x: 10.0, y: 64.0, z: -12.0 });
        assert_eq!(u.item, Slot::new(35, 12, 14));
        assert_eq!(face_offset(1), Some((0, 1, 0)));
        assert_eq!(face_offset(255), None);
    }

    #[test]
    fn player_action_legacy_uses_eight_byte_entity_id() {
        let mut w = ByteWriter::new();
        w.put_i64_be(-1); // client-local entity ID
        w.put_i32_be(action::START_BREAK);
        w.put_i32_be(8);
        w.put_i32_be(64);
        w.put_i32_be(-3);
        w.put_i32_be(1);
        assert_eq!(w.len(), 28);
        let decoded = decode_player_action(w.as_slice()).unwrap();
        assert_eq!(decoded.action, action::START_BREAK);
        assert_eq!((decoded.pos.x, decoded.pos.y, decoded.pos.z), (8, 64, -3));
        assert_eq!(decoded.face, 1);
    }

    #[test]
    fn player_action_matches_supplied_abort_break_trace() {
        let raw = [
            0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        let decoded = decode_player_action(&raw).unwrap();
        assert_eq!(decoded.action, action::ABORT_BREAK);
        assert_eq!(decoded.pos, BlockPos { x: 0, y: 0, z: 0 });
        assert_eq!(decoded.face, 0);
    }

    #[test]
    fn use_item_accepts_observed_four_byte_trailer() {
        let mut w = ByteWriter::new();
        w.put_i32_be(0);
        w.put_i32_be(0);
        w.put_i32_be(0);
        w.put_u8(0xff); // air-use sentinel from the supplied trace
        for _ in 0..6 { w.put_f32_be(0.0); }
        w.put_i32_be(0); // 0.14.3 field observed BEFORE the item in the log
        write_slot(&mut w, &Slot::new(12, 64, 0));
        let decoded = decode_use_item(w.as_slice()).unwrap();
        assert_eq!(decoded.face, 0xff);
        assert_eq!(decoded.item, Slot::new(12, 64, 0));
        assert_eq!(decoded.trailing_i32, Some(0));
    }

    #[test]
    fn use_item_real_0143_log_bytes() {
        let mut raw = vec![0u8; 41];
        raw[12] = 0xff;
        raw.extend_from_slice(&[0x00, 0x0c, 0x40, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(raw.len(), 48);
        let d = decode_use_item(&raw).unwrap();
        assert_eq!(d.face, 0xff);
        assert_eq!(d.item, Slot::new(12, 64, 0));
    }

    #[test]
    fn level_event_uses_varints_for_id_and_data() {
        let packet = level_event(1000, Vec3 { x: 1.0, y: 2.0, z: 3.0 }, 2001);
        assert_eq!(&packet[..3], &[ids::LEVEL_EVENT, 0xe8, 0x07]);
        assert_eq!(&packet[3..15], &[
            0x3f, 0x80, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
            0x40, 0x40, 0x00, 0x00,
        ]);
        assert_eq!(&packet[15..], &[0xd1, 0x0f]);
    }

    #[test]
    fn hologram_packet_uses_invisible_flag_and_always_visible_name_tag() {
        let uuid = [7u8; 16];
        let pkt = add_hologram(&uuid, "§aEMERALD §7| §eNext: 30s", 42, Vec3 { x: 1.5, y: 6.0, z: 2.5 });
        assert_eq!(pkt[0], ids::ADD_PLAYER);
        assert!(pkt.windows(2).any(|w| w == [0x00, 0x20])); // invisible flag
        assert!(pkt.windows(2).any(|w| w == [0x03, 0x01])); // name tag always visible
        let update = set_entity_name_tag(42, "§aEMERALD §7| §eNext: 29s");
        assert_eq!(update[0], ids::SET_ENTITY_DATA);
        assert_eq!(&update[1..9], &42i64.to_be_bytes());
    }

    #[test]
    fn update_block_layout() {
        let pkt = update_block(&[BlockRecord { x: 1, y: 70, z: -2, id: 35, meta: 14 }]);
        assert_eq!(pkt.len(), 1 + 4 + 11);
        assert_eq!(pkt[pkt.len() - 1], 0xbe);
    }
}

#[cfg(test)]
mod gui_tests {
    use super::*;

    #[test]
    fn open_and_nbt_layout() {
        let p = container_open(10, 0, 27, 5, 70, -3);
        assert_eq!(p[0], ids::CONTAINER_OPEN);
        assert_eq!(&p[1..5], &[10, 0, 0, 27]);
        assert_eq!(p.len(), 1 + 1 + 1 + 2 + 12);
        let e = chest_block_entity(1, 2, 3, "Shop");
        assert_eq!(e[0], ids::BLOCK_ENTITY_DATA);
        assert_eq!(e[13], 10); // compound
        assert!(e.windows(4).any(|w| w == b"Shop"));
        assert_eq!(*e.last().unwrap(), 0);
        assert_eq!(container_close(10), vec![ids::CONTAINER_CLOSE, 10]);
    }

    #[test]
    fn client_slot_decode() {
        let mut b = vec![10, 0, 3, 0, 0];
        b.extend_from_slice(&[0x01, 0x0c, 4, 0, 14, 0, 0]); // id 268 x4 meta 14, no nbt
        let (w, s, it) = decode_container_set_slot(&b).unwrap();
        assert_eq!((w, s), (10, 3));
        assert_eq!((it.id, it.count, it.damage), (268, 4, 14));
        assert_eq!(decode_container_close(&[10]).unwrap(), 10);
    }


    #[test]
    fn decode_use_item_matches_mcpe_0143_layout() {
        use crate::binary::ByteWriter;
        use crate::protocol::item::{write_slot, Slot};

        let mut w = ByteWriter::new();
        w.put_i32_be(10); // clicked x
        w.put_i32_be(64); // clicked y
        w.put_i32_be(-3); // clicked z
        w.put_u8(1);      // top face
        w.put_f32_le(0.25);
        w.put_f32_le(0.50);
        w.put_f32_le(0.75);
        w.put_f32_le(10.5);
        w.put_f32_le(65.0);
        w.put_f32_le(-2.5);
        write_slot(&mut w, &Slot::new(12, 64, 1));

        let decoded = decode_use_item(w.as_slice()).unwrap();
        assert_eq!((decoded.pos.x, decoded.pos.y, decoded.pos.z), (10, 64, -3));
        assert_eq!(decoded.face, 1);
        assert_eq!(decoded.click_offset.x, 0.25);
        assert_eq!(decoded.player_pos.y, 65.0);
        assert_eq!(decoded.item, Slot::new(12, 64, 1));
    }
}
