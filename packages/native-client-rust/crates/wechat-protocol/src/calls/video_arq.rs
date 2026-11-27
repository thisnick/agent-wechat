//! Sender side of video ARQ: answering a peer's `0x06` NACK with `0x67` retransmissions.
//!
//! The sender keeps each RSM video packet it sent for a short time, keyed by its 16-bit
//! media packet sequence (`pkg_seq`). When the receiver NACKs a half-open `[start, end)`
//! range, each cached packet in the range is sent again once, unchanged, after a one-byte
//! `0x67` prefix (see [`encode_arq_response`]). Packets older than [`ARQ_CACHE_MAX_AGE_MS`]
//! or beyond [`ARQ_CACHE_MAX_PACKETS`] are dropped, and each packet is retransmitted at
//! most once, matching the official sender's cache.

use crate::calls::video_packet::{parse_rsm_packet_header, VideoNack, VIDEO_ARQ_RESPONSE_PREFIX};
use std::collections::VecDeque;

/// How long a sent packet stays available for retransmission.
pub const ARQ_CACHE_MAX_AGE_MS: u64 = 300;
/// Most sent packets kept for retransmission.
pub const ARQ_CACHE_MAX_PACKETS: usize = 200;
/// Most retransmissions produced for one NACK.
pub const ARQ_MAX_RETRANSMITS_PER_NACK: usize = 64;

struct CachedPacket {
    pkg_seq: u16,
    sent_ms: u64,
    retransmitted: bool,
    packet: Vec<u8>,
}

/// Recently sent RSM video packets that a peer's NACK can ask for again.
#[derive(Default)]
pub struct VideoArqSendCache {
    packets: VecDeque<CachedPacket>,
}

impl VideoArqSendCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember one RSM video packet sent at `now_ms`. Packets without a media sequence
    /// (orientation metadata, padding) are not cached.
    pub fn record_sent(&mut self, packet: &[u8], now_ms: u64) {
        let Ok(header) = parse_rsm_packet_header(packet) else {
            return;
        };
        let Some(pkg_seq) = header.pkg_seq else {
            return;
        };
        self.expire(now_ms);
        if self.packets.len() >= ARQ_CACHE_MAX_PACKETS {
            self.packets.pop_front();
        }
        self.packets.push_back(CachedPacket {
            pkg_seq,
            sent_ms: now_ms,
            retransmitted: false,
            packet: packet.to_vec(),
        });
    }

    /// Return the `0x67` ARQ responses that answer `nack` at `now_ms`, oldest first.
    ///
    /// Only camera NACKs in the official format (`format == 0`) for `stream_id` are
    /// answered. Each cached packet is returned at most once over its lifetime.
    pub fn answer(&mut self, nack: &VideoNack, stream_id: u8, now_ms: u64) -> Vec<Vec<u8>> {
        self.expire(now_ms);
        if nack.format != 0 || nack.is_screen != 0 || nack.target_stream_id != stream_id & 0x0f {
            return Vec::new();
        }
        let mut out = Vec::new();
        for cached in self.packets.iter_mut() {
            if out.len() >= ARQ_MAX_RETRANSMITS_PER_NACK {
                break;
            }
            if cached.retransmitted
                || !nack
                    .ranges
                    .iter()
                    .any(|&(start, end)| seq_in_range(cached.pkg_seq, start, end))
            {
                continue;
            }
            cached.retransmitted = true;
            out.push(encode_arq_response(&cached.packet));
        }
        out
    }

    fn expire(&mut self, now_ms: u64) {
        while self
            .packets
            .front()
            .is_some_and(|p| now_ms.saturating_sub(p.sent_ms) > ARQ_CACHE_MAX_AGE_MS)
        {
            self.packets.pop_front();
        }
    }
}

/// Wrap one original RSM video packet as a `0x67` ARQ response payload.
pub fn encode_arq_response(packet: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(packet.len() + 1);
    out.push(VIDEO_ARQ_RESPONSE_PREFIX);
    out.extend_from_slice(packet);
    out
}

/// Whether `seq` lies in the wrapping half-open range `[start, end)`.
fn seq_in_range(seq: u16, start: u16, end: u16) -> bool {
    seq.wrapping_sub(start) < end.wrapping_sub(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::video_packet::{
        classify_video_channel_payload, encode_video_nack, parse_video_nack, RsmVideoPacketizer,
        VideoChannelPayload, VideoWireCodec,
    };

    /// Synthetic H.264 P-frames split into small RSM packets by our own packetizer.
    fn sent_packets(count: usize) -> Vec<Vec<u8>> {
        let mut packetizer = RsmVideoPacketizer::new(0, 1).with_chunk_bytes(80).unwrap();
        let mut access_unit = vec![0, 0, 0, 1, 0x41];
        access_unit.extend_from_slice(&[0x9a; 300]);
        let mut packets = Vec::new();
        let mut timestamp_ms = 0;
        while packets.len() < count {
            timestamp_ms += 66;
            packets.extend(
                packetizer
                    .packetize(VideoWireCodec::H264, &access_unit, timestamp_ms, 15, 300)
                    .unwrap(),
            );
        }
        packets.truncate(count);
        packets
    }

    fn nack_for(ranges: &[(u16, u16)]) -> VideoNack {
        parse_video_nack(&encode_video_nack(1, 0, 1, ranges)).unwrap()
    }

    fn seq_of(packet: &[u8]) -> u16 {
        parse_rsm_packet_header(packet).unwrap().pkg_seq.unwrap()
    }

    #[test]
    fn answers_nacked_packets_once_with_the_original_bytes() {
        let packets = sent_packets(6);
        let mut cache = VideoArqSendCache::new();
        for packet in &packets {
            cache.record_sent(packet, 10);
        }
        let start = seq_of(&packets[2]);
        let responses = cache.answer(&nack_for(&[(start, start.wrapping_add(2))]), 1, 50);
        assert_eq!(responses.len(), 2);
        for (response, original) in responses.iter().zip(&packets[2..4]) {
            let VideoChannelPayload::ArqResponse(inner) =
                classify_video_channel_payload(response).unwrap()
            else {
                panic!("expected an ARQ response");
            };
            assert_eq!(inner, original.as_slice());
        }
        // The official sender retransmits each packet only once.
        assert!(cache
            .answer(&nack_for(&[(start, start.wrapping_add(2))]), 1, 60)
            .is_empty());
    }

    #[test]
    fn ignores_expired_packets_other_streams_and_nonzero_format() {
        let packets = sent_packets(3);
        let mut cache = VideoArqSendCache::new();
        for packet in &packets {
            cache.record_sent(packet, 0);
        }
        let start = seq_of(&packets[0]);
        let range = [(start, start.wrapping_add(3))];
        assert!(cache.answer(&nack_for(&range), 2, 10).is_empty());
        let mut wrong_format = nack_for(&range);
        wrong_format.format = 1;
        assert!(cache.answer(&wrong_format, 1, 10).is_empty());
        assert!(cache
            .answer(&nack_for(&range), 1, ARQ_CACHE_MAX_AGE_MS + 1)
            .is_empty());
    }

    #[test]
    fn half_open_ranges_wrap_around_the_sequence_space() {
        assert!(seq_in_range(0xffff, 0xfffe, 1));
        assert!(seq_in_range(0, 0xfffe, 1));
        assert!(!seq_in_range(1, 0xfffe, 1));
        assert!(!seq_in_range(5, 5, 5));
    }
}
