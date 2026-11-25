//! Shared per-peer video receive recovery and outbound video burst pacing.
//!
//! Composes the deterministic wire/QoS building blocks from
//! [`wechat_protocol::calls::video_packet`] and [`wechat_protocol::calls::video_qos`]
//! so direct and conference media workers execute the same reassembly, NACK,
//! P2S telemetry, keyframe recovery and TX pacing logic with zero raw byte slicing.

use crate::native_media::{DirectVideoTx, TxVideoQueue};
use anyhow::Result;
use std::{collections::VecDeque, sync::mpsc::Receiver};
use wechat_protocol::calls::{
    video_packet::{
        p2s_codec_support_for, parse_rsm_packet_header, ReassembledVideoFrame, RsmIngestOutcome,
        RsmPacketHeader, RsmVideoPacketizer, RsmVideoReassembler, VideoP2sReport,
    },
    video_qos::{ScheduledIRequest, VideoKeyframePolicy, VideoNackTracker, VideoP2sAccumulator},
};

const VF_METADATA_INTERVAL_MS: u64 = 500;

/// Result of feeding one authenticated inbound RSM video packet into [`VideoStreamReceiver`].
pub(crate) struct VideoIngestBatch {
    /// Parsed RSM header, or `None` when the raw packet failed header validation.
    pub(crate) header: Option<RsmPacketHeader>,
    /// In-order reassembled frames completed or unblocked by this packet.
    pub(crate) frames: Vec<ReassembledVideoFrame>,
    /// True when a frame assembly dropped or errored and needs keyframe recovery.
    pub(crate) assembly_dropped: bool,
    /// Parse or reassembly error, if any (captured for diagnostics).
    pub(crate) ingest_error: Option<anyhow::Error>,
}

/// Unified per-sender video receive state shared by direct calls and conference receivers.
pub(crate) struct VideoStreamReceiver {
    reassembler: RsmVideoReassembler,
    nack_tracker: VideoNackTracker,
    telemetry: VideoP2sAccumulator,
    keyframe_policy: VideoKeyframePolicy,
}

impl VideoStreamReceiver {
    /// Create a fresh receiver anchored at `now_ms`.
    pub(crate) fn new(now_ms: u64) -> Self {
        Self {
            reassembler: RsmVideoReassembler::new(),
            nack_tracker: VideoNackTracker::new(),
            telemetry: VideoP2sAccumulator::new(now_ms),
            keyframe_policy: VideoKeyframePolicy::new(now_ms),
        }
    }

    /// Enable proactive initial keyframe requests (used on direct calls before initial sync).
    pub(crate) fn with_proactive_initial_requests(mut self, max_attempts: u8) -> Self {
        self.keyframe_policy = self
            .keyframe_policy
            .with_proactive_initial_requests(max_attempts);
        self
    }

    /// Atomically reset reassembly, NACK history, P2S telemetry and keyframe-sync state.
    pub(crate) fn reset(&mut self, now_ms: u64) {
        self.reassembler.reset();
        self.nack_tracker.reset();
        self.telemetry.reset(now_ms);
        self.keyframe_policy.reset(now_ms);
    }

    /// Last video stream ID observed on an authenticated inbound RSM video packet or frame.
    pub(crate) fn last_stream_id(&self) -> Option<u8> {
        self.keyframe_policy.last_stream_id()
    }

    /// Latest unwrapped 64-bit `total_pkg_seq` observed on inbound RSM video packets.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn last_total_pkg_seq(&self) -> Option<u64> {
        self.keyframe_policy.last_total_pkg_seq()
    }

    /// Return `true` when inbound video packets or frames arrived in the active telemetry window.
    pub(crate) fn has_active_video_window(&self) -> bool {
        self.telemetry.has_active_window()
    }

    /// Return `true` once codec parameter sets and a keyframe have been observed.
    pub(crate) fn is_keyframe_synced(&self) -> bool {
        self.keyframe_policy.is_keyframe_synced()
    }

    /// Recommended P2S heartbeat interval in milliseconds (`550` pre-sync, `1000` steady-state).
    pub(crate) fn recommended_p2s_interval_ms(&self) -> u64 {
        self.keyframe_policy.recommended_p2s_interval_ms()
    }

    /// Ingest one decrypted `0x20..=0x2f` RSM video packet at `now_ms`, updating NACK tracking,
    /// P2S telemetry, frame reassembly and keyframe-sync policy in one pass.
    pub(crate) fn ingest_rsm(
        &mut self,
        channel_stream_id: u8,
        raw: &[u8],
        now_ms: u64,
    ) -> VideoIngestBatch {
        let header = match parse_rsm_packet_header(raw) {
            Ok(h) => h,
            Err(err) => {
                self.keyframe_policy.note_assembly_drop();
                self.keyframe_policy.track_keyframe_wait(now_ms);
                return VideoIngestBatch {
                    header: None,
                    frames: Vec::new(),
                    assembly_dropped: true,
                    ingest_error: Some(err),
                };
            }
        };

        self.keyframe_policy
            .observe_packet(channel_stream_id, &header);
        let arq_recovered = self.nack_tracker.observe_header(&header, now_ms);
        self.telemetry
            .observe_packet(&header, raw.len(), arq_recovered, now_ms);

        let mut frames = Vec::new();
        let mut assembly_dropped = false;
        let mut ingest_error = None;
        match self.reassembler.ingest_with_header(raw, &header) {
            Ok(RsmIngestOutcome::Frame(frame)) => {
                frames.push(frame);
            }
            Ok(RsmIngestOutcome::FrameDroppedNeedKeyframe) => {
                assembly_dropped = true;
                self.keyframe_policy.note_assembly_drop();
            }
            Err(err) => {
                assembly_dropped = true;
                ingest_error = Some(err);
                self.keyframe_policy.note_assembly_drop();
            }
            Ok(RsmIngestOutcome::Pending) => {}
        }
        while let Some(next_frame) = self.reassembler.pop_ready() {
            frames.push(next_frame);
        }

        for frame in &frames {
            if self.keyframe_policy.observe_frame(frame) {
                assembly_dropped = true;
            }
            self.telemetry
                .observe_frame(frame, self.keyframe_policy.is_keyframe_synced(), now_ms);
        }

        self.keyframe_policy.track_keyframe_wait(now_ms);
        VideoIngestBatch {
            header: Some(header),
            frames,
            assembly_dropped,
            ingest_error,
        }
    }

    /// Queue a bounded local decoder/client keyframe request retry budget.
    pub(crate) fn queue_local_keyframe_request(&mut self, retries: u8) {
        self.keyframe_policy.queue_local_request(retries);
    }

    /// Queue a bounded local decoder/client keyframe request targeted to `target_member`.
    pub(crate) fn queue_local_keyframe_request_for(&mut self, target_member: u16, retries: u8) {
        self.keyframe_policy
            .queue_local_request_for(target_member, retries);
    }

    /// Poll due `0x06` ARQ/NACK missing packet sequence ranges at `now_ms`.
    pub(crate) fn poll_due_nack(&mut self, now_ms: u64) -> Option<(u8, Vec<(u16, u16)>)> {
        self.nack_tracker.poll_due_ranges(now_ms)
    }

    /// Poll due `0x66 IRequest` packets at `now_ms`.
    pub(crate) fn poll_due_irequests(
        &mut self,
        local_member: u16,
        target_member: u16,
        now_ms: u64,
    ) -> Vec<ScheduledIRequest> {
        self.keyframe_policy
            .poll_due(local_member, target_member, now_ms)
    }

    /// Record outbound encoded video bytes (used by direct calls where TX/RX share one report).
    pub(crate) fn observe_tx_frame(&mut self, bytes: usize) {
        self.telemetry.observe_tx_frame(bytes);
    }

    /// Snapshot the current P2S telemetry window for `remote_member` at `now_ms`.
    pub(crate) fn snapshot_p2s(
        &mut self,
        remote_member: u16,
        conn_id: u32,
        codec_support_mask: u32,
        now_ms: u64,
    ) -> VideoP2sReport {
        let mut report = self.telemetry.snapshot(
            remote_member,
            self.last_stream_id().unwrap_or(1),
            conn_id,
            codec_support_mask,
            now_ms,
        );
        report.max_size_format = self.keyframe_policy.requested_max_size_format(now_ms);
        report.codec_support_mask = p2s_codec_support_for(report.decode_type, codec_support_mask);
        report
    }
}

/// Outcome returned by the host transmit callback passed to [`VideoTxPacer::drain_and_pace`].
pub(crate) enum VideoTxSendAction {
    /// Packet was sealed and submitted (or skipped without aborting video).
    Sent,
    /// Video generation or call authority changed; clear pending packets for the old generation.
    StaleGeneration,
    /// 16-bit video sequence space was exhausted; stop outbound video cleanly without aborting audio.
    SequenceExhausted,
}

/// Shared outbound video packetizer, orientation metadata scheduler and burst pacer.
pub(crate) struct VideoTxPacer {
    packetizer: RsmVideoPacketizer,
    queue: TxVideoQueue,
    pending_packets: VecDeque<(u32, Vec<u8>)>,
    exhausted: bool,
    last_vf_tx_ms: Option<u64>,
    window_start_ms: u64,
    window_tx_bytes: u64,
    window_tx_frames: u32,
}

impl VideoTxPacer {
    /// Construct a pacer bound to `own_member` and `stream_id` with default `+90° CW` orientation.
    pub(crate) fn new(own_member: u16, stream_id: u8, now_ms: u64) -> Self {
        // Outgoing wire frames are pre-rotated 90° CCW to match mobile/conference WeChat
        // receivers, which apply +90° CW display rotation; orientation = 1 signals +90° CW
        // for desktop/native peers.
        Self {
            packetizer: RsmVideoPacketizer::new(own_member, stream_id).with_orientation(1),
            queue: TxVideoQueue::new(),
            pending_packets: VecDeque::new(),
            exhausted: false,
            last_vf_tx_ms: None,
            window_start_ms: now_ms,
            window_tx_bytes: 0,
            window_tx_frames: 0,
        }
    }

    /// Return `true` while paced packets of a multi-packet frame are still queued for transmit.
    pub(crate) fn has_pending_packets(&self) -> bool {
        !self.pending_packets.is_empty()
    }

    /// Drain at most one next outbound frame when idle and transmit up to `max_burst` packets.
    pub(crate) fn drain_and_pace(
        &mut self,
        rx: &Receiver<DirectVideoTx>,
        current_vgen: u32,
        now_ms: u64,
        max_burst: usize,
        mut on_new_frame: impl FnMut(usize),
        mut send_packet: impl FnMut(u32, &[u8]) -> Result<VideoTxSendAction>,
    ) -> Result<()> {
        if current_vgen == 0 || self.exhausted {
            self.queue.clear();
            self.pending_packets.clear();
            self.last_vf_tx_ms = None;
            return Ok(());
        }
        self.pending_packets
            .retain(|(gen_id, _)| *gen_id == current_vgen);
        if self.pending_packets.is_empty() {
            if let Some(frame) = self.queue.next_frame(rx, current_vgen) {
                if let Ok(pkts) = self.packetizer.packetize(
                    frame.codec,
                    &frame.annexb,
                    frame.timestamp_ms,
                    frame.fps,
                    frame.kbps,
                ) {
                    self.window_tx_bytes = self
                        .window_tx_bytes
                        .saturating_add(frame.annexb.len() as u64);
                    self.window_tx_frames = self.window_tx_frames.saturating_add(1);
                    on_new_frame(frame.annexb.len());
                    for pkt in pkts {
                        self.pending_packets.push_back((current_vgen, pkt));
                    }
                    if self
                        .last_vf_tx_ms
                        .is_none_or(|t| now_ms.saturating_sub(t) >= VF_METADATA_INTERVAL_MS)
                    {
                        if let Ok(vf) = self.packetizer.build_orientation_metadata_packet(
                            frame.codec,
                            frame.timestamp_ms,
                            frame.fps,
                        ) {
                            self.pending_packets.push_back((current_vgen, vf));
                            self.last_vf_tx_ms = Some(now_ms);
                        }
                    }
                }
            }
        }
        for _ in 0..max_burst {
            let Some((pkt_gen, pkt)) = self.pending_packets.pop_front() else {
                break;
            };
            match send_packet(pkt_gen, &pkt)? {
                VideoTxSendAction::Sent => {}
                VideoTxSendAction::StaleGeneration => {
                    self.pending_packets.clear();
                    break;
                }
                VideoTxSendAction::SequenceExhausted => {
                    self.pending_packets.clear();
                    self.queue.clear();
                    self.exhausted = true;
                    break;
                }
            }
        }
        Ok(())
    }

    /// Snapshot and reset outbound `(send_kbps, send_fps)` over the elapsed window at `now_ms`.
    pub(crate) fn take_window_stats(&mut self, now_ms: u64) -> (u32, u32) {
        let window_ms = u32::try_from(now_ms.saturating_sub(self.window_start_ms))
            .unwrap_or(u32::MAX)
            .max(1);
        let send_kbps =
            u32::try_from(self.window_tx_bytes.saturating_mul(8) / u64::from(window_ms))
                .unwrap_or(u32::MAX);
        let send_fps = self
            .window_tx_frames
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);
        self.window_start_ms = now_ms;
        self.window_tx_bytes = 0;
        self.window_tx_frames = 0;
        (send_kbps, send_fps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use wechat_protocol::calls::{
        video_packet::{parse_rsm_packet_header, VideoWireCodec},
        video_qos::KeyframeRequestReason,
    };

    #[test]
    fn video_stream_receiver_unifies_reassembly_nack_p2s_and_irequest_reset() {
        let mut rx = VideoStreamReceiver::new(0).with_proactive_initial_requests(6);
        let mut tx = RsmVideoPacketizer::new(7, 2)
            .with_orientation(1)
            .with_chunk_bytes(80)
            .unwrap();

        // Send a 3-packet P-frame before keyframe sync, dropping packet 1 to test NACK + P2S stall.
        let mut p_frame = vec![0, 0, 0, 1, 0x41, 0x9a];
        p_frame.extend((0..180u16).map(|i| (i & 0xff) as u8));
        let p_pkts = tx
            .packetize(VideoWireCodec::H264, &p_frame, 100, 15, 500)
            .unwrap();
        assert_eq!(p_pkts.len(), 3);

        let b0 = rx.ingest_rsm(2, &p_pkts[0], 50);
        assert!(b0.header.is_some());
        assert!(b0.frames.is_empty());
        // Skip p_pkts[1] and deliver p_pkts[2] -> creates a 1-packet NACK gap.
        let _ = rx.ingest_rsm(2, &p_pkts[2], 60);
        let (arq_seq, ranges) = rx
            .poll_due_nack(105)
            .expect("missing packet 1 should produce a NACK after reorder delay");
        assert_eq!(arq_seq, 1);
        assert_eq!(ranges, vec![(2, 3)]);

        // Retransmitting p_pkts[1] completes the P-frame and counts ARQ recovery.
        let b1 = rx.ingest_rsm(2, &p_pkts[1], 120);
        assert_eq!(b1.frames.len(), 1);
        assert!(!rx.is_keyframe_synced());
        assert_eq!(rx.recommended_p2s_interval_ms(), 550);

        // Pre-sync P2S reports play_fps = 0 and heavy freeze after >= 500 ms.
        let rep = rx.snapshot_p2s(7, 1, 0x1d, 600);
        assert_eq!(rep.stream_id, 2);
        assert_eq!(rep.recv_arq_resp_pkgs, 1);
        assert_eq!(rep.play_fps, 0);
        assert_eq!(rep.heavy_freeze_cnt, 1);

        // Proactive initial IRequest fires at >= 500 ms with force = true.
        let ireqs = rx.poll_due_irequests(3, 7, 600);
        assert_eq!(ireqs.len(), 1);
        assert_eq!(ireqs[0].reason, KeyframeRequestReason::InitialSync);
        assert_eq!(ireqs[0].stream_id, 2);
        assert!(ireqs[0].force);

        // Self-contained H.264 IDR completes sync and switches P2S cadence back to 1000 ms.
        let idr = vec![
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 0, 1, 0x65, 0x88,
            0xaa, 0xbb,
        ];
        let idr_pkts = tx
            .packetize(VideoWireCodec::H264, &idr, 700, 15, 500)
            .unwrap();
        let mut completed = 0usize;
        for pkt in &idr_pkts {
            completed += rx.ingest_rsm(2, pkt, 700).frames.len();
        }
        assert_eq!(completed, 1);
        assert!(rx.is_keyframe_synced());
        assert_eq!(rx.recommended_p2s_interval_ms(), 1000);

        // Reset clears all state atomically.
        rx.reset(1000);
        assert_eq!(rx.last_stream_id(), None);
        assert_eq!(rx.last_total_pkg_seq(), None);
        assert!(!rx.is_keyframe_synced());
        assert!(!rx.has_active_video_window());
    }

    #[test]
    fn video_tx_pacer_paces_bursts_emits_orientation_vf_and_handles_exhaustion() {
        let (tx, rx) = mpsc::channel();
        let mut pacer = VideoTxPacer::new(3, 1, 0);

        // Build a large keyframe that splits into >= 6 packets so burst=4 spans two ticks.
        let mut big_idr = vec![0, 0, 0, 1, 0x65, 0x88];
        big_idr.extend((0..6000u16).map(|i| (i & 0xff) as u8));
        tx.send(DirectVideoTx {
            generation: 1,
            codec: VideoWireCodec::H264,
            timestamp_ms: 100,
            fps: 15,
            kbps: 500,
            annexb: big_idr.clone(),
        })
        .unwrap();

        let mut sent = Vec::new();
        let mut observed_bytes = 0usize;
        pacer
            .drain_and_pace(
                &rx,
                1,
                100,
                4,
                |b| observed_bytes += b,
                |_gen, pkt| {
                    sent.push(pkt.to_vec());
                    Ok(VideoTxSendAction::Sent)
                },
            )
            .unwrap();
        assert_eq!(observed_bytes, big_idr.len());
        assert_eq!(sent.len(), 4);
        assert!(pacer.has_pending_packets());

        // Drain the remaining media packets + trailing orientation virtual frame.
        pacer
            .drain_and_pace(
                &rx,
                1,
                105,
                8,
                |_| {},
                |_gen, pkt| {
                    sent.push(pkt.to_vec());
                    Ok(VideoTxSendAction::Sent)
                },
            )
            .unwrap();
        assert!(!pacer.has_pending_packets());
        let last_hdr = parse_rsm_packet_header(sent.last().unwrap()).unwrap();
        assert!(last_hdr.is_virtual_frame);
        assert_eq!(last_hdr.virtual_frame_orientation, Some(1));

        let (send_kbps, send_fps) = pacer.take_window_stats(1000);
        assert!(send_kbps > 0);
        assert_eq!(send_fps, 1);

        // Sequence exhaustion clears pending queues and stops further sends cleanly.
        tx.send(DirectVideoTx {
            generation: 1,
            codec: VideoWireCodec::H264,
            timestamp_ms: 200,
            fps: 15,
            kbps: 500,
            annexb: vec![0, 0, 0, 1, 0x65, 0x88, 0x01, 0x02],
        })
        .unwrap();
        pacer
            .drain_and_pace(
                &rx,
                1,
                1100,
                4,
                |_| {},
                |_gen, _pkt| Ok(VideoTxSendAction::SequenceExhausted),
            )
            .unwrap();
        assert!(!pacer.has_pending_packets());
        let mut post_exhaust_calls = 0usize;
        tx.send(DirectVideoTx {
            generation: 1,
            codec: VideoWireCodec::H264,
            timestamp_ms: 300,
            fps: 15,
            kbps: 500,
            annexb: vec![0, 0, 0, 1, 0x65, 0x88, 0x03, 0x04],
        })
        .unwrap();
        pacer
            .drain_and_pace(
                &rx,
                1,
                1200,
                4,
                |_| {},
                |_gen, _pkt| {
                    post_exhaust_calls += 1;
                    Ok(VideoTxSendAction::Sent)
                },
            )
            .unwrap();
        assert_eq!(post_exhaust_calls, 0);
    }
}
