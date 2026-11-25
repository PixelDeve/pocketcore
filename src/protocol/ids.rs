//! MCPE 0.14.x game packet ids.
//!
//! Cross-checked against wiki.vg "Pocket Edition Login" (0.14.3): LOGIN,
//! PLAY_STATUS, SET_TIME, START_GAME, MOVE_PLAYER, SET_SPAWN_POSITION,
//! ADVENTURE_SETTINGS, RESPAWN, FULL_CHUNK_DATA, REQUEST_CHUNK_RADIUS,
//! CHUNK_RADIUS_UPDATE. The rest follow the same contiguous table
//! (PocketMine protocol 46 listing); verify with `trace_packets` if one
//! misbehaves.

pub const LOGIN: u8 = 0x8f;
pub const PLAY_STATUS: u8 = 0x90;
pub const DISCONNECT: u8 = 0x91;
pub const BATCH: u8 = 0x92;
pub const TEXT: u8 = 0x93;
pub const SET_TIME: u8 = 0x94;
pub const START_GAME: u8 = 0x95;
pub const ADD_PLAYER: u8 = 0x96;
pub const REMOVE_PLAYER: u8 = 0x97;
pub const ADD_ENTITY: u8 = 0x98;
pub const REMOVE_ENTITY: u8 = 0x99;
pub const ADD_ITEM_ENTITY: u8 = 0x9a;
pub const TAKE_ITEM_ENTITY: u8 = 0x9b;
pub const MOVE_ENTITY: u8 = 0x9c;
pub const MOVE_PLAYER: u8 = 0x9d;
pub const REMOVE_BLOCK: u8 = 0x9e;
pub const UPDATE_BLOCK: u8 = 0x9f;
pub const ADD_PAINTING: u8 = 0xa0;
pub const EXPLODE: u8 = 0xa1;
pub const LEVEL_EVENT: u8 = 0xa2;
pub const BLOCK_EVENT: u8 = 0xa3;
pub const ENTITY_EVENT: u8 = 0xa4;
pub const MOB_EFFECT: u8 = 0xa5;
pub const UPDATE_ATTRIBUTES: u8 = 0xa6;
pub const MOB_EQUIPMENT: u8 = 0xa7;
pub const MOB_ARMOR_EQUIPMENT: u8 = 0xa8;
pub const INTERACT: u8 = 0xa9;
pub const USE_ITEM: u8 = 0xaa;
pub const PLAYER_ACTION: u8 = 0xab;
pub const HURT_ARMOR: u8 = 0xac;
pub const SET_ENTITY_DATA: u8 = 0xad;
pub const SET_ENTITY_MOTION: u8 = 0xae;
pub const SET_ENTITY_LINK: u8 = 0xaf;
pub const SET_HEALTH: u8 = 0xb0;
pub const SET_SPAWN_POSITION: u8 = 0xb1;
pub const ANIMATE: u8 = 0xb2;
pub const RESPAWN: u8 = 0xb3;
pub const DROP_ITEM: u8 = 0xb4;
pub const CONTAINER_OPEN: u8 = 0xb5;
pub const CONTAINER_CLOSE: u8 = 0xb6;
pub const CONTAINER_SET_SLOT: u8 = 0xb7;
pub const CONTAINER_SET_DATA: u8 = 0xb8;
pub const CONTAINER_SET_CONTENT: u8 = 0xb9;
pub const CRAFTING_DATA: u8 = 0xba;
pub const CRAFTING_EVENT: u8 = 0xbb;
pub const ADVENTURE_SETTINGS: u8 = 0xbc;
pub const BLOCK_ENTITY_DATA: u8 = 0xbd;
pub const PLAYER_INPUT: u8 = 0xbe;
pub const FULL_CHUNK_DATA: u8 = 0xbf;
pub const SET_DIFFICULTY: u8 = 0xc0;
pub const CHANGE_DIMENSION: u8 = 0xc1;
pub const SET_PLAYER_GAMETYPE: u8 = 0xc2;
pub const PLAYER_LIST: u8 = 0xc3;
pub const REQUEST_CHUNK_RADIUS: u8 = 0xc8;
pub const CHUNK_RADIUS_UPDATE: u8 = 0xc9;
