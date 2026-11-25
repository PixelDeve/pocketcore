//! Item slot wire format (0.14): i16 id; if id > 0: u8 count; if count > 0:
//! i16 damage + i16be NBT length (+ NBT, skipped).

use crate::binary::{BinResult, ByteReader, ByteWriter};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    pub id: i16,
    pub count: u8,
    pub damage: i16,
}

impl Slot {
    pub const EMPTY: Slot = Slot { id: 0, count: 0, damage: 0 };

    pub fn new(id: i16, count: u8, damage: i16) -> Self {
        Self { id, count, damage }
    }

    pub fn is_empty(&self) -> bool {
        self.id <= 0 || self.count == 0
    }
}

pub fn read_slot(r: &mut ByteReader<'_>) -> BinResult<Slot> {
    let id = r.i16_be()?;
    if id <= 0 {
        return Ok(Slot::EMPTY);
    }
    let count = r.u8()?;
    if count == 0 {
        return Ok(Slot::EMPTY);
    }
    let damage = r.i16_be()?;
    let nbt_len = r.i16_be()?;
    if nbt_len > 0 {
        r.skip(nbt_len as usize)?;
    }
    Ok(Slot { id, count, damage })
}

pub fn write_slot(w: &mut ByteWriter, s: &Slot) {
    if s.is_empty() {
        w.put_i16_be(0);
        return;
    }
    w.put_i16_be(s.id);
    w.put_u8(s.count);
    w.put_i16_be(s.damage);
    w.put_i16_be(0); // no NBT (legacy PocketMine getShort is big-endian)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_slot_uses_big_endian_nbt_length() {
        let mut w = ByteWriter::new();
        write_slot(&mut w, &Slot::new(12, 64, 1));
        assert_eq!(w.as_slice(), &[0x00, 0x0c, 64, 0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn roundtrip() {
        for s in [Slot::EMPTY, Slot::new(268, 1, 0), Slot::new(35, 16, 14)] {
            let mut w = ByteWriter::new();
            write_slot(&mut w, &s);
            let mut r = ByteReader::new(w.as_slice());
            assert_eq!(read_slot(&mut r).unwrap(), s);
            assert!(r.is_empty());
        }
    }
}
