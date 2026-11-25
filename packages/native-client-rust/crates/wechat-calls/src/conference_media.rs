//! Authenticated conference source decoding. Devices, sockets and persistence
//! belong to the embedding runtime; no packet can announce its own participant.
use crate::{
    audio::{AudioDecoder, AudioFormat},
    opus::OpusDecoder,
    ordering::RtpOrder,
    video_stream::VideoStreamReceiver,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, MediaEpoch},
    relay::SelectedRelay,
    rudp::{self, RudpAckTracker},
    video_packet::{
        classify_video_channel_payload, ReassembledVideoFrame, VideoChannelPayload, VideoP2sReport,
    },
    video_qos::ScheduledIRequest,
};

/// Outcome of consuming one conference relay datagram, including audio, video and RTCP signals.
#[derive(Default)]
pub struct ConferenceConsumeOutput {
    /// Decoded Opus PCM frames from this datagram.
    pub audio: Vec<DecodedAudio>,
    /// Reassembled video frame completed by this datagram, if any.
    pub video: Option<ReassembledVideoFrame>,
    /// True when the remote peer requested a keyframe (`0x66 IRequest`).
    pub peer_requested_keyframe: bool,
    /// True when a video frame assembly dropped and the local side should send `0x66 IRequest`.
    pub need_keyframe: bool,
    /// `(stream_id, max_pkt_num)` to acknowledge when the datagram was in-band RUDP
    /// (`0x50`) data from this member.
    pub rudp_ack: Option<(u32, u64)>,
}

pub struct DecodedAudio {
    pub sequence: u64,
    pub sample_offset: u64,
    pub samples: Vec<i16>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamReport {
    pub authenticated_packets: u64,
    pub replayed_packets: u64,
    pub authenticated_nonce_reuses: BTreeMap<u8, u64>,
    pub invalid_packets: u64,
    pub invalid_while_inactive: u64,
    pub invalid_reasons: BTreeMap<&'static str, u64>,
    pub control_packets: u64,
    pub decoded_frames: u64,
    pub nonzero_frames: u64,
    pub peak_magnitude: u16,
    pub ordering_drops: u64,
    pub unsupported_audio: u64,
    pub feedback_packets: u64,
    pub unsupported_lengths: BTreeMap<usize, u64>,
    pub unsupported_reasons: BTreeMap<&'static str, u64>,
    pub payload_kinds: BTreeMap<u8, u64>,
    pub codec_markers: BTreeMap<String, u64>,
    pub aggregate_packets: u64,
    pub malformed_aggregates: u64,
    pub aggregate_member_hints: BTreeMap<u16, u64>,
    pub nonzero_second_bins: BTreeMap<u16, u64>,
    pub joined_events: u64,
    pub left_events: u64,
    pub identity_quarantined: bool,
    pub decode_errors: u64,
}

const MAX_READY_VIDEO_FRAMES: usize = 8;

pub struct ConferenceReceiver {
    channel: Live96,
    decoder: OpusDecoder,
    order: RtpOrder,
    video: VideoStreamReceiver,
    ready_video: VecDeque<ReassembledVideoFrame>,
    video_clock_anchored: bool,
    pub report: StreamReport,
    timeline: Option<(u32, u64)>,
    sequence: u64,
    decoding: bool,
    rudp_acks: RudpAckTracker,
}

impl ConferenceReceiver {
    pub fn new(selected: &SelectedRelay, member: u16, epoch: MediaEpoch) -> Result<Self> {
        Ok(Self {
            channel: Live96::new_legacy_receive_only(
                *selected.media_key,
                selected.room_key,
                MediaContext {
                    room_id: selected.room_id,
                    own_member: selected.own_member,
                    peer_member: member,
                    connection_id: selected.connection_id,
                },
                epoch,
            )?,
            decoder: OpusDecoder::new(AudioFormat::new(16000, 1, 20)?)?,
            order: RtpOrder::new(),
            video: VideoStreamReceiver::new(0),
            ready_video: VecDeque::new(),
            video_clock_anchored: false,
            report: StreamReport::default(),
            timeline: None,
            sequence: 0,
            decoding: true,
            rudp_acks: RudpAckTracker::default(),
        })
    }

    /// Anchor video P2S/recovery clocks at `now_ms` for a newly joined or rejoined member.
    pub(crate) fn anchor_video_clock(&mut self, now_ms: u64) {
        if !self.video_clock_anchored {
            self.video.reset(now_ms);
            self.video_clock_anchored = true;
        }
    }

    /// Last video stream ID observed on an authenticated inbound `RsmVideo` packet from this member.
    pub(crate) fn last_video_stream_id(&self) -> Option<u8> {
        self.video.last_stream_id()
    }

    /// Latest unwrapped 64-bit `total_pkg_seq` observed on inbound RSM video packets from this member.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last_video_total_pkg_seq(&self) -> Option<u64> {
        self.video.last_total_pkg_seq()
    }

    /// Poll due `0x06` ARQ/NACK missing packet sequence ranges for this member.
    pub(crate) fn poll_due_nack(&mut self, now_ms: u64) -> Option<(u8, Vec<(u16, u16)>)> {
        self.video.poll_due_nack(now_ms)
    }

    pub(crate) fn has_active_video_window(&self) -> bool {
        self.video.has_active_video_window()
    }

    pub(crate) fn is_video_keyframe_synced(&self) -> bool {
        self.video.is_keyframe_synced()
    }

    pub(crate) fn queue_local_keyframe_request(&mut self, retries: u8) {
        self.video.queue_local_keyframe_request(retries);
    }

    pub(crate) fn poll_due_irequests(
        &mut self,
        local_member: u16,
        target_member: u16,
        now_ms: u64,
    ) -> Vec<ScheduledIRequest> {
        self.video
            .poll_due_irequests(local_member, target_member, now_ms)
    }

    pub(crate) fn snapshot_video_p2s(
        &mut self,
        member_id: u16,
        conn_id: u32,
        codec_support_mask: u32,
        now_ms: u64,
    ) -> VideoP2sReport {
        self.video
            .snapshot_p2s(member_id, conn_id, codec_support_mask, now_ms)
    }

    /// Pop the next buffered in-order video frame that is ready for delivery after [`Self::consume_media`].
    pub fn pop_ready_video(&mut self) -> Option<ReassembledVideoFrame> {
        self.ready_video.pop_front()
    }

    pub fn consume(
        &mut self,
        epoch: MediaEpoch,
        packet: &[u8],
        elapsed_ms: u64,
        decode_audio: bool,
    ) -> Result<Vec<DecodedAudio>> {
        let out = self.consume_media(epoch, packet, elapsed_ms, decode_audio)?;
        self.ready_video.clear();
        Ok(out.audio)
    }

    /// Consume one conference relay packet and return decoded audio, reassembled video
    /// and keyframe-request signals.
    pub fn consume_media(
        &mut self,
        epoch: MediaEpoch,
        packet: &[u8],
        elapsed_ms: u64,
        decode_audio: bool,
    ) -> Result<ConferenceConsumeOutput> {
        let mut out = ConferenceConsumeOutput::default();
        let result = self.channel.open(epoch, packet);
        self.report.authenticated_nonce_reuses = self.channel.received_nonce_reuses().clone();
        let opened = match result {
            Ok(Some(opened)) => opened,
            Ok(None) => {
                self.report.replayed_packets += 1;
                return Ok(out);
            }
            Err(error) => {
                self.report.invalid_packets += 1;
                self.report.invalid_while_inactive += u64::from(!decode_audio);
                let reason = match error.to_string().as_str() {
                    "Call media authentication failed" => "authentication",
                    "Authenticated media nonce conflict" => "same_sender_nonce_conflict",
                    "Media generation changed or closed" => "closed_or_stale",
                    "Media room/member mismatch" => "room_or_clear_member",
                    "Protected media member mismatch or extent" => "protected_member",
                    "Unsupported Live96 packet family or extension" => "family_or_extension",
                    _ => "other",
                };
                *self.report.invalid_reasons.entry(reason).or_default() += 1;
                return Ok(out);
            }
        };
        self.report.authenticated_packets += 1;
        if matches!(opened.packed_kind & 0xf0, 0x20 | 0x60) {
            if let Ok(kind) = classify_video_channel_payload(opened.payload()) {
                match kind {
                    VideoChannelPayload::IRequest(req) => {
                        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                            eprintln!(
                                "[wechat-call-video] conf rx ireq magic=0x{:02x} kind=0x{:02x} ext={} seq={} from={} stream={} target={} tstream={} ts={} raw={:02x?}",
                                opened.magic(),
                                opened.packed_kind,
                                opened.extension_byte(),
                                opened.sequence32(),
                                req.local_member,
                                req.local_stream_id,
                                req.target_member,
                                req.target_stream_id,
                                req.timestamp_ms,
                                opened.payload()
                            );
                        }
                        out.peer_requested_keyframe = true;
                    }
                    VideoChannelPayload::Nack(raw_nack) => {
                        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                            eprintln!(
                                "[wechat-call-video] conf rx nack magic=0x{:02x} kind=0x{:02x} ext={} seq={} len={} raw={:02x?}",
                                opened.magic(),
                                opened.packed_kind,
                                opened.extension_byte(),
                                opened.sequence32(),
                                raw_nack.len(),
                                raw_nack
                            );
                        }
                        self.report.feedback_packets += 1;
                    }
                    VideoChannelPayload::RsmVideo(raw) => {
                        if (opened.packed_kind & 0xf0) != 0x20 {
                            self.report.control_packets += 1;
                            return Ok(out);
                        }
                        self.anchor_video_clock(elapsed_ms);
                        let batch =
                            self.video
                                .ingest_rsm(opened.packed_kind & 0x0f, raw, elapsed_ms);
                        out.need_keyframe = batch.assembly_dropped;
                        self.ready_video.extend(batch.frames);
                        while self.ready_video.len() > MAX_READY_VIDEO_FRAMES {
                            self.ready_video.pop_front();
                        }
                        out.video = self.ready_video.pop_front();
                    }
                }
            }
            return Ok(out);
        }
        if opened.packed_kind == 0x50 {
            // Official members retransmit in-band RUDP data until it is acknowledged.
            if let Ok(Some(data)) = rudp::parse_rudp_data(opened.payload()) {
                out.rudp_ack = self
                    .rudp_acks
                    .record(data.stream_id, data.pkt_num)
                    .map(|max_pkt| (data.stream_id, max_pkt));
            }
            self.report.control_packets += 1;
            return Ok(out);
        }
        if opened.packed_kind != 0x10 {
            self.report.control_packets += 1;
            return Ok(out);
        }
        if let Some(kind) = opened.payload().first() {
            *self.report.payload_kinds.entry(*kind).or_default() += 1;
        }
        if opened.payload().first() == Some(&2) {
            self.report.feedback_packets += 1;
            return Ok(out);
        }
        if opened.payload().first() == Some(&0x65) {
            match audio_packet::parse_aggregate_audio(opened.payload()) {
                Ok(aggregate) => {
                    self.report.aggregate_packets += 1;
                    for (member, _) in aggregate.members {
                        if self.report.aggregate_member_hints.len() < 128
                            || self.report.aggregate_member_hints.contains_key(&member)
                        {
                            *self
                                .report
                                .aggregate_member_hints
                                .entry(member)
                                .or_default() += 1;
                        }
                    }
                }
                Err(_) => self.report.malformed_aggregates += 1,
            }
            // One nested aggregate payload is not separate per-member PCM.
            return Ok(out);
        }
        if self.decoding != decode_audio {
            self.decoder.reset()?;
            self.decoding = decode_audio;
        }
        if let Some(marker) = audio_packet::ordinary_audio_marker(opened.payload()) {
            let key = format!("{:02x}{:02x}", marker[0], marker[1]);
            if self.report.codec_markers.len() < 16 || self.report.codec_markers.contains_key(&key)
            {
                *self.report.codec_markers.entry(key).or_default() += 1;
            }
        }
        // Only roster-admitted ordinary audio reaches its source decoder.
        let primary = match audio_packet::parse_ordinary95(opened.payload()) {
            Ok(primary) => primary,
            Err(error) => {
                self.report.unsupported_audio += 1;
                let length = opened.payload().len();
                if self.report.unsupported_lengths.len() < 16
                    || self.report.unsupported_lengths.contains_key(&length)
                {
                    *self.report.unsupported_lengths.entry(length).or_default() += 1;
                }
                let reason = match error.to_string().as_str() {
                    "Unsupported ordinary95 audio shape" => "shape",
                    "Invalid ordinary95 primary count" => "primary_count",
                    "Invalid ordinary95 primary length" => "primary_length",
                    "Truncated ordinary95 primary" => "truncated_primary",
                    "Trailing ordinary95 bytes" => "trailing_bytes",
                    _ => "other",
                };
                *self.report.unsupported_reasons.entry(reason).or_default() += 1;
                return Ok(out);
            }
        };
        let admitted = match self.order.admit(
            primary.last_sequence,
            primary.last_timestamp,
            primary.primaries.len(),
        ) {
            Ok(admitted) => admitted,
            Err(_) => {
                self.report.ordering_drops += 1;
                return Ok(out);
            }
        };
        let count = primary.primaries.len();
        let first_timestamp = primary
            .last_timestamp
            .wrapping_sub((count as u32 - 1) * 320);
        let first_offset = (elapsed_ms / 20)
            .saturating_mul(320)
            .saturating_sub((count as u64 - 1) * 320);
        let (origin, offset) = *self.timeline.get_or_insert((first_timestamp, first_offset));
        for (index, frame) in primary
            .primaries
            .into_iter()
            .enumerate()
            .skip(admitted.skip)
        {
            let timestamp = first_timestamp.wrapping_add(index as u32 * 320);
            let delta = timestamp.wrapping_sub(origin);
            let sequence = self.sequence;
            self.sequence = self
                .sequence
                .checked_add(1)
                .context("Conference PCM sequence exhausted")?;
            if !decode_audio || delta >= 1 << 31 {
                continue;
            }
            let sample_offset = offset
                .checked_add(u64::from(delta))
                .context("Conference presentation clock exhausted")?;
            let pcm = match self.decoder.decode(frame) {
                Ok(pcm) => pcm,
                Err(_) => {
                    self.report.decode_errors += 1;
                    self.decoder.reset()?;
                    continue;
                }
            };
            self.report.decoded_frames += 1;
            let peak = pcm
                .iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap_or(0);
            self.report.peak_magnitude = self.report.peak_magnitude.max(peak);
            self.report.nonzero_frames += u64::from(peak > 8);
            if peak > 8 {
                *self
                    .report
                    .nonzero_second_bins
                    .entry((elapsed_ms / 1000).min(u64::from(u16::MAX)) as u16)
                    .or_default() += 1;
            }
            out.audio.push(DecodedAudio {
                sequence,
                sample_offset,
                samples: pcm,
            });
        }
        Ok(out)
    }

    pub fn close(&mut self) {
        self.channel.close();
    }

    /// New application track incarnation; keep authenticated replay/order state
    /// and the shared presentation clock, while discarding codec history.
    pub fn reset_decoder(&mut self) -> Result<()> {
        self.video.reset(0);
        self.video_clock_anchored = false;
        self.ready_video.clear();
        self.decoder.reset().map_err(Into::into)
    }
}

impl Drop for ConferenceReceiver {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_protocol::calls::{
        audio_packet::TransmitScope,
        video_packet::{encode_irequest, RsmVideoPacketizer, VideoWireCodec},
    };

    #[test]
    fn video_stream_id_ignores_irequest_and_nack_and_tracks_rsm_video() {
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:6000".parse().unwrap(),
            media_key: zeroize::Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let mut rx = ConferenceReceiver::new(&selected, 1, epoch).unwrap();
        let mut peer_tx = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_tx
            .authorize_transmit(
                epoch,
                TransmitScope::LegacyRelayAudio {
                    key_generation: epoch.key_generation,
                },
            )
            .unwrap();

        // Peer IRequest rewritten by relay onto 0x21 (carrying our stream 1) must not set last_video_stream_id.
        let ireq = encode_irequest(1, 0, 1, 100, 1);
        let ireq_pkt = peer_tx.seal_video(epoch, 100, 1, false, &ireq).unwrap();
        let out_ireq = rx.consume_media(epoch, &ireq_pkt, 100, true).unwrap();
        assert!(out_ireq.peer_requested_keyframe);
        assert_eq!(rx.last_video_stream_id(), None);

        // Peer NACK on 0x21 must not set last_video_stream_id.
        let nack = [0x06, 1, 0, 0, 0, 0];
        let nack_pkt = peer_tx.seal_video(epoch, 120, 1, false, &nack).unwrap();
        let _out_nack = rx.consume_media(epoch, &nack_pkt, 120, true).unwrap();
        assert_eq!(rx.last_video_stream_id(), None);
        assert_eq!(rx.report.feedback_packets, 1);

        // Actual RSM video packet on stream 2 records stream 2.
        let mut packetizer = RsmVideoPacketizer::new(1, 2);
        let mut annexb = vec![0, 0, 0, 1, 0x26, 0x01];
        annexb.extend_from_slice(&[0x55; 32]);
        let chunks = packetizer
            .packetize(VideoWireCodec::H265, &annexb, 140, 15, 350)
            .unwrap();
        let mut saw_frame = false;
        for (idx, chunk) in chunks.iter().enumerate() {
            let pkt = peer_tx
                .seal_video(epoch, 140 + idx as u64, 2, true, chunk)
                .unwrap();
            let out = rx
                .consume_media(epoch, &pkt, 140 + idx as u64, true)
                .unwrap();
            if let Some(frame) = out.video {
                assert_eq!(frame.stream_id, 2);
                saw_frame = true;
            }
        }
        assert_eq!(rx.last_video_stream_id(), Some(2));
        assert_eq!(rx.last_video_total_pkg_seq(), Some(1));
        assert!(saw_frame);
    }

    #[test]
    fn rudp_data_from_a_member_requests_an_acknowledgement() {
        use wechat_protocol::{calls::rudp::encode_rudp_feedback, protobuf as p};
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:6000".parse().unwrap(),
            media_key: zeroize::Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let mut rx = ConferenceReceiver::new(&selected, 1, epoch).unwrap();
        let mut peer_tx = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_tx
            .authorize_transmit(
                epoch,
                TransmitScope::LegacyRelayAudio {
                    key_generation: epoch.key_generation,
                },
            )
            .unwrap();

        let cmd = [p::uint(1, 2), p::bytes(2, &[8, 9])].concat();
        let raw = [p::uint(1, 0), p::uint(2, 5), p::bytes(3, &cmd)].concat();
        let data = [
            p::uint(1, 1),
            p::uint(2, 0),
            p::uint(3, 1234),
            p::bytes(4, &raw),
            p::uint(5, 0),
        ]
        .concat();
        let packet = peer_tx.seal_rudp(epoch, 10, &data).unwrap().unwrap();
        let out = rx.consume_media(epoch, &packet, 10, true).unwrap();
        assert_eq!(out.rudp_ack, Some((0, 5)));

        // A feedback packet from the member needs no acknowledgement.
        let feedback = encode_rudp_feedback(1, 0, 3, 0).unwrap();
        let packet = peer_tx.seal_rudp(epoch, 20, &feedback).unwrap().unwrap();
        let out = rx.consume_media(epoch, &packet, 20, true).unwrap();
        assert_eq!(out.rudp_ack, None);
    }

    #[test]
    fn unknown_0x60_rtcp_payload_is_counted_as_control_not_rsm_video() {
        use zeroize::Zeroizing;
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:8080".parse().unwrap(),
            media_key: Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let mut rx = ConferenceReceiver::new(&selected, 1, epoch).unwrap();
        let mut peer_tx = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_tx
            .authorize_transmit(
                epoch,
                TransmitScope::LegacyRelayAudio {
                    key_generation: epoch.key_generation,
                },
            )
            .unwrap();

        // Unknown 0x61 RTCP subtype (leading byte 0x77, not 0x66/0x06) must never enter RSM ingest.
        let unknown_rtcp = [0x77u8; 28];
        let pkt = peer_tx
            .seal_video_rtcp(epoch, 100, 1, &unknown_rtcp)
            .unwrap();
        let out = rx.consume_media(epoch, &pkt, 100, true).unwrap();
        assert!(!out.need_keyframe);
        assert!(out.video.is_none());
        assert_eq!(rx.last_video_stream_id(), None);
        assert_eq!(rx.report.control_packets, 1);
    }

    #[test]
    fn conference_receiver_telemetry_clears_on_idle_window_and_decoder_reset() {
        use zeroize::Zeroizing;
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:8081".parse().unwrap(),
            media_key: Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let mut rx = ConferenceReceiver::new(&selected, 1, epoch).unwrap();
        let mut peer_tx = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_tx
            .authorize_transmit(
                epoch,
                TransmitScope::LegacyRelayAudio {
                    key_generation: epoch.key_generation,
                },
            )
            .unwrap();

        // Self-contained H.265 VPS + IDR so keyframe sync completes inside consume_media.
        let mut packetizer = RsmVideoPacketizer::new(1, 1);
        let idr = vec![
            0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01, 0, 0, 0, 1, 0x26, 0x01, 0xaa, 0xbb, 0xcc, 0xdd,
        ];
        let chunks = packetizer
            .packetize(VideoWireCodec::H265, &idr, 100, 15, 350)
            .unwrap();
        let pkt = peer_tx.seal_video(epoch, 100, 1, true, &chunks[0]).unwrap();
        let out = rx.consume_media(epoch, &pkt, 100, true).unwrap();
        assert!(out.video.is_some());
        assert!(rx.is_video_keyframe_synced());
        assert!(rx.has_active_video_window());

        let rep1 = rx.snapshot_video_p2s(1, 1, 0x1d, 1100);
        assert_eq!(rep1.play_frames, 1);
        assert!(!rx.has_active_video_window());

        // Snapshotting an idle camera-off window resets anchors without reporting freeze.
        let idle = rx.snapshot_video_p2s(1, 1, 0x1d, 5100);
        assert_eq!(idle.heavy_freeze_cnt, 0);
        assert_eq!(idle.actual_member_num, 0);

        // Rejoin / reset_decoder resets video telemetry and keyframe sync completely.
        rx.reset_decoder().unwrap();
        assert!(!rx.is_video_keyframe_synced());
        let after_reset = rx.snapshot_video_p2s(1, 1, 0x1d, 6100);
        assert_eq!(after_reset.play_frames, 0);
    }

    #[test]
    fn late_join_and_rejoin_anchor_p2s_window_and_audio_only_consume_clears_ready_video() {
        use zeroize::Zeroizing;
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:8082".parse().unwrap(),
            media_key: Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let mut rx = ConferenceReceiver::new(&selected, 1, epoch).unwrap();
        let mut peer_tx = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_tx
            .authorize_transmit(
                epoch,
                TransmitScope::LegacyRelayAudio {
                    key_generation: epoch.key_generation,
                },
            )
            .unwrap();

        // Member joins 120 seconds into the conference sending pre-sync H.264 P-frames.
        // Anchoring on first video arrival ensures window_ms is ~550 ms, NOT 120_550 ms.
        let mut pktz = RsmVideoPacketizer::new(1, 1);
        let p_frame = vec![0, 0, 0, 1, 0x41, 0x9a, 0x11, 0x22, 0x33, 0x44];
        for i in 0..8u64 {
            let at = 120_000 + i * 66;
            let chunks = pktz
                .packetize(VideoWireCodec::H264, &p_frame, at as u32, 15, 500)
                .unwrap();
            let pkt = peer_tx.seal_video(epoch, at, 1, true, &chunks[0]).unwrap();
            let _ = rx.consume_media(epoch, &pkt, at, true).unwrap();
        }
        let late_rep = rx.snapshot_video_p2s(1, 1, 0x1d, 120_550);
        assert!(late_rep.recv_fps >= 12);
        assert_eq!(late_rep.play_fps, 0);
        assert_eq!(late_rep.heavy_freeze_cnt, 1);
        assert!(late_rep.heavy_freeze_time_ratio >= 80);

        // Member departs and rejoins another 120 seconds later (`reset_decoder()`):
        // window_start_ms re-anchors to 240_000 ms rather than freezing at 120_550 ms.
        rx.reset_decoder().unwrap();
        let idr = vec![
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 0, 1, 0x65, 0x88,
            0xaa, 0xbb,
        ];
        for i in 0..10u64 {
            let at = 240_000 + i * 66;
            let frame = if i == 0 { &idr } else { &p_frame };
            let chunks = pktz
                .packetize(VideoWireCodec::H264, frame, at as u32, 15, 500)
                .unwrap();
            let pkt = peer_tx.seal_video(epoch, at, 1, true, &chunks[0]).unwrap();
            let _ = rx.consume_media(epoch, &pkt, at, true).unwrap();
        }
        let rejoin_rep = rx.snapshot_video_p2s(1, 1, 0x1d, 240_700);
        assert!(rejoin_rep.play_fps >= 12);
        assert_eq!(rejoin_rep.heavy_freeze_cnt, 0);

        // Multi-frame release (completing a 2-packet IDR while a newer P-frame is buffered
        // behind it) must emit frames in FIFO order via consume_media + pop_ready_video, and
        // audio-only consume() must clear the extra buffered frames rather than retaining them.
        let mut multi_pktz = RsmVideoPacketizer::new(1, 1)
            .with_orientation(1)
            .with_chunk_bytes(64)
            .unwrap();
        let mut two_pkt_idr = idr.clone();
        two_pkt_idr.extend_from_slice(&[0x55; 96]);
        let idr_pair = multi_pktz
            .packetize(VideoWireCodec::H264, &two_pkt_idr, 241_000, 15, 500)
            .unwrap();
        assert!(idr_pair.len() >= 2);
        let trailing_p = multi_pktz
            .packetize(VideoWireCodec::H264, &p_frame, 241_066, 15, 500)
            .unwrap();
        assert_eq!(trailing_p.len(), 1);

        // Positive FIFO order check via consume_media: send IDR pkt 0, then P-frame (buffered
        // behind incomplete IDR), then remaining IDR packets so both release on the final packet.
        let d0 = peer_tx
            .seal_video(epoch, 241_000, 1, true, &idr_pair[0])
            .unwrap();
        assert!(rx
            .consume_media(epoch, &d0, 241_000, true)
            .unwrap()
            .video
            .is_none());
        let dp = peer_tx
            .seal_video(epoch, 241_020, 1, true, &trailing_p[0])
            .unwrap();
        assert!(rx
            .consume_media(epoch, &dp, 241_020, true)
            .unwrap()
            .video
            .is_none());
        let mut released_first = None;
        for (idx, chunk) in idr_pair.iter().enumerate().skip(1) {
            let at = 241_030 + idx as u64;
            let d = peer_tx.seal_video(epoch, at, 1, true, chunk).unwrap();
            let out = rx.consume_media(epoch, &d, at, true).unwrap();
            if out.video.is_some() {
                released_first = out.video;
            }
        }
        let f_idr = released_first.expect("final IDR packet must release IDR first");
        assert!(f_idr.is_keyframe);
        let f_p = rx
            .pop_ready_video()
            .expect("buffered P-frame must follow IDR in FIFO order");
        assert!(!f_p.is_keyframe);
        assert!(rx.pop_ready_video().is_none());

        // Audio-only consume() receiving the same 2-frame release burst must clear ready_video.
        let idr_pair2 = multi_pktz
            .packetize(VideoWireCodec::H264, &two_pkt_idr, 242_000, 15, 500)
            .unwrap();
        let trailing_p2 = multi_pktz
            .packetize(VideoWireCodec::H264, &p_frame, 242_066, 15, 500)
            .unwrap();
        let d2_0 = peer_tx
            .seal_video(epoch, 242_000, 1, true, &idr_pair2[0])
            .unwrap();
        let _ = rx.consume(epoch, &d2_0, 242_000, false).unwrap();
        let d2_p = peer_tx
            .seal_video(epoch, 242_020, 1, true, &trailing_p2[0])
            .unwrap();
        let _ = rx.consume(epoch, &d2_p, 242_020, false).unwrap();
        for (idx, chunk) in idr_pair2.iter().enumerate().skip(1) {
            let at = 242_030 + idx as u64;
            let d = peer_tx.seal_video(epoch, at, 1, true, chunk).unwrap();
            let _ = rx.consume(epoch, &d, at, false).unwrap();
        }
        assert!(rx.pop_ready_video().is_none());
    }
}
