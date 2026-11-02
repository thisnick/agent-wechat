//! Fresh pinned MMTLS handshake and authenticated shortlink exchange.
use crate::crypto::*;
use anyhow::{ensure, Result};
use openssl::{memcmp, nid::Nid};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

pub const VERSION: u16 = 0xf104;
pub struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.bytes.len() - self.offset, "Truncated structure");
        let b = &self.bytes[self.offset..self.offset + n];
        self.offset += n;
        Ok(b)
    }
    pub fn uint(&mut self, n: usize) -> Result<u64> {
        ensure!(n <= 8, "Integer too wide");
        Ok(self.take(n)?.iter().fold(0, |a, &b| (a << 8) | b as u64))
    }
    pub fn sized(&mut self, width: usize) -> Result<&'a [u8]> {
        let n = usize::try_from(self.uint(width)?)?;
        self.take(n)
    }
    pub fn end(&self) -> Result<()> {
        ensure!(self.offset == self.bytes.len(), "Trailing structure bytes");
        Ok(())
    }
}
pub fn section(b: &[u8]) -> Vec<u8> {
    [(b.len() as u32).to_be_bytes().as_slice(), b].concat()
}
fn sized(b: &[u8]) -> Result<Vec<u8>> {
    ensure!(b.len() <= u16::MAX as usize, "Section too large");
    Ok([(b.len() as u16).to_be_bytes().as_slice(), b].concat())
}
#[derive(Clone)]
pub struct Record {
    pub header: [u8; 5],
    pub body: Vec<u8>,
}
pub fn frame(kind: u8, body: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        (0x15..=0x19).contains(&kind) && body.len() <= 65535,
        "Invalid MMTLS record"
    );
    Ok([
        vec![kind],
        VERSION.to_be_bytes().to_vec(),
        (body.len() as u16).to_be_bytes().to_vec(),
        body.to_vec(),
    ]
    .concat())
}
pub fn nonce(iv: &[u8], sequence: u64) -> Result<[u8; 12]> {
    ensure!(iv.len() == 12, "Invalid IV");
    let mut b: [u8; 12] = iv.try_into()?;
    for (i, v) in sequence.to_be_bytes().iter().enumerate() {
        b[i + 4] ^= v;
    }
    Ok(b)
}
pub fn seal(kind: u8, plain: &[u8], key: &[u8], iv: &[u8], sequence: u64) -> Result<Vec<u8>> {
    ensure!(plain.len() <= 65519, "MMTLS plaintext too large");
    let header = frame(kind, &vec![0; plain.len() + 16])?[..5].to_vec();
    let aad = [sequence.to_be_bytes().as_slice(), &header].concat();
    let (cipher, tag) = gcm_seal(key, &nonce(iv, sequence)?, &aad, plain)?;
    Ok([header, cipher, tag.to_vec()].concat())
}
pub fn open(record: &Record, key: &[u8], iv: &[u8], sequence: u64) -> Result<Vec<u8>> {
    ensure!(record.body.len() >= 16, "Missing authentication tag");
    let n = record.body.len() - 16;
    gcm_open(
        key,
        &nonce(iv, sequence)?,
        &[sequence.to_be_bytes().as_slice(), &record.header].concat(),
        &record.body[..n],
        &record.body[n..],
    )
}
struct Traffic {
    client_key: Vec<u8>,
    server_key: Vec<u8>,
    client_iv: Vec<u8>,
    server_iv: Vec<u8>,
}
impl Drop for Traffic {
    fn drop(&mut self) {
        self.client_key.zeroize();
        self.server_key.zeroize();
        self.client_iv.zeroize();
        self.server_iv.zeroize();
    }
}
fn traffic(secret: &[u8], label: &str, transcript: &[u8]) -> Result<Traffic> {
    let mut b = expand(
        secret,
        &[label.as_bytes(), &sha256(transcript)].concat(),
        56,
    )?;
    let t = Traffic {
        client_key: b[..16].to_vec(),
        server_key: b[16..32].to_vec(),
        client_iv: b[32..44].to_vec(),
        server_iv: b[44..].to_vec(),
    };
    b.zeroize();
    Ok(t)
}
fn signed_payload(bytes: &[u8], kind: u8) -> Result<Vec<u8>> {
    let mut top = Cursor::new(bytes);
    let mut body = Cursor::new(top.sized(4)?);
    top.end()?;
    ensure!(body.uint(1)? == kind as u64, "Unexpected handshake message");
    let value = body.sized(2)?.to_vec();
    body.end()?;
    Ok(value)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub kind: u8,
    pub lifetime: u32,
    pub reserved: u32,
    pub nonce: Vec<u8>,
    pub opaque: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ShortSession {
    pub access_psk: Vec<u8>,
    pub ticket: Ticket,
}
impl Drop for ShortSession {
    fn drop(&mut self) {
        self.access_psk.zeroize();
        self.ticket.opaque.zeroize();
        self.ticket.nonce.zeroize();
    }
}
pub struct Session {
    socket: TcpStream,
    deadline: Instant,
    received: usize,
    receive_budget: usize,
    pub short: ShortSession,
    pub certificate_region: u32,
    pub certificate_version: u32,
    traffic: Traffic,
    outgoing: u64,
    incoming: u64,
    record_pending: Vec<u8>,
    application_pending: LonglinkDecoder,
}

fn host_valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.is_ascii()
        && name.split('.').all(|s| {
            !s.is_empty()
                && s.len() <= 63
                && s.bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && !s.starts_with('-')
                && !s.ends_with('-')
        })
        && (name.ends_with(".wechat.com") || name.ends_with(".weixin.qq.com"))
}
pub fn validate_host(name: &str) -> Result<()> {
    ensure!(host_valid(name), "Unsupported native server host");
    Ok(())
}
fn read_exact_deadline(
    socket: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
) -> Result<()> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "MMTLS deadline exceeded");
        socket.set_read_timeout(Some(remaining))?;
        match socket.read(bytes) {
            Ok(0) => anyhow::bail!("MMTLS connection closed"),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn read_record(
    socket: &mut TcpStream,
    deadline: Instant,
    received: &mut usize,
    budget: usize,
) -> Result<Record> {
    let mut h = [0; 5];
    read_exact_deadline(socket, &mut h, deadline)?;
    ensure!(
        (0x15..=0x19).contains(&h[0]) && u16::from_be_bytes([h[1], h[2]]) == VERSION,
        "Unexpected MMTLS header"
    );
    let n = u16::from_be_bytes([h[3], h[4]]) as usize;
    *received = received
        .checked_add(n + 5)
        .ok_or_else(|| anyhow::anyhow!("Receive overflow"))?;
    ensure!(*received <= budget, "MMTLS receive budget exceeded");
    let mut body = vec![0; n];
    read_exact_deadline(socket, &mut body, deadline)?;
    ensure!(h[0] != 0x15, "Server returned MMTLS alert");
    Ok(Record { header: h, body })
}

impl Session {
    pub fn connect(host: &str, timeout: Duration, receive_budget: usize) -> Result<Self> {
        validate_host(host)?;
        ensure!(
            (1024..=crate::MAX_MESSAGE_BYTES).contains(&receive_budget),
            "Invalid receive budget"
        );
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(120),
            "Invalid MMTLS timeout"
        );
        let deadline = Instant::now() + timeout;
        // Resolver and connect are constrained by the caller's blocking worker. Socket
        // read/write deadlines bound every subsequent protocol operation.
        use std::net::ToSocketAddrs;
        let addresses = (host, 80).to_socket_addrs()?;
        let mut socket = None;
        for addr in addresses {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Ok(s) = TcpStream::connect_timeout(&addr, remaining) {
                socket = Some(s);
                break;
            }
        }
        let mut socket = socket.ok_or_else(|| anyhow::anyhow!("Native connection failed"))?;
        socket.set_write_timeout(Some(timeout))?;
        let keys = [
            Ecdh::generate(Nid::X9_62_PRIME256V1)?,
            Ecdh::generate(Nid::X9_62_PRIME256V1)?,
        ];
        let mut shares = vec![];
        for (i, k) in keys.iter().enumerate() {
            let point = k.public()?;
            shares.extend(section(
                &[((i + 5) as u32).to_be_bytes().to_vec(), sized(&point)?].concat(),
            ));
        }
        let mut extra = vec![0; 4];
        extra.push(2);
        extra.extend(5u32.to_be_bytes());
        extra.extend(1000u32.to_be_bytes());
        let extension = section(&[0x10u16.to_be_bytes().to_vec(), vec![2], shares, extra].concat());
        let hello = section(
            &[
                vec![1],
                VERSION.to_le_bytes().to_vec(),
                vec![1],
                0xc02bu16.to_be_bytes().to_vec(),
                random::<32>()?.to_vec(),
                crate::wire::unix_time()?.to_be_bytes().to_vec(),
                section(&[vec![1], extension].concat()),
            ]
            .concat(),
        );
        socket.write_all(&frame(0x16, &hello)?)?;
        let mut received = 0;
        let server_record = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        ensure!(server_record.header[0] == 0x16, "Expected handshake");
        let server = parse_server_hello(&server_record.body)?;
        let index = server
            .key_id
            .checked_sub(5)
            .ok_or_else(|| anyhow::anyhow!("Unoffered key"))? as usize;
        ensure!(index < 2, "Unoffered key");
        let mut x = keys[index].derive(&server.point)?;
        let first = x.iter().position(|v| *v != 0).unwrap_or(x.len() - 1);
        let mut secret = sha256(&x[first..]);
        x.zeroize();
        let mut transcript = [hello, server_record.body].concat();
        let handshake = traffic(&secret, "handshake key expansion", &transcript)?;
        let r = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        ensure!(r.header[0] == 0x16, "Expected signature record");
        let signature_body = open(&r, &handshake.server_key, &handshake.server_iv, 1)?;
        let signature = signed_payload(&signature_body, 0x0f)?;
        let pins: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/mmtls-public-keys.json"))?;
        let pin = pins["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["version"].as_u64() == Some(server.version as u64))
            .ok_or_else(|| anyhow::anyhow!("Unsupported MMTLS certificate version"))?;
        ensure!([5, 1000].contains(&server.version), "Unoffered MMTLS pin");
        verify_signature(
            pin["ecdsaPEM"].as_str().unwrap(),
            &sha256(&transcript),
            &signature,
        )?;
        transcript.extend(signature_body);
        let r = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        ensure!(r.header[0] == 0x16, "Expected ticket record");
        let ticket_body = open(&r, &handshake.server_key, &handshake.server_iv, 2)?;
        let tickets = parse_tickets(&ticket_body)?;
        let access_psk = expand(
            &secret,
            &[b"PSK_ACCESS".as_slice(), &sha256(&transcript)].concat(),
            32,
        )?;
        transcript.extend(ticket_body);
        let r = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        ensure!(r.header[0] == 0x16, "Expected Finished");
        let finish = signed_payload(
            &open(&r, &handshake.server_key, &handshake.server_iv, 3)?,
            0x14,
        )?;
        let expected = hmac(
            &expand(&secret, b"server finished", 32)?,
            &sha256(&transcript),
        )?;
        ensure!(
            finish.len() == expected.len() && memcmp::eq(&finish, &expected),
            "Server Finished verification failed"
        );
        let client_finish = hmac(
            &expand(&secret, b"client finished", 32)?,
            &sha256(&transcript),
        )?;
        socket.write_all(&seal(
            0x16,
            &section(&[vec![0x14], sized(&client_finish)?].concat()),
            &handshake.client_key,
            &handshake.client_iv,
            1,
        )?)?;
        let mut expanded = expand(
            &secret,
            &[b"expanded secret".as_slice(), &sha256(&transcript)].concat(),
            32,
        )?;
        let application = traffic(&expanded, "application data key expansion", &transcript)?;
        secret.zeroize();
        expanded.zeroize();
        let noop = pack_longlink(6, u32::MAX, &[])?;
        socket.write_all(&seal(
            0x17,
            &noop,
            &application.client_key,
            &application.client_iv,
            2,
        )?)?;
        let r = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        ensure!(r.header[0] == 0x17, "Expected NOOP reply");
        let p = unpack_longlink(&open(
            &r,
            &application.server_key,
            &application.server_iv,
            4,
        )?)?;
        ensure!(
            p.command == 0x3b9aca06 && p.sequence == u32::MAX && p.payload.is_empty(),
            "Unexpected NOOP reply"
        );
        Ok(Self {
            socket,
            deadline,
            received,
            receive_budget,
            short: ShortSession {
                access_psk,
                ticket: tickets[0].clone(),
            },
            certificate_region: server.region,
            certificate_version: server.version,
            traffic: application,
            outgoing: 3,
            incoming: 5,
            record_pending: Vec::new(),
            application_pending: LonglinkDecoder {
                pending: Vec::new(),
                max_payload: receive_budget,
            },
        })
    }
    pub fn send_application(&mut self, command: u32, sequence: u32, payload: &[u8]) -> Result<()> {
        let bytes = seal(
            0x17,
            &pack_longlink(command, sequence, payload)?,
            &self.traffic.client_key,
            &self.traffic.client_iv,
            self.outgoing,
        )?;
        self.socket.write_all(&bytes)?;
        self.outgoing = self
            .outgoing
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Sequence exhausted"))?;
        Ok(())
    }
    pub fn receive_application(&mut self) -> Result<Longlink> {
        let r = read_record(
            &mut self.socket,
            self.deadline,
            &mut self.received,
            self.receive_budget,
        )?;
        ensure!(r.header[0] == 0x17, "Expected application record");
        let plain = open(
            &r,
            &self.traffic.server_key,
            &self.traffic.server_iv,
            self.incoming,
        )?;
        self.incoming = self
            .incoming
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Sequence exhausted"))?;
        unpack_longlink(&plain)
    }

    /// A bounded idle wait on an established connection. Partial encrypted
    /// records and application packets survive a timeout; MMTLS record
    /// boundaries need not coincide with longlink packet boundaries.
    pub fn poll_application(&mut self, timeout: Duration) -> Result<Option<Longlink>> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(120),
            "Invalid longlink poll timeout"
        );
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(packet) = self.application_pending.next_packet()? {
                return Ok(Some(packet));
            }
            if self.record_pending.len() >= 5 {
                let h: [u8; 5] = self.record_pending[..5].try_into()?;
                ensure!(
                    (0x15..=0x19).contains(&h[0]) && u16::from_be_bytes([h[1], h[2]]) == VERSION,
                    "Unexpected MMTLS header"
                );
                let n = u16::from_be_bytes([h[3], h[4]]) as usize;
                if self.record_pending.len() >= n + 5 {
                    ensure!(h[0] == 0x17, "Unexpected persistent MMTLS record");
                    let record = Record {
                        header: h,
                        body: self.record_pending[5..n + 5].to_vec(),
                    };
                    let plain = open(
                        &record,
                        &self.traffic.server_key,
                        &self.traffic.server_iv,
                        self.incoming,
                    )?;
                    self.incoming = self
                        .incoming
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("Sequence exhausted"))?;
                    self.record_pending.drain(..n + 5);
                    self.application_pending.feed(&plain)?;
                    continue;
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.socket.set_read_timeout(Some(remaining))?;
            let mut bytes = [0u8; 65540];
            match self.socket.read(&mut bytes) {
                Ok(0) => anyhow::bail!("MMTLS connection closed"),
                Ok(n) => {
                    // At most one incomplete record precedes a socket read.
                    ensure!(
                        self.record_pending.len() + n <= 131080,
                        "MMTLS pending record limit"
                    );
                    self.record_pending.extend_from_slice(&bytes[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}
struct ServerHello {
    key_id: u32,
    point: Vec<u8>,
    region: u32,
    version: u32,
}
fn parse_server_hello(bytes: &[u8]) -> Result<ServerHello> {
    let mut top = Cursor::new(bytes);
    let mut b = Cursor::new(top.sized(4)?);
    top.end()?;
    ensure!(
        b.uint(1)? == 2 && b.take(2)? == VERSION.to_le_bytes() && b.uint(2)? == 0xc02b,
        "Unsupported ServerHello"
    );
    b.take(32)?;
    let mut e = Cursor::new(b.sized(4)?);
    b.end()?;
    let count = e.uint(1)?;
    let mut seen = std::collections::HashSet::new();
    let mut share = None;
    let mut cert = None;
    for _ in 0..count {
        let mut x = Cursor::new(e.sized(4)?);
        let kind = x.uint(2)?;
        ensure!(seen.insert(kind), "Duplicate extension");
        match kind {
            0x11 => {
                let id = x.uint(4)? as u32;
                let point = x.sized(2)?.to_vec();
                ensure!(point.len() == 65 && point[0] == 4, "Invalid ECDH point");
                x.end()?;
                share = Some((id, point));
            }
            0x13 => {
                let region = x.uint(4)? as u32;
                let version = x.uint(4)? as u32;
                x.end()?;
                cert = Some((region, version));
            }
            _ => {}
        }
    }
    e.end()?;
    let (key_id, point) = share.ok_or_else(|| anyhow::anyhow!("Missing ECDH selection"))?;
    let (region, version) = cert.ok_or_else(|| anyhow::anyhow!("Missing certificate selection"))?;
    Ok(ServerHello {
        key_id,
        point,
        region,
        version,
    })
}
fn parse_tickets(input: &[u8]) -> Result<Vec<Ticket>> {
    let mut top = Cursor::new(input);
    let mut b = Cursor::new(top.sized(4)?);
    top.end()?;
    ensure!(b.uint(1)? == 4, "Expected tickets");
    let count = b.uint(1)?;
    ensure!((1..=4).contains(&count), "Invalid ticket count");
    let mut out = vec![];
    for _ in 0..count {
        let mut t = Cursor::new(b.sized(4)?);
        let kind = t.uint(1)? as u8;
        let lifetime = t.uint(4)? as u32;
        t.sized(2)?;
        let reserved = t.uint(4)? as u32;
        let nonce = t.sized(2)?.to_vec();
        let opaque = t.sized(2)?.to_vec();
        t.end()?;
        ensure!(
            nonce.len() == 12 && !opaque.is_empty(),
            "Invalid shortlink ticket"
        );
        out.push(Ticket {
            kind,
            lifetime,
            reserved,
            nonce,
            opaque,
        });
    }
    b.end()?;
    Ok(out)
}
pub struct Longlink {
    pub command: u32,
    pub sequence: u32,
    pub payload: Vec<u8>,
}
struct LonglinkDecoder {
    pending: Vec<u8>,
    max_payload: usize,
}
impl Default for LonglinkDecoder {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            max_payload: crate::MAX_MESSAGE_BYTES,
        }
    }
}
impl LonglinkDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.pending.len().saturating_add(bytes.len()) <= self.max_payload + 65540,
            "Longlink pending packet limit"
        );
        self.pending.extend_from_slice(bytes);
        Ok(())
    }
    fn next_packet(&mut self) -> Result<Option<Longlink>> {
        if self.pending.len() < 4 {
            return Ok(None);
        }
        let n = u32::from_be_bytes(self.pending[..4].try_into()?) as usize;
        ensure!(
            (16..=self.max_payload + 16).contains(&n),
            "Invalid longlink packet length"
        );
        if self.pending.len() >= 8 {
            ensure!(
                self.pending[4..8] == [0, 16, 0, 1],
                "Invalid longlink envelope"
            );
        }
        if self.pending.len() < n {
            return Ok(None);
        }
        let packet = unpack_longlink(&self.pending[..n])?;
        self.pending.drain(..n);
        Ok(Some(packet))
    }
}
pub fn pack_longlink(command: u32, sequence: u32, payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(payload.len() <= 65503, "Longlink body too large");
    Ok([
        ((16 + payload.len()) as u32).to_be_bytes().to_vec(),
        0x10u16.to_be_bytes().to_vec(),
        1u16.to_be_bytes().to_vec(),
        command.to_be_bytes().to_vec(),
        sequence.to_be_bytes().to_vec(),
        payload.to_vec(),
    ]
    .concat())
}
pub fn unpack_longlink(input: &[u8]) -> Result<Longlink> {
    let mut c = Cursor::new(input);
    ensure!(
        c.uint(4)? == input.len() as u64 && c.uint(2)? == 0x10 && c.uint(2)? == 1,
        "Invalid longlink envelope"
    );
    let command = c.uint(4)? as u32;
    let sequence = c.uint(4)? as u32;
    let payload = c.take(input.len() - 16)?.to_vec();
    Ok(Longlink {
        command,
        sequence,
        payload,
    })
}

/// A route rejected locally before any short-link request bytes are submitted.
#[derive(Debug)]
pub(crate) struct UnsupportedNativeRoute;
impl std::fmt::Display for UnsupportedNativeRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Unsupported native route")
    }
}
impl std::error::Error for UnsupportedNativeRoute {}
pub(crate) fn validate_short_route(path: &str) -> Result<()> {
    let ordinary = path.starts_with("/cgi-bin/micromsg-bin/")
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'/' || b == b'-');
    if ordinary || path == "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth" {
        Ok(())
    } else {
        Err(UnsupportedNativeRoute.into())
    }
}

pub fn short_exchange(
    session: &ShortSession,
    host: &str,
    path: &str,
    payload: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    validate_host(host)?;
    validate_short_route(path)?;
    ensure!(
        payload.len() <= crate::MAX_MESSAGE_BYTES,
        "Payload too large"
    );
    let (hello, extensions) = early_hello(&session.ticket)?;
    let keys = expand(
        &session.access_psk,
        &[b"early data key expansion".as_slice(), &sha256(&hello)].concat(),
        28,
    )?;
    let route = section(
        &[
            sized(path.as_bytes())?,
            sized(host.as_bytes())?,
            section(payload),
        ]
        .concat(),
    );
    let wire = [
        frame(0x19, &hello)?,
        seal(0x19, &extensions, &keys[..16], &keys[16..], 1)?,
        seal(0x17, &route, &keys[..16], &keys[16..], 2)?,
        seal(0x15, &section(&[0, 1, 1]), &keys[..16], &keys[16..], 3)?,
    ]
    .concat();
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()?;
    let mut response = client
        .post(format!(
            "http://{host}/mmtls/{}",
            hex::encode(random::<4>()?)
        ))
        .header("Content-Type", "application/octet-stream")
        .header("User-Agent", "MicroMessenger Client")
        .header("Upgrade", "mmtls")
        .header("Accept", "*/*")
        .header("Cache-Control", "no-cache")
        .header("Connection", "close")
        .body(wire)
        .send()?;
    ensure!(
        response.status() == 200,
        "Native shortlink HTTP status {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((crate::MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= crate::MAX_MESSAGE_BYTES,
        "Shortlink receive budget exceeded"
    );
    decode_short_reply(&bytes, session, &hello, &extensions)
}
fn early_hello(ticket: &Ticket) -> Result<(Vec<u8>, Vec<u8>)> {
    ensure!(
        ticket.nonce.len() == 12 && !ticket.opaque.is_empty(),
        "Invalid short ticket"
    );
    let now = crate::wire::unix_time()?;
    let encoded = [
        vec![ticket.kind],
        ticket.lifetime.to_be_bytes().to_vec(),
        sized(&[])?,
        ticket.reserved.to_be_bytes().to_vec(),
        sized(&ticket.nonce)?,
        sized(&ticket.opaque)?,
    ]
    .concat();
    let ext = section(&[0x0fu16.to_be_bytes().to_vec(), vec![1], section(&encoded)].concat());
    let hello = section(
        &[
            vec![1],
            VERSION.to_le_bytes().to_vec(),
            vec![1],
            0xa8u16.to_be_bytes().to_vec(),
            random::<32>()?.to_vec(),
            now.to_be_bytes().to_vec(),
            section(&[vec![1], ext].concat()),
        ]
        .concat(),
    );
    let extensions = section(
        &[
            vec![8],
            section(
                &[
                    vec![1],
                    section(&[0x12u16.to_be_bytes().to_vec(), now.to_be_bytes().to_vec()].concat()),
                ]
                .concat(),
            ),
        ]
        .concat(),
    );
    Ok((hello, extensions))
}
fn decode_short_reply(
    input: &[u8],
    session: &ShortSession,
    hello: &[u8],
    extensions: &[u8],
) -> Result<Vec<u8>> {
    let mut c = Cursor::new(input);
    let mut records = vec![];
    while c.offset < input.len() {
        let h: [u8; 5] = c.take(5)?.try_into()?;
        ensure!(
            (0x15..=0x19).contains(&h[0]) && u16::from_be_bytes([h[1], h[2]]) == VERSION,
            "Invalid shortlink header"
        );
        records.push(Record {
            header: h,
            body: c.take(u16::from_be_bytes([h[3], h[4]]) as usize)?.to_vec(),
        });
    }
    ensure!(
        records.len() >= 4
            && records[0].header[0] == 0x16
            && records[1].header[0] == 0x16
            && records.last().unwrap().header[0] == 0x15
            && records[2..records.len() - 1]
                .iter()
                .all(|r| r.header[0] == 0x17),
        "Invalid shortlink sequence"
    );
    let server = &records[0];
    let mut top = Cursor::new(&server.body);
    let mut b = Cursor::new(top.sized(4)?);
    top.end()?;
    ensure!(
        b.uint(1)? == 2 && b.take(2)? == VERSION.to_le_bytes() && b.uint(2)? == 0xa8,
        "Unsupported shortlink selection"
    );
    b.take(32)?;
    let mut e = Cursor::new(b.sized(4)?);
    b.end()?;
    ensure!(e.uint(1)? == 0, "Unsupported shortlink extensions");
    e.end()?;
    let transcript = [hello, extensions, &server.body].concat();
    let keys = expand(
        &session.access_psk,
        &[b"handshake key expansion".as_slice(), &sha256(&transcript)].concat(),
        28,
    )?;
    let finish = signed_payload(&open(&records[1], &keys[..16], &keys[16..], 1)?, 0x14)?;
    let expected = hmac(
        &expand(&session.access_psk, b"server finished", 32)?,
        &sha256(&transcript),
    )?;
    ensure!(
        finish.len() == expected.len() && memcmp::eq(&finish, &expected),
        "Shortlink Finished failed"
    );
    let mut plain = vec![];
    for (i, r) in records[2..records.len() - 1].iter().enumerate() {
        plain.extend(open(r, &keys[..16], &keys[16..], i as u64 + 2)?);
        ensure!(
            plain.len() <= crate::MAX_MESSAGE_BYTES,
            "Shortlink plaintext too large"
        );
    }
    ensure!(
        open(
            records.last().unwrap(),
            &keys[..16],
            &keys[16..],
            records.len() as u64 - 1
        )? == section(&[0, 1, 1]),
        "Unexpected shortlink close"
    );
    Ok(plain)
}

#[cfg(test)]
mod tests {
    #[test]
    fn exact_call_app_auth_route_does_not_admit_arbitrary_route_bytes() {
        assert!(validate_short_route("/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth").is_ok());
        assert!(validate_short_route("/cgi-bin/micromsg-bin/heartbeat").is_ok());
        assert!(validate_short_route("/cgi-bin/micromsg-bin/voipilinkack").is_ok());
        for bad in [
            "/cgi-bin/micromsg-bin/another_route",
            "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth/extra",
            "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth?x=1",
            "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth#x",
            "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth\r\nX: y",
            "/cgi-bin/micromsg-bin/mm_ilink_cloud_app_auth\0",
            "/cgi-bin/micromsg-bin/%2e%2e",
            "/cgi-bin/micromsg-bin/../x",
            "/cgi-bin/micromsg-bin/\\x",
            "/cgi-bin/micromsg-bin/音",
            "http://example.test/cgi-bin/micromsg-bin/heartbeat",
        ] {
            assert!(validate_short_route(bad)
                .unwrap_err()
                .is::<UnsupportedNativeRoute>());
        }
    }
    use super::*;
    #[test]
    fn longlink_frames_can_split_or_share_records_and_reject_bad_lengths_early() {
        let a = pack_longlink(24, 0, &6u32.to_be_bytes()).unwrap();
        let b = pack_longlink(1000000238, 23, &[2; 256]).unwrap();
        let both = [a.as_slice(), b.as_slice()].concat();
        for cut in 0..a.len() {
            let mut decoder = LonglinkDecoder::default();
            decoder.feed(&both[..cut]).unwrap();
            assert!(decoder.next_packet().unwrap().is_none());
            decoder.feed(&both[cut..]).unwrap();
            assert_eq!(decoder.next_packet().unwrap().unwrap().command, 24);
            assert_eq!(decoder.next_packet().unwrap().unwrap().sequence, 23);
            assert!(decoder.next_packet().unwrap().is_none());
        }
        for n in [0u32, 15, (crate::MAX_MESSAGE_BYTES + 17) as u32, u32::MAX] {
            let mut decoder = LonglinkDecoder::default();
            decoder.feed(&n.to_be_bytes()).unwrap();
            assert!(decoder.next_packet().is_err());
        }
        let mut invalid = a.clone();
        invalid[7] = 2;
        let mut decoder = LonglinkDecoder::default();
        decoder.feed(&invalid[..8]).unwrap();
        assert!(decoder.next_packet().is_err());
        let mut limited = LonglinkDecoder {
            pending: Vec::new(),
            max_payload: 128,
        };
        limited.feed(&b[..4]).unwrap();
        assert!(limited.next_packet().is_err());
        let payload = vec![7; 200_000];
        let large = [
            (payload.len() as u32 + 16).to_be_bytes().to_vec(),
            vec![0, 16, 0, 1],
            1000000121u32.to_be_bytes().to_vec(),
            9u32.to_be_bytes().to_vec(),
            payload.clone(),
        ]
        .concat();
        let mut decoder = LonglinkDecoder::default();
        for part in large.chunks(20_000) {
            decoder.feed(part).unwrap();
        }
        assert_eq!(decoder.next_packet().unwrap().unwrap().payload, payload);
    }
    #[test]
    fn encrypted_idle_timeout_keeps_partial_record_and_packet_bytes() {
        use std::{net::TcpListener, sync::mpsc, thread};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let first = pack_longlink(24, 0, &6u32.to_be_bytes()).unwrap();
        let second = pack_longlink(1000000006, u32::MAX, &[]).unwrap();
        let key = vec![1; 16];
        let iv = vec![2; 12];
        let r1 = seal(0x17, &first[..10], &key, &iv, 5).unwrap();
        let r2 = seal(0x17, &[first[10..].to_vec(), second].concat(), &key, &iv, 6).unwrap();
        let mut corrupt = seal(0x17, &first, &key, &iv, 7).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;
        let (sent, observed) = mpsc::channel();
        let (resume, waiting) = mpsc::channel();
        let writer = thread::spawn(move || {
            peer.write_all(&r1[..3]).unwrap();
            sent.send(()).unwrap();
            waiting.recv().unwrap();
            peer.write_all(&r1[3..]).unwrap();
            peer.write_all(&r2).unwrap();
            peer.write_all(&corrupt).unwrap();
        });
        let mut session = Session {
            socket,
            deadline: Instant::now(),
            received: 0,
            receive_budget: 1024,
            short: ShortSession {
                access_psk: vec![],
                ticket: Ticket {
                    kind: 1,
                    lifetime: 0,
                    reserved: 0,
                    nonce: vec![],
                    opaque: vec![],
                },
            },
            certificate_region: 0,
            certificate_version: 0,
            traffic: Traffic {
                client_key: key.clone(),
                server_key: key,
                client_iv: iv.clone(),
                server_iv: iv,
            },
            outgoing: 3,
            incoming: 5,
            record_pending: Vec::new(),
            application_pending: LonglinkDecoder::default(),
        };
        observed.recv().unwrap();
        assert!(session
            .poll_application(Duration::from_millis(10))
            .unwrap()
            .is_none());
        assert_eq!(session.record_pending.len(), 3);
        resume.send(()).unwrap();
        assert_eq!(
            session
                .poll_application(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .command,
            24
        );
        assert_eq!(
            session
                .poll_application(Duration::from_secs(1))
                .unwrap()
                .unwrap()
                .sequence,
            u32::MAX
        );
        assert!(session.poll_application(Duration::from_secs(1)).is_err());
        assert_eq!(session.incoming, 7);
        writer.join().unwrap();
    }
    #[test]
    fn published_mmtls_vector() {
        let key = hex::decode("29b37affd98238fb0370c564d8fa992e").unwrap();
        let wire_nonce = hex::decode("36ee037a258fbb83ca1f9762").unwrap();
        let base = nonce(&wire_nonce, 1).unwrap();
        let plain = hex::decode(
            "0000002314002009d6fc43a65a48dd64e3b6846562d13752f5abc2cb254539906ecd429df71082",
        )
        .unwrap();
        let expected=hex::decode("16f1040037e4bdea652cd80a273fd2ef549df4c87c1083c23f658fcc2d4152d163bc809f35a56fd0f4a62caa138d765a086eec02e21704892a981cf7").unwrap();
        assert_eq!(seal(0x16, &plain, &key, &base, 1).unwrap(), expected);
        let r = Record {
            header: expected[..5].try_into().unwrap(),
            body: expected[5..].to_vec(),
        };
        assert_eq!(open(&r, &key, &base, 1).unwrap(), plain);
        assert!(open(&r, &key, &base, 2).is_err());
    }
    #[test]
    fn hosts_restricted() {
        for s in [
            "localhost",
            "127.0.0.1",
            "long.weixin.qq.com.attacker.org",
            "-x.wechat.com",
            "x.wechat.com:80",
        ] {
            assert!(validate_host(s).is_err());
        }
        assert!(validate_host("long.weixin.qq.com").is_ok());
    }
    #[test]
    fn malformed_envelopes() {
        assert!(unpack_longlink(&[]).is_err());
        assert!(parse_server_hello(&[0; 4]).is_err());
        let mut v = pack_longlink(6, 123, &[1, 2]).unwrap();
        v[3] ^= 1;
        assert!(unpack_longlink(&v).is_err());
    }
}
