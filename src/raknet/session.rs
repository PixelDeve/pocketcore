//! Per-client RakNet session: reliability layer + connected handshake.
//!
//! The session is I/O free: it consumes datagrams, emits events through a
//! callback and writes outgoing datagrams through a sink closure. This keeps
//! it unit-testable and lets the server loop own the socket.
//!
//! Receive side: ACK/NAK generation, reliable de-duplication, ordering per
//! channel, sequenced drop-stale, split reassembly.
//! Send side: frame queues, MTU splitting, datagram packing, resend on
//! NAK / timeout with adaptive RTO.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use super::consts::*;
use super::frame::{frame_header_len, Frame, FrameHeader, Reliability, SplitInfo};
use super::packets;
use crate::binary::{ByteReader, ByteWriter};

const ORDER_CHANNELS: usize = 32;
const RELIABLE_WINDOW: u32 = 4096;
const ORDER_WINDOW: u32 = 1024;
const MAX_HELD_PER_CHANNEL: usize = 512;
const MAX_SPLIT_COUNT: u32 = 512;
const MAX_SPLIT_ASSEMBLIES: usize = 32;
const MAX_ACK_QUEUE: usize = 4096;
const MAX_NAK_GAP: u32 = 256;
const MAX_UNACKED: usize = 4096;
const MAX_QUEUED_FRAMES: usize = 8192;
const MIN_RTO: Duration = Duration::from_millis(100);
const MAX_RTO: Duration = Duration::from_secs(2);
const INITIAL_RTO: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    Client,
    Server,
    Timeout,
    Congested,
}

pub enum SessionEvent<'a> {
    /// RakNet handshake complete.
    Connected,
    /// Complete game packet (payload[0] >= 0x80).
    Packet(&'a [u8]),
    /// Client sent DisconnectNotification.
    Disconnected(DisconnectReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Offline handshake done, waiting for ConnectionRequest (0x09).
    Handshake,
    /// ConnectionRequestAccepted sent, waiting for NewIncomingConnection.
    Accepted,
    Connected,
    Closed,
}

/// 24-bit modular difference `a - b`.
#[inline]
fn seq_diff(a: u32, b: u32) -> u32 {
    a.wrapping_sub(b) & MASK24
}

/// True if `a` is strictly newer than `b` in 24-bit modular order.
#[inline]
fn seq_newer(a: u32, b: u32) -> bool {
    let d = seq_diff(a, b);
    d != 0 && d < HALF24
}

#[derive(Default)]
struct ReliableWindow {
    next: u32,
    seen: HashSet<u32>,
}

impl ReliableWindow {
    /// Returns true if `idx` is new (should be processed).
    fn accept(&mut self, idx: u32) -> bool {
        let d = seq_diff(idx, self.next);
        if d >= HALF24 || d >= RELIABLE_WINDOW {
            return false; // duplicate / too old / absurdly far ahead
        }
        if d == 0 {
            self.next = (self.next + 1) & MASK24;
            while self.seen.remove(&self.next) {
                self.next = (self.next + 1) & MASK24;
            }
            true
        } else {
            self.seen.insert(idx)
        }
    }
}

#[derive(Default)]
struct OrderChannel {
    expected: u32,
    highest_seq: Option<u32>,
    held: HashMap<u32, Vec<u8>>,
}

struct SplitAssembly {
    count: usize,
    received: usize,
    parts: Vec<Option<Vec<u8>>>,
}

struct OutFrame {
    header: FrameHeader,
    payload: Vec<u8>,
}

impl OutFrame {
    #[inline]
    fn encoded_len(&self) -> usize {
        self.header.encoded_len() + self.payload.len()
    }
}

struct SentDatagram {
    /// Encoded reliable frames only (everything after the 4-byte header).
    blob: Vec<u8>,
    sent_at: Instant,
    resent: bool,
}

pub struct Session {
    pub addr: SocketAddr,
    pub guid: i64,
    mtu: usize,
    state: State,
    announced: bool,
    epoch: Instant,
    last_recv: Instant,
    timeout: Duration,

    // receive side
    highest_seq: Option<u32>,
    ack_queue: Vec<u32>,
    nak_queue: Vec<u32>,
    reliable: ReliableWindow,
    channels: Vec<OrderChannel>,
    splits: HashMap<u16, SplitAssembly>,

    // send side
    next_seq: u32,
    next_message_index: u32,
    order_out: [u32; ORDER_CHANNELS],
    seq_out: [u32; ORDER_CHANNELS],
    next_split_id: u16,
    reliable_q: VecDeque<OutFrame>,
    unreliable_q: VecDeque<OutFrame>,
    resend_q: VecDeque<Vec<u8>>,
    sent: HashMap<u32, SentDatagram>,
    expired: Vec<u32>,
    srtt: Duration,
    rto: Duration,
}

impl Session {
    pub fn new(addr: SocketAddr, guid: i64, mtu: usize, now: Instant, timeout: Duration) -> Self {
        Self {
            addr,
            guid,
            mtu: mtu.clamp(MIN_MTU, MAX_MTU),
            state: State::Handshake,
            announced: false,
            epoch: now,
            last_recv: now,
            timeout,
            highest_seq: None,
            ack_queue: Vec::new(),
            nak_queue: Vec::new(),
            reliable: ReliableWindow::default(),
            channels: (0..ORDER_CHANNELS).map(|_| OrderChannel::default()).collect(),
            splits: HashMap::new(),
            next_seq: 0,
            next_message_index: 0,
            order_out: [0; ORDER_CHANNELS],
            seq_out: [0; ORDER_CHANNELS],
            next_split_id: 0,
            reliable_q: VecDeque::new(),
            unreliable_q: VecDeque::new(),
            resend_q: VecDeque::new(),
            sent: HashMap::new(),
            expired: Vec::new(),
            srtt: Duration::ZERO,
            rto: INITIAL_RTO,
        }
    }

    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// True once the handshake completed (handler.on_connect was fired).
    pub fn announced(&self) -> bool {
        self.announced
    }

    /// Offline handshake finished but no ConnectionRequest seen yet.
    pub fn is_handshaking(&self) -> bool {
        self.state == State::Handshake
    }

    pub fn rtt(&self) -> Duration {
        self.srtt
    }

    fn time_ms(&self) -> i64 {
        self.epoch.elapsed().as_millis() as i64
    }

    // =======================================================================
    // Receive path
    // =======================================================================

    pub fn on_datagram(&mut self, data: &[u8], now: Instant, ev: &mut dyn FnMut(SessionEvent<'_>)) {
        let Some(&id) = data.first() else { return };
        if self.state == State::Closed {
            return;
        }
        self.last_recv = now;
        match id {
            ID_ACK => self.on_ack(data, now),
            ID_NAK => self.on_nak(data),
            ID_FRAME_SET_MIN..=ID_FRAME_SET_MAX => self.on_frame_set(data, ev),
            _ => {}
        }
    }

    fn on_frame_set(&mut self, data: &[u8], ev: &mut dyn FnMut(SessionEvent<'_>)) {
        let mut r = ByteReader::new(&data[1..]);
        let Ok(seq) = r.u24_le() else { return };
        self.note_datagram_seq(seq);
        while r.remaining() > 0 {
            let frame = match Frame::decode(&mut r) {
                Ok(f) => f,
                Err(_) => break, // malformed tail: drop the rest of the datagram
            };
            self.on_frame(frame, ev);
            if self.state == State::Closed {
                break;
            }
        }
    }

    fn note_datagram_seq(&mut self, seq: u32) {
        if self.ack_queue.len() < MAX_ACK_QUEUE {
            self.ack_queue.push(seq);
        }
        match self.highest_seq {
            None => {
                // Fresh session: sequence numbers start at 0, so anything
                // below the first seen number is missing.
                for k in 0..seq.min(MAX_NAK_GAP) {
                    if self.nak_queue.len() < MAX_ACK_QUEUE {
                        self.nak_queue.push(k);
                    }
                }
                self.highest_seq = Some(seq);
            }
            Some(h) => {
                let d = seq_diff(seq, h);
                if d != 0 && d < HALF24 {
                    // newer: anything skipped is presumed lost
                    for k in 1..d.min(MAX_NAK_GAP + 1) {
                        if self.nak_queue.len() < MAX_ACK_QUEUE {
                            self.nak_queue.push((h + k) & MASK24);
                        }
                    }
                    self.highest_seq = Some(seq);
                }
            }
        }
    }

    fn on_frame(&mut self, f: Frame<'_>, ev: &mut dyn FnMut(SessionEvent<'_>)) {
        let h = f.header;
        if h.reliability.is_reliable() && !self.reliable.accept(h.message_index) {
            return;
        }
        match h.split {
            Some(split) => {
                if let Some(full) = self.add_split(split, f.payload) {
                    let mut whole = h;
                    whole.split = None;
                    self.dispatch(&whole, &full, ev);
                }
            }
            None => self.dispatch(&h, f.payload, ev),
        }
    }

    fn add_split(&mut self, s: SplitInfo, payload: &[u8]) -> Option<Vec<u8>> {
        if s.count > MAX_SPLIT_COUNT || s.index >= s.count {
            return None;
        }
        if !self.splits.contains_key(&s.id) {
            if self.splits.len() >= MAX_SPLIT_ASSEMBLIES {
                return None;
            }
            self.splits.insert(
                s.id,
                SplitAssembly { count: s.count as usize, received: 0, parts: vec![None; s.count as usize] },
            );
        }
        let asm = self.splits.get_mut(&s.id)?;
        if asm.count != s.count as usize {
            return None;
        }
        let slot = &mut asm.parts[s.index as usize];
        if slot.is_some() {
            return None;
        }
        *slot = Some(payload.to_vec());
        asm.received += 1;
        if asm.received < asm.count {
            return None;
        }
        let asm = self.splits.remove(&s.id)?;
        let total: usize = asm.parts.iter().map(|p| p.as_ref().map_or(0, Vec::len)).sum();
        let mut out = Vec::with_capacity(total);
        for part in asm.parts.into_iter().flatten() {
            out.extend_from_slice(&part);
        }
        Some(out)
    }

    /// Applies sequencing / ordering, then hands the payload to
    /// `handle_payload`.
    fn dispatch(&mut self, h: &FrameHeader, payload: &[u8], ev: &mut dyn FnMut(SessionEvent<'_>)) {
        if payload.is_empty() {
            return;
        }
        let rel = h.reliability;
        if rel.has_order() {
            let ci = h.order_channel as usize;
            if ci >= ORDER_CHANNELS {
                return;
            }
            if rel.is_sequenced() {
                let ch = &mut self.channels[ci];
                if seq_diff(h.order_index, ch.expected) >= HALF24 {
                    return; // belongs to an older ordering epoch
                }
                let fresh = match ch.highest_seq {
                    None => true,
                    Some(hs) => seq_newer(h.sequence_index, hs),
                };
                if !fresh {
                    return;
                }
                ch.highest_seq = Some(h.sequence_index);
                self.handle_payload(payload, ev);
            } else {
                let d = seq_diff(h.order_index, self.channels[ci].expected);
                if d == 0 {
                    {
                        let ch = &mut self.channels[ci];
                        ch.expected = (ch.expected + 1) & MASK24;
                        ch.highest_seq = None; // sender resets its sequence counter
                    }
                    self.handle_payload(payload, ev);
                    loop {
                        let next = {
                            let ch = &mut self.channels[ci];
                            match ch.held.remove(&ch.expected) {
                                Some(p) => {
                                    ch.expected = (ch.expected + 1) & MASK24;
                                    ch.highest_seq = None;
                                    p
                                }
                                None => break,
                            }
                        };
                        self.handle_payload(&next, ev);
                        if self.state == State::Closed {
                            break;
                        }
                    }
                } else if d < ORDER_WINDOW {
                    let ch = &mut self.channels[ci];
                    if ch.held.len() < MAX_HELD_PER_CHANNEL {
                        ch.held.insert(h.order_index, payload.to_vec());
                    }
                }
                // else: old duplicate or too far ahead -> drop
            }
        } else {
            self.handle_payload(payload, ev);
        }
    }

    fn handle_payload(&mut self, payload: &[u8], ev: &mut dyn FnMut(SessionEvent<'_>)) {
        match payload[0] {
            ID_CONNECTED_PING => {
                if let Ok(t) = ByteReader::new(&payload[1..]).i64_be() {
                    self.queue_pong(t);
                }
            }
            ID_CONNECTED_PONG => {}
            ID_CONNECTION_REQUEST => self.on_connection_request(payload),
            ID_NEW_INCOMING_CONNECTION => {
                if self.state == State::Accepted {
                    self.promote(ev);
                }
            }
            ID_DISCONNECT => {
                if self.state != State::Closed {
                    self.state = State::Closed;
                    ev(SessionEvent::Disconnected(DisconnectReason::Client));
                }
            }
            id if id >= 0x80 => {
                // Lenient: some clients send their first game packet right
                // behind 0x13; promote if we already accepted them.
                if self.state == State::Accepted {
                    self.promote(ev);
                }
                if self.state == State::Connected {
                    ev(SessionEvent::Packet(payload));
                }
            }
            _ => {}
        }
    }

    fn promote(&mut self, ev: &mut dyn FnMut(SessionEvent<'_>)) {
        self.state = State::Connected;
        if !self.announced {
            self.announced = true;
            ev(SessionEvent::Connected);
        }
    }

    fn on_connection_request(&mut self, payload: &[u8]) {
        if !matches!(self.state, State::Handshake | State::Accepted) {
            return;
        }
        let mut r = ByteReader::new(&payload[1..]);
        let (Ok(_guid), Ok(time)) = (r.i64_be(), r.i64_be()) else { return };
        self.state = State::Accepted;

        let mut w = ByteWriter::with_capacity(96);
        w.put_u8(ID_CONNECTION_REQUEST_ACCEPTED);
        packets::put_address(&mut w, self.addr);
        w.put_u16_be(0); // system index
        let internal = SocketAddr::from(([127, 0, 0, 1], 0));
        for _ in 0..10 {
            packets::put_address(&mut w, internal);
        }
        w.put_i64_be(time);
        w.put_i64_be(self.time_ms());
        self.send(w.into_inner(), Reliability::Reliable, 0);
    }

    fn queue_pong(&mut self, ping_time: i64) {
        let mut w = ByteWriter::with_capacity(17);
        w.put_u8(ID_CONNECTED_PONG);
        w.put_i64_be(ping_time);
        w.put_i64_be(self.time_ms());
        self.send(w.into_inner(), Reliability::Unreliable, 0);
    }

    // --- ACK / NAK -------------------------------------------------------

    fn on_ack(&mut self, data: &[u8], now: Instant) {
        let sent = &mut self.sent;
        let srtt = &mut self.srtt;
        let rto = &mut self.rto;
        for_each_record(data, |seq| {
            if let Some(sd) = sent.remove(&seq) {
                if !sd.resent {
                    let sample = now.saturating_duration_since(sd.sent_at);
                    *srtt = if srtt.is_zero() { sample } else { (*srtt * 7 + sample) / 8 };
                    *rto = (*srtt * 2).clamp(MIN_RTO, MAX_RTO);
                }
            }
        });
    }

    fn on_nak(&mut self, data: &[u8]) {
        let sent = &mut self.sent;
        let resend_q = &mut self.resend_q;
        for_each_record(data, |seq| {
            if let Some(sd) = sent.remove(&seq) {
                resend_q.push_back(sd.blob);
            }
        });
    }

    // =======================================================================
    // Send path
    // =======================================================================

    fn take_message_index(&mut self) -> u32 {
        let i = self.next_message_index;
        self.next_message_index = (i + 1) & MASK24;
        i
    }

    fn new_header(&mut self, rel: Reliability, ch: usize) -> FrameHeader {
        let mut h = FrameHeader {
            reliability: rel,
            message_index: 0,
            sequence_index: 0,
            order_index: 0,
            order_channel: ch as u8,
            split: None,
        };
        if rel.is_reliable() {
            h.message_index = self.take_message_index();
        }
        if rel.is_sequenced() {
            h.order_index = self.order_out[ch];
            h.sequence_index = self.seq_out[ch];
            self.seq_out[ch] = (self.seq_out[ch] + 1) & MASK24;
        } else if rel.has_order() {
            h.order_index = self.order_out[ch];
            self.order_out[ch] = (self.order_out[ch] + 1) & MASK24;
            self.seq_out[ch] = 0;
        }
        h
    }

    fn enqueue(&mut self, f: OutFrame) {
        if f.header.reliability.is_reliable() {
            self.reliable_q.push_back(f);
        } else {
            self.unreliable_q.push_back(f);
        }
    }

    /// Queues a payload for sending; splits it if it exceeds the MTU.
    pub fn send(&mut self, payload: Vec<u8>, rel: Reliability, channel: u8) {
        if payload.is_empty() {
            return;
        }
        let ch = channel as usize % ORDER_CHANNELS;
        let max_body = self.mtu - UDP_HEADER_SIZE - DATAGRAM_HEADER_SIZE;
        if payload.len() + frame_header_len(rel, false) <= max_body {
            let header = self.new_header(rel, ch);
            self.enqueue(OutFrame { header, payload });
            return;
        }

        let rel = rel.for_split();
        let chunk = max_body - frame_header_len(rel, true);
        let count = (payload.len() + chunk - 1) / chunk;
        if count > MAX_SPLIT_COUNT as usize {
            log::warn!("{}: dropping oversized packet ({} bytes)", self.addr, payload.len());
            return;
        }
        let split_id = self.next_split_id;
        self.next_split_id = self.next_split_id.wrapping_add(1);
        let base = self.new_header(rel, ch); // message index for fragment 0
        for (i, part) in payload.chunks(chunk).enumerate() {
            let mut header = base;
            if i > 0 {
                header.message_index = self.take_message_index();
            }
            header.split = Some(SplitInfo { count: count as u32, id: split_id, index: i as u32 });
            self.enqueue(OutFrame { header, payload: part.to_vec() });
        }
    }

    /// Queues a DisconnectNotification and marks the session closed.
    pub fn disconnect(&mut self) {
        self.send(vec![ID_DISCONNECT], Reliability::Reliable, 0);
        self.state = State::Closed;
    }

    /// Periodic work (call every ~10 ms): timeouts, ACK/NAK, resends and
    /// frame flushing. Returns `Some(reason)` if the session should be dropped.
    pub fn update(
        &mut self,
        now: Instant,
        scratch: &mut ByteWriter,
        sink: &mut dyn FnMut(&[u8]),
    ) -> Option<DisconnectReason> {
        if now.saturating_duration_since(self.last_recv) > self.timeout {
            return Some(DisconnectReason::Timeout);
        }
        if self.sent.len() > MAX_UNACKED || self.reliable_q.len() > MAX_QUEUED_FRAMES {
            return Some(DisconnectReason::Congested);
        }

        self.flush_acks(scratch, sink);

        if !self.sent.is_empty() {
            let rto = self.rto;
            self.expired.clear();
            for (seq, sd) in &self.sent {
                if now.saturating_duration_since(sd.sent_at) >= rto {
                    self.expired.push(*seq);
                }
            }
            if !self.expired.is_empty() {
                for i in 0..self.expired.len() {
                    let seq = self.expired[i];
                    if let Some(sd) = self.sent.remove(&seq) {
                        self.resend_q.push_back(sd.blob);
                    }
                }
                self.rto = (self.rto * 2).min(MAX_RTO); // back off
            }
        }

        self.flush_resends(now, scratch, sink);
        self.flush_frames(now, scratch, sink);
        None
    }

    fn flush_acks(&mut self, scratch: &mut ByteWriter, sink: &mut dyn FnMut(&[u8])) {
        let max_len = self.mtu - UDP_HEADER_SIZE;
        if !self.ack_queue.is_empty() {
            self.ack_queue.sort_unstable();
            self.ack_queue.dedup();
            send_records(ID_ACK, &self.ack_queue, max_len, scratch, sink);
            self.ack_queue.clear();
        }
        if !self.nak_queue.is_empty() {
            self.nak_queue.sort_unstable();
            self.nak_queue.dedup();
            send_records(ID_NAK, &self.nak_queue, max_len, scratch, sink);
            self.nak_queue.clear();
        }
    }

    fn take_seq(&mut self) -> u32 {
        let s = self.next_seq;
        self.next_seq = (s + 1) & MASK24;
        s
    }

    fn flush_resends(&mut self, now: Instant, scratch: &mut ByteWriter, sink: &mut dyn FnMut(&[u8])) {
        while let Some(blob) = self.resend_q.pop_front() {
            let seq = self.take_seq();
            scratch.clear();
            scratch.put_u8(ID_FRAME_SET_SEND);
            scratch.put_u24_le(seq);
            scratch.put_bytes(&blob);
            sink(scratch.as_slice());
            self.sent.insert(seq, SentDatagram { blob, sent_at: now, resent: true });
        }
    }

    fn flush_frames(&mut self, now: Instant, scratch: &mut ByteWriter, sink: &mut dyn FnMut(&[u8])) {
        let max_len = self.mtu - UDP_HEADER_SIZE;
        while !self.reliable_q.is_empty() || !self.unreliable_q.is_empty() {
            scratch.clear();
            scratch.put_u8(ID_FRAME_SET_SEND);
            scratch.put_u24_le(0); // patched below

            // Reliable frames first so they form a contiguous resend blob.
            while let Some(f) = self.reliable_q.front() {
                if scratch.len() > DATAGRAM_HEADER_SIZE && scratch.len() + f.encoded_len() > max_len {
                    break;
                }
                f.header.encode(scratch, &f.payload);
                self.reliable_q.pop_front();
            }
            let rel_end = scratch.len();
            while let Some(f) = self.unreliable_q.front() {
                if scratch.len() > DATAGRAM_HEADER_SIZE && scratch.len() + f.encoded_len() > max_len {
                    break;
                }
                f.header.encode(scratch, &f.payload);
                self.unreliable_q.pop_front();
            }

            let seq = self.take_seq();
            scratch.patch_u24_le(1, seq);
            if rel_end > DATAGRAM_HEADER_SIZE {
                self.sent.insert(
                    seq,
                    SentDatagram {
                        blob: scratch.as_slice()[DATAGRAM_HEADER_SIZE..rel_end].to_vec(),
                        sent_at: now,
                        resent: false,
                    },
                );
            }
            sink(scratch.as_slice());
        }
    }
}

/// Iterates the sequence numbers of an ACK/NAK packet.
fn for_each_record(data: &[u8], mut f: impl FnMut(u32)) {
    let mut r = ByteReader::new(&data[1..]);
    let Ok(count) = r.u16_be() else { return };
    for _ in 0..count {
        let Ok(single) = r.u8() else { return };
        if single != 0 {
            let Ok(s) = r.u24_le() else { return };
            f(s);
        } else {
            let (Ok(a), Ok(b)) = (r.u24_le(), r.u24_le()) else { return };
            if b >= a && b - a < 8192 {
                for s in a..=b {
                    f(s);
                }
            }
        }
    }
}

/// Encodes sorted, deduplicated sequence numbers into one or more ACK/NAK
/// packets (ranges where contiguous) and emits them through `sink`.
fn send_records(id: u8, seqs: &[u32], max_len: usize, scratch: &mut ByteWriter, sink: &mut dyn FnMut(&[u8])) {
    let mut i = 0;
    while i < seqs.len() {
        scratch.clear();
        scratch.put_u8(id);
        scratch.put_u16_be(0);
        let mut count: u16 = 0;
        while i < seqs.len() && scratch.len() + 7 <= max_len {
            let start = seqs[i];
            let mut end = start;
            let mut j = i + 1;
            while j < seqs.len() && seqs[j] == end + 1 {
                end = seqs[j];
                j += 1;
            }
            if start == end {
                scratch.put_u8(1);
                scratch.put_u24_le(start);
            } else {
                scratch.put_u8(0);
                scratch.put_u24_le(start);
                scratch.put_u24_le(end);
            }
            count += 1;
            i = j;
        }
        scratch.patch_u16_be(1, count);
        sink(scratch.as_slice());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> SocketAddr {
        "127.0.0.1:19132".parse().unwrap()
    }

    fn connected(mtu: usize, now: Instant) -> Session {
        let mut s = Session::new(addr(), 1, mtu, now, Duration::from_secs(10));
        s.state = State::Connected;
        s.announced = true;
        s
    }

    fn flush(s: &mut Session, now: Instant) -> Vec<Vec<u8>> {
        let mut scratch = ByteWriter::new();
        let mut out = Vec::new();
        let _ = s.update(now, &mut scratch, &mut |d: &[u8]| out.push(d.to_vec()));
        out
    }

    fn feed(s: &mut Session, dgs: &[Vec<u8>], now: Instant) -> Vec<Vec<u8>> {
        let mut got = Vec::new();
        for d in dgs {
            s.on_datagram(d, now, &mut |ev: SessionEvent<'_>| {
                if let SessionEvent::Packet(p) = ev {
                    got.push(p.to_vec());
                }
            });
        }
        got
    }

    #[test]
    fn split_roundtrip() {
        let now = Instant::now();
        let mut a = connected(1400, now);
        let mut b = connected(1400, now);
        let payload: Vec<u8> = (0..6000u32).map(|i| if i == 0 { 0x8f } else { (i % 251) as u8 }).collect();
        a.send(payload.clone(), Reliability::ReliableOrdered, 0);
        let dg = flush(&mut a, now);
        assert!(dg.len() >= 5);
        for d in &dg {
            assert!(d.len() <= 1400 - UDP_HEADER_SIZE);
        }
        let got = feed(&mut b, &dg, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], payload);
    }

    #[test]
    fn ordered_delivery_when_reordered() {
        let now = Instant::now();
        let mut a = connected(1400, now);
        let mut b = connected(1400, now);
        let mut dgs = Vec::new();
        for n in 1..=3u8 {
            a.send(vec![0x80, n], Reliability::ReliableOrdered, 0);
            dgs.extend(flush(&mut a, now));
        }
        assert_eq!(dgs.len(), 3);
        let got = feed(&mut b, &[dgs[2].clone(), dgs[0].clone(), dgs[1].clone()], now);
        assert_eq!(got, vec![vec![0x80, 1], vec![0x80, 2], vec![0x80, 3]]);
    }

    #[test]
    fn duplicate_datagram_delivered_once() {
        let now = Instant::now();
        let mut a = connected(1400, now);
        let mut b = connected(1400, now);
        a.send(vec![0x80, 7], Reliability::ReliableOrdered, 0);
        let dg = flush(&mut a, now);
        let got = feed(&mut b, &[dg[0].clone(), dg[0].clone()], now);
        assert_eq!(got.len(), 1);
    }

    #[test]
    fn timeout_resend_and_ack() {
        let now = Instant::now();
        let mut a = connected(1400, now);
        let mut b = connected(1400, now);
        a.send(vec![0x80, 9], Reliability::ReliableOrdered, 0);
        let lost = flush(&mut a, now);
        assert_eq!(lost.len(), 1);
        assert_eq!(a.sent.len(), 1);

        let later = now + Duration::from_secs(1);
        a.last_recv = later; // keep the session alive for this test
        let resent = flush(&mut a, later);
        assert_eq!(resent.len(), 1);
        let got = feed(&mut b, &resent, later);
        assert_eq!(got, vec![vec![0x80, 9]]);

        let acks = flush(&mut b, later);
        assert!(acks.iter().any(|d| d[0] == ID_ACK));
        feed(&mut a, &acks, later);
        assert!(a.sent.is_empty());
    }

    #[test]
    fn nak_triggers_resend() {
        let now = Instant::now();
        let mut a = connected(1400, now);
        let mut b = connected(1400, now);
        a.send(vec![0x80, 1], Reliability::ReliableOrdered, 0);
        let d0 = flush(&mut a, now);
        a.send(vec![0x80, 2], Reliability::ReliableOrdered, 0);
        let d1 = flush(&mut a, now);

        // first datagram lost
        assert!(feed(&mut b, &d1, now).is_empty()); // held: order index 1, expected 0
        let out = flush(&mut b, now);
        let nak: Vec<Vec<u8>> = out.into_iter().filter(|d| d[0] == ID_NAK).collect();
        assert_eq!(nak.len(), 1);
        feed(&mut a, &nak, now);
        let resent = flush(&mut a, now);
        assert_eq!(resent.len(), 1);
        let got = feed(&mut b, &resent, now);
        assert_eq!(got, vec![vec![0x80, 1], vec![0x80, 2]]);
        let _ = d0;
    }

    #[test]
    fn ack_records_range_encoding() {
        let mut scratch = ByteWriter::new();
        let mut out = Vec::new();
        send_records(ID_ACK, &[1, 2, 3, 7], 1000, &mut scratch, &mut |d: &[u8]| out.push(d.to_vec()));
        assert_eq!(out.len(), 1);
        let mut seen = Vec::new();
        for_each_record(&out[0], |s| seen.push(s));
        assert_eq!(seen, vec![1, 2, 3, 7]);
        assert_eq!(u16::from_be_bytes([out[0][1], out[0][2]]), 2); // one range + one single
    }

    #[test]
    fn handshake_accepts_and_connects() {
        let now = Instant::now();
        let mut s = Session::new(addr(), 1, 1400, now, Duration::from_secs(10));
        let mut events = 0;
        let mut req = ByteWriter::new();
        req.put_u8(ID_CONNECTION_REQUEST);
        req.put_i64_be(1);
        req.put_i64_be(1234);
        req.put_u8(0);
        s.handle_payload(req.as_slice(), &mut |_| events += 1);
        assert_eq!(s.state, State::Accepted);
        assert_eq!(s.reliable_q.len(), 1);
        s.handle_payload(&[ID_NEW_INCOMING_CONNECTION], &mut |ev: SessionEvent<'_>| {
            if let SessionEvent::Connected = ev {
                events += 10;
            }
        });
        assert!(s.announced());
        assert_eq!(events, 10);
    }
}
