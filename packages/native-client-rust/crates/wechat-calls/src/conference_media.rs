//! Authenticated conference source decoding. Devices, sockets and persistence
//! belong to the embedding runtime; no packet can announce its own participant.
use crate::{
    audio::{AudioDecoder, AudioFormat},
    native_media::direct::{advance_rx_total_pkg_seq, DirectVideoRxTelemetry, VideoNackTracker},
    opus::OpusDecoder,
    ordering::RtpOrder,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::{collections::BTreeMap, time::Instant};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, MediaEpoch},
    relay::SelectedRelay,
    video_packet::{
        classify_video_channel_payload, ReassembledVideoFrame, RsmIngestOutcome,
        RsmVideoReassembler, VideoChannelPayload, VideoP2sReport,
    },
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
pub struct ConferenceReceiver {
    channel: Live96,
    decoder: OpusDecoder,
    order: RtpOrder,
    video: RsmVideoReassembler,
    last_video_stream_id: Option<u8>,
    last_video_total_pkg_seq: Option<u64>,
    nack_tracker: VideoNackTracker,
    video_telemetry: DirectVideoRxTelemetry,
    pub report: StreamReport,
    timeline: Option<(u32, u64)>,
    sequence: u64,
    decoding: bool,
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
            video: RsmVideoReassembler::new(),
            last_video_stream_id: None,
            last_video_total_pkg_seq: None,
            nack_tracker: VideoNackTracker::new(),
            video_telemetry: DirectVideoRxTelemetry::new(Instant::now()),
            report: StreamReport::default(),
            timeline: None,
            sequence: 0,
            decoding: true,
        })
    }

    /// Last video stream ID observed on an authenticated inbound `RsmVideo` packet from this member.
    pub(crate) fn last_video_stream_id(&self) -> Option<u8> {
        self.last_video_stream_id
    }

    /// Latest unwrapped 16-bit `total_pkg_seq` observed on inbound RSM video packets from this member.
    pub(crate) fn last_video_total_pkg_seq(&self) -> Option<u64> {
        self.last_video_total_pkg_seq
    }

    /// Poll due `0x06` ARQ/NACK missing packet sequence ranges for this member.
    pub(crate) fn poll_due_nack(&mut self, now: Instant) -> Option<(u8, Vec<(u16, u16)>)> {
        self.nack_tracker.poll_due_ranges(now)
    }

    pub(crate) fn has_active_video_window(&self) -> bool {
        self.video_telemetry.has_active_window()
    }

    pub(crate) fn observe_reassembled_video_frame(
        &mut self,
        frame: &ReassembledVideoFrame,
        keyframe_synced: bool,
        now: Instant,
    ) {
        self.video_telemetry.observe_reassembled_frame(
            frame.codec,
            frame.is_keyframe,
            frame.prior_frames_dropped,
            keyframe_synced,
            now,
        );
    }

    pub(crate) fn snapshot_video_p2s(
        &mut self,
        member_id: u16,
        conn_id: u32,
        codec_support_mask: u32,
        now: Instant,
    ) -> VideoP2sReport {
        self.video_telemetry.snapshot_p2s(
            member_id,
            self.last_video_stream_id.unwrap_or(1),
            conn_id,
            codec_support_mask,
            now,
        )
    }

    /// Pop the next buffered in-order video frame that is ready for delivery after [`Self::consume_media`].
    pub fn pop_ready_video(&mut self) -> Option<ReassembledVideoFrame> {
        let frame = self.video.pop_ready()?;
        self.last_video_stream_id = Some(frame.stream_id);
        Some(frame)
    }
    pub fn consume(
        &mut self,
        epoch: MediaEpoch,
        packet: &[u8],
        elapsed_ms: u64,
        decode_audio: bool,
    ) -> Result<Vec<DecodedAudio>> {
        Ok(self
            .consume_media(epoch, packet, elapsed_ms, decode_audio)?
            .audio)
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
                        self.last_video_stream_id = Some(opened.packed_kind & 0x0f);
                        self.last_video_total_pkg_seq =
                            advance_rx_total_pkg_seq(self.last_video_total_pkg_seq, raw);
                        let pkt_now = Instant::now();
                        let arq_recovered = self.nack_tracker.observe_raw_packet(raw, pkt_now);
                        self.video_telemetry
                            .observe_raw_packet(raw, arq_recovered, pkt_now);
                        match self.video.ingest(raw) {
                            Ok(RsmIngestOutcome::Frame(frame)) => {
                                self.last_video_stream_id = Some(frame.stream_id);
                                if frame.prior_frames_dropped && !frame.is_keyframe {
                                    out.need_keyframe = true;
                                }
                                out.video = Some(frame);
                            }
                            Ok(RsmIngestOutcome::FrameDroppedNeedKeyframe) | Err(_) => {
                                out.need_keyframe = true;
                            }
                            Ok(RsmIngestOutcome::Pending) => {}
                        }
                    }
                }
            }
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
        self.video.reset();
        self.last_video_stream_id = None;
        self.last_video_total_pkg_seq = None;
        self.nack_tracker = VideoNackTracker::new();
        self.video_telemetry = DirectVideoRxTelemetry::new(Instant::now());
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
        use std::time::Duration;
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

        let mut packetizer = RsmVideoPacketizer::new(1, 1);
        let idr = vec![0, 0, 0, 1, 0x26, 0x01, 0xaa, 0xbb, 0xcc, 0xdd];
        let chunks = packetizer
            .packetize(VideoWireCodec::H265, &idr, 100, 15, 350)
            .unwrap();
        let pkt = peer_tx.seal_video(epoch, 100, 1, true, &chunks[0]).unwrap();
        let out = rx.consume_media(epoch, &pkt, 100, true).unwrap();
        let frame = out.video.expect("expected IDR");
        let now = Instant::now();
        rx.observe_reassembled_video_frame(&frame, true, now);
        assert!(rx.has_active_video_window());

        let rep1 = rx.snapshot_video_p2s(1, 1, 0x1d, now + Duration::from_secs(1));
        assert_eq!(rep1.play_frames, 1);
        assert!(!rx.has_active_video_window());

        // Snapshotting an idle camera-off window resets anchors without reporting freeze.
        let idle = rx.snapshot_video_p2s(1, 1, 0x1d, now + Duration::from_secs(5));
        assert_eq!(idle.heavy_freeze_cnt, 0);
        assert_eq!(idle.actual_member_num, 0);

        // Rejoin / reset_decoder resets video telemetry completely.
        rx.reset_decoder().unwrap();
        let after_reset = rx.snapshot_video_p2s(1, 1, 0x1d, now + Duration::from_secs(6));
        assert_eq!(after_reset.play_frames, 0);
    }
}
