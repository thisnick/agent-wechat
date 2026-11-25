//! Fresh pinned MMTLS handshake and authenticated shortlink exchange.
use crate::crypto::*;
use crate::transport::{
    AsyncByteStream, Clock, HttpMethod, HttpRequest, HttpTransport, ReadOutcome, StreamTransport,
    TransportError,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Instant,
};
use std::{sync::Arc, time::Duration};
use zeroize::Zeroize;

pub const VERSION: u16 = 0xf104;
const MAX_RECORD_PLAINTEXT: usize = u16::MAX as usize - 16;
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
    ensure!(
        plain.len() <= MAX_RECORD_PLAINTEXT,
        "MMTLS plaintext too large"
    );
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
fn handshake_secret(shared_x: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        shared_x.len() == 32,
        "Invalid MMTLS P256 shared secret width"
    );
    // The P256 coordinate has a fixed width. A leading zero is part of the
    // shared secret and must participate in the key schedule.
    Ok(sha256(shared_x))
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

/// Longest reuse of one short-link ticket, even when its lifetime is longer.
const SHORT_LINK_MAX_REUSE: Duration = Duration::from_secs(3600);
/// Stop reusing a ticket this long before its advertised lifetime ends.
const SHORT_LINK_EXPIRY_MARGIN: Duration = Duration::from_secs(60);

/// The short-link resumption ticket an account's owner reuses between
/// requests. The long-link handshake issues an access PSK and ticket with a
/// lifetime; short-link requests present it, so a new long-link handshake is
/// only needed when no ticket is cached for the host, the cached one is near
/// the end of its lifetime, or a short exchange that used it failed. Held only
/// in memory and zeroized with the session when replaced.
#[derive(Default)]
pub(crate) struct ShortLinkCache {
    entry: std::sync::Mutex<Option<(String, ShortSession, u64)>>,
}
impl ShortLinkCache {
    /// A clone of the cached ticket for `host` while it is still reusable.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn get(&self, host: &str) -> Option<ShortSession> {
        self.get_at_ms(host, crate::transport::native_monotonic_ms())
    }
    pub(crate) fn get_at_ms(&self, host: &str, now_ms: u64) -> Option<ShortSession> {
        let entry = self.entry.lock().ok()?;
        let (cached_host, session, issued_ms) = entry.as_ref()?;
        let lifetime_ms = u64::from(session.ticket.lifetime)
            .saturating_mul(1000)
            .saturating_sub(SHORT_LINK_EXPIRY_MARGIN.as_millis() as u64)
            .min(SHORT_LINK_MAX_REUSE.as_millis() as u64);
        (cached_host == host && now_ms.saturating_sub(*issued_ms) < lifetime_ms)
            .then(|| session.clone())
    }
    /// Remember the ticket a long-link handshake to `host` just issued.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn put(&self, host: &str, session: ShortSession) {
        self.put_at_ms(host, session, crate::transport::native_monotonic_ms());
    }
    pub(crate) fn put_at_ms(&self, host: &str, session: ShortSession, issued_ms: u64) {
        if let Ok(mut entry) = self.entry.lock() {
            *entry = Some((host.to_string(), session, issued_ms));
        }
    }
    /// Forget the cached ticket so the next request performs a handshake.
    pub(crate) fn invalidate(&self) {
        if let Ok(mut entry) = self.entry.lock() {
            *entry = None;
        }
    }
}

/// Sans-I/O cryptographic and framing state for an established MMTLS long-link
/// session, shared between the blocking [`Session`] and async [`AsyncSession`].
struct SessionCore {
    #[cfg(not(target_arch = "wasm32"))]
    receive_budget: usize,
    short: ShortSession,
    certificate_region: u32,
    certificate_version: u32,
    traffic: Traffic,
    outgoing: u64,
    incoming: u64,
    record_pending: Vec<u8>,
    application_pending: LonglinkDecoder,
}

impl SessionCore {
    fn seal_application(&mut self, command: u32, sequence: u32, payload: &[u8]) -> Result<Vec<u8>> {
        let bytes = seal(
            0x17,
            &pack_longlink(command, sequence, payload)?,
            &self.traffic.client_key,
            &self.traffic.client_iv,
            self.outgoing,
        )?;
        self.outgoing = self
            .outgoing
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Sequence exhausted"))?;
        Ok(bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_application_record(&mut self, record: &Record) -> Result<Longlink> {
        ensure!(record.header[0] == 0x17, "Expected application record");
        let plain = open(
            record,
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

    fn drain_pending_packet(&mut self) -> Result<Option<Longlink>> {
        loop {
            if let Some(packet) = self.application_pending.next_packet()? {
                return Ok(Some(packet));
            }
            if self.record_pending.len() < 5 {
                return Ok(None);
            }
            let h: [u8; 5] = self.record_pending[..5].try_into()?;
            ensure!(
                (0x15..=0x19).contains(&h[0]) && u16::from_be_bytes([h[1], h[2]]) == VERSION,
                "Unexpected MMTLS header"
            );
            let n = u16::from_be_bytes([h[3], h[4]]) as usize;
            if self.record_pending.len() < n + 5 {
                return Ok(None);
            }
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
        }
    }

    fn push_pending_bytes(&mut self, chunk: &[u8]) -> Result<()> {
        // At most one incomplete record precedes a socket read.
        ensure!(
            self.record_pending.len().saturating_add(chunk.len()) <= 131080,
            "MMTLS pending record limit"
        );
        self.record_pending.extend_from_slice(chunk);
        Ok(())
    }
}

struct HandshakeClient {
    keys: [Ecdh; 2],
    hello: Vec<u8>,
}

struct HandshakeVerify {
    server: ServerHello,
    secret: Vec<u8>,
    handshake: Traffic,
    transcript: Vec<u8>,
    access_psk: Option<Vec<u8>>,
    ticket: Option<Ticket>,
}

impl Drop for HandshakeVerify {
    fn drop(&mut self) {
        self.secret.zeroize();
        if let Some(psk) = &mut self.access_psk {
            psk.zeroize();
        }
        if let Some(ticket) = &mut self.ticket {
            ticket.opaque.zeroize();
            ticket.nonce.zeroize();
        }
    }
}

struct HandshakeEstablished {
    server: ServerHello,
    short: ShortSession,
    application: Traffic,
}

impl HandshakeClient {
    fn start(now_secs: u32) -> Result<(Self, Vec<u8>)> {
        let keys = [
            Ecdh::generate_for(Curve::P256).context("MMTLS key generation")?,
            Ecdh::generate_for(Curve::P256).context("MMTLS key generation")?,
        ];
        let mut shares = vec![];
        for (i, k) in keys.iter().enumerate() {
            let point = k.public().context("MMTLS public key encoding")?;
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
                random::<32>().context("MMTLS hello random")?.to_vec(),
                now_secs.to_be_bytes().to_vec(),
                section(&[vec![1], extension].concat()),
            ]
            .concat(),
        );
        let wire = frame(0x16, &hello)?;
        Ok((Self { keys, hello }, wire))
    }

    fn process_server_hello(self, server_record: Record) -> Result<HandshakeVerify> {
        ensure!(server_record.header[0] == 0x16, "Expected handshake");
        let server = parse_server_hello(&server_record.body)?;
        let index = server
            .key_id
            .checked_sub(5)
            .ok_or_else(|| anyhow::anyhow!("Unoffered key"))? as usize;
        ensure!(index < 2, "Unoffered key");
        let mut x = self.keys[index]
            .derive(&server.point)
            .context("MMTLS shared secret derivation")?;
        let secret = handshake_secret(&x)?;
        x.zeroize();
        let transcript = [self.hello, server_record.body].concat();
        let handshake = traffic(&secret, "handshake key expansion", &transcript)
            .context("MMTLS handshake key derivation")?;
        Ok(HandshakeVerify {
            server,
            secret,
            handshake,
            transcript,
            access_psk: None,
            ticket: None,
        })
    }
}

impl HandshakeVerify {
    fn verify_signature(&mut self, record: Record) -> Result<()> {
        ensure!(record.header[0] == 0x16, "Expected signature record");
        let signature_body = open(
            &record,
            &self.handshake.server_key,
            &self.handshake.server_iv,
            1,
        )
        .context("MMTLS signature record authentication")?;
        let signature = signed_payload(&signature_body, 0x0f)?;
        let pins: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/mmtls-public-keys.json"))?;
        let pin = pins["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["version"].as_u64() == Some(self.server.version as u64))
            .ok_or_else(|| anyhow::anyhow!("Unsupported MMTLS certificate version"))?;
        ensure!(
            [5, 1000].contains(&self.server.version),
            "Unoffered MMTLS pin"
        );
        verify_signature(
            pin["ecdsaPEM"].as_str().unwrap(),
            &sha256(&self.transcript),
            &signature,
        )
        .context("MMTLS signature verification")?;
        self.transcript.extend(signature_body);
        Ok(())
    }

    fn process_tickets(&mut self, record: Record) -> Result<()> {
        ensure!(record.header[0] == 0x16, "Expected ticket record");
        let ticket_body = open(
            &record,
            &self.handshake.server_key,
            &self.handshake.server_iv,
            2,
        )
        .context("MMTLS ticket record authentication")?;
        let tickets = parse_tickets(&ticket_body)?;
        let access_psk = expand(
            &self.secret,
            &[b"PSK_ACCESS".as_slice(), &sha256(&self.transcript)].concat(),
            32,
        )?;
        self.transcript.extend(ticket_body);
        self.access_psk = Some(access_psk);
        self.ticket = Some(tickets[0].clone());
        Ok(())
    }

    fn finish(mut self, record: Record) -> Result<(HandshakeEstablished, Vec<u8>, Vec<u8>)> {
        ensure!(record.header[0] == 0x16, "Expected Finished");
        let finish = signed_payload(
            &open(
                &record,
                &self.handshake.server_key,
                &self.handshake.server_iv,
                3,
            )
            .context("MMTLS finished record authentication")?,
            0x14,
        )?;
        let expected = hmac(
            &expand(&self.secret, b"server finished", 32)?,
            &sha256(&self.transcript),
        )?;
        ensure!(
            constant_time_eq(&finish, &expected),
            "Server Finished verification failed"
        );
        let client_finish = hmac(
            &expand(&self.secret, b"client finished", 32)?,
            &sha256(&self.transcript),
        )?;
        let client_finish_wire = seal(
            0x16,
            &section(&[vec![0x14], sized(&client_finish)?].concat()),
            &self.handshake.client_key,
            &self.handshake.client_iv,
            1,
        )?;
        let mut expanded = expand(
            &self.secret,
            &[b"expanded secret".as_slice(), &sha256(&self.transcript)].concat(),
            32,
        )?;
        let application = traffic(
            &expanded,
            "application data key expansion",
            &self.transcript,
        )
        .context("MMTLS application key derivation")?;
        expanded.zeroize();
        let noop = pack_longlink(6, u32::MAX, &[])?;
        let noop_wire = seal(
            0x17,
            &noop,
            &application.client_key,
            &application.client_iv,
            2,
        )
        .context("MMTLS noop record sealing")?;
        let access_psk = self
            .access_psk
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing MMTLS access PSK"))?;
        let ticket = self
            .ticket
            .take()
            .ok_or_else(|| anyhow::anyhow!("Missing MMTLS shortlink ticket"))?;
        let established = HandshakeEstablished {
            server: ServerHello {
                key_id: self.server.key_id,
                point: std::mem::take(&mut self.server.point),
                region: self.server.region,
                version: self.server.version,
            },
            short: ShortSession { access_psk, ticket },
            application,
        };
        Ok((established, client_finish_wire, noop_wire))
    }
}

impl HandshakeEstablished {
    fn verify_noop(self, record: Record, receive_budget: usize) -> Result<SessionCore> {
        ensure!(record.header[0] == 0x17, "Expected NOOP reply");
        let p = unpack_longlink(
            &open(
                &record,
                &self.application.server_key,
                &self.application.server_iv,
                4,
            )
            .context("MMTLS noop record authentication")?,
        )?;
        ensure!(
            p.command == 0x3b9aca06 && p.sequence == u32::MAX && p.payload.is_empty(),
            "Unexpected NOOP reply"
        );
        Ok(SessionCore {
            #[cfg(not(target_arch = "wasm32"))]
            receive_budget,
            short: self.short,
            certificate_region: self.server.region,
            certificate_version: self.server.version,
            traffic: self.application,
            outgoing: 3,
            incoming: 5,
            record_pending: Vec::new(),
            application_pending: LonglinkDecoder {
                pending: Vec::new(),
                max_payload: receive_budget,
            },
        })
    }
}

/// Blocking MMTLS long-link session backed by `std::net::TcpStream`.
#[cfg(not(target_arch = "wasm32"))]
pub struct Session {
    socket: TcpStream,
    deadline: Instant,
    received: usize,
    pub short: ShortSession,
    pub certificate_region: u32,
    pub certificate_version: u32,
    core: SessionCore,
}

/// Asynchronous MMTLS long-link session backed by a pluggable [`AsyncByteStream`].
pub struct AsyncSession {
    stream: Box<dyn AsyncByteStream>,
    clock: Arc<dyn Clock>,
    timeout: Duration,
    /// Resumption ticket and access PSK issued by this long-link handshake.
    pub short: ShortSession,
    /// Server certificate region advertised during the handshake.
    pub certificate_region: u32,
    /// Server certificate pin version verified during the handshake.
    pub certificate_version: u32,
    core: SessionCore,
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
#[cfg(not(target_arch = "wasm32"))]
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
#[cfg(not(target_arch = "wasm32"))]
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

async fn read_exact_async_deadline(
    stream: &mut dyn AsyncByteStream,
    mut bytes: &mut [u8],
    clock: &dyn Clock,
    deadline_ms: u64,
) -> Result<()> {
    while !bytes.is_empty() {
        let now = clock.monotonic_ms();
        ensure!(now < deadline_ms, "MMTLS deadline exceeded");
        let remaining = Duration::from_millis(deadline_ms - now);
        match stream.read(bytes, remaining).await? {
            ReadOutcome::Bytes(0) => anyhow::bail!("Empty transport stream read"),
            ReadOutcome::Bytes(n) => {
                ensure!(n <= bytes.len(), "Transport stream read exceeded buffer");
                bytes = &mut bytes[n..];
            }
            ReadOutcome::Eof => {
                return Err(anyhow::Error::new(TransportError::Disconnected)
                    .context("MMTLS connection closed"));
            }
            ReadOutcome::TimedOut => {
                return Err(
                    anyhow::Error::new(TransportError::Timeout).context("MMTLS deadline exceeded")
                );
            }
        }
    }
    Ok(())
}

async fn read_record_async(
    stream: &mut dyn AsyncByteStream,
    clock: &dyn Clock,
    deadline_ms: u64,
    received: &mut usize,
    budget: usize,
) -> Result<Record> {
    let mut h = [0; 5];
    read_exact_async_deadline(stream, &mut h, clock, deadline_ms).await?;
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
    read_exact_async_deadline(stream, &mut body, clock, deadline_ms).await?;
    ensure!(h[0] != 0x15, "Server returned MMTLS alert");
    Ok(Record { header: h, body })
}

fn validate_connect_params(host: &str, timeout: Duration, receive_budget: usize) -> Result<()> {
    validate_host(host)?;
    ensure!(
        (1024..=crate::MAX_MESSAGE_BYTES).contains(&receive_budget),
        "Invalid receive budget"
    );
    ensure!(
        !timeout.is_zero() && timeout <= Duration::from_secs(120),
        "Invalid MMTLS timeout"
    );
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
impl Session {
    pub fn connect(host: &str, timeout: Duration, receive_budget: usize) -> Result<Self> {
        validate_connect_params(host, timeout, receive_budget)?;
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
        let (client, hello_wire) = HandshakeClient::start(crate::wire::unix_time()?)?;
        socket.write_all(&hello_wire)?;
        let mut received = 0;
        let server_record = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        let mut verify = client.process_server_hello(server_record)?;
        verify.verify_signature(read_record(
            &mut socket,
            deadline,
            &mut received,
            receive_budget,
        )?)?;
        verify.process_tickets(read_record(
            &mut socket,
            deadline,
            &mut received,
            receive_budget,
        )?)?;
        let (established, client_finish_wire, noop_wire) = verify.finish(read_record(
            &mut socket,
            deadline,
            &mut received,
            receive_budget,
        )?)?;
        socket.write_all(&client_finish_wire)?;
        socket.write_all(&noop_wire)?;
        let noop_reply = read_record(&mut socket, deadline, &mut received, receive_budget)?;
        let core = established.verify_noop(noop_reply, receive_budget)?;
        let short = core.short.clone();
        let certificate_region = core.certificate_region;
        let certificate_version = core.certificate_version;
        Ok(Self {
            socket,
            deadline,
            received,
            short,
            certificate_region,
            certificate_version,
            core,
        })
    }
    pub fn send_application(&mut self, command: u32, sequence: u32, payload: &[u8]) -> Result<()> {
        let bytes = self.core.seal_application(command, sequence, payload)?;
        self.socket.write_all(&bytes)?;
        Ok(())
    }
    pub fn receive_application(&mut self) -> Result<Longlink> {
        let r = read_record(
            &mut self.socket,
            self.deadline,
            &mut self.received,
            self.core.receive_budget,
        )?;
        self.core.open_application_record(&r)
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
            if let Some(packet) = self.core.drain_pending_packet()? {
                return Ok(Some(packet));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.socket.set_read_timeout(Some(remaining))?;
            let mut bytes = [0u8; 65540];
            match self.socket.read(&mut bytes) {
                Ok(0) => anyhow::bail!("MMTLS connection closed"),
                Ok(n) => self.core.push_pending_bytes(&bytes[..n])?,
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

impl AsyncSession {
    /// Open and authenticate an MMTLS long-link session over `streams`,
    /// enforcing host allowlists, pinned ECDSA verification, Finished MACs,
    /// and the handshake receive budget.
    pub async fn connect(
        host: &str,
        timeout: Duration,
        receive_budget: usize,
        streams: &dyn StreamTransport,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        validate_connect_params(host, timeout, receive_budget)?;
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline_ms = clock.monotonic_ms().saturating_add(timeout_ms);
        let remaining_for = |clock: &dyn Clock| -> Result<Duration> {
            let now = clock.monotonic_ms();
            ensure!(now < deadline_ms, "MMTLS deadline exceeded");
            Ok(Duration::from_millis(deadline_ms - now))
        };
        let mut stream = streams
            .connect(host, 80, remaining_for(&*clock)?)
            .await
            .context("Native connection failed")?;
        let core_result: Result<SessionCore> = async {
            let (client, hello_wire) = HandshakeClient::start(clock.unix_time_secs()?)?;
            stream
                .write_all(&hello_wire, remaining_for(&*clock)?)
                .await?;
            let mut received = 0usize;
            let server_record = read_record_async(
                &mut *stream,
                &*clock,
                deadline_ms,
                &mut received,
                receive_budget,
            )
            .await?;
            let mut verify = client.process_server_hello(server_record)?;
            verify.verify_signature(
                read_record_async(
                    &mut *stream,
                    &*clock,
                    deadline_ms,
                    &mut received,
                    receive_budget,
                )
                .await?,
            )?;
            verify.process_tickets(
                read_record_async(
                    &mut *stream,
                    &*clock,
                    deadline_ms,
                    &mut received,
                    receive_budget,
                )
                .await?,
            )?;
            let (established, client_finish_wire, noop_wire) = verify.finish(
                read_record_async(
                    &mut *stream,
                    &*clock,
                    deadline_ms,
                    &mut received,
                    receive_budget,
                )
                .await?,
            )?;
            stream
                .write_all(&client_finish_wire, remaining_for(&*clock)?)
                .await?;
            stream
                .write_all(&noop_wire, remaining_for(&*clock)?)
                .await?;
            let noop_reply = read_record_async(
                &mut *stream,
                &*clock,
                deadline_ms,
                &mut received,
                receive_budget,
            )
            .await?;
            established.verify_noop(noop_reply, receive_budget)
        }
        .await;
        let core = match core_result {
            Ok(core) => core,
            Err(error) => {
                let _ = stream.close().await;
                return Err(error);
            }
        };
        let short = core.short.clone();
        let certificate_region = core.certificate_region;
        let certificate_version = core.certificate_version;
        Ok(Self {
            stream,
            clock,
            timeout,
            short,
            certificate_region,
            certificate_version,
            core,
        })
    }

    /// Seal and send one encrypted MMTLS application record containing a
    /// long-link frame.
    pub async fn send_application(
        &mut self,
        command: u32,
        sequence: u32,
        payload: &[u8],
    ) -> Result<()> {
        let bytes = self.core.seal_application(command, sequence, payload)?;
        self.stream.write_all(&bytes, self.timeout).await
    }

    /// Bounded async wait for the next decrypted long-link packet. Partial
    /// encrypted records and application packets survive a timeout.
    pub async fn poll_application(&mut self, timeout: Duration) -> Result<Option<Longlink>> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(120),
            "Invalid longlink poll timeout"
        );
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline_ms = self.clock.monotonic_ms().saturating_add(timeout_ms);
        let mut bytes = vec![0u8; 65540];
        loop {
            if let Some(packet) = self.core.drain_pending_packet()? {
                return Ok(Some(packet));
            }
            let now = self.clock.monotonic_ms();
            if now >= deadline_ms {
                return Ok(None);
            }
            let remaining = Duration::from_millis(deadline_ms - now);
            match self.stream.read(&mut bytes, remaining).await? {
                ReadOutcome::Bytes(0) => anyhow::bail!("Empty transport stream read"),
                ReadOutcome::Bytes(n) => {
                    ensure!(n <= bytes.len(), "Transport stream read exceeded buffer");
                    self.core.push_pending_bytes(&bytes[..n])?;
                }
                ReadOutcome::TimedOut => return Ok(None),
                ReadOutcome::Eof => {
                    return Err(anyhow::Error::new(TransportError::Disconnected)
                        .context("MMTLS connection closed"));
                }
            }
        }
    }

    /// Best-effort graceful close of the underlying stream.
    pub async fn close(mut self) -> Result<()> {
        self.stream.close().await
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

/// Blocking MMTLS short-link exchange using `reqwest::blocking`.
#[cfg(not(target_arch = "wasm32"))]
pub fn short_exchange(
    session: &ShortSession,
    host: &str,
    path: &str,
    payload: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let (hello, extensions, wire) =
        prepare_short_exchange_at(session, host, path, payload, crate::wire::unix_time()?)?;
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

/// Asynchronous MMTLS short-link exchange using a pluggable [`HttpTransport`]
/// and [`Clock`].
pub async fn short_exchange_async(
    session: &ShortSession,
    host: &str,
    path: &str,
    payload: &[u8],
    timeout: Duration,
    http: &dyn HttpTransport,
    clock: &dyn Clock,
) -> Result<Vec<u8>> {
    let (hello, extensions, wire) =
        prepare_short_exchange_at(session, host, path, payload, clock.unix_time_secs()?)?;
    let url = url::Url::parse(&format!(
        "http://{host}/mmtls/{}",
        hex::encode(random::<4>()?)
    ))?;
    let response = http
        .send(HttpRequest {
            method: HttpMethod::Post,
            url,
            headers: vec![
                ("Content-Type".into(), "application/octet-stream".into()),
                ("User-Agent".into(), "MicroMessenger Client".into()),
                ("Upgrade".into(), "mmtls".into()),
                ("Accept".into(), "*/*".into()),
                ("Cache-Control".into(), "no-cache".into()),
                ("Connection".into(), "close".into()),
            ],
            body: wire,
            timeout,
            max_response_bytes: crate::MAX_MESSAGE_BYTES,
        })
        .await?;
    ensure!(
        response.status == 200,
        "Native shortlink HTTP status {}",
        response.status
    );
    ensure!(
        response.body.len() <= crate::MAX_MESSAGE_BYTES,
        "Shortlink receive budget exceeded"
    );
    decode_short_reply(&response.body, session, &hello, &extensions)
}

fn prepare_short_exchange_at(
    session: &ShortSession,
    host: &str,
    path: &str,
    payload: &[u8],
    now_secs: u32,
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    validate_host(host)?;
    validate_short_route(path)?;
    ensure!(
        payload.len() <= crate::MAX_MESSAGE_BYTES,
        "Payload too large"
    );
    let (hello, extensions) = early_hello_at(&session.ticket, now_secs)?;
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
    let mut wire = [
        frame(0x19, &hello)?,
        seal(0x19, &extensions, &keys[..16], &keys[16..], 1)?,
    ]
    .concat();
    // The route is one application message, but large encrypted upload chunks
    // span records. Every data record and the close use consecutive nonces.
    let mut sequence = 2;
    for chunk in route.chunks(MAX_RECORD_PLAINTEXT) {
        wire.extend(seal(0x17, chunk, &keys[..16], &keys[16..], sequence)?);
        sequence += 1;
    }
    wire.extend(seal(
        0x15,
        &section(&[0, 1, 1]),
        &keys[..16],
        &keys[16..],
        sequence,
    )?);
    Ok((hello, extensions, wire))
}

/// The pre-answer poll can stop waiting without applying a late reply.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn short_exchange_interruptible(
    session: &ShortSession,
    host: &str,
    path: &str,
    payload: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let (hello, extensions, wire) =
        prepare_short_exchange_at(session, host, path, payload, crate::wire::unix_time()?)?;
    let client = reqwest::Client::builder()
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
        .send()
        .await?;
    ensure!(
        response.status() == 200,
        "Native shortlink HTTP status {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= crate::MAX_MESSAGE_BYTES - bytes.len(),
            "Shortlink receive budget exceeded"
        );
        bytes.extend_from_slice(&chunk);
    }
    decode_short_reply(&bytes, session, &hello, &extensions)
}
fn early_hello_at(ticket: &Ticket, now: u32) -> Result<(Vec<u8>, Vec<u8>)> {
    ensure!(
        ticket.nonce.len() == 12 && !ticket.opaque.is_empty(),
        "Invalid short ticket"
    );
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
        constant_time_eq(&finish, &expected),
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
pub(crate) mod tests {
    #[test]
    fn short_link_tickets_are_reused_per_host_within_their_lifetime() {
        use super::*;
        let session = |lifetime: u32| ShortSession {
            access_psk: vec![0x42; 32],
            ticket: Ticket {
                kind: 1,
                lifetime,
                reserved: 0,
                nonce: vec![0; 12],
                opaque: vec![1],
            },
        };
        let start = 10_000u64;
        let cache = ShortLinkCache::default();
        assert!(cache.get_at_ms("long.example", start).is_none());
        cache.put_at_ms("long.example", session(600), start);
        let reused = cache
            .get_at_ms("long.example", start + 539_000)
            .expect("reused inside the lifetime margin");
        assert_eq!(reused.access_psk, vec![0x42; 32]);
        // Another host, the expiry margin and invalidation all force a handshake.
        assert!(cache.get_at_ms("other.example", start).is_none());
        assert!(cache.get_at_ms("long.example", start + 540_000).is_none());
        cache.invalidate();
        assert!(cache.get_at_ms("long.example", start).is_none());
        // Reuse is capped at an hour, and a ticket without lifetime is not reused.
        cache.put_at_ms("long.example", session(86_400), start);
        assert!(cache.get_at_ms("long.example", start + 3_599_000).is_some());
        assert!(cache.get_at_ms("long.example", start + 3_600_000).is_none());
        cache.put_at_ms("long.example", session(0), start);
        assert!(cache.get_at_ms("long.example", start).is_none());
    }
    #[test]
    fn short_requests_split_large_uploads_and_authenticate_the_close_sequence() {
        let session = ShortSession {
            access_psk: vec![0x42; 32],
            ticket: Ticket {
                kind: 1,
                lifetime: 3600,
                reserved: 0,
                nonce: vec![0x24; 12],
                opaque: vec![0x17; 32],
            },
        };
        for size in [0, 60_000, 98_400, 200_000] {
            let payload = vec![0xa5; size];
            let (hello, extensions, wire) = prepare_short_exchange_at(
                &session,
                "short.weixin.qq.com",
                "/cgi-bin/micromsg-bin/mmemojiupload",
                &payload,
                1_700_000_000,
            )
            .unwrap();
            let keys = expand(
                &session.access_psk,
                &[b"early data key expansion".as_slice(), &sha256(&hello)].concat(),
                28,
            )
            .unwrap();
            let mut cursor = Cursor::new(&wire);
            let mut records = Vec::new();
            while cursor.offset < wire.len() {
                let header: [u8; 5] = cursor.take(5).unwrap().try_into().unwrap();
                let length = u16::from_be_bytes([header[3], header[4]]) as usize;
                records.push(Record {
                    header,
                    body: cursor.take(length).unwrap().to_vec(),
                });
            }
            assert_eq!(records[0].header[0], 0x19);
            assert_eq!(records[0].body, hello);
            assert_eq!(
                open(&records[1], &keys[..16], &keys[16..], 1).unwrap(),
                extensions
            );
            let mut route = Vec::new();
            for (sequence, record) in records[2..records.len() - 1].iter().enumerate() {
                assert_eq!(record.header[0], 0x17);
                route.extend(open(record, &keys[..16], &keys[16..], sequence as u64 + 2).unwrap());
            }
            let expected = section(
                &[
                    sized(b"/cgi-bin/micromsg-bin/mmemojiupload").unwrap(),
                    sized(b"short.weixin.qq.com").unwrap(),
                    section(&payload),
                ]
                .concat(),
            );
            assert_eq!(route, expected);
            let close = records.last().unwrap();
            assert_eq!(close.header[0], 0x15);
            assert_eq!(
                open(close, &keys[..16], &keys[16..], records.len() as u64 - 1).unwrap(),
                section(&[0, 1, 1])
            );
            assert!(open(close, &keys[..16], &keys[16..], records.len() as u64).is_err());
            if size <= 60_000 {
                assert_eq!(records.len(), 4);
            } else {
                assert!(records.len() > 4);
            }
        }
    }

    #[test]
    fn fresh_handshake_secret_requires_the_selected_p256_width() {
        for wrong in [0, 28, 31, 33] {
            assert!(handshake_secret(&vec![1; wrong]).is_err());
        }
        assert_eq!(handshake_secret(&[1; 32]).unwrap(), sha256(&[1; 32]));
    }
    #[test]
    fn zero_prefixed_p256_secret_authenticates_an_independent_server_record() {
        // Synthetic P256 private scalars 1 and 379. The shared coordinate and
        // record were calculated independently using Python cryptography,
        // hashlib and hmac. No account data or captured traffic is used.
        let own = Ecdh::from_private_for(Curve::P256, &[1]).unwrap();
        let peer = Ecdh::from_private_for(Curve::P256, &[1, 123]).unwrap();
        let shared = own.derive(&peer.public().unwrap()).unwrap();
        assert_eq!(
            hex::encode(&shared),
            "005543894af3d00ed7d740abdbd75c96b06877b787db5f70eea78b90a8d7c00a"
        );
        let keys = traffic(
            &handshake_secret(&shared).unwrap(),
            "handshake key expansion",
            b"synthetic client hellosynthetic server hello",
        )
        .unwrap();
        let wire = hex::decode("16f10400314bbd4e0fa2f28903e143496cf3f42dd756b3e792e8359afb180b5bfaff820fcb03742b63ba60e98e9e3ac32f99f851f031").unwrap();
        let record = Record {
            header: wire[..5].try_into().unwrap(),
            body: wire[5..].to_vec(),
        };
        assert_eq!(
            open(&record, &keys.server_key, &keys.server_iv, 1).unwrap(),
            b"synthetic server signature record"
        );
        assert!(open(&record, &keys.server_key, &keys.server_iv, 2).is_err());
        let mut tampered = record;
        *tampered.body.last_mut().unwrap() ^= 1;
        assert!(open(&tampered, &keys.server_key, &keys.server_iv, 1).is_err());
    }
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
    #[cfg(not(target_arch = "wasm32"))]
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
        let short = ShortSession {
            access_psk: vec![],
            ticket: Ticket {
                kind: 1,
                lifetime: 0,
                reserved: 0,
                nonce: vec![],
                opaque: vec![],
            },
        };
        let mut session = Session {
            socket,
            deadline: Instant::now(),
            received: 0,
            short: short.clone(),
            certificate_region: 0,
            certificate_version: 0,
            core: SessionCore {
                receive_budget: 1024,
                short,
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
            },
        };
        observed.recv().unwrap();
        assert!(session
            .poll_application(Duration::from_millis(10))
            .unwrap()
            .is_none());
        assert_eq!(session.core.record_pending.len(), 3);
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
        assert_eq!(session.core.incoming, 7);
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

    /// Decrypt one client MMTLS short-link request and build an authenticated
    /// synthetic MMTLS short-link reply for `reply_payload`.
    pub(crate) fn handle_mock_short_exchange(
        session: &ShortSession,
        wire: &[u8],
        reply_payload: &[u8],
    ) -> Result<(String, String, Vec<u8>, Vec<u8>)> {
        let mut c = Cursor::new(wire);
        let mut records = Vec::new();
        while c.offset < wire.len() {
            let h: [u8; 5] = c.take(5)?.try_into()?;
            let n = u16::from_be_bytes([h[3], h[4]]) as usize;
            records.push(Record {
                header: h,
                body: c.take(n)?.to_vec(),
            });
        }
        ensure!(records.len() >= 4, "Shortlink request too short");
        let hello = &records[0].body;
        let early_keys = expand(
            &session.access_psk,
            &[b"early data key expansion".as_slice(), &sha256(hello)].concat(),
            28,
        )?;
        let extensions = open(&records[1], &early_keys[..16], &early_keys[16..], 1)?;
        let mut route_bytes = Vec::new();
        for (i, rec) in records[2..records.len() - 1].iter().enumerate() {
            route_bytes.extend(open(
                rec,
                &early_keys[..16],
                &early_keys[16..],
                i as u64 + 2,
            )?);
        }
        let close = open(
            records.last().unwrap(),
            &early_keys[..16],
            &early_keys[16..],
            records.len() as u64 - 1,
        )?;
        ensure!(close == section(&[0, 1, 1]), "Unexpected request close");
        let mut top = Cursor::new(&route_bytes);
        let mut route = Cursor::new(top.sized(4)?);
        top.end()?;
        let path = std::str::from_utf8(route.sized(2)?)?.to_string();
        let host = std::str::from_utf8(route.sized(2)?)?.to_string();
        let payload = route.sized(4)?.to_vec();
        route.end()?;

        let server_hello = section(
            &[
                vec![2],
                VERSION.to_le_bytes().to_vec(),
                0xa8u16.to_be_bytes().to_vec(),
                vec![0x55; 32],
                section(&[0]),
            ]
            .concat(),
        );
        let transcript = [
            hello.as_slice(),
            extensions.as_slice(),
            server_hello.as_slice(),
        ]
        .concat();
        let reply_keys = expand(
            &session.access_psk,
            &[b"handshake key expansion".as_slice(), &sha256(&transcript)].concat(),
            28,
        )?;
        let finish = hmac(
            &expand(&session.access_psk, b"server finished", 32)?,
            &sha256(&transcript),
        )?;
        let finish_body = section(&[vec![0x14], sized(&finish)?].concat());
        let mut out = Vec::new();
        out.extend(frame(0x16, &server_hello)?);
        out.extend(seal(
            0x16,
            &finish_body,
            &reply_keys[..16],
            &reply_keys[16..],
            1,
        )?);
        let mut seq = 2u64;
        if reply_payload.is_empty() {
            out.extend(seal(0x17, &[], &reply_keys[..16], &reply_keys[16..], seq)?);
            seq += 1;
        } else {
            for chunk in reply_payload.chunks(MAX_RECORD_PLAINTEXT) {
                out.extend(seal(
                    0x17,
                    chunk,
                    &reply_keys[..16],
                    &reply_keys[16..],
                    seq,
                )?);
                seq += 1;
            }
        }
        out.extend(seal(
            0x15,
            &section(&[0, 1, 1]),
            &reply_keys[..16],
            &reply_keys[16..],
            seq,
        )?);
        Ok((path, host, payload, out))
    }

    pub(crate) fn async_session_for_test(
        stream: Box<dyn AsyncByteStream>,
        clock: Arc<dyn Clock>,
        key: Vec<u8>,
        iv: Vec<u8>,
    ) -> AsyncSession {
        let short = ShortSession {
            access_psk: vec![0x42; 32],
            ticket: Ticket {
                kind: 1,
                lifetime: 3600,
                reserved: 0,
                nonce: vec![0x24; 12],
                opaque: vec![0x17; 32],
            },
        };
        AsyncSession {
            stream,
            clock,
            timeout: Duration::from_secs(5),
            short: short.clone(),
            certificate_region: 1,
            certificate_version: 5,
            core: SessionCore {
                #[cfg(not(target_arch = "wasm32"))]
                receive_budget: 65536,
                short,
                certificate_region: 1,
                certificate_version: 5,
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
            },
        }
    }

    struct ScriptedClock(std::sync::atomic::AtomicU64);
    impl Clock for ScriptedClock {
        fn monotonic_ms(&self) -> u64 {
            self.0.load(std::sync::atomic::Ordering::Acquire)
        }
        fn unix_time_ms(&self) -> Result<u64> {
            Ok(1_700_000_000_000 + self.monotonic_ms())
        }
    }

    enum StreamStep {
        Bytes(Vec<u8>),
        TimedOut,
    }

    struct ScriptedStream {
        steps: std::collections::VecDeque<StreamStep>,
        written: Arc<std::sync::Mutex<Vec<u8>>>,
        closed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl AsyncByteStream for ScriptedStream {
        fn read<'a>(
            &'a mut self,
            buf: &'a mut [u8],
            _timeout: Duration,
        ) -> crate::transport::TransportFuture<'a, Result<ReadOutcome>> {
            let outcome = match self.steps.pop_front() {
                Some(StreamStep::Bytes(mut chunk)) => {
                    let take = chunk.len().min(buf.len());
                    buf[..take].copy_from_slice(&chunk[..take]);
                    if take < chunk.len() {
                        chunk.drain(..take);
                        self.steps.push_front(StreamStep::Bytes(chunk));
                    }
                    ReadOutcome::Bytes(take)
                }
                Some(StreamStep::TimedOut) => ReadOutcome::TimedOut,
                None => ReadOutcome::Eof,
            };
            Box::pin(async move { Ok(outcome) })
        }

        fn write_all<'a>(
            &'a mut self,
            buf: &'a [u8],
            _timeout: Duration,
        ) -> crate::transport::TransportFuture<'a, Result<()>> {
            self.written.lock().unwrap().extend_from_slice(buf);
            Box::pin(async { Ok(()) })
        }

        fn close<'a>(&'a mut self) -> crate::transport::TransportFuture<'a, Result<()>> {
            self.closed
                .store(true, std::sync::atomic::Ordering::Release);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn async_session_idle_timeout_keeps_partial_record_and_packet_bytes() {
        let first = pack_longlink(24, 0, &6u32.to_be_bytes()).unwrap();
        let second = pack_longlink(1000000006, u32::MAX, &[]).unwrap();
        let key = vec![1; 16];
        let iv = vec![2; 12];
        let r1 = seal(0x17, &first[..10], &key, &iv, 5).unwrap();
        let r2 = seal(0x17, &[first[10..].to_vec(), second].concat(), &key, &iv, 6).unwrap();
        let mut corrupt = seal(0x17, &first, &key, &iv, 7).unwrap();
        *corrupt.last_mut().unwrap() ^= 1;

        let written = Arc::new(std::sync::Mutex::new(Vec::new()));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stream = ScriptedStream {
            steps: [
                StreamStep::Bytes(r1[..3].to_vec()),
                StreamStep::TimedOut,
                StreamStep::Bytes(r1[3..].to_vec()),
                StreamStep::Bytes(r2),
                StreamStep::Bytes(corrupt),
            ]
            .into_iter()
            .collect(),
            written: written.clone(),
            closed: closed.clone(),
        };
        let clock = Arc::new(ScriptedClock(std::sync::atomic::AtomicU64::new(100)));
        let mut session = async_session_for_test(Box::new(stream), clock, key, iv);

        session.send_application(6, u32::MAX, &[]).await.unwrap();
        assert!(!written.lock().unwrap().is_empty());
        assert!(session
            .poll_application(Duration::from_millis(50))
            .await
            .unwrap()
            .is_none());
        assert_eq!(session.core.record_pending.len(), 3);
        assert_eq!(
            session
                .poll_application(Duration::from_secs(1))
                .await
                .unwrap()
                .unwrap()
                .command,
            24
        );
        assert_eq!(
            session
                .poll_application(Duration::from_secs(1))
                .await
                .unwrap()
                .unwrap()
                .sequence,
            u32::MAX
        );
        assert!(session
            .poll_application(Duration::from_secs(1))
            .await
            .is_err());
        assert_eq!(session.core.incoming, 7);
        session.close().await.unwrap();
        assert!(closed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[tokio::test]
    async fn short_exchange_async_roundtrips_over_http_over_stream() {
        let session = ShortSession {
            access_psk: vec![0x42; 32],
            ticket: Ticket {
                kind: 1,
                lifetime: 3600,
                reserved: 0,
                nonce: vec![0x24; 12],
                opaque: vec![0x17; 32],
            },
        };
        let session_for_server = session.clone();
        struct ShortLinkServerStream {
            session: ShortSession,
            written: Vec<u8>,
            reply_bytes: Option<Vec<u8>>,
            offset: usize,
        }
        impl AsyncByteStream for ShortLinkServerStream {
            fn write_all<'a>(
                &'a mut self,
                buf: &'a [u8],
                _timeout: Duration,
            ) -> crate::transport::TransportFuture<'a, Result<()>> {
                self.written.extend_from_slice(buf);
                Box::pin(async { Ok(()) })
            }
            fn read<'a>(
                &'a mut self,
                buf: &'a mut [u8],
                _timeout: Duration,
            ) -> crate::transport::TransportFuture<'a, Result<ReadOutcome>> {
                if self.reply_bytes.is_none() {
                    let sep = self
                        .written
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .expect("http header separator");
                    let wire = &self.written[sep + 4..];
                    let (path, host, payload, reply_wire) =
                        handle_mock_short_exchange(&self.session, wire, b"synthetic-short-reply")
                            .expect("valid shortlink request");
                    assert_eq!(path, "/cgi-bin/micromsg-bin/heartbeat");
                    assert_eq!(host, "short.weixin.qq.com");
                    assert_eq!(payload, b"synthetic-short-req");
                    let mut http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply_wire.len()
                    )
                    .into_bytes();
                    http.extend_from_slice(&reply_wire);
                    self.reply_bytes = Some(http);
                }
                let data = self.reply_bytes.as_ref().unwrap();
                let outcome = if self.offset >= data.len() {
                    ReadOutcome::Eof
                } else {
                    let take = (data.len() - self.offset).min(buf.len());
                    buf[..take].copy_from_slice(&data[self.offset..self.offset + take]);
                    self.offset += take;
                    ReadOutcome::Bytes(take)
                };
                Box::pin(async move { Ok(outcome) })
            }
            fn close<'a>(&'a mut self) -> crate::transport::TransportFuture<'a, Result<()>> {
                Box::pin(async { Ok(()) })
            }
        }
        struct ShortLinkServerTransport(ShortSession);
        impl StreamTransport for ShortLinkServerTransport {
            fn connect<'a>(
                &'a self,
                host: &'a str,
                port: u16,
                _timeout: Duration,
            ) -> crate::transport::TransportFuture<'a, Result<Box<dyn AsyncByteStream>>>
            {
                assert_eq!(host, "short.weixin.qq.com");
                assert_eq!(port, 80);
                let stream: Box<dyn AsyncByteStream> = Box::new(ShortLinkServerStream {
                    session: self.0.clone(),
                    written: Vec::new(),
                    reply_bytes: None,
                    offset: 0,
                });
                Box::pin(async move { Ok(stream) })
            }
        }

        let clock: Arc<dyn Clock> = Arc::new(ScriptedClock(std::sync::atomic::AtomicU64::new(500)));
        let transport = crate::transport::ProtocolTransport::from_streams(
            Arc::new(ShortLinkServerTransport(session_for_server)),
            clock.clone(),
        );
        let reply = short_exchange_async(
            &session,
            "short.weixin.qq.com",
            "/cgi-bin/micromsg-bin/heartbeat",
            b"synthetic-short-req",
            Duration::from_secs(5),
            &*transport.http,
            &*clock,
        )
        .await
        .unwrap();
        assert_eq!(reply, b"synthetic-short-reply");
    }
}
