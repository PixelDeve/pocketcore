//! Boundary between the RakNet front-end and game logic.

use std::net::SocketAddr;

use super::frame::Reliability;
use crate::protocol::GAME_PACKET_WRAPPER;
use super::session::DisconnectReason;

pub enum Command {
    Send { addr: SocketAddr, payload: Vec<u8>, reliability: Reliability, channel: u8 },
    Disconnect { addr: SocketAddr },
}

/// Commands queued by the handler; drained by the server after every
/// callback. The backing Vec is recycled so steady state allocates nothing.
#[derive(Default)]
pub struct Outbox {
    pub(crate) cmds: Vec<Command>,
}

impl Outbox {
    /// Reliable-ordered on channel 0 (what game packets normally use).
    pub fn send(&mut self, addr: SocketAddr, payload: Vec<u8>) {
        self.send_with(addr, payload, Reliability::ReliableOrdered, 0);
    }

    pub fn send_with(&mut self, addr: SocketAddr, payload: Vec<u8>, reliability: Reliability, channel: u8) {
        // game packets are wrapped in 0x8e on the wire
        let mut wrapped = Vec::with_capacity(payload.len() + 1);
        wrapped.push(GAME_PACKET_WRAPPER);
        wrapped.extend_from_slice(&payload);
        self.cmds.push(Command::Send { addr, payload: wrapped, reliability, channel });
    }

    pub fn disconnect(&mut self, addr: SocketAddr) {
        self.cmds.push(Command::Disconnect { addr });
    }
}

/// Implemented by the game layer (Step 3+). Called synchronously from the
/// network loop on a single thread.
pub trait GameHandler {
    /// RakNet handshake finished.
    fn on_connect(&mut self, addr: SocketAddr, out: &mut Outbox);
    /// A complete game packet (first byte is the MCPE packet id).
    fn on_packet(&mut self, addr: SocketAddr, data: &[u8], out: &mut Outbox);
    fn on_disconnect(&mut self, addr: SocketAddr, reason: DisconnectReason, out: &mut Outbox);
    /// 20 TPS.
    fn on_tick(&mut self, _tick: u64, _out: &mut Outbox) {}
}
