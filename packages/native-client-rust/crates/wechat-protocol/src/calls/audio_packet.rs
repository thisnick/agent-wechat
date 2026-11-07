//! Selected Live96 envelope and ordinary95 Opus-primary framing. Encoding these
//! bytes does not establish negotiated capabilities, room ownership or permission
//! to send. Media transmission requires a separately admitted generation/scope.

use anyhow::{bail, ensure, Result};
use openssl::{
    hash::MessageDigest,
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
const MAX_NONCE_VARIANTS: usize = 8;
// Leave 60,000 sequences for the current 20-minute limit at 50 audio packets/sec.
const MAX_LEGACY_AUDIO_START: u16 = u16::MAX - 59_999;

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

/// An untrusted routing hint for selecting a roster-bound receiver. The chosen
/// receiver must still authenticate the packet and check its protected member.
pub fn unverified_live96_member(packet: &[u8]) -> Result<u16> {
    ensure!(
        (38..=MAX_PACKET).contains(&packet.len()) && packet[..2] == [0x96, 19],
        "Unsupported Live96 packet shape"
    );
    Ok(u16::from_le_bytes(packet[10..12].try_into()?))
}

/// The owner must establish a fresh media generation before authorizing sends.
/// The legacy relay scope trades cross-direction audio nonce uniqueness for
/// interoperability with peers that share media key material. It also permits
/// authenticated receive nonce reuse, with bounded ciphertext replay filtering.
pub enum TransmitScope {
    VerifiedGeneration {
        key_generation: u64,
    },
    LegacyRelayAudio {
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
    receive_only: bool,
    first_tx_audio: u16,
    next_audio: u32,
    next_control: u32,
    clear_sequence: u32,
    rx_highest: BTreeMap<u8, u16>,
    rx_seen: BTreeMap<u8, Box<[u64; 1024]>>,
    rx: BTreeMap<[u8; 8], Vec<[u8; 32]>>,
    legacy_receive_nonce_reuse: bool,
    receive_nonce_reuses: BTreeMap<u8, u64>,
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
        Self::create(key, nonce_base, context, epoch, false)
    }

    /// Receive-only room inspection can include relayed self-member packets.
    /// The caller still checks socket source and roster membership before any
    /// audio delivery. This instance can never acquire transmit permission.
    pub fn new_receive_only(
        key: [u8; 32],
        nonce_base: u64,
        context: MediaContext,
        epoch: MediaEpoch,
    ) -> Result<Self> {
        Self::create(key, nonce_base, context, epoch, true)
    }

    /// Legacy relay reception tolerates authenticated nonce reuse without
    /// resetting counters or granting transmit permission. Exact ciphertext
    /// replays are filtered within the receive window; excess variants for one
    /// nonce are dropped rather than closing the receiver.
    pub fn new_legacy_receive_only(
        key: [u8; 32],
        nonce_base: u64,
        context: MediaContext,
        epoch: MediaEpoch,
    ) -> Result<Self> {
        let mut channel = Self::create(key, nonce_base, context, epoch, true)?;
        channel.legacy_receive_nonce_reuse = true;
        Ok(channel)
    }

    fn create(
        key: [u8; 32],
        nonce_base: u64,
        context: MediaContext,
        epoch: MediaEpoch,
        receive_only: bool,
    ) -> Result<Self> {
        epoch.validate()?;
        ensure!(
            context.room_id != 0
                && nonce_base != 0
                && (receive_only || context.own_member != context.peer_member),
            "Invalid media room/member context"
        );
        Ok(Self {
            key: Zeroizing::new(key),
            base: Zeroizing::new(nonce_base),
            context,
            epoch,
            tx_scope: None,
            receive_only,
            first_tx_audio: 0,
            next_audio: 0,
            next_control: 0,
            clear_sequence: 0,
            rx_highest: BTreeMap::new(),
            rx_seen: BTreeMap::new(),
            rx: BTreeMap::new(),
            legacy_receive_nonce_reuse: false,
            receive_nonce_reuses: BTreeMap::new(),
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
        ensure!(!self.receive_only, "Receive-only media cannot transmit");
        ensure!(
            self.tx_scope.is_none(),
            "Media transmit scope already installed"
        );
        match &scope {
            TransmitScope::VerifiedGeneration { key_generation }
            | TransmitScope::LegacyRelayAudio { key_generation } => ensure!(
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
        if matches!(scope, TransmitScope::LegacyRelayAudio { .. }) {
            self.first_tx_audio = self.legacy_audio_start()?;
            self.next_audio = u32::from(self.first_tx_audio);
            self.legacy_receive_nonce_reuse = true;
        }
        self.tx_scope = Some(scope);
        Ok(())
    }
    fn legacy_audio_start(&self) -> Result<u16> {
        let key = PKey::hmac(&self.key[..])?;
        let mut mac = Signer::new(MessageDigest::sha256(), &key)?;
        mac.update(b"agent-wechat/live96/audio-start/v1")?;
        mac.update(&self.context.room_id.to_le_bytes())?;
        mac.update(&self.base.to_le_bytes())?;
        mac.update(&self.context.own_member.to_le_bytes())?;
        let digest = Zeroizing::new(mac.sign_to_vec()?);
        let prefix = u32::from_le_bytes(digest[..4].try_into()?);
        Ok((prefix % u32::from(MAX_LEGACY_AUDIO_START) + 1) as u16)
    }
    fn tx_check(&mut self, current: MediaEpoch, now_ms: u64) -> Result<()> {
        self.check(current)?;
        match &self.tx_scope {
            None => bail!("Media transmission is not authorized"),
            Some(
                TransmitScope::VerifiedGeneration { .. } | TransmitScope::LegacyRelayAudio { .. },
            ) => Ok(()),
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
                Some(
                    TransmitScope::LegacyRelayAudio { .. }
                        | TransmitScope::IsolatedCompatibilityTest {
                            allow_known_audio_overlap: true,
                            ..
                        }
                )
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
    pub fn received_nonce_reuses(&self) -> &BTreeMap<u8, u64> {
        &self.receive_nonce_reuses
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
        let member = unverified_live96_member(packet)?;
        ensure!(
            u64::from_le_bytes(packet[2..10].try_into()?) == self.context.room_id
                && member == self.context.peer_member,
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
        ensure!(
            clear.len() > 2
                && clear.len() <= 1484
                && u16::from_le_bytes(clear[..2].try_into()?) == self.context.peer_member,
            "Protected media member mismatch or extent"
        );
        let digest: [u8; 32] = openssl::sha::sha256(&packet[HEADER..]);
        if let Some(previous) = self.rx.get(&nonce) {
            if previous.contains(&digest) {
                return Ok(None);
            }
            if !self.legacy_receive_nonce_reuse {
                self.close();
                bail!("Authenticated media nonce conflict");
            }
            let count = self.receive_nonce_reuses.entry(kind).or_default();
            *count = count.saturating_add(1);
            if previous.len() >= MAX_NONCE_VARIANTS {
                return Ok(None);
            }
        }
        let own_next = if kind == 0x10 {
            self.next_audio
        } else if kind == 0x70 {
            self.next_control
        } else {
            0
        };
        if u32::from(sequence) < own_next && (kind != 0x10 || sequence >= self.first_tx_audio) {
            self.diagnostic_audio_overlap(kind)?;
        }
        // Retire every authenticated, sender-bound nonce before payload classification.
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
        self.rx.entry(nonce).or_default().push(digest);
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
            [8, 1, 18, 2, 8, 1] => {
                // Leave the probe unanswered if a reply would consume an
                // inbound nonce. The sender's reuse fence stays unchanged.
                if self.next_control <= u32::from(u16::MAX)
                    && self.rx_seen.get(&0x70).is_some_and(|seen| {
                        let sequence = self.next_control as usize;
                        seen[sequence / 64] & (1u64 << (sequence % 64)) != 0
                    })
                {
                    return Ok(None);
                }
                Ok(Some(self.seal(
                    current,
                    now_ms,
                    0x70,
                    &[8, 1, 18, 2, 8, 2],
                )?))
            }
            [8, 1, 18, 2, 8, 0 | 2] => Ok(None),
            // Other authenticated control messages have no implemented reply.
            // Ignore them without inventing a response or stopping audio.
            _ => Ok(None),
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

/// Member metadata accompanying one nested audio payload. The member list does
/// not establish independent PCM tracks or prove that every listed member spoke.
pub struct AggregateAudio<'a> {
    pub members: Vec<(u16, u8)>,
    pub nested_kind: u8,
    pub nested_payload: &'a [u8],
}

/// Header marker for classifying an ordinary audio variant. This does not
/// validate its codec body or grant decoder admission.
pub fn ordinary_audio_marker(payload: &[u8]) -> Option<[u8; 2]> {
    (payload.len() >= 6 && payload[0] == 1).then(|| [payload[4], payload[5]])
}

pub fn parse_aggregate_audio(payload: &[u8]) -> Result<AggregateAudio<'_>> {
    ensure!(
        payload.len() >= 3 && payload.len() <= 1482 && payload[0] == 0x65,
        "Invalid aggregate audio extent"
    );
    let count = usize::from(payload[1]);
    ensure!(count <= 128, "Too many aggregate audio members");
    let end = 2 + 3 * count;
    ensure!(
        end + 1 < payload.len() && matches!(payload[end], 1 | 19),
        "Truncated or unsupported aggregate audio"
    );
    let members = payload[2..end]
        .chunks_exact(3)
        .map(|entry| (u16::from_le_bytes([entry[0], entry[1]]), entry[2]))
        .collect();
    Ok(AggregateAudio {
        members,
        nested_kind: payload[end],
        nested_payload: &payload[end + 1..],
    })
}

/// Configured ordinary95/74 N1 sender for format106.
pub fn ordinary95_single(
    metadata: u8,
    sequence: u16,
    timestamp: u32,
    frame: &[u8],
) -> Result<Vec<u8>> {
    ensure!(
        !frame.is_empty() && frame.len() <= 1023 && 15 + frame.len() <= MAX_CONF,
        "Invalid ordinary95 primary extent"
    );
    let mut out = Vec::with_capacity(15 + frame.len());
    out.extend([1, metadata, 0, 0, 0x95, 0x74]);
    out.extend(sequence.to_be_bytes());
    out.extend(timestamp.to_be_bytes());
    out.push(1);
    out.extend((frame.len() as i16).to_le_bytes());
    out.extend(frame);
    Ok(out)
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
    #[test]
    fn aggregate_keeps_member_metadata_separate_from_single_nested_payload() {
        let bytes = [0x65, 2, 0x34, 0x12, 7, 0x78, 0x56, 9, 1, 0xaa];
        let aggregate = parse_aggregate_audio(&bytes).unwrap();
        assert_eq!(aggregate.members, [(0x1234, 7), (0x5678, 9)]);
        assert_eq!(aggregate.nested_kind, 1);
        assert_eq!(aggregate.nested_payload, [0xaa]);
        for length in 0..bytes.len() {
            assert!(parse_aggregate_audio(&bytes[..length]).is_err());
        }
        assert!(parse_aggregate_audio(&[0x65, 0, 2, 1]).is_err());
    }

    #[test]
    fn receive_only_context_observes_protected_self_member_without_transmit_permission() {
        let mut receive = Live96::new_receive_only(
            [7; 32],
            11,
            MediaContext {
                own_member: 1,
                peer_member: 1,
                ..context()
            },
            epoch(),
        )
        .unwrap();
        let raw = ordinary95_single(0, 0, 0, &[1]).unwrap();
        let mut sending = peer();
        let packet = sending.seal_audio(epoch(), 1, &raw).unwrap();
        assert!(receive.open(epoch(), &packet).unwrap().is_some());
        assert!(receive
            .authorize_transmit(
                epoch(),
                TransmitScope::LegacyRelayAudio { key_generation: 1 }
            )
            .is_err());
        assert!(receive.seal_audio(epoch(), 1, &raw).is_err());
    }
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
    fn legacy_audio_start_depends_on_the_authenticated_member() {
        let mut local = receiver();
        let mut peer_context = context();
        std::mem::swap(&mut peer_context.own_member, &mut peer_context.peer_member);
        let mut remote = Live96::new([7; 32], 11, peer_context, epoch()).unwrap();
        for channel in [&mut local, &mut remote] {
            channel
                .authorize_transmit(
                    epoch(),
                    TransmitScope::LegacyRelayAudio { key_generation: 1 },
                )
                .unwrap();
        }
        // Independent HMAC-SHA256 vectors for the same room and two member IDs.
        assert_eq!(local.first_tx_audio, 440);
        assert_eq!(remote.first_tx_audio, 429);
        assert_eq!(local.next_audio, 440);
        assert_eq!(remote.next_audio, 429);
    }

    #[test]
    fn legacy_relay_allows_opposite_direction_audio_overlap_in_either_order() {
        let raw = ordinary95_pair(0, 1, 320, &[1], &[2]).unwrap();
        for transmit_first in [false, true] {
            let mut local = receiver();
            local
                .authorize_transmit(
                    epoch(),
                    TransmitScope::LegacyRelayAudio { key_generation: 1 },
                )
                .unwrap();
            let mut remote = peer();
            remote.next_audio = u32::from(local.first_tx_audio);
            let inbound = remote.seal_audio(epoch(), 0, &raw).unwrap();
            if transmit_first {
                local.seal_audio(epoch(), 0, &raw).unwrap();
            }
            assert_eq!(
                local.open(epoch(), &inbound).unwrap().unwrap().payload(),
                raw
            );
            if !transmit_first {
                local.seal_audio(epoch(), 0, &raw).unwrap();
            }
            assert!(local.open(epoch(), &inbound).unwrap().is_none());
            assert_eq!(local.known_audio_overlaps(), 1);

            let mut conflicting_peer = peer();
            conflicting_peer.next_audio = u32::from(local.first_tx_audio);
            let different = ordinary95_pair(0, 1, 320, &[3], &[4]).unwrap();
            let conflict = conflicting_peer.seal_audio(epoch(), 0, &different).unwrap();
            assert_eq!(
                local.open(epoch(), &conflict).unwrap().unwrap().payload(),
                different
            );
            assert!(local.open(epoch(), &conflict).unwrap().is_none());
            assert!(local.open(epoch(), &inbound).unwrap().is_none());
            assert_eq!(local.received_nonce_reuses().get(&0x10), Some(&1));
            assert!(local.seal_audio(epoch(), 0, &raw).is_ok());
        }
    }

    #[test]
    fn legacy_receive_reuse_filters_all_retained_variants_without_closing() {
        for kind in [0x10, 0x70] {
            let mut receive =
                Live96::new_legacy_receive_only([7; 32], 11, context(), epoch()).unwrap();
            let mut packets = Vec::new();
            for variant in 0..=MAX_NONCE_VARIANTS {
                let packet = peer().seal(epoch(), 0, kind, &[8, variant as u8]).unwrap();
                let opened = receive.open(epoch(), &packet).unwrap();
                assert_eq!(opened.is_some(), variant < MAX_NONCE_VARIANTS);
                packets.push(packet);
            }
            for packet in &packets {
                assert!(receive.open(epoch(), packet).unwrap().is_none());
            }
            assert_eq!(receive.rx.len(), 1);
            assert_eq!(
                receive.rx.values().next().unwrap().len(),
                MAX_NONCE_VARIANTS
            );
            let mut sending = peer();
            sending.seal(epoch(), 0, kind, &[8, 0]).unwrap();
            let next = sending.seal(epoch(), 0, kind, &[8, 1]).unwrap();
            assert!(receive.open(epoch(), &next).unwrap().is_some());
            assert!(receive
                .authorize_transmit(
                    epoch(),
                    TransmitScope::LegacyRelayAudio { key_generation: 1 }
                )
                .is_err());
        }
    }

    #[test]
    fn strict_receive_conflicting_ciphertext_still_closes() {
        let first = peer().seal(epoch(), 0, 0x70, &[8, 1]).unwrap();
        let different = peer().seal(epoch(), 0, 0x70, &[8, 2]).unwrap();
        let mut receive = receiver();
        receive.open(epoch(), &first).unwrap();
        assert!(receive.open(epoch(), &different).is_err());
        assert!(receive.open(epoch(), &first).is_err());
    }

    #[test]
    fn forged_clear_sender_cannot_poison_a_legacy_receivers_nonce_ledger() {
        let mut receive = Live96::new_legacy_receive_only([7; 32], 11, context(), epoch()).unwrap();
        let mut sending = peer();
        let first = sending.seal(epoch(), 0, 0x70, &[8, 1]).unwrap();
        receive.open(epoch(), &first).unwrap();
        let mut wrong = Live96::new(
            [7; 32],
            11,
            MediaContext {
                own_member: 2,
                ..context()
            },
            epoch(),
        )
        .unwrap();
        wrong
            .authorize_transmit(
                epoch(),
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        let mut forged = wrong.seal(epoch(), 0, 0x70, &[8, 2]).unwrap();
        forged[10..12].copy_from_slice(&context().peer_member.to_le_bytes());
        assert_eq!(
            receive.open(epoch(), &forged).err().unwrap().to_string(),
            "Protected media member mismatch or extent"
        );
        assert!(receive.received_nonce_reuses().is_empty());
        assert!(receive.open(epoch(), &first).unwrap().is_none());
        let next = sending.seal(epoch(), 0, 0x70, &[8, 3]).unwrap();
        assert!(receive.open(epoch(), &next).unwrap().is_some());
    }

    #[test]
    fn legacy_relay_keeps_control_and_send_exhaustion_fences() {
        let mut local = receiver();
        local
            .authorize_transmit(
                epoch(),
                TransmitScope::LegacyRelayAudio { key_generation: 1 },
            )
            .unwrap();
        let mut remote = peer();
        local.seal(epoch(), 0, 0x70, &[8, 1, 18, 2, 8, 2]).unwrap();
        let control = remote.seal(epoch(), 0, 0x70, &[8, 1, 18, 2, 8, 1]).unwrap();
        assert!(local.open(epoch(), &control).is_err());

        let mut local = receiver();
        local
            .authorize_transmit(
                epoch(),
                TransmitScope::LegacyRelayAudio { key_generation: 1 },
            )
            .unwrap();
        local.next_audio = u32::from(u16::MAX);
        let raw = ordinary95_pair(0, 1, 320, &[1], &[2]).unwrap();
        local.seal_audio(epoch(), 0, &raw).unwrap();
        assert!(local.seal_audio(epoch(), 0, &raw).is_err());
    }

    #[test]
    fn original_aead_matches_independent_literal_vectors() {
        // Independent original eight-byte-nonce ChaCha/Poly1305 known answers.
        // This tests the primitive only, not a relay envelope or key lifecycle.
        let vectors = [
            (
                "43b718d5a398bf36a5f3904a469febac12f7b5b22efa1ae5cc5deb54e75f5574",
                "f06be3d7749e299c",
                "",
                "a02210edb5c4fd736d7366ae81089103",
            ),
            (
                "9456a10f0c4b0644e9524fedcc329f052c443f55df5d911f4c172e07121e3740",
                "beb908121cb81960",
                "67736a080137aef7b92f53145b6ec56622",
                "4576fc20a665cd99f668062c319677cd82a2bbd02898dbd27db411e002570cf3d5",
            ),
            (
                "4b0fdc9081934247c7e945a10ca2ef52989dad5a2477fe041a6780e06d9254ed",
                "948fb8afa967f654",
                "1169e988a0972d513ca73734aad2e21f053d292b78d3eb9d3a6486da320d3e9bc67871bb80fdceb0f18b197aa8862b69b2b83993bab8bff256ff7c87ff9a486beb",
                "ff673095b4835ccdbbd73719c1351799309fca1063534ccd3fb8a5a5b4492b282ef72fa53cb0af85ed86900773e081dcd78ae54215970a122fef411d62561ad3dd67a359f949c8d666c180287b67f297d3",
            ),
        ];
        for (key_hex, nonce_hex, plaintext_hex, expected_hex) in vectors {
            let key = hex::decode(key_hex).unwrap().try_into().unwrap();
            let nonce = hex::decode(nonce_hex).unwrap().try_into().unwrap();
            let plaintext = hex::decode(plaintext_hex).unwrap();
            let expected = hex::decode(expected_hex).unwrap();
            assert_eq!(seal_original(&key, nonce, &plaintext).unwrap(), expected);
            assert_eq!(&*open_original(&key, nonce, &expected).unwrap(), &plaintext);
        }
    }
    #[test]
    fn literal_primary_layout_and_extent() {
        let single = ordinary95_single(3, 1, 320, &[1, 2]).unwrap();
        assert_eq!(
            single,
            [1, 3, 0, 0, 0x95, 0x74, 0, 1, 0, 0, 1, 64, 1, 2, 0, 1, 2]
        );
        assert_eq!(
            parse_ordinary95(&single).unwrap().primaries,
            vec![&[1, 2][..]]
        );
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
    fn strict_scope_rejects_overlap_and_isolated_scope_expires() {
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
        // The same collision fails closed when local transmission happens first.
        let mut sender_first = self::receiver();
        sender_first
            .authorize_transmit(
                epoch(),
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        sender_first.seal_audio(epoch(), 0, &raw).unwrap();
        assert!(sender_first.open(epoch(), &packet).is_err());
        assert!(sender_first.seal_audio(epoch(), 1, &raw).is_err());
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
    fn authenticated_controls_preserve_nonce_fence_and_ignore_unhandled_messages() {
        let mut other_peer = peer();
        let other = other_peer.seal(epoch(), 0, 0x70, &[8, 7]).unwrap();
        let mut other_receiver = receiver();
        let opened = other_receiver.open(epoch(), &other).unwrap().unwrap();
        assert!(other_receiver
            .reply_to_probe(epoch(), 0, &opened)
            .unwrap()
            .is_none());
        assert!(other_receiver.check(epoch()).is_ok());

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
        assert!(receiver
            .reply_to_probe(epoch(), 0, &opened)
            .unwrap()
            .is_none());
        assert!(receiver.check(epoch()).is_ok());
        // Omitting a reply does not disable the underlying reuse fence.
        assert!(receiver
            .seal(epoch(), 0, 0x70, &[8, 1, 18, 2, 8, 2])
            .is_err());
    }
}
