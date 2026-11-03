//! Selected IPv4 personal relay setup: one initial check-in, at most one learned
//! continuation and explicit Ready. CRC/TEA structural checks are not media AEAD.
//! The host owns sockets, current account/key generations, time and randomness.

use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};
use std::net::{Ipv4Addr, SocketAddrV4};
use zeroize::Zeroizing;

const MAX_PACKET: usize = 8197;
const MAX_BODY: usize = 8192;

/// An endpoint selected from an admitted Strategy. No DNS or fallback is used.
/// Room key and media material intentionally have no Debug representation.
pub struct SelectedRelay {
    pub room_id: u64,
    pub room_key: u64,
    pub own_member: u16,
    pub connection_id: u16,
    pub endpoint: SocketAddrV4,
    pub media_key: Zeroizing<[u8; 32]>,
    pub merge_mode: bool,
}
impl SelectedRelay {
    /// Strategy field15 contains modern connection records. Indices must be
    /// chosen by the caller from the current admitted Strategy, not by retries.
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
        let connection = fields(
            connections
                .get(connection_index)
                .ok_or_else(|| anyhow::anyhow!("Relay connection selection unavailable"))?,
        )?;
        let connection_id = u16::try_from(number(&connection, 1, None)?)?;
        if let Some(nic) = p::optional_data(&connection, 2)? {
            ensure!(
                nic.len() <= 128 && !nic.contains(&0),
                "Invalid relay NIC identity"
            );
            std::str::from_utf8(nic)?;
        }
        u16::try_from(number(&connection, 3, Some(0))?)?;
        let endpoints = repeated(&connection, 4, 32)?;
        let endpoint = fields(
            endpoints
                .get(udp_index)
                .ok_or_else(|| anyhow::anyhow!("Relay UDP selection unavailable"))?,
        )?;
        let address = ipv4(data(&endpoint, 1)?, false)?;
        let port = u16::try_from(number(&endpoint, 2, None)?)?;
        ensure!(port != 0, "Invalid relay endpoint port");
        let key = data(&strategy, 10)?;
        ensure!(key.len() == 32, "Unsupported relay media key size");
        Ok(Self {
            room_id,
            room_key,
            own_member,
            connection_id,
            endpoint: SocketAddrV4::new(address, port),
            media_key: Zeroizing::new(key.try_into()?),
            merge_mode: number(&strategy, 16, Some(0))? & 1 != 0,
        })
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
        Self {
            room_id: selected.room_id,
            own_member: selected.own_member,
            connection_id: selected.connection_id,
            client_ip: 0,
            client_text: String::new(),
            client_port: 0,
            server_ip: u32::from_le_bytes(selected.endpoint.ip().octets()),
            server_text: selected.endpoint.ip().to_string(),
            server_port: selected.endpoint.port(),
            checkin_sequence: 1,
            network_type,
            arq_flag: 0,
            has_accept: true,
            data_sequence: 0,
            mix_timestamp: 0,
            mix_sequence: 0,
            merge_mode: selected.merge_mode,
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
        ensure!(!self.merge_mode, "Merged relay check-in unsupported");
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
    endpoint: SocketAddrV4,
    epoch: RelayEpoch,
    deadline_ms: u64,
    last_ms: Option<u64>,
    phase: Phase,
}
impl TwoRttRelay {
    pub fn new(
        room: RoomInputs,
        room_key: u64,
        endpoint: SocketAddrV4,
        epoch: RelayEpoch,
        deadline_ms: u64,
    ) -> Result<Self> {
        room.validate()?;
        epoch.validate()?;
        ensure!(
            room_key != 0 && endpoint.port() != 0,
            "Invalid relay key or endpoint"
        );
        ensure!(
            room.server_text == endpoint.ip().to_string()
                && room.server_ip == u32::from_le_bytes(endpoint.ip().octets())
                && room.server_port == endpoint.port(),
            "Relay request endpoint mismatch"
        );
        Ok(Self {
            room,
            key: Zeroizing::new(room_key),
            endpoint,
            epoch,
            deadline_ms,
            last_ms: None,
            phase: Phase::New,
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

    fn live(&mut self, epoch: RelayEpoch, now_ms: u64) -> Result<()> {
        if epoch.validate().is_err()
            || epoch != self.epoch
            || now_ms >= self.deadline_ms
            || self.last_ms.is_some_and(|last| now_ms < last)
            || self.phase == Phase::Closed
        {
            self.close();
            bail!("Relay generation, clock or deadline changed");
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
    pub fn receive(
        &mut self,
        packet: &[u8],
        source: SocketAddrV4,
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
    fn receive_checked(&mut self, packet: &[u8], source: SocketAddrV4) -> Result<RelayProgress> {
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
        ensure!(
            number(&body, 6, None)? == u64::from(self.room.checkin_sequence)
                && number(&body, 14, Some(0))? == 0,
            "Relay check-in sequence or merge mismatch"
        );
        let client = p::optional_data(&body, 12)?.unwrap_or_default();
        let server = p::optional_data(&body, 13)?.unwrap_or_default();
        let client_address = ipv4(client, true)?;
        let server_address = ipv4(server, true)?;
        let client_numeric = number(&body, 2, None)? as u32;
        self.room.client_ip = if client_numeric == 0 {
            u32::from_le_bytes(client_address.octets())
        } else {
            client_numeric
        };
        self.room.client_text = std::str::from_utf8(client)?.to_owned();
        self.room.server_text = std::str::from_utf8(server)?.to_owned();
        self.room.server_ip = u32::from_le_bytes(server_address.octets());
        self.room.client_port = number(&body, 4, None)? as u16;
        self.room.server_port = number(&body, 5, None)? as u16;
        self.phase = Phase::ContinuationPending;
        Ok(RelayProgress::FirstCheckin)
    }
    pub fn close(&mut self) {
        self.key = Zeroizing::new(0);
        self.phase = Phase::Closed;
    }
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
fn ipv4(raw: &[u8], allow_empty: bool) -> Result<Ipv4Addr> {
    if raw.is_empty() && allow_empty {
        return Ok(Ipv4Addr::UNSPECIFIED);
    }
    ensure!(raw.len() <= 15, "Relay IPv4 text too large");
    let text = std::str::from_utf8(raw)?;
    let ip: Ipv4Addr = text.parse()?;
    ensure!(ip.to_string() == text, "Noncanonical relay IPv4 address");
    Ok(ip)
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
            endpoint: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 6000),
            media_key: Zeroizing::new([3; 32]),
            merge_mode: false,
        }
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
}
