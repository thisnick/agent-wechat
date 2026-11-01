//! Selected Live96 envelope and ordinary95 Opus-primary framing. Encoding these
//! bytes does not establish negotiated capabilities, room ownership or permission
//! to send. Media transmission requires a separately admitted generation/scope.

use anyhow::{bail, ensure, Result};
use openssl::{
    memcmp,
    pkey::{Id, PKey},
    sign::Signer,
    symm::{Cipher, Crypter, Mode},
};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

const HEADER: usize = 19;
const MAX_PACKET: usize = 1519;
const MAX_CONF: usize = 1024;
const WINDOW: u16 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaEpoch {
    pub account_generation: u64,
    pub logout_epoch: u64,
    pub call_generation: u64,
    pub key_generation: u64,
}
impl MediaEpoch {
    fn validate(self) -> Result<()> {
        ensure!(
            self.account_generation != 0 && self.call_generation != 0 && self.key_generation != 0,
            "Inactive media generation"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaContext {
    pub room_id: u64,
    pub own_member: u16,
    pub peer_member: u16,
    pub connection_id: u16,
}

/// The owner must independently establish a new cryptographic generation before
/// using VerifiedGeneration. IsolatedCompatibilityTest is intentionally separate:
/// it may reproduce a known diagnostic audio overlap for one approved experiment,
/// never silently relabel that behavior as production-safe nonce uniqueness.
pub enum TransmitScope {
    VerifiedGeneration {
        key_generation: u64,
    },
    IsolatedCompatibilityTest {
        intent_id: [u8; 32],
        deadline_ms: u64,
        allow_known_audio_overlap: bool,
    },
}

/// Authenticated current-generation payload. Clear headers are not fully in AAD;
/// protected member equality and external source/room/roster checks remain needed.
/// Contents have no Debug/Serialize implementation.
pub struct OpenedPayload {
    pub packed_kind: u8,
    pub sequence: u16,
    payload: Zeroizing<Vec<u8>>,
    epoch: MediaEpoch,
    context: MediaContext,
}
impl OpenedPayload {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// One key/base/generation owner. No Clone, reset or implicit rekey is provided.
/// The 64-sequence RX window is a local policy; rollover requires a verified new
/// generation. Socket-source validation must happen before `open`.
pub struct Live96 {
    key: Zeroizing<[u8; 32]>,
    base: Zeroizing<u64>,
    context: MediaContext,
    epoch: MediaEpoch,
    tx_scope: Option<TransmitScope>,
    next_audio: u32,
    next_control: u32,
    clear_sequence: u32,
    rx_highest: BTreeMap<u8, u16>,
    rx_seen: BTreeMap<u8, Box<[u64; 1024]>>,
    rx: BTreeMap<[u8; 8], [u8; 32]>,
    audio_overlaps: u64,
    closed: bool,
}
impl Live96 {
    pub fn new(
        key: [u8; 32],
        nonce_base: u64,
        context: MediaContext,
        epoch: MediaEpoch,
    ) -> Result<Self> {
        epoch.validate()?;
        ensure!(
            context.room_id != 0 && nonce_base != 0 && context.own_member != context.peer_member,
            "Invalid media room/member context"
        );
        Ok(Self {
            key: Zeroizing::new(key),
            base: Zeroizing::new(nonce_base),
            context,
            epoch,
            tx_scope: None,
            next_audio: 0,
            next_control: 0,
            clear_sequence: 0,
            rx_highest: BTreeMap::new(),
            rx_seen: BTreeMap::new(),
            rx: BTreeMap::new(),
            audio_overlaps: 0,
            closed: false,
        })
    }
    fn check(&mut self, current: MediaEpoch) -> Result<()> {
        if self.closed || self.epoch != current {
            self.close();
            bail!("Media generation changed or closed");
        }
        Ok(())
    }
    pub fn authorize_transmit(&mut self, current: MediaEpoch, scope: TransmitScope) -> Result<()> {
        self.check(current)?;
        ensure!(
            self.tx_scope.is_none(),
            "Media transmit scope already installed"
        );
        match &scope {
            TransmitScope::VerifiedGeneration { key_generation } => ensure!(
                *key_generation == current.key_generation,
                "Media key generation mismatch"
            ),
            TransmitScope::IsolatedCompatibilityTest {
                intent_id,
                deadline_ms,
                ..
            } => ensure!(
                *intent_id != [0; 32] && *deadline_ms != 0,
                "Missing isolated media test scope"
            ),
        };
        self.tx_scope = Some(scope);
        Ok(())
    }
    fn tx_check(&mut self, current: MediaEpoch, now_ms: u64) -> Result<()> {
        self.check(current)?;
        match &self.tx_scope {
            None => bail!("Media transmission is not authorized"),
            Some(TransmitScope::VerifiedGeneration { .. }) => Ok(()),
            Some(TransmitScope::IsolatedCompatibilityTest { deadline_ms, .. }) => {
                if now_ms >= *deadline_ms {
                    self.close();
                    bail!("Isolated media test expired");
                }
                Ok(())
            }
        }
    }
    fn nonce(&self, kind: u8, sequence: u16) -> [u8; 8] {
        self.base
            .wrapping_add(u64::from(kind) << 32)
            .wrapping_add(u64::from(sequence))
            .to_le_bytes()
    }
    fn diagnostic_audio_overlap(&mut self, kind: u8) -> Result<()> {
        if kind == 0x10
            && matches!(
                self.tx_scope,
                Some(TransmitScope::IsolatedCompatibilityTest {
                    allow_known_audio_overlap: true,
                    ..
                })
            )
        {
            self.audio_overlaps = self
                .audio_overlaps
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("Media diagnostic count exhausted"))?;
            Ok(())
        } else {
            self.close();
            bail!("Known media cross-direction nonce reuse")
        }
    }
    pub fn known_audio_overlaps(&self) -> u64 {
        self.audio_overlaps
    }
    pub fn seal_audio(
        &mut self,
        current: MediaEpoch,
        now_ms: u64,
        conf_payload: &[u8],
    ) -> Result<Vec<u8>> {
        parse_ordinary95(conf_payload)?;
        self.seal(current, now_ms, 0x10, conf_payload)
    }
    fn seal(
        &mut self,
        current: MediaEpoch,
        now_ms: u64,
        kind: u8,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        self.tx_check(current, now_ms)?;
        ensure!(
            !payload.is_empty() && payload.len() <= 1482,
            "Invalid media payload extent"
        );
        let next = match kind {
            0x10 => &mut self.next_audio,
            0x70 => &mut self.next_control,
            _ => bail!("Unsupported media sender family"),
        };
        ensure!(
            *next <= u32::from(u16::MAX) && self.clear_sequence < u32::MAX,
            "Media sequence exhausted; verified rekey required"
        );
        let sequence = *next as u16;
        *next += 1;
        let nonce = self.nonce(kind, sequence);
        if self
            .rx_seen
            .get(&kind)
            .is_some_and(|seen| seen[usize::from(sequence) / 64] & (1u64 << (sequence % 64)) != 0)
        {
            self.diagnostic_audio_overlap(kind)?;
        }
        let mut protected = Zeroizing::new(Vec::with_capacity(payload.len() + 2));
        protected.extend(self.context.own_member.to_le_bytes());
        protected.extend(payload);
        let cipher = seal_original(&self.key, nonce, &protected)?;
        let mut packet = Vec::with_capacity(HEADER + cipher.len());
        packet.extend([0x96, 19]);
        packet.extend(self.context.room_id.to_le_bytes());
        packet.extend(self.context.own_member.to_le_bytes());
        packet.extend((self.clear_sequence as u16).to_le_bytes());
        packet.push(kind);
        packet.extend(sequence.to_le_bytes());
        packet.extend([0, self.context.connection_id as u8]);
        packet.extend(cipher);
        self.clear_sequence += 1;
        Ok(packet)
    }
    pub fn open(&mut self, current: MediaEpoch, packet: &[u8]) -> Result<Option<OpenedPayload>> {
        self.check(current)?;
        ensure!(
            (38..=MAX_PACKET).contains(&packet.len()) && packet[..2] == [0x96, 19],
            "Unsupported Live96 packet shape"
        );
        ensure!(
            u64::from_le_bytes(packet[2..10].try_into()?) == self.context.room_id
                && u16::from_le_bytes(packet[10..12].try_into()?) == self.context.peer_member,
            "Media room/member mismatch"
        );
        let kind = packet[14];
        ensure!(
            matches!(kind, 0x10 | 0x20 | 0x70) && packet[17] == 0,
            "Unsupported Live96 packet family or extension"
        );
        let sequence = u16::from_le_bytes(packet[15..17].try_into()?);
        let nonce = self.nonce(kind, sequence);
        let clear = open_original(&self.key, nonce, &packet[HEADER..])?;
        let digest: [u8; 32] = openssl::sha::sha256(&packet[HEADER..]);
        if let Some(previous) = self.rx.get(&nonce) {
            if *previous != digest {
                self.close();
                bail!("Authenticated media nonce conflict");
            }
            return Ok(None);
        }
        let own_next = if kind == 0x10 {
            self.next_audio
        } else if kind == 0x70 {
            self.next_control
        } else {
            0
        };
        if u32::from(sequence) < own_next {
            self.diagnostic_audio_overlap(kind)?;
        }
        // Retire every authenticated nonce before payload/member classification.
        // Fixed 65536-bit ledgers preserve known cross-direction overlap checks
        // after ciphertext digests leave the smaller reordering window.
        let seen = self
            .rx_seen
            .entry(kind)
            .or_insert_with(|| Box::new([0; 1024]));
        seen[usize::from(sequence) / 64] |= 1u64 << (sequence % 64);
        if self
            .rx_highest
            .get(&kind)
            .is_some_and(|high| sequence < *high && *high - sequence >= WINDOW)
        {
            return Ok(None);
        }
        let high = self.rx_highest.entry(kind).or_insert(sequence);
        *high = (*high).max(sequence);
        let minimum = high.saturating_sub(WINDOW - 1);
        let base = *self.base;
        self.rx.retain(|nonce, _| {
            let combined = u64::from_le_bytes(*nonce).wrapping_sub(base);
            let k = (combined >> 32) as u8;
            let s = combined as u16;
            k != kind || s >= minimum
        });
        self.rx.insert(nonce, digest);
        ensure!(
            clear.len() > 2
                && clear.len() <= 1484
                && u16::from_le_bytes(clear[..2].try_into()?) == self.context.peer_member,
            "Protected media member mismatch or extent"
        );
        // RX connection ID is not assumed equal to the local TX connection ID.
        Ok(Some(OpenedPayload {
            packed_kind: kind,
            sequence,
            payload: Zeroizing::new(clear[2..].to_vec()),
            epoch: current,
            context: self.context,
        }))
    }
    /// Reply only to the exact selected authenticated faking_type1 probe. A
    /// periodic/control response receives no echo. No general control sender is
    /// inferred, and retries would consume another semantic operation.
    pub fn reply_to_probe(
        &mut self,
        current: MediaEpoch,
        now_ms: u64,
        opened: &OpenedPayload,
    ) -> Result<Option<Vec<u8>>> {
        self.check(current)?;
        ensure!(
            opened.epoch == current && opened.context == self.context,
            "Stale authenticated control payload"
        );
        if opened.packed_kind != 0x70 {
            return Ok(None);
        }
        match opened.payload() {
            [8, 1, 18, 2, 8, 1] => Ok(Some(self.seal(
                current,
                now_ms,
                0x70,
                &[8, 1, 18, 2, 8, 2],
            )?)),
            [8, 1, 18, 2, 8, 0 | 2] => Ok(None),
            _ => bail!("Unsupported authenticated call control"),
        }
    }
    pub fn close(&mut self) {
        self.key = Zeroizing::new([0; 32]);
        self.base = Zeroizing::new(0);
        self.tx_scope = None;
        self.closed = true;
        self.rx.clear();
        self.rx_highest.clear();
        self.rx_seen.clear();
    }
}

/// Configured ordinary95/74 N2 sender. RTP sequence/time identify the LAST
/// primary, independent from cryptographic counters and clear room sequence.
pub fn ordinary95_pair(
    metadata: u8,
    last_sequence: u16,
    last_timestamp: u32,
    first: &[u8],
    second: &[u8],
) -> Result<Vec<u8>> {
    ensure!(
        !first.is_empty()
            && !second.is_empty()
            && first.len() <= 1023
            && second.len() <= 1023
            && 17 + first.len() + second.len() <= MAX_CONF,
        "Invalid ordinary95 primary extent"
    );
    let mut out = Vec::with_capacity(17 + first.len() + second.len());
    out.extend([1, metadata, 0, 0, 0x95, 0x74]);
    out.extend(last_sequence.to_be_bytes());
    out.extend(last_timestamp.to_be_bytes());
    out.push(2);
    out.extend((first.len() as i16).to_le_bytes());
    out.extend((second.len() as i16).to_le_bytes());
    out.extend(first);
    out.extend(second);
    Ok(out)
}

/// Encoded Opus primaries, not decoded audio. The caller must establish that the
/// current negotiation selects this codec/layout before decoding.
pub struct OrdinaryAudio<'a> {
    pub metadata: [u8; 4],
    pub last_sequence: u16,
    pub last_timestamp: u32,
    pub primaries: Vec<&'a [u8]>,
}
pub fn parse_ordinary95(payload: &[u8]) -> Result<OrdinaryAudio<'_>> {
    ensure!(
        (16..=MAX_CONF).contains(&payload.len())
            && payload[0] == 1
            && payload[4..6] == [0x95, 0x74],
        "Unsupported ordinary95 audio shape"
    );
    let count = usize::from(payload[12]);
    ensure!(
        (1..=3).contains(&count) && 13 + 2 * count <= payload.len(),
        "Invalid ordinary95 primary count"
    );
    let mut cursor = 13 + 2 * count;
    let mut primaries = Vec::with_capacity(count);
    for n in 0..count {
        let length = i16::from_le_bytes(payload[13 + 2 * n..15 + 2 * n].try_into()?);
        ensure!(
            (1..=1023).contains(&length),
            "Invalid ordinary95 primary length"
        );
        let length = length as usize;
        ensure!(
            length <= payload.len() - cursor,
            "Truncated ordinary95 primary"
        );
        primaries.push(&payload[cursor..cursor + length]);
        cursor += length;
    }
    ensure!(cursor == payload.len(), "Trailing ordinary95 bytes");
    Ok(OrdinaryAudio {
        metadata: payload[..4].try_into()?,
        last_sequence: u16::from_be_bytes(payload[6..8].try_into()?),
        last_timestamp: u32::from_be_bytes(payload[8..12].try_into()?),
        primaries,
    })
}
fn xor(key: &[u8; 32], nonce: [u8; 8], counter: u64, input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut iv = Zeroizing::new([0; 16]);
    iv[..8].copy_from_slice(&counter.to_le_bytes());
    iv[8..].copy_from_slice(&nonce);
    let cipher = Cipher::chacha20();
    let mut crypter = Crypter::new(cipher, Mode::Encrypt, key, Some(&*iv))?;
    crypter.pad(false);
    let mut out = Zeroizing::new(vec![0; input.len() + cipher.block_size()]);
    let n = crypter.update(input, &mut out)?;
    let end = crypter.finalize(&mut out[n..])?;
    out.truncate(n + end);
    ensure!(out.len() == input.len(), "Unexpected call cipher size");
    Ok(out)
}
fn tag(key: &[u8; 32], nonce: [u8; 8], cipher: &[u8]) -> Result<[u8; 16]> {
    let one = xor(key, nonce, 0, &[0; 32])?;
    let key = PKey::private_key_from_raw_bytes(&one, Id::POLY1305)?;
    let mut signer = Signer::new_without_digest(&key)?;
    signer.update(&0u64.to_le_bytes())?;
    signer.update(cipher)?;
    signer.update(&(cipher.len() as u64).to_le_bytes())?;
    let mut tag = [0; 16];
    ensure!(signer.sign(&mut tag)? == 16, "Unexpected call tag size");
    Ok(tag)
}
fn seal_original(key: &[u8; 32], nonce: [u8; 8], clear: &[u8]) -> Result<Vec<u8>> {
    ensure!(clear.len() <= 1484, "Call cipher input too large");
    let cipher = xor(key, nonce, 1, clear)?;
    let tag = tag(key, nonce, &cipher)?;
    let mut out = Vec::with_capacity(cipher.len() + 16);
    out.extend(&*cipher);
    out.extend(tag);
    Ok(out)
}
fn open_original(key: &[u8; 32], nonce: [u8; 8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        (16..=1500).contains(&sealed.len()),
        "Invalid call ciphertext extent"
    );
    let (cipher, expected) = sealed.split_at(sealed.len() - 16);
    let actual = tag(key, nonce, cipher)?;
    ensure!(
        memcmp::eq(&actual, expected),
        "Call media authentication failed"
    );
    xor(key, nonce, 1, cipher)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn epoch() -> MediaEpoch {
        MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        }
    }
    fn context() -> MediaContext {
        MediaContext {
            room_id: 9,
            own_member: 0,
            peer_member: 1,
            connection_id: 2,
        }
    }
    fn receiver() -> Live96 {
        Live96::new([7; 32], 11, context(), epoch()).unwrap()
    }
    fn peer() -> Live96 {
        let mut c = context();
        std::mem::swap(&mut c.own_member, &mut c.peer_member);
        let mut sender = Live96::new([7; 32], 11, c, epoch()).unwrap();
        sender
            .authorize_transmit(
                epoch(),
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        sender
    }
    #[test]
    fn literal_primary_layout_and_extent() {
        let raw = ordinary95_pair(3, 1, 320, &[1, 2], &[3]).unwrap();
        assert_eq!(
            &raw[..17],
            &[1, 3, 0, 0, 0x95, 0x74, 0, 1, 0, 0, 1, 64, 2, 2, 0, 1, 0]
        );
        let decoded = parse_ordinary95(&raw).unwrap();
        assert_eq!(decoded.primaries, vec![&[1, 2][..], &[3][..]]);
        let mut trailing = raw;
        trailing.push(0);
        assert!(parse_ordinary95(&trailing).is_err());
    }
    #[test]
    fn unauthorized_sender_fails_and_authenticated_receiver_rejects_replay() {
        let raw = ordinary95_pair(0, 1, 320, &[1], &[2]).unwrap();
        let mut receiver = receiver();
        assert!(receiver.seal_audio(epoch(), 0, &raw).is_err());
        let mut peer = peer();
        let packet = peer.seal_audio(epoch(), 0, &raw).unwrap();
        assert_eq!(&packet[..2], &[0x96, 19]);
        let clear = receiver.open(epoch(), &packet).unwrap().unwrap();
        assert_eq!(clear.payload(), raw);
        assert!(receiver.open(epoch(), &packet).unwrap().is_none());
        let mut corrupt = packet;
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(receiver.open(epoch(), &corrupt).is_err());
    }
    #[test]
    fn clear_header_member_cannot_replace_protected_member() {
        let raw = ordinary95_pair(0, 1, 320, &[1], &[2]).unwrap();
        let mut peer = peer();
        let mut packet = peer.seal_audio(epoch(), 0, &raw).unwrap();
        packet[10..12].copy_from_slice(&2u16.to_le_bytes());
        assert!(receiver().open(epoch(), &packet).is_err());
    }
    #[test]
    fn known_overlap_fails_closed_outside_explicit_test_scope() {
        let raw = ordinary95_pair(0, 1, 320, &[1], &[2]).unwrap();
        let mut peer = peer();
        let packet = peer.seal_audio(epoch(), 0, &raw).unwrap();
        let mut receiver = receiver();
        receiver.open(epoch(), &packet).unwrap();
        receiver
            .authorize_transmit(
                epoch(),
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        assert!(receiver.seal_audio(epoch(), 0, &raw).is_err());
        let mut diagnostic = self::receiver();
        diagnostic
            .authorize_transmit(
                epoch(),
                TransmitScope::IsolatedCompatibilityTest {
                    intent_id: [1; 32],
                    deadline_ms: 100,
                    allow_known_audio_overlap: true,
                },
            )
            .unwrap();
        diagnostic.open(epoch(), &packet).unwrap();
        assert!(diagnostic.seal_audio(epoch(), 1, &raw).is_ok());
        assert_eq!(diagnostic.known_audio_overlaps(), 1);
        assert!(diagnostic.seal_audio(epoch(), 100, &raw).is_err());
    }
    #[test]
    fn authenticated_probe_response_is_distinct_and_not_echoed() {
        let mut peer = peer();
        let request = peer.seal(epoch(), 0, 0x70, &[8, 1, 18, 2, 8, 1]).unwrap();
        let mut receiver = receiver();
        let opened = receiver.open(epoch(), &request).unwrap().unwrap();
        receiver
            .authorize_transmit(
                epoch(),
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        assert!(receiver.reply_to_probe(epoch(), 0, &opened).is_err()); /* same control nonce is not silently reused */
    }
}
