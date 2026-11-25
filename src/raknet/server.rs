//! Tokio UDP front-end: offline handshake, session table, update loop.
//!
//! Everything runs on one thread (current-thread runtime). The loop wakes on
//! datagram arrival and on a 10 ms timer; every 5th timer tick is a 20 TPS
//! game tick.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;
use tokio::time::{interval, MissedTickBehavior};

use super::consts::*;
use super::handler::{Command, GameHandler, Outbox};
use super::packets;
use super::session::{DisconnectReason, Session, SessionEvent};
use crate::binary::ByteWriter;
use crate::protocol;

const PING_BUDGET_PER_SEC: u32 = 200;
const PENDING_TTL: Duration = Duration::from_secs(10);
const MAX_PENDING: usize = 1024;
const RECV_BATCH: usize = 64;

#[derive(Debug, Clone)]
pub struct RakConfig {
    pub bind: SocketAddr,
    pub name: String,
    pub max_players: u32,
    pub max_mtu: usize,
    pub session_timeout: Duration,
    pub max_per_ip: usize,
}

struct Pending {
    at: Instant,
}

pub struct RakServer<H: GameHandler> {
    socket: Arc<UdpSocket>,
    cfg: RakConfig,
    guid: i64,
    sessions: HashMap<SocketAddr, Session>,
    pending: HashMap<SocketAddr, Pending>,
    handler: H,
    outbox: Outbox,
    scratch: ByteWriter,
    motd: String,
    motd_online: usize,
    ping_budget: u32,
    update_counter: u64,
    tick: u64,
}

fn send_to(socket: &UdpSocket, data: &[u8], addr: SocketAddr) {
    match socket.try_send_to(data, addr) {
        Ok(_) => {}
        // UDP is lossy; the reliability layer resends what matters.
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(e) => log::debug!("send to {addr} failed: {e}"),
    }
}

fn make_guid() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0x5eed);
    nanos ^ (std::process::id() as i64).rotate_left(32)
}

impl<H: GameHandler> RakServer<H> {
    pub async fn bind(mut cfg: RakConfig, handler: H) -> io::Result<Self> {
        cfg.name = cfg.name.replace(';', ":"); // ; separates MOTD fields
        let socket = UdpSocket::bind(cfg.bind).await?;
        log::info!("RakNet v{RAKNET_PROTOCOL} listening on {}", socket.local_addr()?);
        Ok(Self {
            socket: Arc::new(socket),
            cfg,
            guid: make_guid(),
            sessions: HashMap::new(),
            pending: HashMap::new(),
            handler,
            outbox: Outbox::default(),
            scratch: ByteWriter::with_capacity(2048),
            motd: String::new(),
            motd_online: usize::MAX,
            ping_budget: PING_BUDGET_PER_SEC,
            update_counter: 0,
            tick: 0,
        })
    }

    pub fn online(&self) -> usize {
        self.sessions.values().filter(|s| s.announced()).count()
    }

    pub async fn run(mut self) -> io::Result<()> {
        let socket = Arc::clone(&self.socket);
        let mut buf = vec![0u8; 2048];
        let mut ticker = interval(Duration::from_millis(10));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                res = socket.recv_from(&mut buf) => {
                    match res {
                        Ok((n, addr)) => {
                            self.handle_datagram(&buf[..n], addr, Instant::now());
                            // Drain whatever else is already queued in the kernel.
                            for _ in 0..RECV_BATCH {
                                match socket.try_recv_from(&mut buf) {
                                    Ok((n, addr)) => self.handle_datagram(&buf[..n], addr, Instant::now()),
                                    Err(_) => break,
                                }
                            }
                        }
                        // Windows reports ICMP port-unreachable as a recv error.
                        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
                        Err(e) => {
                            log::error!("recv error: {e}");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
                _ = ticker.tick() => self.on_update(Instant::now()),
            }
        }
    }

    // ---------------------------------------------------------------------

    fn handle_datagram(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Some(&id) = data.first() else { return };
        if id >= ID_FRAME_SET_MIN {
            self.handle_connected(data, addr, now);
        } else {
            match id {
                ID_UNCONNECTED_PING | ID_UNCONNECTED_PING_OPEN => self.on_ping(data, addr),
                ID_OPEN_REQUEST_1 => self.on_open_request1(data, addr, now),
                ID_OPEN_REQUEST_2 => self.on_open_request2(data, addr, now),
                _ => {}
            }
        }
        self.apply_commands(now);
    }

    fn handle_connected(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let mut closed = None;
        {
            let Self { sessions, handler, outbox, .. } = self;
            let Some(session) = sessions.get_mut(&addr) else { return };
            session.on_datagram(data, now, &mut |ev: SessionEvent<'_>| match ev {
                SessionEvent::Connected => handler.on_connect(addr, outbox),
                SessionEvent::Packet(p) => handler.on_packet(addr, p, outbox),
                SessionEvent::Disconnected(r) => closed = Some(r),
            });
        }
        if let Some(reason) = closed {
            self.remove_session(addr, reason);
        }
    }

    fn on_ping(&mut self, data: &[u8], addr: SocketAddr) {
        if self.ping_budget == 0 {
            return;
        }
        let Ok(time) = packets::parse_unconnected_ping(data) else { return };
        self.ping_budget -= 1;
        let online = self.online();
        if online != self.motd_online {
            self.motd.clear();
            let _ = write!(
                self.motd,
                "MCPE;{};{};{};{};{}",
                self.cfg.name,
                protocol::MOTD_PROTOCOL,
                protocol::GAME_VERSION,
                online,
                self.cfg.max_players
            );
            self.motd_online = online;
        }
        self.scratch.clear();
        packets::write_unconnected_pong(&mut self.scratch, time, self.guid, &self.motd);
        send_to(&self.socket, self.scratch.as_slice(), addr);
    }

    fn on_open_request1(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Ok((proto, mtu)) = packets::parse_open_request1(data) else { return };
        self.scratch.clear();
        if proto != RAKNET_PROTOCOL {
            packets::write_incompatible_protocol(&mut self.scratch, self.guid);
        } else if self.sessions.len() >= self.cfg.max_players as usize && !self.sessions.contains_key(&addr) {
            packets::write_no_free_connections(&mut self.scratch, self.guid);
        } else {
            if self.pending.len() >= MAX_PENDING && !self.pending.contains_key(&addr) {
                return;
            }
            self.pending.insert(addr, Pending { at: now });
            let mtu = mtu.clamp(MIN_MTU, self.cfg.max_mtu);
            packets::write_open_reply1(&mut self.scratch, self.guid, mtu as u16);
        }
        send_to(&self.socket, self.scratch.as_slice(), addr);
    }

    fn on_open_request2(&mut self, data: &[u8], addr: SocketAddr, now: Instant) {
        let Ok(req) = packets::parse_open_request2(data) else { return };
        if !self.pending.contains_key(&addr) && !self.sessions.contains_key(&addr) {
            return; // never saw Request1: ignore (anti-spoof)
        }
        if let Some(existing) = self.sessions.get(&addr) {
            if existing.is_handshaking() {
                // Our Reply2 was probably lost; answer again.
                let mtu = existing.mtu();
                self.reply2(addr, mtu);
                return;
            }
            self.remove_session(addr, DisconnectReason::Client); // client restarted
        }
        if self.sessions.len() >= self.cfg.max_players as usize {
            self.scratch.clear();
            packets::write_no_free_connections(&mut self.scratch, self.guid);
            send_to(&self.socket, self.scratch.as_slice(), addr);
            return;
        }
        let from_ip = self.sessions.keys().filter(|a| a.ip() == addr.ip()).count();
        if from_ip >= self.cfg.max_per_ip {
            log::debug!("{addr}: too many connections from this address");
            return;
        }
        let mtu = (req.mtu as usize).clamp(MIN_MTU, self.cfg.max_mtu);
        self.pending.remove(&addr);
        self.sessions
            .insert(addr, Session::new(addr, req.client_guid, mtu, now, self.cfg.session_timeout));
        self.reply2(addr, mtu);
    }

    fn reply2(&mut self, addr: SocketAddr, mtu: usize) {
        self.scratch.clear();
        packets::write_open_reply2(&mut self.scratch, self.guid, addr, mtu as u16);
        send_to(&self.socket, self.scratch.as_slice(), addr);
    }

    // ---------------------------------------------------------------------

    fn on_update(&mut self, now: Instant) {
        self.update_counter += 1;
        if self.update_counter % 5 == 0 {
            self.tick += 1;
            self.handler.on_tick(self.tick, &mut self.outbox);
            self.apply_commands(now);
        }
        if self.update_counter % 100 == 0 {
            self.ping_budget = PING_BUDGET_PER_SEC;
            self.pending.retain(|_, p| now.saturating_duration_since(p.at) < PENDING_TTL);
        }

        let mut dead = Vec::new();
        {
            let Self { sessions, scratch, socket, .. } = self;
            let sock: &UdpSocket = &**socket;
            for (addr, session) in sessions.iter_mut() {
                let addr = *addr;
                if let Some(reason) = session.update(now, scratch, &mut |d: &[u8]| send_to(sock, d, addr)) {
                    dead.push((addr, reason));
                }
            }
        }
        for (addr, reason) in dead {
            self.remove_session(addr, reason);
        }
        self.apply_commands(now);
    }

    fn flush_session(&mut self, addr: SocketAddr, now: Instant) {
        let Self { sessions, scratch, socket, .. } = self;
        let sock: &UdpSocket = &**socket;
        if let Some(s) = sessions.get_mut(&addr) {
            let _ = s.update(now, scratch, &mut |d: &[u8]| send_to(sock, d, addr));
        }
    }

    fn remove_session(&mut self, addr: SocketAddr, reason: DisconnectReason) {
        if let Some(s) = self.sessions.remove(&addr) {
            if s.announced() {
                self.handler.on_disconnect(addr, reason, &mut self.outbox);
            }
        }
    }

    /// Executes commands queued by the handler. Loops because on_disconnect
    /// callbacks may queue more.
    fn apply_commands(&mut self, now: Instant) {
        while !self.outbox.cmds.is_empty() {
            let mut cmds = std::mem::take(&mut self.outbox.cmds);
            for cmd in cmds.drain(..) {
                match cmd {
                    Command::Send { addr, payload, reliability, channel } => {
                        if let Some(s) = self.sessions.get_mut(&addr) {
                            s.send(payload, reliability, channel);
                        }
                    }
                    Command::Disconnect { addr } => {
                        if let Some(s) = self.sessions.get_mut(&addr) {
                            s.disconnect();
                        }
                        self.flush_session(addr, now);
                        self.remove_session(addr, DisconnectReason::Server);
                    }
                }
            }
            if self.outbox.cmds.is_empty() {
                self.outbox.cmds = cmds; // recycle capacity
            }
        }
    }
}
