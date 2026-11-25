//! RakNet wire constants.

pub const MAGIC: [u8; 16] = [
    0x00, 0xff, 0xff, 0x00, 0xfe, 0xfe, 0xfe, 0xfe, 0xfd, 0xfd, 0xfd, 0xfd, 0x12, 0x34, 0x56, 0x78,
];

pub const RAKNET_PROTOCOL: u8 = 7;

// Connected (in-frame) packet ids.
pub const ID_CONNECTED_PING: u8 = 0x00;
pub const ID_CONNECTED_PONG: u8 = 0x03;
pub const ID_CONNECTION_REQUEST: u8 = 0x09;
pub const ID_CONNECTION_REQUEST_ACCEPTED: u8 = 0x10;
pub const ID_NEW_INCOMING_CONNECTION: u8 = 0x13;
pub const ID_DISCONNECT: u8 = 0x15;

// Offline packet ids.
pub const ID_UNCONNECTED_PING: u8 = 0x01;
pub const ID_UNCONNECTED_PING_OPEN: u8 = 0x02;
pub const ID_OPEN_REQUEST_1: u8 = 0x05;
pub const ID_OPEN_REPLY_1: u8 = 0x06;
pub const ID_OPEN_REQUEST_2: u8 = 0x07;
pub const ID_OPEN_REPLY_2: u8 = 0x08;
pub const ID_NO_FREE_CONNECTIONS: u8 = 0x14;
pub const ID_INCOMPATIBLE_PROTOCOL: u8 = 0x19;
pub const ID_UNCONNECTED_PONG: u8 = 0x1c;

// Datagram-level ids.
pub const ID_FRAME_SET_MIN: u8 = 0x80;
pub const ID_FRAME_SET_MAX: u8 = 0x8f;
/// Id we use for every frame set we send (what RakLib/PocketMine use).
pub const ID_FRAME_SET_SEND: u8 = 0x84;
pub const ID_NAK: u8 = 0xa0;
pub const ID_ACK: u8 = 0xc0;

/// IPv4 (20) + UDP (8) header bytes counted in RakNet's MTU figure.
pub const UDP_HEADER_SIZE: usize = 28;
/// Frame-set header: id (1) + sequence number u24le (3).
pub const DATAGRAM_HEADER_SIZE: usize = 4;
pub const MIN_MTU: usize = 400;
pub const MAX_MTU: usize = 1492;

/// Sequence numbers / message indices are 24-bit and wrap.
pub const MASK24: u32 = 0x00FF_FFFF;
pub const HALF24: u32 = 0x0080_0000;
