//! Encapsulated frame (reliability layer) encoding and decoding.
//!
//! Layout:
//! ```text
//! u8   flags            reliability << 5 | split << 4
//! u16  length in bits   (big-endian)
//! u24  message index    if reliable            (2,3,4,6,7)
//! u24  sequence index   if sequenced           (1,4)
//! u24  order index      if ordered/sequenced   (1,3,4,7)
//! u8   order channel    "
//! i32  split count      if split (big-endian)
//! u16  split id         "
//! i32  split index      "
//! ...  payload
//! ```

use crate::binary::{BinResult, BinaryError, ByteReader, ByteWriter};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reliability {
    Unreliable = 0,
    UnreliableSequenced = 1,
    Reliable = 2,
    ReliableOrdered = 3,
    ReliableSequenced = 4,
    UnreliableAckReceipt = 5,
    ReliableAckReceipt = 6,
    ReliableOrderedAckReceipt = 7,
}

impl Reliability {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Unreliable,
            1 => Self::UnreliableSequenced,
            2 => Self::Reliable,
            3 => Self::ReliableOrdered,
            4 => Self::ReliableSequenced,
            5 => Self::UnreliableAckReceipt,
            6 => Self::ReliableAckReceipt,
            7 => Self::ReliableOrderedAckReceipt,
            _ => return None,
        })
    }

    #[inline]
    pub fn is_reliable(self) -> bool {
        matches!(
            self,
            Self::Reliable
                | Self::ReliableOrdered
                | Self::ReliableSequenced
                | Self::ReliableAckReceipt
                | Self::ReliableOrderedAckReceipt
        )
    }

    #[inline]
    pub fn is_sequenced(self) -> bool {
        matches!(self, Self::UnreliableSequenced | Self::ReliableSequenced)
    }

    /// True if the frame carries an order index + channel.
    #[inline]
    pub fn has_order(self) -> bool {
        matches!(
            self,
            Self::UnreliableSequenced
                | Self::ReliableOrdered
                | Self::ReliableSequenced
                | Self::ReliableOrderedAckReceipt
        )
    }

    /// RakNet sends split packets reliably; unreliable types are upgraded.
    pub fn for_split(self) -> Self {
        match self {
            Self::Unreliable => Self::Reliable,
            Self::UnreliableSequenced => Self::ReliableSequenced,
            Self::UnreliableAckReceipt => Self::ReliableAckReceipt,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SplitInfo {
    pub count: u32,
    pub id: u16,
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub reliability: Reliability,
    pub message_index: u32,
    pub sequence_index: u32,
    pub order_index: u32,
    pub order_channel: u8,
    pub split: Option<SplitInfo>,
}

/// Header size in bytes for a frame of the given shape.
pub fn frame_header_len(rel: Reliability, split: bool) -> usize {
    let mut n = 3;
    if rel.is_reliable() {
        n += 3;
    }
    if rel.is_sequenced() {
        n += 3;
    }
    if rel.has_order() {
        n += 4;
    }
    if split {
        n += 10;
    }
    n
}

impl FrameHeader {
    #[inline]
    pub fn encoded_len(&self) -> usize {
        frame_header_len(self.reliability, self.split.is_some())
    }

    pub fn encode(&self, w: &mut ByteWriter, payload: &[u8]) {
        let rel = self.reliability;
        w.put_u8((rel as u8) << 5 | if self.split.is_some() { 0x10 } else { 0 });
        w.put_u16_be((payload.len() * 8) as u16);
        if rel.is_reliable() {
            w.put_u24_le(self.message_index);
        }
        if rel.is_sequenced() {
            w.put_u24_le(self.sequence_index);
        }
        if rel.has_order() {
            w.put_u24_le(self.order_index);
            w.put_u8(self.order_channel);
        }
        if let Some(s) = self.split {
            w.put_i32_be(s.count as i32);
            w.put_u16_be(s.id);
            w.put_i32_be(s.index as i32);
        }
        w.put_bytes(payload);
    }
}

/// A decoded frame borrowing its payload from the datagram buffer.
#[derive(Debug)]
pub struct Frame<'a> {
    pub header: FrameHeader,
    pub payload: &'a [u8],
}

impl<'a> Frame<'a> {
    pub fn decode(r: &mut ByteReader<'a>) -> BinResult<Self> {
        let flags = r.u8()?;
        let reliability =
            Reliability::from_u8(flags >> 5).ok_or(BinaryError::Invalid("reliability"))?;
        let has_split = flags & 0x10 != 0;
        let len = (r.u16_be()? as usize + 7) >> 3;
        if len == 0 {
            return Err(BinaryError::Invalid("empty frame"));
        }
        let mut header = FrameHeader {
            reliability,
            message_index: 0,
            sequence_index: 0,
            order_index: 0,
            order_channel: 0,
            split: None,
        };
        if reliability.is_reliable() {
            header.message_index = r.u24_le()?;
        }
        if reliability.is_sequenced() {
            header.sequence_index = r.u24_le()?;
        }
        if reliability.has_order() {
            header.order_index = r.u24_le()?;
            header.order_channel = r.u8()?;
        }
        if has_split {
            let count = r.i32_be()?;
            let id = r.u16_be()?;
            let index = r.i32_be()?;
            if count <= 0 || index < 0 || index >= count {
                return Err(BinaryError::Invalid("split info"));
            }
            header.split = Some(SplitInfo { count: count as u32, id, index: index as u32 });
        }
        let payload = r.bytes(len)?;
        Ok(Frame { header, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_shapes() {
        for rel in 0..8u8 {
            let rel = Reliability::from_u8(rel).unwrap();
            for split in [None, Some(SplitInfo { count: 3, id: 9, index: 2 })] {
                let h = FrameHeader {
                    reliability: rel,
                    message_index: if rel.is_reliable() { 0x123456 } else { 0 },
                    sequence_index: if rel.is_sequenced() { 77 } else { 0 },
                    order_index: if rel.has_order() { 5 } else { 0 },
                    order_channel: if rel.has_order() { 3 } else { 0 },
                    split,
                };
                let payload = [1u8, 2, 3, 4, 5];
                let mut w = ByteWriter::new();
                h.encode(&mut w, &payload);
                assert_eq!(w.len(), h.encoded_len() + payload.len());
                let mut r = ByteReader::new(w.as_slice());
                let f = Frame::decode(&mut r).unwrap();
                assert_eq!(f.header, h);
                assert_eq!(f.payload, &payload);
                assert!(r.is_empty());
            }
        }
    }
}
