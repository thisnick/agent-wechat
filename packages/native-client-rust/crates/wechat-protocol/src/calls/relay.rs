//! Selected IPv4/IPv6 relay setup: initial check-in, at most one learned
//! continuation (or direct 1-RTT merged check-in) and explicit Ready. CRC/TEA
//! structural checks are not media AEAD. The host owns sockets, current
//! account/key generations, time and randomness.

use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use zeroize::Zeroizing;

const MAX_PACKET: usize = 8197;
const MAX_BODY: usize = 8192;
const MAX_IP_TEXT: usize = 45;

/// Relay endpoint identity sufficient to run the UDP check-in handshake
/// (before media keys are admitted on `Join`). Room key intentionally has no
/// `Debug` representation, matching [`SelectedRelay`].
#[derive(Clone, PartialEq, Eq)]
pub struct RelayTarget {
    /// Allocated call room identifier.
    pub room_id: u64,
    /// Allocated room key used to derive the CheckIn TEA key.
    pub room_key: u64,
    /// Local member index in the room.
    pub own_member: u16,
    /// Selected relay connection identifier.
    pub connection_id: u16,
    /// Selected UDP relay endpoint (IPv4 or IPv6) from the Strategy.
    pub endpoint: SocketAddr,
    /// Whether the Strategy advertises 1-RTT merged check-in support.
    pub merge_mode: bool,
}

impl RelayTarget {
    /// Extract a UDP relay check-in target from a Strategy message without requiring
    /// field 10 (`media_key`). Used for ringing pre-connect when `ConfAck` carries
    /// relay endpoints ahead of `Join`.
    pub fn from_strategy(
        room_id: u64,
        room_key: u64,
        own_member: u16,
        strategy: &[u8],
        connection_index: usize,
        udp_index: usize,
    ) -> Result<Self> {
        ensure!(room_id != 0 && room_key != 0, "Unallocated relay room");
        let strategy = fields(strategy)?;
        let connections = repeated(&strategy, 15, 32)?;
        let (connection_id, endpoint) = if connections.is_empty() {
            ensure!(
                connection_index == 0,
                "Relay connection selection unavailable"
            );
            let endpoints = repeated(&strategy, 1, 32)?;
            let raw = endpoints
                .get(udp_index)
                .ok_or_else(|| anyhow::anyhow!("Relay UDP selection unavailable"))?;
            (0u16, parse_endpoint(raw)?)
        } else {
            let connection = fields(
                connections
                    .get(connection_index)
                    .ok_or_else(|| anyhow::anyhow!("Relay connection selection unavailable"))?,
            )?;
            let connection_id = u16::try_from(number(&connection, 1, None)?)?;
            validate_connection_metadata(&connection)?;
            let endpoints = repeated(&connection, 4, 32)?;
            let raw = endpoints
                .get(udp_index)
                .ok_or_else(|| anyhow::anyhow!("Relay UDP selection unavailable"))?;
            (connection_id, parse_endpoint(raw)?)
        };
        Ok(Self {
            room_id,
            room_key,
            own_member,
            connection_id,
            endpoint,
            merge_mode: number(&strategy, 16, Some(0))? & 1 != 0,
        })
    }
}

/// An endpoint selected from an admitted Strategy. No DNS or fallback is used.
/// Room key and media material intentionally have no Debug representation.
pub struct SelectedRelay {
    pub room_id: u64,
    pub room_key: u64,
    pub own_member: u16,
    pub connection_id: u16,
    /// Selected UDP relay endpoint (IPv4 or IPv6) from the admitted Strategy.
    pub endpoint: SocketAddr,
    pub media_key: Zeroizing<[u8; 32]>,
    pub merge_mode: bool,
}
impl SelectedRelay {
    /// Strategy field15 contains modern connection records (with field1 legacy
    /// UDP addresses used only when field15 is empty). Indices must be chosen by
    /// the caller from the current admitted Strategy, not by retries.
    pub fn from_strategy(
        room_id: u64,
        room_key: u64,
        own_member: u16,
        strategy: &[u8],
        connection_index: usize,
        udp_index: usize,
    ) -> Result<Self> {
        let target = RelayTarget::from_strategy(
            room_id,
            room_key,
            own_member,
            strategy,
            connection_index,
            udp_index,
        )?;
        let strategy = fields(strategy)?;
        let key = data(&strategy, 10)?;
        ensure!(key.len() == 32, "Unsupported relay media key size");
        Ok(Self {
            room_id: target.room_id,
            room_key: target.room_key,
            own_member: target.own_member,
            connection_id: target.connection_id,
            endpoint: target.endpoint,
            media_key: Zeroizing::new(key.try_into()?),
            merge_mode: target.merge_mode,
        })
    }

    /// Return the check-in target metadata for this selected relay (without media key material).
    pub fn target(&self) -> RelayTarget {
        RelayTarget {
            room_id: self.room_id,
            room_key: self.room_key,
            own_member: self.own_member,
            connection_id: self.connection_id,
            endpoint: self.endpoint,
            merge_mode: self.merge_mode,
        }
    }

    /// List valid UDP relay candidates as `(connection_index, udp_index, endpoint)`
    /// from an admitted Strategy in server order.
    pub fn strategy_candidates(strategy: &[u8]) -> Result<Vec<(usize, usize, SocketAddr)>> {
        let strategy = fields(strategy)?;
        let connections = repeated(&strategy, 15, 32)?;
        let mut out = Vec::new();
        if connections.is_empty() {
            let Ok(endpoints) = repeated(&strategy, 1, 32) else {
                return Ok(out);
            };
            for (udp_index, raw) in endpoints.into_iter().enumerate() {
                if let Ok(endpoint) = parse_endpoint(raw) {
                    out.push((0, udp_index, endpoint));
                }
            }
            return Ok(out);
        }
        for (connection_index, raw_connection) in connections.into_iter().enumerate() {
            let Ok(connection) = fields(raw_connection) else {
                continue;
            };
            let Ok(connection_id) = number(&connection, 1, None) else {
                continue;
            };
            if u16::try_from(connection_id).is_err() {
                continue;
            }
            if validate_connection_metadata(&connection).is_err() {
                continue;
            }
            let Ok(endpoints) = repeated(&connection, 4, 32) else {
                continue;
            };
            for (udp_index, raw) in endpoints.into_iter().enumerate() {
                if let Ok(endpoint) = parse_endpoint(raw) {
                    out.push((connection_index, udp_index, endpoint));
                }
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayEpoch {
    pub account_generation: u64,
    pub logout_epoch: u64,
    pub call_generation: u64,
    pub key_generation: u64,
    pub socket_generation: u64,
    pub attempt_id: u64,
}
impl RelayEpoch {
    fn validate(self) -> Result<()> {
        ensure!(
            [
                self.account_generation,
                self.call_generation,
                self.key_generation,
                self.socket_generation,
                self.attempt_id
            ]
            .into_iter()
            .all(|v| v != 0),
            "Inactive relay generation"
        );
        Ok(())
    }
}

/// Selected initial/reset room values. Constructors are deliberately scoped to
/// this personal profile; they do not describe every supported server transport.
#[derive(Clone)]
pub struct RoomInputs {
    pub room_id: u64,
    pub own_member: u16,
    pub connection_id: u16,
    pub client_ip: u32,
    pub client_text: String,
    pub client_port: u16,
    pub server_ip: u32,
    pub server_text: String,
    pub server_port: u16,
    pub checkin_sequence: u16,
    pub network_type: u16,
    pub arq_flag: u16,
    pub has_accept: bool,
    pub data_sequence: u32,
    pub mix_timestamp: u32,
    pub mix_sequence: u16,
    pub merge_mode: bool,
    pub header_timestamp: u32,
    pub body_timestamp: u32,
}
impl RoomInputs {
    pub fn initial(
        selected: &SelectedRelay,
        network_type: u16,
        header_timestamp: u32,
        body_timestamp: u32,
    ) -> Self {
        Self::initial_for_target(
            &selected.target(),
            network_type,
            header_timestamp,
            body_timestamp,
        )
    }

    /// Build initial check-in inputs from a [`RelayTarget`] without requiring media keys.
    pub fn initial_for_target(
        target: &RelayTarget,
        network_type: u16,
        header_timestamp: u32,
        body_timestamp: u32,
    ) -> Self {
        let (server_ip, server_text) = match target.endpoint.ip() {
            IpAddr::V4(ip) => (u32::from_le_bytes(ip.octets()), ip.to_string()),
            IpAddr::V6(ip) => (0, ip.to_string()),
        };
        Self {
            room_id: target.room_id,
            own_member: target.own_member,
            connection_id: target.connection_id,
            client_ip: 0,
            client_text: String::new(),
            client_port: 0,
            server_ip,
            server_text,
            server_port: target.endpoint.port(),
            checkin_sequence: 1,
            network_type,
            arq_flag: 0,
            has_accept: true,
            data_sequence: 0,
            mix_timestamp: 0,
            mix_sequence: 0,
            merge_mode: target.merge_mode,
            header_timestamp,
            body_timestamp,
        }
    }
    fn validate(&self) -> Result<()> {
        ensure!(self.room_id != 0, "Unallocated relay room");
        for text in [&self.client_text, &self.server_text] {
            ensure!(
                text.len() <= 128 && text.is_ascii() && !text.as_bytes().contains(&0),
                "Invalid relay address text"
            );
        }
        Ok(())
    }
    pub fn body(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut values = vec![
            p::uint(1, 0),
            p::uint(2, self.room_id & 0xffff_ffff),
            p::uint(3, u64::from(self.client_ip)),
            p::uint(4, u64::from(self.server_ip)),
            p::uint(5, u64::from(self.client_port)),
            p::uint(6, u64::from(self.server_port)),
            p::uint(7, u64::from(self.checkin_sequence)),
            p::uint(8, u64::from(self.own_member)),
            p::uint(9, u64::from(self.own_member)),
            p::uint(10, u64::from(self.body_timestamp)),
            p::uint(12, u64::from(self.arq_flag)),
            p::uint(13, u64::from(self.network_type)),
            p::bytes(14, self.client_text.as_bytes()),
            p::bytes(15, self.server_text.as_bytes()),
            p::uint(16, 1),
            p::uint(17, u64::from(self.data_sequence)),
            p::uint(18, u64::from(self.mix_timestamp)),
            p::uint(19, u64::from(self.mix_sequence)),
            p::uint(20, u64::from(self.has_accept)),
        ];
        if self.merge_mode {
            values.push(p::uint(21, 1));
        }
        bounded_message(&values)
    }
    pub fn random_bytes_needed(&self) -> Result<usize> {
        Ok((8 - (self.body()?.len() + 10) % 8) % 8 + 3)
    }
    fn packet(&self, room_key: u64, random: &[u8]) -> Result<Vec<u8>> {
        let cipher = encrypt_checkin(&self.body()?, &*checkin_key(room_key)?, random)?;
        let pub_header = bounded_message(&[p::uint(1, 2), p::uint(2, 3), p::uint(3, 0)])?;
        let (room32, room64) = room_pair(self.room_id);
        let header = bounded_message(&[
            p::bytes(1, &pub_header),
            p::uint(2, room32),
            p::uint(3, 10),
            p::uint(4, u64::from(self.own_member)),
            p::bytes(5, &[]),
            p::uint(6, u64::from(self.own_member)),
            p::uint(8, u64::from(self.header_timestamp)),
            p::uint(9, room64),
            p::uint(10, u64::from(self.connection_id)),
        ])?;
        frame_outer(&bounded_message(&[
            p::bytes(1, &header),
            p::uint(2, cipher.len() as u64),
            p::bytes(3, &cipher),
        ])?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayProgress {
    Opaque,
    FirstCheckin,
    DuplicateCheckin,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    New,
    AwaitingCheckin,
    ContinuationPending,
    AwaitingReady,
    Ready,
    Closed,
}

/// No retries or network I/O. Every preparation is spent before randomness or
/// encryption; failure closes this attempt instead of making it reusable.
pub struct TwoRttRelay {
    room: RoomInputs,
    key: Zeroizing<u64>,
    endpoint: SocketAddr,
    epoch: RelayEpoch,
    deadline_ms: u64,
    last_ms: Option<u64>,
    phase: Phase,
    merge_requested: bool,
}
impl TwoRttRelay {
    /// Create a relay check-in state machine bound to the selected IPv4 or IPv6 UDP endpoint.
    pub fn new(
        room: RoomInputs,
        room_key: u64,
        endpoint: SocketAddr,
        epoch: RelayEpoch,
        deadline_ms: u64,
    ) -> Result<Self> {
        room.validate()?;
        epoch.validate()?;
        ensure!(
            room_key != 0 && endpoint.port() != 0,
            "Invalid relay key or endpoint"
        );
        let expected_server_ip = match endpoint.ip() {
            IpAddr::V4(ip) => u32::from_le_bytes(ip.octets()),
            IpAddr::V6(_) => 0,
        };
        ensure!(
            room.server_text == endpoint.ip().to_string()
                && room.server_ip == expected_server_ip
                && room.server_port == endpoint.port(),
            "Relay request endpoint mismatch"
        );
        let merge_requested = room.merge_mode;
        Ok(Self {
            room,
            key: Zeroizing::new(room_key),
            endpoint,
            epoch,
            deadline_ms,
            last_ms: None,
            phase: Phase::New,
            merge_requested,
        })
    }
    pub fn room(&self) -> &RoomInputs {
        &self.room
    }
    pub fn is_ready(&self) -> bool {
        self.phase == Phase::Ready
    }
    pub fn random_bytes_needed(&self) -> Result<usize> {
        self.room.random_bytes_needed()
    }
    pub fn continuation_random_bytes_needed(
        &self,
        header_timestamp: u32,
        body_timestamp: u32,
    ) -> Result<usize> {
        let mut room = self.room.clone();
        room.header_timestamp = header_timestamp;
        room.body_timestamp = body_timestamp;
        room.random_bytes_needed()
    }
    /// Return the random padding byte count needed to rebuild the initial check-in
    /// packet with `merge_mode` cleared for 2-RTT fallback.
    pub fn merge_fallback_random_bytes_needed(
        &self,
        header_timestamp: u32,
        body_timestamp: u32,
    ) -> Result<usize> {
        let mut room = self.room.clone();
        room.merge_mode = false;
        room.header_timestamp = header_timestamp;
        room.body_timestamp = body_timestamp;
        room.random_bytes_needed()
    }

    fn live(&mut self, epoch: RelayEpoch, now_ms: u64) -> Result<()> {
        if epoch.validate().is_err() || epoch != self.epoch {
            self.close();
            bail!("Relay generation changed");
        }
        if now_ms >= self.deadline_ms {
            self.close();
            bail!("Relay check-in deadline exceeded");
        }
        if self.last_ms.is_some_and(|last| now_ms < last) || self.phase == Phase::Closed {
            self.close();
            bail!("Relay clock or state invalid");
        }
        self.last_ms = Some(now_ms);
        Ok(())
    }
    pub fn prepare_initial(
        &mut self,
        epoch: RelayEpoch,
        now_ms: u64,
        random: &[u8],
    ) -> Result<Vec<u8>> {
        self.live(epoch, now_ms)?;
        ensure!(
            self.phase == Phase::New,
            "Relay initial check-in already spent"
        );
        self.phase = Phase::Closed;
        let packet = self.room.packet(*self.key, random)?;
        self.phase = Phase::AwaitingCheckin;
        Ok(packet)
    }
    /// Clear `merge_mode` after a 1-RTT timeout and rebuild the initial check-in
    /// packet for standard 2-RTT fallback.
    pub fn prepare_merge_fallback(
        &mut self,
        epoch: RelayEpoch,
        now_ms: u64,
        header_timestamp: u32,
        body_timestamp: u32,
        random: &[u8],
    ) -> Result<Vec<u8>> {
        self.live(epoch, now_ms)?;
        ensure!(
            self.phase == Phase::AwaitingCheckin && self.room.merge_mode,
            "Relay merge fallback unavailable"
        );
        self.phase = Phase::Closed;
        self.room.merge_mode = false;
        self.room.header_timestamp = header_timestamp;
        self.room.body_timestamp = body_timestamp;
        let packet = self.room.packet(*self.key, random)?;
        self.phase = Phase::AwaitingCheckin;
        Ok(packet)
    }
    pub fn prepare_continuation(
        &mut self,
        epoch: RelayEpoch,
        now_ms: u64,
        header_timestamp: u32,
        body_timestamp: u32,
        random: &[u8],
    ) -> Result<Vec<u8>> {
        self.live(epoch, now_ms)?;
        ensure!(
            self.phase == Phase::ContinuationPending,
            "Relay continuation unavailable or spent"
        );
        self.phase = Phase::Closed;
        self.room.header_timestamp = header_timestamp;
        self.room.body_timestamp = body_timestamp;
        let packet = self.room.packet(*self.key, random)?;
        self.phase = Phase::AwaitingReady;
        Ok(packet)
    }
    /// Process one inbound UDP datagram from `source` and advance the relay check-in state.
    pub fn receive(
        &mut self,
        packet: &[u8],
        source: SocketAddr,
        epoch: RelayEpoch,
        now_ms: u64,
    ) -> Result<RelayProgress> {
        self.live(epoch, now_ms)?;
        ensure!(
            matches!(
                self.phase,
                Phase::AwaitingCheckin | Phase::ContinuationPending | Phase::AwaitingReady
            ),
            "Relay receive unavailable"
        );
        let result = self.receive_checked(packet, source);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn receive_checked(&mut self, packet: &[u8], source: SocketAddr) -> Result<RelayProgress> {
        ensure!(source == self.endpoint, "Unexpected relay source endpoint");
        ensure!(packet.len() <= MAX_PACKET, "Relay packet too large");
        if packet.first() != Some(&0xd5) {
            return Ok(RelayProgress::Opaque);
        }
        let outer = fields(unframe_outer(packet)?)?;
        let header = fields(data(&outer, 1)?)?;
        let public = fields(data(&header, 1)?)?;
        let command = number(&public, 1, None)?;
        if !matches!(command, 0x1002 | 0x1004) {
            return Ok(RelayProgress::Opaque);
        }
        ensure!(
            number(&public, 2, None)? == 3 && number(&public, 3, None)? == 0,
            "Unsupported relay response profile"
        );
        let (room32, room64) = room_pair(self.room.room_id);
        ensure!(
            number(&header, 2, None)? == room32
                && number(&header, 5, Some(0))? == room64
                && number(&header, 3, None)? == 10
                && number(&header, 7, Some(0))? == 0,
            "Relay response context mismatch"
        );
        let cipher = data(&outer, 3)?;
        ensure!(
            number(&outer, 2, None)? == cipher.len() as u64,
            "Relay response length mismatch"
        );
        let clear = decrypt_checkin(cipher, &*checkin_key(*self.key)?, 4096)?;
        if command == 0x1002 && self.phase != Phase::AwaitingCheckin {
            return Ok(RelayProgress::DuplicateCheckin);
        }
        let body = fields(&clear)?;
        if command == 0x1004 {
            for n in [1, 2, 3, 5, 6, 7] {
                let v = number(&body, n, None)?;
                if n != 1 {
                    u32::try_from(v)?;
                }
            }
            repeated(&body, 4, 64)?;
            ensure!(
                [number(&body, 5, None)?, number(&body, 7, None)?]
                    .contains(&u64::from(self.room.own_member)),
                "Relay Ready member mismatch"
            );
            self.phase = Phase::Ready;
            return Ok(RelayProgress::Ready);
        }
        for n in 1..=9 {
            u32::try_from(number(&body, n, None)?)?;
        }
        let merged = number(&body, 14, Some(0))?;
        ensure!(
            number(&body, 6, None)? == u64::from(self.room.checkin_sequence)
                && (merged == 0 || (merged == 1 && self.merge_requested)),
            "Relay check-in sequence or merge mismatch"
        );
        let client = p::optional_data(&body, 12)?.unwrap_or_default();
        let server = p::optional_data(&body, 13)?.unwrap_or_default();
        let (client_text, client_v4) = parse_relay_ip_text(client, true)?;
        let (server_text, server_v4) = parse_relay_ip_text(server, true)?;
        let client_numeric = number(&body, 2, None)? as u32;
        self.room.client_ip = if client_numeric == 0 {
            client_v4.map_or(0, |ip| u32::from_le_bytes(ip.octets()))
        } else {
            client_numeric
        };
        self.room.client_text = client_text.to_owned();
        self.room.server_text = server_text.to_owned();
        self.room.server_ip = server_v4.map_or(0, |ip| u32::from_le_bytes(ip.octets()));
        self.room.client_port = number(&body, 4, None)? as u16;
        self.room.server_port = number(&body, 5, None)? as u16;
        if merged == 1 {
            self.phase = Phase::Ready;
            return Ok(RelayProgress::Ready);
        }
        self.room.merge_mode = false;
        self.phase = Phase::ContinuationPending;
        Ok(RelayProgress::FirstCheckin)
    }
    pub fn close(&mut self) {
        self.key = Zeroizing::new(0);
        self.phase = Phase::Closed;
    }
}

fn validate_connection_metadata(connection: &[Field]) -> Result<()> {
    if let Some(nic) = p::optional_data(connection, 2)? {
        ensure!(
            nic.len() <= 128 && !nic.contains(&0),
            "Invalid relay NIC identity"
        );
        std::str::from_utf8(nic)?;
    }
    u16::try_from(number(connection, 3, Some(0))?)?;
    Ok(())
}
fn parse_endpoint(raw: &[u8]) -> Result<SocketAddr> {
    let endpoint = fields(raw)?;
    let address = parse_relay_ip(data(&endpoint, 1)?)?;
    let port = u16::try_from(number(&endpoint, 2, None)?)?;
    ensure!(port != 0, "Invalid relay endpoint port");
    Ok(SocketAddr::new(address, port))
}
fn room_pair(room: u64) -> (u64, u64) {
    if room <= u64::from(u32::MAX) {
        (room, 0)
    } else {
        (0, room)
    }
}
fn fields(raw: &[u8]) -> Result<Vec<Field>> {
    ensure!(raw.len() <= MAX_BODY, "Relay message too large");
    super::signaling::decode_fields(raw)
}
fn bounded_message(parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    let out = p::message(parts)?;
    ensure!(out.len() <= MAX_BODY, "Relay message too large");
    Ok(out)
}
fn data(values: &[Field], n: u32) -> Result<&[u8]> {
    p::optional_data(values, n)?.ok_or_else(|| anyhow::anyhow!("Missing relay bytes field {n}"))
}
fn number(values: &[Field], n: u32, default: Option<u64>) -> Result<u64> {
    match p::optional(values, n)? {
        Some(Value::Varint(v)) => Ok(*v),
        None => default.ok_or_else(|| anyhow::anyhow!("Missing relay integer {n}")),
        _ => bail!("Invalid relay integer {n}"),
    }
}
fn repeated(values: &[Field], n: u32, limit: usize) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    for f in values.iter().filter(|v| v.number == n) {
        ensure!(out.len() < limit, "Too many relay entries");
        if let Value::Bytes(raw) = &f.value {
            ensure!(raw.len() <= 4096, "Relay entry too large");
            fields(raw)?;
            out.push(raw.as_slice());
        } else {
            bail!("Invalid relay entry wire type");
        }
    }
    Ok(out)
}
fn parse_relay_ip(raw: &[u8]) -> Result<IpAddr> {
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_IP_TEXT && !raw.contains(&0),
        "Relay IP text too large or empty"
    );
    let text = std::str::from_utf8(raw)?;
    ensure!(text.is_ascii(), "Non-ASCII relay IP address");
    let ip: IpAddr = text.parse()?;
    if let IpAddr::V4(v4) = ip {
        ensure!(v4.to_string() == text, "Noncanonical relay IPv4 address");
    }
    Ok(ip)
}
fn parse_relay_ip_text(raw: &[u8], allow_empty: bool) -> Result<(&str, Option<Ipv4Addr>)> {
    if raw.is_empty() && allow_empty {
        return Ok(("", None));
    }
    let ip = parse_relay_ip(raw)?;
    let text = std::str::from_utf8(raw)?;
    Ok((
        text,
        match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        },
    ))
}
fn checkin_key(room_key: u64) -> Result<Zeroizing<[u8; 16]>> {
    ensure!(room_key != 0, "Missing relay key");
    let digest = openssl::hash::hash(openssl::hash::MessageDigest::md5(), &room_key.to_le_bytes())?;
    Ok(Zeroizing::new(digest.as_ref().try_into()?))
}

pub fn frame_outer(body: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !body.is_empty() && body.len() <= MAX_BODY,
        "Invalid relay outer size"
    );
    let mut out = vec![0xd5];
    out.extend(crc32(body).to_be_bytes());
    out.extend(body);
    Ok(out)
}
pub fn unframe_outer(packet: &[u8]) -> Result<&[u8]> {
    ensure!(
        (6..=MAX_PACKET).contains(&packet.len()) && packet[0] == 0xd5,
        "Invalid relay outer packet"
    );
    let body = &packet[5..];
    ensure!(
        u32::from_be_bytes(packet[1..5].try_into()?) == crc32(body),
        "Relay CRC mismatch"
    );
    Ok(body)
}
fn crc32(body: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in body {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = (value >> 1) ^ if value & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    value ^ u32::MAX
}
fn tea(block: [u8; 8], key: &[u8; 16], decrypt: bool) -> [u8; 8] {
    let mut a = u32::from_be_bytes(block[..4].try_into().expect("block width"));
    let mut b = u32::from_be_bytes(block[4..].try_into().expect("block width"));
    let k: [u32; 4] = std::array::from_fn(|n| {
        u32::from_be_bytes(key[n * 4..n * 4 + 4].try_into().expect("key width"))
    });
    if decrypt {
        let mut sum = 0xe3779b90u32;
        for _ in 0..16 {
            b = b.wrapping_sub(
                (a.wrapping_shl(4).wrapping_add(k[2]))
                    ^ (a.wrapping_add(sum))
                    ^ ((a >> 5).wrapping_add(k[3])),
            );
            a = a.wrapping_sub(
                (b.wrapping_shl(4).wrapping_add(k[0]))
                    ^ (b.wrapping_add(sum))
                    ^ ((b >> 5).wrapping_add(k[1])),
            );
            sum = sum.wrapping_sub(0x9e3779b9);
        }
    } else {
        let mut sum = 0u32;
        for _ in 0..16 {
            sum = sum.wrapping_add(0x9e3779b9);
            a = a.wrapping_add(
                (b.wrapping_shl(4).wrapping_add(k[0]))
                    ^ (b.wrapping_add(sum))
                    ^ ((b >> 5).wrapping_add(k[1])),
            );
            b = b.wrapping_add(
                (a.wrapping_shl(4).wrapping_add(k[2]))
                    ^ (a.wrapping_add(sum))
                    ^ ((a >> 5).wrapping_add(k[3])),
            );
        }
    }
    let mut out = [0; 8];
    out[..4].copy_from_slice(&a.to_be_bytes());
    out[4..].copy_from_slice(&b.to_be_bytes());
    out
}
/// Selector0 TEA chaining with its structural trailer. This is not an AEAD.
pub fn encrypt_checkin(plain: &[u8], key: &[u8; 16], random: &[u8]) -> Result<Vec<u8>> {
    ensure!(plain.len() <= 4096, "Relay check-in plaintext too large");
    let pad = (8 - (plain.len() + 10) % 8) % 8;
    ensure!(
        random.len() == pad + 3,
        "Wrong relay check-in randomness length"
    );
    let mut clear = Zeroizing::new(Vec::with_capacity(plain.len() + pad + 10));
    clear.push((random[0] & 0xf8) | pad as u8);
    clear.extend(&random[1..]);
    clear.extend(plain);
    clear.extend([0; 7]);
    let mut previous_cipher = [0; 8];
    let mut previous_x = [0; 8];
    let mut out = Vec::with_capacity(clear.len());
    for block in clear.chunks_exact(8) {
        let x = std::array::from_fn(|n| block[n] ^ previous_cipher[n]);
        let encrypted = tea(x, key, false);
        let cipher = std::array::from_fn(|n| encrypted[n] ^ previous_x[n]);
        out.extend(cipher);
        previous_x = x;
        previous_cipher = cipher;
    }
    Ok(out)
}
pub fn decrypt_checkin(cipher: &[u8], key: &[u8; 16], limit: usize) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        (16..=4113).contains(&cipher.len()) && cipher.len().is_multiple_of(8) && limit <= 4096,
        "Invalid relay check-in ciphertext size"
    );
    let mut previous_cipher = [0; 8];
    let mut previous_x = [0; 8];
    let mut clear = Zeroizing::new(Vec::with_capacity(cipher.len()));
    for block in cipher.chunks_exact(8) {
        let c: [u8; 8] = block.try_into()?;
        let x = tea(std::array::from_fn(|n| c[n] ^ previous_x[n]), key, true);
        clear.extend((0..8).map(|n| x[n] ^ previous_cipher[n]));
        previous_x = x;
        previous_cipher = c;
    }
    let start = usize::from(clear[0] & 7) + 3;
    ensure!(
        start + 7 <= clear.len() && clear[clear.len() - 7..] == [0; 7],
        "Invalid relay check-in trailer"
    );
    ensure!(
        clear.len() - start - 7 <= limit,
        "Relay check-in output exceeds limit"
    );
    Ok(Zeroizing::new(clear[start..clear.len() - 7].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tea_and_crc_literal_vectors() {
        assert_eq!(
            tea([0; 8], &[0; 16], false),
            [0xa8, 0x89, 0xf7, 0x98, 0x18, 0x2d, 0x80, 0x83]
        );
        assert_eq!(
            tea(
                [0xa8, 0x89, 0xf7, 0x98, 0x18, 0x2d, 0x80, 0x83],
                &[0; 16],
                true
            ),
            [0; 8]
        );
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
    }
    #[test]
    fn transform_roundtrip_structural_and_output_bounds() {
        for n in [0, 1, 7, 8, 128, 4096] {
            let input = vec![42; n];
            let random = vec![9; (8 - (n + 10) % 8) % 8 + 3];
            let key = [7; 16];
            let cipher = encrypt_checkin(&input, &key, &random).unwrap();
            assert_eq!(&*decrypt_checkin(&cipher, &key, 4096).unwrap(), &input);
            assert!(decrypt_checkin(&cipher, &key, n.saturating_sub(1)).is_err() || n == 0);
            let mut corrupt = cipher;
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(decrypt_checkin(&corrupt, &key, 4096).is_err());
        }
    }
    fn epoch() -> RelayEpoch {
        RelayEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
            socket_generation: 1,
            attempt_id: 1,
        }
    }
    fn selected() -> SelectedRelay {
        SelectedRelay {
            room_id: 7,
            room_key: 9,
            own_member: 0,
            connection_id: 1,
            endpoint: SocketAddr::from((Ipv4Addr::LOCALHOST, 6000)),
            media_key: Zeroizing::new([3; 32]),
            merge_mode: false,
        }
    }
    fn decode_c2s_body(packet: &[u8]) -> Vec<Field> {
        let outer = fields(unframe_outer(packet).unwrap()).unwrap();
        let cipher = data(&outer, 3).unwrap();
        let clear = decrypt_checkin(cipher, &checkin_key(9).unwrap(), 4096).unwrap();
        fields(&clear).unwrap()
    }
    fn reply(command: u64, body: &[u8]) -> Vec<u8> {
        let public = p::message(&[p::uint(1, command), p::uint(2, 3), p::uint(3, 0)]).unwrap();
        let header = p::message(&[p::bytes(1, &public), p::uint(2, 7), p::uint(3, 10)]).unwrap();
        let rand = vec![5; (8 - (body.len() + 10) % 8) % 8 + 3];
        let encrypted = encrypt_checkin(body, &checkin_key(9).unwrap(), &rand).unwrap();
        frame_outer(
            &p::message(&[
                p::bytes(1, &header),
                p::uint(2, encrypted.len() as u64),
                p::bytes(3, &encrypted),
            ])
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn two_turn_setup_never_changes_actual_reply_destination() {
        let selected = selected();
        let mut attempt = TwoRttRelay::new(
            RoomInputs::initial(&selected, 0, 0, 0),
            9,
            selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; attempt.random_bytes_needed().unwrap()];
        attempt.prepare_initial(epoch(), 1, &random).unwrap();
        assert!(attempt.prepare_initial(epoch(), 2, &random).is_err());
        let body = p::message(
            &(1..=9)
                .map(|n| p::uint(n, if n == 6 { 1 } else { 0 }))
                .chain([p::bytes(12, b"192.0.2.1"), p::bytes(13, b"198.51.100.1")])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            attempt
                .receive(&reply(0x1002, &body), selected.endpoint, epoch(), 3)
                .unwrap(),
            RelayProgress::FirstCheckin
        );
        assert_eq!(attempt.room().server_text, "198.51.100.1");
        let random = vec![2; attempt.random_bytes_needed().unwrap()];
        attempt
            .prepare_continuation(epoch(), 4, 1, 1, &random)
            .unwrap();
        let ready = p::message(
            &(1..=7)
                .filter(|n| *n != 4)
                .map(|n| p::uint(n, 0))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            attempt
                .receive(&reply(0x1004, &ready), selected.endpoint, epoch(), 5)
                .unwrap(),
            RelayProgress::Ready
        );
        assert!(attempt.is_ready());
    }

    #[test]
    fn first_checkin_accepts_ipv6_server_and_client_text_and_zeroes_ipv4_numeric() {
        let selected = selected();
        let mut attempt = TwoRttRelay::new(
            RoomInputs::initial(&selected, 0, 0, 0),
            9,
            selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; attempt.random_bytes_needed().unwrap()];
        attempt.prepare_initial(epoch(), 1, &random).unwrap();

        let client_ip_u32 = u32::from_le_bytes([203, 0, 113, 5]);
        let server_ip_u32 = u32::from_le_bytes([198, 51, 100, 9]);
        let body = p::message(&[
            p::uint(1, 0),
            p::uint(2, u64::from(client_ip_u32)),
            p::uint(3, u64::from(server_ip_u32)),
            p::uint(4, 40_664),
            p::uint(5, 0),
            p::uint(6, 1),
            p::uint(7, 0),
            p::uint(8, 2),
            p::uint(9, 1),
            p::bytes(12, b"203.0.113.5"),
            p::bytes(13, b"2001:db8:10:2212:2e::"),
            p::uint(14, 0),
        ])
        .unwrap();
        assert_eq!(
            attempt
                .receive(&reply(0x1002, &body), selected.endpoint, epoch(), 3)
                .unwrap(),
            RelayProgress::FirstCheckin
        );
        assert_eq!(attempt.room().client_ip, client_ip_u32);
        assert_eq!(attempt.room().client_text, "203.0.113.5");
        assert_eq!(attempt.room().client_port, 40_664);
        assert_eq!(attempt.room().server_ip, 0);
        assert_eq!(attempt.room().server_text, "2001:db8:10:2212:2e::");
        assert_eq!(attempt.room().server_port, 0);

        let random = vec![2; attempt.continuation_random_bytes_needed(1, 1).unwrap()];
        let cont = attempt
            .prepare_continuation(epoch(), 4, 1, 1, &random)
            .unwrap();
        let decoded = decode_c2s_body(&cont);
        assert_eq!(number(&decoded, 3, None).unwrap(), u64::from(client_ip_u32));
        assert_eq!(number(&decoded, 4, None).unwrap(), 0);
        assert_eq!(number(&decoded, 5, None).unwrap(), 40_664);
        assert_eq!(number(&decoded, 6, None).unwrap(), 0);
        assert_eq!(data(&decoded, 14).unwrap(), b"203.0.113.5");
        assert_eq!(data(&decoded, 15).unwrap(), b"2001:db8:10:2212:2e::");

        let ready = p::message(
            &(1..=7)
                .filter(|n| *n != 4)
                .map(|n| p::uint(n, 0))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            attempt
                .receive(&reply(0x1004, &ready), selected.endpoint, epoch(), 5)
                .unwrap(),
            RelayProgress::Ready
        );
        assert!(attempt.is_ready());

        // IPv6 clientIpStr with zero numeric clientIp keeps client_ip = 0 and text verbatim.
        let mut v6_client = TwoRttRelay::new(
            RoomInputs::initial(&selected, 0, 0, 0),
            9,
            selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; v6_client.random_bytes_needed().unwrap()];
        v6_client.prepare_initial(epoch(), 1, &random).unwrap();
        let v6_body = p::message(
            &(1..=9)
                .map(|n| p::uint(n, if n == 6 { 1 } else { 0 }))
                .chain([p::bytes(12, b"2001:db8::42"), p::bytes(13, b"2001:db8::1")])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            v6_client
                .receive(&reply(0x1002, &v6_body), selected.endpoint, epoch(), 2)
                .unwrap(),
            RelayProgress::FirstCheckin
        );
        assert_eq!(v6_client.room().client_ip, 0);
        assert_eq!(v6_client.room().client_text, "2001:db8::42");
        assert_eq!(v6_client.room().server_ip, 0);
        assert_eq!(v6_client.room().server_text, "2001:db8::1");

        // Hostnames or oversized strings remain rejected.
        let mut bad = TwoRttRelay::new(
            RoomInputs::initial(&selected, 0, 0, 0),
            9,
            selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; bad.random_bytes_needed().unwrap()];
        bad.prepare_initial(epoch(), 1, &random).unwrap();
        let bad_body = p::message(
            &(1..=9)
                .map(|n| p::uint(n, if n == 6 { 1 } else { 0 }))
                .chain([p::bytes(13, b"relay.example.com")])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(bad
            .receive(&reply(0x1002, &bad_body), selected.endpoint, epoch(), 2)
            .is_err());
    }

    #[test]
    fn strategy_selection_supports_ipv6_mixed_conn_set_and_legacy_udp_addrs() {
        let ep_v6 =
            p::message(&[p::bytes(1, b"2001:db8:10:2212:2e::"), p::uint(2, 16285)]).unwrap();
        let ep_v4 = p::message(&[p::bytes(1, b"198.51.100.7"), p::uint(2, 8080)]).unwrap();
        let conn = p::message(&[p::uint(1, 5), p::bytes(4, &ep_v6), p::bytes(4, &ep_v4)]).unwrap();
        // A secondary connection omitting field 1 (or otherwise invalid) must be skipped,
        // not abort candidate enumeration when earlier connections are valid.
        let secondary_missing_id = p::message(&[p::bytes(4, &ep_v4)]).unwrap();
        let strategy = p::message(&[
            p::bytes(10, &[7; 32]),
            p::bytes(15, &conn),
            p::bytes(15, &secondary_missing_id),
            p::uint(16, 1),
        ])
        .unwrap();

        let candidates = SelectedRelay::strategy_candidates(&strategy).unwrap();
        assert_eq!(
            candidates,
            vec![
                (0, 0, "[2001:db8:10:2212:2e::]:16285".parse().unwrap()),
                (0, 1, "198.51.100.7:8080".parse().unwrap()),
            ]
        );

        let selected_v6 = SelectedRelay::from_strategy(7, 9, 0, &strategy, 0, 0).unwrap();
        assert_eq!(selected_v6.connection_id, 5);
        assert_eq!(
            selected_v6.endpoint,
            "[2001:db8:10:2212:2e::]:16285"
                .parse::<SocketAddr>()
                .unwrap()
        );
        assert!(selected_v6.merge_mode);
        let initial_v6 = RoomInputs::initial(&selected_v6, 0, 0, 0);
        assert_eq!(initial_v6.server_ip, 0);
        assert_eq!(initial_v6.server_text, "2001:db8:10:2212:2e::");
        assert_eq!(initial_v6.server_port, 16285);
        assert!(TwoRttRelay::new(initial_v6, 9, selected_v6.endpoint, epoch(), 100).is_ok());

        let selected_v4 = SelectedRelay::from_strategy(7, 9, 0, &strategy, 0, 1).unwrap();
        assert_eq!(
            selected_v4.endpoint,
            "198.51.100.7:8080".parse::<SocketAddr>().unwrap()
        );

        // Legacy Strategy.udp_addrs (field 1) fallback when conn_set (field 15) is absent.
        let legacy_strategy = p::message(&[
            p::bytes(1, &ep_v6),
            p::bytes(1, &ep_v4),
            p::bytes(10, &[7; 32]),
        ])
        .unwrap();
        assert_eq!(
            SelectedRelay::strategy_candidates(&legacy_strategy).unwrap(),
            vec![
                (0, 0, "[2001:db8:10:2212:2e::]:16285".parse().unwrap()),
                (0, 1, "198.51.100.7:8080".parse().unwrap()),
            ]
        );
        let legacy = SelectedRelay::from_strategy(7, 9, 0, &legacy_strategy, 0, 1).unwrap();
        assert_eq!(legacy.connection_id, 0);
        assert_eq!(legacy.endpoint, "198.51.100.7:8080".parse().unwrap());
    }

    #[test]
    fn merge_mode_supports_one_rtt_legacy_response_and_timeout_fallback() {
        // 1. 1-RTT merged response completes directly on 0x1002 (f14 = 1).
        let mut merged_selected = selected();
        merged_selected.merge_mode = true;
        let mut one_rtt = TwoRttRelay::new(
            RoomInputs::initial(&merged_selected, 0, 0, 0),
            9,
            merged_selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; one_rtt.random_bytes_needed().unwrap()];
        let initial = one_rtt.prepare_initial(epoch(), 1, &random).unwrap();
        assert_eq!(number(&decode_c2s_body(&initial), 21, Some(0)).unwrap(), 1);
        let merged_reply = p::message(
            &(1..=9)
                .map(|n| p::uint(n, if n == 6 { 1 } else { 0 }))
                .chain([p::bytes(13, b"2001:db8:10:2212:2e::"), p::uint(14, 1)])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            one_rtt
                .receive(
                    &reply(0x1002, &merged_reply),
                    merged_selected.endpoint,
                    epoch(),
                    2
                )
                .unwrap(),
            RelayProgress::Ready
        );
        assert!(one_rtt.is_ready());
        assert_eq!(one_rtt.room().server_text, "2001:db8:10:2212:2e::");

        // 2. Legacy 0x1002 (f14 = 0) clears merge_mode and finishes via 2-RTT continuation.
        let mut legacy = TwoRttRelay::new(
            RoomInputs::initial(&merged_selected, 0, 0, 0),
            9,
            merged_selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; legacy.random_bytes_needed().unwrap()];
        legacy.prepare_initial(epoch(), 1, &random).unwrap();
        let unmerged_reply = p::message(
            &(1..=9)
                .map(|n| p::uint(n, if n == 6 { 1 } else { 0 }))
                .chain([p::bytes(13, b"198.51.100.1"), p::uint(14, 0)])
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(
            legacy
                .receive(
                    &reply(0x1002, &unmerged_reply),
                    merged_selected.endpoint,
                    epoch(),
                    2
                )
                .unwrap(),
            RelayProgress::FirstCheckin
        );
        assert!(!legacy.room().merge_mode);
        let random = vec![2; legacy.continuation_random_bytes_needed(2, 2).unwrap()];
        let cont = legacy
            .prepare_continuation(epoch(), 3, 2, 2, &random)
            .unwrap();
        assert_eq!(number(&decode_c2s_body(&cont), 21, Some(0)).unwrap(), 0);

        // 3. Timeout fallback clears merge_mode before retransmit and still accepts late 1-RTT.
        let mut fallback = TwoRttRelay::new(
            RoomInputs::initial(&merged_selected, 0, 0, 0),
            9,
            merged_selected.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; fallback.random_bytes_needed().unwrap()];
        fallback.prepare_initial(epoch(), 1, &random).unwrap();
        let random = vec![2; fallback.merge_fallback_random_bytes_needed(2, 2).unwrap()];
        let retried = fallback
            .prepare_merge_fallback(epoch(), 2, 2, 2, &random)
            .unwrap();
        assert!(!fallback.room().merge_mode);
        assert_eq!(number(&decode_c2s_body(&retried), 21, Some(0)).unwrap(), 0);
        assert_eq!(
            fallback
                .receive(
                    &reply(0x1002, &merged_reply),
                    merged_selected.endpoint,
                    epoch(),
                    3
                )
                .unwrap(),
            RelayProgress::Ready
        );
        assert!(fallback.is_ready());

        // 4. Non-merge attempt (Strategy without merge_mode bit) rejects f14 = 1.
        let non_merge = selected();
        assert!(!non_merge.merge_mode);
        let mut standard = TwoRttRelay::new(
            RoomInputs::initial(&non_merge, 0, 0, 0),
            9,
            non_merge.endpoint,
            epoch(),
            100,
        )
        .unwrap();
        let random = vec![1; standard.random_bytes_needed().unwrap()];
        standard.prepare_initial(epoch(), 1, &random).unwrap();
        assert!(standard
            .receive(
                &reply(0x1002, &merged_reply),
                non_merge.endpoint,
                epoch(),
                2
            )
            .is_err());
    }

    #[test]
    fn relay_target_parses_ringing_strategy_without_media_key() {
        let address = p::message(&[p::bytes(1, b"127.0.0.1"), p::uint(2, 6000)]).unwrap();
        let strategy = p::message(&[
            p::bytes(
                15,
                &p::message(&[p::uint(1, 2), p::bytes(4, &address)]).unwrap(),
            ),
            p::uint(16, 1),
        ])
        .unwrap();
        let target = RelayTarget::from_strategy(7, 9, 3, &strategy, 0, 0).unwrap();
        assert_eq!(target.room_id, 7);
        assert_eq!(target.room_key, 9);
        assert_eq!(target.own_member, 3);
        assert_eq!(target.connection_id, 2);
        assert_eq!(target.endpoint, "127.0.0.1:6000".parse().unwrap());
        assert!(target.merge_mode);
        // Full SelectedRelay still requires the 32-byte media key.
        assert!(SelectedRelay::from_strategy(7, 9, 3, &strategy, 0, 0).is_err());
    }
}
