//! Zero-copy binary reader and reusable writer for RakNet and MCPE 0.14.x
//! wire types.
//!
//! Conventions in 0.14.x:
//! * Legacy game-packet integer fields: big-endian; RakNet u24 triads are little-endian.
//! * Legacy Pocket Edition interaction vectors use little-endian IEEE-754 floats.
//! * Strings in packets: u16 big-endian length + UTF-8.
//! * Entity metadata: little-endian values, u16le string lengths.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryError {
    Eof,
    InvalidUtf8,
    Invalid(&'static str),
}

impl fmt::Display for BinaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinaryError::Eof => f.write_str("unexpected end of buffer"),
            BinaryError::InvalidUtf8 => f.write_str("invalid utf-8"),
            BinaryError::Invalid(what) => write!(f, "invalid data: {what}"),
        }
    }
}

impl std::error::Error for BinaryError {}

pub type BinResult<T> = Result<T, BinaryError>;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Entity metadata value (0.14.x type ids 0..=7).
#[derive(Debug, Clone, PartialEq)]
pub enum MetaValue {
    Byte(i8),
    Short(i16),
    Int(i32),
    Float(f32),
    String(String),
    Slot { id: i16, count: u8, damage: i16 },
    Pos(i32, i32, i32),
    Long(i64),
}

impl MetaValue {
    fn type_id(&self) -> u8 {
        match self {
            MetaValue::Byte(_) => 0,
            MetaValue::Short(_) => 1,
            MetaValue::Int(_) => 2,
            MetaValue::Float(_) => 3,
            MetaValue::String(_) => 4,
            MetaValue::Slot { .. } => 5,
            MetaValue::Pos(..) => 6,
            MetaValue::Long(_) => 7,
        }
    }
}

/// `(key 0..=31, value)` pairs.
pub type Metadata = Vec<(u8, MetaValue)>;
pub const META_END: u8 = 0x7f;
const MAX_META_ENTRIES: usize = 64;

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

pub struct ByteReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

macro_rules! reader_ints {
    ($($name:ident => $ty:ty, $conv:ident;)*) => {
        $(
            #[inline]
            pub fn $name(&mut self) -> BinResult<$ty> {
                Ok(<$ty>::$conv(self.array()?))
            }
        )*
    };
}

impl<'a> ByteReader<'a> {
    #[inline]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Consumes and returns everything left.
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }

    pub fn skip(&mut self, n: usize) -> BinResult<()> {
        self.bytes(n).map(|_| ())
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> BinResult<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(BinaryError::Eof)?;
        let s = self.buf.get(self.pos..end).ok_or(BinaryError::Eof)?;
        self.pos = end;
        Ok(s)
    }

    #[inline]
    fn array<const N: usize>(&mut self) -> BinResult<[u8; N]> {
        let s = self.bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    #[inline]
    pub fn u8(&mut self) -> BinResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    #[inline]
    pub fn i8(&mut self) -> BinResult<i8> {
        Ok(self.u8()? as i8)
    }

    #[inline]
    pub fn bool(&mut self) -> BinResult<bool> {
        Ok(self.u8()? != 0)
    }

    reader_ints! {
        u16_be => u16, from_be_bytes;
        u16_le => u16, from_le_bytes;
        i16_be => i16, from_be_bytes;
        i16_le => i16, from_le_bytes;
        u32_be => u32, from_be_bytes;
        u32_le => u32, from_le_bytes;
        i32_be => i32, from_be_bytes;
        i32_le => i32, from_le_bytes;
        u64_be => u64, from_be_bytes;
        u64_le => u64, from_le_bytes;
        i64_be => i64, from_be_bytes;
        i64_le => i64, from_le_bytes;
        f32_be => f32, from_be_bytes;
        f32_le => f32, from_le_bytes;
        f64_be => f64, from_be_bytes;
        f64_le => f64, from_le_bytes;
    }

    /// RakNet "triad".
    #[inline]
    pub fn u24_le(&mut self) -> BinResult<u32> {
        let b = self.bytes(3)?;
        Ok(b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16)
    }

    #[inline]
    pub fn u24_be(&mut self) -> BinResult<u32> {
        let b = self.bytes(3)?;
        Ok((b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32)
    }

    /// u16 big-endian length-prefixed UTF-8 (packet strings), borrowed.
    pub fn str_be(&mut self) -> BinResult<&'a str> {
        let len = self.u16_be()? as usize;
        std::str::from_utf8(self.bytes(len)?).map_err(|_| BinaryError::InvalidUtf8)
    }

    pub fn string_be(&mut self) -> BinResult<String> {
        self.str_be().map(str::to_owned)
    }

    /// u16 little-endian length-prefixed UTF-8 (metadata strings), borrowed.
    pub fn str_le(&mut self) -> BinResult<&'a str> {
        let len = self.u16_le()? as usize;
        std::str::from_utf8(self.bytes(len)?).map_err(|_| BinaryError::InvalidUtf8)
    }

    pub fn string_le(&mut self) -> BinResult<String> {
        self.str_le().map(str::to_owned)
    }

    /// Three big-endian floats.
    pub fn vec3(&mut self) -> BinResult<Vec3> {
        Ok(Vec3 { x: self.f32_be()?, y: self.f32_be()?, z: self.f32_be()? })
    }

    /// Three little-endian floats (used by legacy Pocket Edition interaction packets).
    pub fn vec3_le(&mut self) -> BinResult<Vec3> {
        Ok(Vec3 { x: self.f32_le()?, y: self.f32_le()?, z: self.f32_le()? })
    }

    /// Block coordinates as three big-endian i32 (UseItem layout).
    pub fn block_pos_xyz(&mut self) -> BinResult<BlockPos> {
        Ok(BlockPos { x: self.i32_be()?, y: self.i32_be()?, z: self.i32_be()? })
    }

    /// Block coordinates as x:i32, z:i32, y:u8 (RemoveBlock/UpdateBlock layout).
    pub fn block_pos_xzy(&mut self) -> BinResult<BlockPos> {
        let x = self.i32_be()?;
        let z = self.i32_be()?;
        let y = self.u8()? as i32;
        Ok(BlockPos { x, y, z })
    }

    pub fn metadata(&mut self) -> BinResult<Metadata> {
        let mut out = Vec::new();
        loop {
            let hdr = self.u8()?;
            if hdr == META_END {
                return Ok(out);
            }
            if out.len() >= MAX_META_ENTRIES {
                return Err(BinaryError::Invalid("too many metadata entries"));
            }
            let key = hdr & 0x1f;
            let value = match hdr >> 5 {
                0 => MetaValue::Byte(self.i8()?),
                1 => MetaValue::Short(self.i16_le()?),
                2 => MetaValue::Int(self.i32_le()?),
                3 => MetaValue::Float(self.f32_le()?),
                4 => MetaValue::String(self.string_le()?),
                5 => MetaValue::Slot { id: self.i16_le()?, count: self.u8()?, damage: self.i16_le()? },
                6 => MetaValue::Pos(self.i32_le()?, self.i32_le()?, self.i32_le()?),
                7 => MetaValue::Long(self.i64_le()?),
                _ => return Err(BinaryError::Invalid("metadata type")),
            };
            out.push((key, value));
        }
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Growable output buffer. Keep one around and `clear()` it between packets
/// to avoid per-packet allocations.
#[derive(Default, Debug, Clone)]
pub struct ByteWriter {
    buf: Vec<u8>,
}

fn clip_utf8(s: &str) -> &[u8] {
    let mut n = s.len().min(u16::MAX as usize);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s.as_bytes()[..n]
}

macro_rules! writer_ints {
    ($($name:ident => $ty:ty, $conv:ident;)*) => {
        $(
            #[inline]
            pub fn $name(&mut self, v: $ty) {
                self.buf.extend_from_slice(&v.$conv());
            }
        )*
    };
}

impl ByteWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n) }
    }

    pub fn from_vec(buf: Vec<u8>) -> Self {
        Self { buf }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    #[inline]
    pub fn put_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    #[inline]
    pub fn put_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    #[inline]
    pub fn put_bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }

    #[inline]
    pub fn put_bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    writer_ints! {
        put_u16_be => u16, to_be_bytes;
        put_u16_le => u16, to_le_bytes;
        put_i16_be => i16, to_be_bytes;
        put_i16_le => i16, to_le_bytes;
        put_u32_be => u32, to_be_bytes;
        put_u32_le => u32, to_le_bytes;
        put_i32_be => i32, to_be_bytes;
        put_i32_le => i32, to_le_bytes;
        put_u64_be => u64, to_be_bytes;
        put_u64_le => u64, to_le_bytes;
        put_i64_be => i64, to_be_bytes;
        put_i64_le => i64, to_le_bytes;
        put_f32_be => f32, to_be_bytes;
        put_f32_le => f32, to_le_bytes;
        put_f64_be => f64, to_be_bytes;
        put_f64_le => f64, to_le_bytes;
    }

    #[inline]
    pub fn put_u24_le(&mut self, v: u32) {
        self.buf.extend_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]);
    }

    #[inline]
    pub fn put_u24_be(&mut self, v: u32) {
        self.buf.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
    }

    /// Overwrites 3 bytes at `at` (e.g. a datagram sequence number).
    #[inline]
    pub fn patch_u24_le(&mut self, at: usize, v: u32) {
        self.buf[at..at + 3].copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]);
    }

    #[inline]
    pub fn patch_u16_be(&mut self, at: usize, v: u16) {
        self.buf[at..at + 2].copy_from_slice(&v.to_be_bytes());
    }

    /// u16be length + UTF-8. Longer strings are truncated at a character
    /// boundary so the output stays valid UTF-8.
    pub fn put_string_be(&mut self, s: &str) {
        let b = clip_utf8(s);
        self.put_u16_be(b.len() as u16);
        self.put_bytes(b);
    }

    pub fn put_string_le(&mut self, s: &str) {
        let b = clip_utf8(s);
        self.put_u16_le(b.len() as u16);
        self.put_bytes(b);
    }

    pub fn put_vec3(&mut self, v: Vec3) {
        self.put_f32_be(v.x);
        self.put_f32_be(v.y);
        self.put_f32_be(v.z);
    }

    pub fn put_block_pos_xyz(&mut self, p: BlockPos) {
        self.put_i32_be(p.x);
        self.put_i32_be(p.y);
        self.put_i32_be(p.z);
    }

    pub fn put_block_pos_xzy(&mut self, p: BlockPos) {
        self.put_i32_be(p.x);
        self.put_i32_be(p.z);
        self.put_u8(p.y as u8);
    }

    pub fn put_metadata(&mut self, meta: &[(u8, MetaValue)]) {
        for (key, value) in meta {
            self.put_u8(value.type_id() << 5 | (key & 0x1f));
            match value {
                MetaValue::Byte(v) => self.put_i8(*v),
                MetaValue::Short(v) => self.put_i16_le(*v),
                MetaValue::Int(v) => self.put_i32_le(*v),
                MetaValue::Float(v) => self.put_f32_le(*v),
                MetaValue::String(s) => self.put_string_le(s),
                MetaValue::Slot { id, count, damage } => {
                    self.put_i16_le(*id);
                    self.put_u8(*count);
                    self.put_i16_le(*damage);
                }
                MetaValue::Pos(x, y, z) => {
                    self.put_i32_le(*x);
                    self.put_i32_le(*y);
                    self.put_i32_le(*z);
                }
                MetaValue::Long(v) => self.put_i64_le(*v),
            }
        }
        self.put_u8(META_END);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_roundtrip() {
        let mut w = ByteWriter::new();
        w.put_u16_be(0x1234);
        w.put_u24_le(0xabcdef);
        w.put_i32_le(-5);
        w.put_i64_be(-1234567890123);
        w.put_f32_be(1.5);
        let mut r = ByteReader::new(w.as_slice());
        assert_eq!(r.u16_be().unwrap(), 0x1234);
        assert_eq!(r.u24_le().unwrap(), 0xabcdef);
        assert_eq!(r.i32_le().unwrap(), -5);
        assert_eq!(r.i64_be().unwrap(), -1234567890123);
        assert_eq!(r.f32_be().unwrap(), 1.5);
        assert!(r.is_empty());
        assert_eq!(r.u8(), Err(BinaryError::Eof));
    }

    #[test]
    fn strings_and_positions() {
        let mut w = ByteWriter::new();
        w.put_string_be("héllo");
        w.put_block_pos_xzy(BlockPos { x: -3, y: 64, z: 9 });
        let mut r = ByteReader::new(w.as_slice());
        assert_eq!(r.str_be().unwrap(), "héllo");
        assert_eq!(r.block_pos_xzy().unwrap(), BlockPos { x: -3, y: 64, z: 9 });
    }

    #[test]
    fn metadata_roundtrip() {
        let meta = vec![
            (0, MetaValue::Byte(1)),
            (1, MetaValue::Short(300)),
            (2, MetaValue::String("Steve".into())),
            (3, MetaValue::Slot { id: 267, count: 1, damage: 0 }),
            (4, MetaValue::Pos(1, 2, 3)),
            (5, MetaValue::Long(1 << 40)),
            (6, MetaValue::Float(0.5)),
            (7, MetaValue::Int(-9)),
        ];
        let mut w = ByteWriter::new();
        w.put_metadata(&meta);
        let mut r = ByteReader::new(w.as_slice());
        assert_eq!(r.metadata().unwrap(), meta);
        assert!(r.is_empty());
    }

    #[test]
    fn long_string_truncates_on_char_boundary() {
        let s = "é".repeat(40000); // 80000 bytes
        let mut w = ByteWriter::new();
        w.put_string_be(&s);
        let mut r = ByteReader::new(w.as_slice());
        let back = r.str_be().unwrap(); // would fail if cut mid-character
        assert_eq!(back.len(), 65534);
    }

    #[test]
    fn truncated_is_error() {
        let mut r = ByteReader::new(&[0x00, 0x05, b'a']);
        assert_eq!(r.str_be(), Err(BinaryError::Eof));
    }
}
