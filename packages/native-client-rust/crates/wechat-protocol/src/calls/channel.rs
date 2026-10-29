//! Experimental, offline relay packet codecs for call media.
//!
//! These helpers do not negotiate a call, select a codec, open a socket or obtain
//! keys. Key provenance, peer direction and key/base transitions still need
//! verification before this module can be connected to a live transport. The
//! sender stops at sequence exhaustion instead of guessing a new key or base.
//!
//! The envelope uses the original eight-byte-nonce ChaCha20-Poly1305
//! construction. It is incompatible with the IETF AEAD construction. Only the
//! payload is authenticated here: callers must check room/member context, and
//! must not treat the opaque header fields as authenticated metadata.

use anyhow::{bail, ensure, Result};
use openssl::{
    memcmp,
    pkey::{Id, PKey},
    sign::Signer,
    symm::{Cipher, Crypter, Mode},
};
use zeroize::Zeroizing;

/// The maximum plaintext size accepted by the recovered mode-6 receive helper.
pub const MAX_RELAY_PAYLOAD_BYTES: usize = 1500;
const TAG_BYTES: usize = 16;
const MODE5_HEADER_BYTES: usize = 15;
const MODE6_HEADER_BYTES: usize = 17;
const MAX_RELAY_PACKET_BYTES: usize = MODE6_HEADER_BYTES + MAX_RELAY_PAYLOAD_BYTES + TAG_BYTES;

/// Two recovered relay envelope layouts, independent of audio/video selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayFormat {
    Mode5,
    Mode6,
}

impl RelayFormat {
    fn header_bytes(self) -> usize {
        match self {
            Self::Mode5 => MODE5_HEADER_BYTES,
            Self::Mode6 => MODE6_HEADER_BYTES,
        }
    }
}

/// Clear packet header. Opaque fields retain their bytes without inferred names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayHeader {
    pub format: RelayFormat,
    pub room_id: u64,
    pub member_id: u8,
    pub opaque_u16: u16,
    /// Present only in mode 6; mode 5 requires this field to be zero.
    pub opaque_u8: u8,
    /// A channel packet type, not a voice/video call-kind enum.
    pub packet_type: u8,
    pub sequence: u16,
}

impl RelayHeader {
    /// Encode a canonical header without a payload or authentication tag.
    pub fn encode(self) -> Result<Vec<u8>> {
        let mut output = Vec::with_capacity(self.format.header_bytes());
        match self.format {
            RelayFormat::Mode5 => {
                ensure!(self.opaque_u8 == 0, "Mode 5 has no extra opaque byte");
                output.push(0x75);
                output.extend_from_slice(&self.opaque_u16.to_be_bytes());
                output.extend_from_slice(&self.room_id.to_be_bytes());
                output.push(self.member_id);
            }
            RelayFormat::Mode6 => {
                output.extend_from_slice(&[0x97, MODE6_HEADER_BYTES as u8]);
                output.extend_from_slice(&self.room_id.to_be_bytes());
                output.extend_from_slice(&[self.member_id, self.opaque_u8]);
                output.extend_from_slice(&self.opaque_u16.to_be_bytes());
            }
        }
        output.push(self.packet_type);
        output.extend_from_slice(&self.sequence.to_be_bytes());
        Ok(output)
    }

    /// Decode only the canonical 15/17-byte layouts at the start of a packet.
    ///
    /// Longer mode-6 header variants are rejected until their extension fields
    /// are understood. This function does not authenticate or admit a packet.
    pub fn decode(packet: &[u8]) -> Result<Self> {
        match packet.first() {
            Some(0x75) => {
                ensure!(
                    packet.len() >= MODE5_HEADER_BYTES,
                    "Truncated mode-5 header"
                );
                Ok(Self {
                    format: RelayFormat::Mode5,
                    room_id: u64::from_be_bytes(packet[3..11].try_into()?),
                    member_id: packet[11],
                    opaque_u16: u16::from_be_bytes(packet[1..3].try_into()?),
                    opaque_u8: 0,
                    packet_type: packet[12],
                    sequence: u16::from_be_bytes(packet[13..15].try_into()?),
                })
            }
            Some(0x97) => {
                ensure!(
                    packet.len() >= MODE6_HEADER_BYTES,
                    "Truncated mode-6 header"
                );
                ensure!(
                    usize::from(packet[1]) == MODE6_HEADER_BYTES,
                    "Unsupported mode-6 header extension"
                );
                Ok(Self {
                    format: RelayFormat::Mode6,
                    room_id: u64::from_be_bytes(packet[2..10].try_into()?),
                    member_id: packet[10],
                    opaque_u16: u16::from_be_bytes(packet[12..14].try_into()?),
                    opaque_u8: packet[11],
                    packet_type: packet[14],
                    sequence: u16::from_be_bytes(packet[15..17].try_into()?),
                })
            }
            _ => bail!("Unsupported relay packet header"),
        }
    }
}

/// Non-secret fixed header values for a sender. Sequence numbers are assigned
/// by [`RelaySender`] and cannot be supplied or reset by its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelaySendContext {
    pub format: RelayFormat,
    pub room_id: u64,
    pub member_id: u8,
    pub opaque_u16: u16,
    pub opaque_u8: u8,
}

/// Required context checks for the mode-6 receiver. The key alone does not bind
/// the clear room/member fields to the encrypted payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayReceiveContext {
    pub room_id: u64,
    pub member_id: u8,
}

struct KeyMaterial {
    key: Zeroizing<[u8; 32]>,
    nonce_base: Zeroizing<u64>,
}

impl KeyMaterial {
    fn new(key: [u8; 32], nonce_base: u64) -> Self {
        Self {
            key: Zeroizing::new(key),
            nonce_base: Zeroizing::new(nonce_base),
        }
    }

    fn nonce(&self, packet_type: u8, sequence: u16) -> [u8; 8] {
        self.nonce_base
            .wrapping_add(u64::from(packet_type) << 32)
            .wrapping_add(u64::from(sequence))
            .to_le_bytes()
    }
}

/// An experimental send-key owner with non-repeating, non-wrapping sequences.
///
/// There is intentionally no clone, serialization, reset or automatic rekey API.
/// Reconstructing another sender with the same key/base would defeat this guard.
/// A live integration must first establish exclusive ownership, direction and a
/// verified new key/base epoch; this module cannot establish those facts.
pub struct RelaySender {
    material: KeyMaterial,
    context: RelaySendContext,
    next_sequences: [u32; 20],
}

impl RelaySender {
    pub fn new(key: [u8; 32], nonce_base: u64, context: RelaySendContext) -> Result<Self> {
        ensure!(
            context.format != RelayFormat::Mode5 || context.opaque_u8 == 0,
            "Mode 5 has no extra opaque byte"
        );
        Ok(Self {
            material: KeyMaterial::new(key, nonce_base),
            context,
            next_sequences: [1; 20],
        })
    }

    /// Seal one offline payload, consuming its sequence even if encryption fails.
    /// Sequence zero and a guessed key/base transition are never emitted.
    pub fn seal(&mut self, packet_type: u8, payload: &[u8]) -> Result<Vec<u8>> {
        ensure!(
            !payload.is_empty() && payload.len() <= MAX_RELAY_PAYLOAD_BYTES,
            "Relay payload length is outside the supported range"
        );
        let next = &mut self.next_sequences[counter_index(packet_type)];
        ensure!(
            *next <= u32::from(u16::MAX),
            "Relay sequence exhausted; verified rekey required"
        );
        let sequence = u16::try_from(*next)?;
        // Reserve before any cryptographic operation. Errors must not make a
        // nonce available for another attempt with possibly different bytes.
        *next += 1;
        let header = RelayHeader {
            format: self.context.format,
            room_id: self.context.room_id,
            member_id: self.context.member_id,
            opaque_u16: self.context.opaque_u16,
            opaque_u8: self.context.opaque_u8,
            packet_type,
            sequence,
        };
        let mut packet = header.encode()?;
        packet.extend_from_slice(&seal_payload(
            &self.material,
            self.material.nonce(packet_type, sequence),
            payload,
        )?);
        Ok(packet)
    }
}

/// A successfully authenticated mode-6 payload. This is still encoded channel
/// data, not decoded audio/video or a proof of negotiated media compatibility.
pub struct OpenedRelayPacket {
    pub header: RelayHeader,
    payload: Zeroizing<Vec<u8>>,
}

impl OpenedRelayPacket {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    pub fn into_payload(self) -> Zeroizing<Vec<u8>> {
        self.payload
    }
}

/// Experimental receiver for canonical mode-6 packets from one expected member.
///
/// A 64-packet replay window is a local safety policy, not an established peer
/// requirement. It accepts modest reordering, rejects duplicates/old packets
/// and never interprets a sequence rollover as a new key epoch. Only successful
/// authentication advances the window. No live transport uses this type yet.
pub struct RelayReceiver {
    material: KeyMaterial,
    context: RelayReceiveContext,
    replay: [ReplayWindow; 256],
}

impl RelayReceiver {
    pub fn new(key: [u8; 32], nonce_base: u64, context: RelayReceiveContext) -> Self {
        Self {
            material: KeyMaterial::new(key, nonce_base),
            context,
            replay: [ReplayWindow::default(); 256],
        }
    }

    pub fn open(&mut self, packet: &[u8]) -> Result<OpenedRelayPacket> {
        ensure!(
            (MODE6_HEADER_BYTES + TAG_BYTES..=MAX_RELAY_PACKET_BYTES).contains(&packet.len()),
            "Relay packet length is outside the supported range"
        );
        let header = RelayHeader::decode(packet)?;
        ensure!(
            header.format == RelayFormat::Mode6,
            "Mode-5 receive is not implemented"
        );
        ensure!(
            header.room_id == self.context.room_id && header.member_id == self.context.member_id,
            "Relay packet does not match the expected room/member"
        );
        let nonce_type = receive_nonce_type(header.packet_type);
        self.replay[usize::from(nonce_type)].check(header.sequence)?;
        let payload = open_payload(
            &self.material,
            self.material.nonce(nonce_type, header.sequence),
            &packet[MODE6_HEADER_BYTES..],
        )?;
        self.replay[usize::from(nonce_type)].record(header.sequence);
        Ok(OpenedRelayPacket { header, payload })
    }
}

#[derive(Clone, Copy, Default)]
struct ReplayWindow {
    highest: Option<u16>,
    seen: u64,
}

impl ReplayWindow {
    fn check(self, sequence: u16) -> Result<()> {
        if let Some(highest) = self.highest {
            if sequence <= highest {
                let distance = u32::from(highest - sequence);
                ensure!(
                    distance < u64::BITS,
                    "Relay packet is outside the replay window"
                );
                ensure!(
                    self.seen & (1u64 << distance) == 0,
                    "Duplicate relay packet"
                );
            }
        }
        Ok(())
    }

    fn record(&mut self, sequence: u16) {
        match self.highest {
            None => {
                self.highest = Some(sequence);
                self.seen = 1;
            }
            Some(highest) if sequence > highest => {
                let distance = u32::from(sequence - highest);
                self.seen = self.seen.checked_shl(distance).unwrap_or(0) | 1;
                self.highest = Some(sequence);
            }
            Some(highest) => self.seen |= 1u64 << u32::from(highest - sequence),
        }
    }
}

fn counter_index(packet_type: u8) -> usize {
    match packet_type {
        0xd0 => 0,
        0xd1 => 1,
        0xd5 => 2,
        0xd6 => 3,
        0xd9 => 4,
        0x50 => 5,
        0x51 => 6,
        0x52 => 7,
        0x53 => 8,
        0x54 => 9,
        0x55 => 10,
        0x76 => 11,
        0x77 | 0x79 => 12,
        0x93 => 13,
        0x6a => 14,
        0x80 => 15,
        0x78 => 16,
        0x83 => 17,
        0x84 => 18,
        _ => 19,
    }
}

fn receive_nonce_type(packet_type: u8) -> u8 {
    match packet_type {
        0x51 => 0x76,
        0x55 | 0x56 => 0x51,
        _ => packet_type,
    }
}

fn chacha_xor(
    material: &KeyMaterial,
    nonce: [u8; 8],
    counter: u64,
    input: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let mut iv = Zeroizing::new([0; 16]);
    iv[..8].copy_from_slice(&counter.to_le_bytes());
    iv[8..].copy_from_slice(&nonce);
    // EVP_chacha20 takes a 64-bit little-endian block counter followed by the
    // original 64-bit nonce. EVP_chacha20_poly1305 has a different construction.
    let cipher = Cipher::chacha20();
    let mut crypter = Crypter::new(
        cipher,
        Mode::Encrypt,
        material.key.as_ref(),
        Some(iv.as_ref()),
    )?;
    crypter.pad(false);
    let mut output = Zeroizing::new(vec![0; input.len() + cipher.block_size()]);
    let count = crypter.update(input, &mut output)?;
    let final_count = crypter.finalize(&mut output[count..])?;
    output.truncate(count + final_count);
    ensure!(
        output.len() == input.len(),
        "Unexpected ChaCha20 output size"
    );
    Ok(output)
}

fn payload_tag(
    material: &KeyMaterial,
    nonce: [u8; 8],
    ciphertext: &[u8],
) -> Result<[u8; TAG_BYTES]> {
    let one_time_key = chacha_xor(material, nonce, 0, &[0; 32])?;
    let key = PKey::private_key_from_raw_bytes(&one_time_key, Id::POLY1305)?;
    let mut signer = Signer::new_without_digest(&key)?;
    // Original construction: AAD || LE64(AAD length) || C || LE64(C length).
    // Here AAD is empty. Do not replace this with the IETF padded transcript.
    signer.update(&0u64.to_le_bytes())?;
    signer.update(ciphertext)?;
    signer.update(&u64::try_from(ciphertext.len())?.to_le_bytes())?;
    let mut tag = [0; TAG_BYTES];
    ensure!(
        signer.sign(&mut tag)? == TAG_BYTES,
        "Unexpected Poly1305 tag size"
    );
    Ok(tag)
}

fn seal_payload(material: &KeyMaterial, nonce: [u8; 8], payload: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        payload.len() <= MAX_RELAY_PAYLOAD_BYTES,
        "Relay payload is too large"
    );
    let ciphertext = chacha_xor(material, nonce, 1, payload)?;
    let tag = payload_tag(material, nonce, &ciphertext)?;
    let mut output = Vec::with_capacity(ciphertext.len() + TAG_BYTES);
    output.extend_from_slice(&ciphertext);
    output.extend_from_slice(&tag);
    Ok(output)
}

fn open_payload(
    material: &KeyMaterial,
    nonce: [u8; 8],
    encrypted: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        (TAG_BYTES..=MAX_RELAY_PAYLOAD_BYTES + TAG_BYTES).contains(&encrypted.len()),
        "Encrypted relay payload length is outside the supported range"
    );
    let (ciphertext, tag) = encrypted.split_at(encrypted.len() - TAG_BYTES);
    let expected = payload_tag(material, nonce, ciphertext)?;
    ensure!(
        memcmp::eq(tag, &expected),
        "Relay payload authentication failed"
    );
    // Authentication must precede producing any plaintext.
    chacha_xor(material, nonce, 1, ciphertext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(hexadecimal: &str) -> Vec<u8> {
        hex::decode(hexadecimal).unwrap()
    }

    fn key() -> [u8; 32] {
        std::array::from_fn(|index| index as u8)
    }

    fn header(format: RelayFormat) -> RelayHeader {
        RelayHeader {
            format,
            room_id: 0x0123_4567_89ab_cdef,
            member_id: 0x42,
            opaque_u16: 0x2468,
            opaque_u8: if format == RelayFormat::Mode6 {
                0x83
            } else {
                0
            },
            packet_type: 0x50,
            sequence: 0x1357,
        }
    }

    fn sender(format: RelayFormat) -> RelaySender {
        let h = header(format);
        RelaySender::new(
            key(),
            0x0123_4567_89ab_cdef,
            RelaySendContext {
                format,
                room_id: h.room_id,
                member_id: h.member_id,
                opaque_u16: h.opaque_u16,
                opaque_u8: h.opaque_u8,
            },
        )
        .unwrap()
    }

    fn receiver() -> RelayReceiver {
        RelayReceiver::new(
            key(),
            0x0123_4567_89ab_cdef,
            RelayReceiveContext {
                room_id: 0x0123_4567_89ab_cdef,
                member_id: 0x42,
            },
        )
    }

    fn incoming_packet(sequence: u16, packet_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut h = header(RelayFormat::Mode6);
        h.sequence = sequence;
        h.packet_type = packet_type;
        let material = KeyMaterial::new(key(), 0x0123_4567_89ab_cdef);
        let mut packet = h.encode().unwrap();
        packet.extend_from_slice(
            &seal_payload(
                &material,
                material.nonce(receive_nonce_type(packet_type), sequence),
                payload,
            )
            .unwrap(),
        );
        packet
    }

    #[test]
    fn literal_headers_keep_opaque_fields_and_byte_order() {
        for (format, expected) in [
            (RelayFormat::Mode5, "7524680123456789abcdef42501357"),
            (RelayFormat::Mode6, "97110123456789abcdef42832468501357"),
        ] {
            let h = header(format);
            assert_eq!(h.encode().unwrap(), bytes(expected));
            assert_eq!(RelayHeader::decode(&bytes(expected)).unwrap(), h);
        }
    }

    #[test]
    fn truncated_and_unknown_headers_are_rejected() {
        for format in [RelayFormat::Mode5, RelayFormat::Mode6] {
            let full = header(format).encode().unwrap();
            for end in 0..full.len() {
                assert!(RelayHeader::decode(&full[..end]).is_err());
            }
        }
        let mut extended = header(RelayFormat::Mode6).encode().unwrap();
        extended[1] = 21;
        extended.extend_from_slice(b"EXT!");
        assert!(RelayHeader::decode(&extended).is_err());
        extended[0] = 0x96;
        assert!(RelayHeader::decode(&extended).is_err());
        let mut invalid = header(RelayFormat::Mode5);
        invalid.opaque_u8 = 1;
        assert!(invalid.encode().is_err());
    }

    #[test]
    fn original_aead_matches_independent_literal_vectors() {
        // Synthetic known-answer bytes independently cross-checked against an
        // existing original-ChaCha20-Poly1305 implementation and matching
        // reference packet helpers. No account or captured key material.
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
            let material = KeyMaterial::new(bytes(key_hex).try_into().unwrap(), 0);
            let nonce = bytes(nonce_hex).try_into().unwrap();
            let plaintext = bytes(plaintext_hex);
            let expected = bytes(expected_hex);
            assert_eq!(
                seal_payload(&material, nonce, &plaintext).unwrap(),
                expected
            );
            assert_eq!(
                open_payload(&material, nonce, &expected)
                    .unwrap()
                    .as_slice(),
                plaintext
            );
        }
    }

    #[test]
    fn full_packet_literals_match_both_envelopes() {
        let ciphertext_tag = "7a1bb2c6cba8b91b5779628a774d955652587b9cf26727e299676edf3efd491b616abd824b50f2ced6a1f1959dd28f";
        for (format, header_hex) in [
            (RelayFormat::Mode5, "7524680123456789abcdef42501357"),
            (RelayFormat::Mode6, "97110123456789abcdef42832468501357"),
        ] {
            let mut sender = sender(format);
            sender.next_sequences[counter_index(0x50)] = 0x1357;
            let packet = sender
                .seal(0x50, b"offline synthetic media payload")
                .unwrap();
            let expected = bytes(&format!("{header_hex}{ciphertext_tag}"));
            assert_eq!(packet, expected);
            if format == RelayFormat::Mode6 {
                let opened = receiver().open(&packet).unwrap();
                assert_eq!(opened.header, header(format));
                assert_eq!(opened.payload(), b"offline synthetic media payload");
            } else {
                assert!(receiver().open(&packet).is_err());
            }
        }
    }

    #[test]
    fn send_counter_domains_and_aliases_are_preserved() {
        let mut sender = sender(RelayFormat::Mode6);
        for (packet_type, sequence) in [(0x50, 1), (0x50, 2), (0x51, 1), (0x77, 1), (0x79, 2)] {
            let packet = sender.seal(packet_type, b"synthetic").unwrap();
            let header = RelayHeader::decode(&packet).unwrap();
            assert_eq!(
                (header.packet_type, header.sequence),
                (packet_type, sequence)
            );
        }
    }

    #[test]
    fn send_sequence_exhaustion_never_wraps_or_resets() {
        let mut sender = sender(RelayFormat::Mode6);
        sender.next_sequences[counter_index(0x50)] = u32::from(u16::MAX);
        let last = sender.seal(0x50, b"last synthetic packet").unwrap();
        assert_eq!(RelayHeader::decode(&last).unwrap().sequence, u16::MAX);
        assert!(sender.seal(0x50, b"must fail").is_err());
        assert!(sender.seal(0x50, b"must still fail").is_err());
        assert_eq!(sender.next_sequences[counter_index(0x50)], 65536);
    }

    #[test]
    fn sender_rejects_payload_limits_before_allocating_a_sequence() {
        let mut sender = sender(RelayFormat::Mode6);
        assert!(sender.seal(0x50, &[]).is_err());
        assert!(sender
            .seal(0x50, &vec![0; MAX_RELAY_PAYLOAD_BYTES + 1])
            .is_err());
        assert_eq!(sender.next_sequences[counter_index(0x50)], 1);
        let maximum = vec![0x6a; MAX_RELAY_PAYLOAD_BYTES];
        let packet = sender.seal(0x50, &maximum).unwrap();
        assert_eq!(packet.len(), MAX_RELAY_PACKET_BYTES);
        assert_eq!(receiver().open(&packet).unwrap().payload(), maximum);
        let mut oversized = packet;
        oversized.push(0);
        assert!(receiver().open(&oversized).is_err());
    }

    #[test]
    fn tag_ciphertext_type_sequence_and_wrong_context_fail() {
        let packet = incoming_packet(7, 0x50, b"synthetic plaintext must not escape on failure");
        for index in MODE6_HEADER_BYTES..packet.len() {
            let mut modified = packet.clone();
            modified[index] ^= 1;
            assert!(receiver().open(&modified).is_err());
        }
        for index in [2, 10, 14, 15, 16] {
            let mut modified = packet.clone();
            modified[index] ^= 1;
            assert!(receiver().open(&modified).is_err());
        }
        for end in 0..MODE6_HEADER_BYTES + TAG_BYTES {
            assert!(receiver().open(&packet[..end]).is_err());
        }
    }

    #[test]
    fn failed_authentication_does_not_consume_the_receive_sequence() {
        let mut receiver = receiver();
        let packet = incoming_packet(7, 0x50, b"synthetic");
        let mut modified = packet.clone();
        *modified.last_mut().unwrap() ^= 1;
        assert!(receiver.open(&modified).is_err());
        assert_eq!(receiver.open(&packet).unwrap().payload(), b"synthetic");
        assert!(receiver.open(&packet).is_err());
    }

    #[test]
    fn replay_window_accepts_reordering_but_not_duplicates_or_rollover() {
        let mut receiver = receiver();
        assert!(receiver.open(&incoming_packet(100, 0x50, b"newer")).is_ok());
        assert!(receiver
            .open(&incoming_packet(99, 0x50, b"reordered"))
            .is_ok());
        assert!(receiver
            .open(&incoming_packet(99, 0x50, b"duplicate"))
            .is_err());
        assert!(receiver
            .open(&incoming_packet(36, 0x50, b"too old"))
            .is_err());
        assert!(receiver
            .open(&incoming_packet(u16::MAX, 0x50, b"last"))
            .is_ok());
        assert!(receiver
            .open(&incoming_packet(0, 0x50, b"not a new epoch"))
            .is_err());
    }

    #[test]
    fn receive_type_aliases_share_one_nonce_replay_domain() {
        let mut receiver = receiver();
        let first = incoming_packet(17, 0x55, b"one authenticated nonce");
        let alias = incoming_packet(17, 0x56, b"one authenticated nonce");
        assert_eq!(&first[MODE6_HEADER_BYTES..], &alias[MODE6_HEADER_BYTES..]);
        assert_eq!(
            receiver.open(&first).unwrap().payload(),
            b"one authenticated nonce"
        );
        assert!(receiver.open(&alias).is_err());
        assert!(receiver
            .open(&incoming_packet(18, 0x56, b"next nonce"))
            .is_ok());
        assert_eq!(receive_nonce_type(0x51), 0x76);
    }
}
