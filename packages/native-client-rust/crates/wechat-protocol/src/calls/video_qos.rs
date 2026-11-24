//! Deterministic video ARQ/NACK, P2S QoS telemetry and keyframe-recovery state machines.
//!
//! All state machines in this module are I/O-free and driven by caller-supplied
//! monotonic millisecond timestamps (`now_ms: u64`) together with parsed
//! [`crate::calls::video_packet::RsmPacketHeader`] and
//! [`crate::calls::video_packet::ReassembledVideoFrame`] values. Callers own
//! UDP sockets, wall clocks, AEAD sealing (`Live96`) and codec workers.

use crate::calls::video_packet::{
    advance_seq16, encode_irequest_with_force, ReassembledVideoFrame, RsmPacketHeader,
    VideoP2sReport, VideoWireCodec, MAX_VIDEO_NACK_RANGES,
};
use std::collections::BTreeMap;

const NACK_MAX_GAP: u64 = 32;
const NACK_MAX_HISTORY: u64 = 48;
const NACK_REORDER_DELAY_MS: u64 = 35;
const NACK_RETRY_INTERVAL_MS: u64 = 90;
const NACK_MAX_AGE_MS: u64 = 350;
const NACK_MIN_DATAGRAM_SPACING_MS: u64 = 40;
const NACK_MAX_ATTEMPTS: u8 = 3;

/// Maximum duration P-frames are held awaiting a clean recovery IDR after an
/// unrecovered packet drop (aligned with receive transcoder post-drop IDR hold).
const TELEMETRY_POST_DROP_IDR_HOLD_MS: u64 = 1200;

const DROP_IREQUEST_COOLDOWN_MS: u64 = 1200;
const LOCAL_IREQUEST_COOLDOWN_MS: u64 = 1000;
const INITIAL_IREQUEST_DELAY_MS: u64 = 500;
const INITIAL_IREQUEST_INTERVAL_MS: u64 = 1500;
const PRESYNC_P2S_INTERVAL_MS: u64 = 550;
const STEADY_P2S_INTERVAL_MS: u64 = 1000;

#[derive(Clone, Copy, Debug)]
struct PendingNack {
    first_missing_ms: u64,
    last_sent_ms: Option<u64>,
    attempts: u8,
}

/// Tracks missing 16-bit `pkg_seq` numbers on inbound RSM video and emits bounded
/// `0x06` ARQ/NACK half-open `[start, end)` ranges after a short reorder grace window.
pub struct VideoNackTracker {
    highest_seq: Option<u64>,
    missing: BTreeMap<u64, PendingNack>,
    last_datagram_sent_ms: Option<u64>,
    arq_seq: u8,
    arq_recovered_total: u64,
}

impl Default for VideoNackTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoNackTracker {
    /// Create a fresh NACK tracker with empty sequence history.
    pub fn new() -> Self {
        Self {
            highest_seq: None,
            missing: BTreeMap::new(),
            last_datagram_sent_ms: None,
            arq_seq: 0,
            arq_recovered_total: 0,
        }
    }

    /// Clear tracked sequence gaps and ARQ counters (for example on decoder reset).
    pub fn reset(&mut self) {
        self.highest_seq = None;
        self.missing.clear();
        self.last_datagram_sent_ms = None;
        self.arq_seq = 0;
        self.arq_recovered_total = 0;
    }

    /// Total number of missing packets recovered after at least one NACK attempt.
    pub fn arq_recovered_total(&self) -> u64 {
        self.arq_recovered_total
    }

    /// Number of currently tracked missing sequence numbers.
    pub fn pending_count(&self) -> usize {
        self.missing.len()
    }

    /// Observe one parsed RSM video packet header at `now_ms`.
    ///
    /// Non-media packets (`header.pkg_seq == None`, such as virtual-frame orientation
    /// metadata or padding) are ignored. Returns `true` when this packet filled a gap
    /// that had already been NACKed at least once.
    pub fn observe_header(&mut self, header: &RsmPacketHeader, now_ms: u64) -> bool {
        let Some(seq16) = header.pkg_seq else {
            return false;
        };
        self.observe_seq(seq16, now_ms)
    }

    /// Observe one 16-bit media packet sequence (`pkg_seq`) at `now_ms`.
    ///
    /// Returns `true` when this packet resolved a previously NACKed gap.
    pub fn observe_seq(&mut self, pkg_seq: u16, now_ms: u64) -> bool {
        let Some(highest) = self.highest_seq else {
            self.highest_seq = Some(u64::from(pkg_seq));
            return false;
        };
        let delta = pkg_seq.wrapping_sub(highest as u16) as i16;
        if delta > 0 {
            let step = delta as u64;
            let new_highest = highest.wrapping_add(step);
            let gap = step.saturating_sub(1);
            if gap > 0 && gap <= NACK_MAX_GAP {
                for missing_seq in (highest + 1)..new_highest {
                    self.missing.entry(missing_seq).or_insert(PendingNack {
                        first_missing_ms: now_ms,
                        last_sent_ms: None,
                        attempts: 0,
                    });
                }
            } else if gap > NACK_MAX_GAP {
                // Sequence discontinuity or severe outage: drop stale entries instead of
                // flooding NACKs for an unrecoverable window.
                self.missing.clear();
            }
            self.highest_seq = Some(new_highest);
            self.missing
                .retain(|&seq, _| new_highest.saturating_sub(seq) <= NACK_MAX_HISTORY);
            false
        } else if delta < 0 {
            let back = u64::from(delta.unsigned_abs());
            if let Some(seq) = highest.checked_sub(back) {
                if let Some(entry) = self.missing.remove(&seq) {
                    if entry.attempts > 0 {
                        self.arq_recovered_total = self.arq_recovered_total.saturating_add(1);
                        return true;
                    }
                }
            }
            false
        } else {
            false
        }
    }

    /// Poll due `0x06` ARQ/NACK missing packet sequence ranges at `now_ms`.
    ///
    /// Returns `Some((arq_seq, ranges))` ready for [`crate::calls::video_packet::encode_video_nack`],
    /// enforcing reorder delay, retry backoff, maximum attempts and minimum datagram spacing.
    pub fn poll_due_ranges(&mut self, now_ms: u64) -> Option<(u8, Vec<(u16, u16)>)> {
        let highest = self.highest_seq?;
        // Keep attempt-exhausted entries until NACK_MAX_AGE_MS / NACK_MAX_HISTORY so a
        // retransmission triggered by the final attempt still registers as ARQ-recovered.
        self.missing.retain(|&seq, entry| {
            highest.saturating_sub(seq) <= NACK_MAX_HISTORY
                && now_ms.saturating_sub(entry.first_missing_ms) <= NACK_MAX_AGE_MS
        });
        if self.missing.is_empty() {
            return None;
        }
        if self
            .last_datagram_sent_ms
            .is_some_and(|t| now_ms.saturating_sub(t) < NACK_MIN_DATAGRAM_SPACING_MS)
        {
            return None;
        }
        let due_seqs: Vec<u64> = self
            .missing
            .iter()
            .filter_map(|(&seq, entry)| {
                if entry.attempts >= NACK_MAX_ATTEMPTS {
                    return None;
                }
                let is_due = if entry.attempts == 0 {
                    now_ms.saturating_sub(entry.first_missing_ms) >= NACK_REORDER_DELAY_MS
                } else {
                    entry
                        .last_sent_ms
                        .is_some_and(|t| now_ms.saturating_sub(t) >= NACK_RETRY_INTERVAL_MS)
                };
                is_due.then_some(seq)
            })
            .collect();
        if due_seqs.is_empty() {
            return None;
        }
        let mut ranges: Vec<(u16, u16)> = Vec::new();
        let mut sent_seqs: Vec<u64> = Vec::new();
        let mut run_start = due_seqs[0];
        let mut run_end = run_start + 1;
        let mut run_members = vec![run_start];
        for &seq in &due_seqs[1..] {
            if seq == run_end && (run_end - run_start) < NACK_MAX_GAP {
                run_end += 1;
                run_members.push(seq);
            } else {
                if ranges.len() < MAX_VIDEO_NACK_RANGES {
                    ranges.push((run_start as u16, run_end as u16));
                    sent_seqs.extend_from_slice(&run_members);
                }
                run_start = seq;
                run_end = seq + 1;
                run_members.clear();
                run_members.push(seq);
            }
        }
        if ranges.len() < MAX_VIDEO_NACK_RANGES {
            ranges.push((run_start as u16, run_end as u16));
            sent_seqs.extend_from_slice(&run_members);
        }
        if ranges.is_empty() {
            return None;
        }
        for seq in sent_seqs {
            if let Some(entry) = self.missing.get_mut(&seq) {
                entry.attempts = entry.attempts.saturating_add(1);
                entry.last_sent_ms = Some(now_ms);
            }
        }
        self.arq_seq = self.arq_seq.wrapping_add(1);
        self.last_datagram_sent_ms = Some(now_ms);
        Some((self.arq_seq, ranges))
    }
}

/// Sliding-window video receive/transmit telemetry accumulator feeding `0x0b` MP P2S QoS reports.
///
/// Separates received frame rate (`recv_fps`) from decodable playback rate (`play_fps`) and
/// tracks light/heavy freeze ratios so WeChat's QoS server observes receiver stalls prior to
/// initial or post-drop keyframe synchronization.
pub struct VideoP2sAccumulator {
    window_start_ms: u64,
    ever_received_video: bool,
    rx_pkts_in_window: u32,
    rx_bytes_in_window: u64,
    arq_recovered_in_window: u32,
    highest_seq: Option<u64>,
    window_base_seq: Option<u64>,
    long_loss_ewma_x100: u32,
    last_pkt_arrival_ms: Option<u64>,
    prev_pkt_gap_ms: Option<u32>,
    max_pkt_gap_ms: u32,
    jitter_ms_x16: u32,
    rx_frames_in_window: u32,
    play_frames_in_window: u32,
    total_play_frames: u32,
    first_rx_video_ms: Option<u64>,
    last_frame_ms: Option<u64>,
    post_drop_hold_until_ms: Option<u64>,
    freeze_cursor_ms: Option<u64>,
    counted_light_stall: bool,
    counted_heavy_stall: bool,
    light_freeze_cnt: u32,
    heavy_freeze_cnt: u32,
    light_freeze_ms: u32,
    heavy_freeze_ms: u32,
    last_rx_codec: VideoWireCodec,
    est_bw_kbps: u32,
    tx_bytes_in_window: u64,
    tx_frames_in_window: u32,
}

impl VideoP2sAccumulator {
    /// Create a fresh telemetry accumulator anchored at `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self {
            window_start_ms: now_ms,
            ever_received_video: false,
            rx_pkts_in_window: 0,
            rx_bytes_in_window: 0,
            arq_recovered_in_window: 0,
            highest_seq: None,
            window_base_seq: None,
            long_loss_ewma_x100: 0,
            last_pkt_arrival_ms: None,
            prev_pkt_gap_ms: None,
            max_pkt_gap_ms: 0,
            jitter_ms_x16: 0,
            rx_frames_in_window: 0,
            play_frames_in_window: 0,
            total_play_frames: 0,
            first_rx_video_ms: None,
            last_frame_ms: None,
            post_drop_hold_until_ms: None,
            freeze_cursor_ms: None,
            counted_light_stall: false,
            counted_heavy_stall: false,
            light_freeze_cnt: 0,
            heavy_freeze_cnt: 0,
            light_freeze_ms: 0,
            heavy_freeze_ms: 0,
            last_rx_codec: VideoWireCodec::H265,
            est_bw_kbps: 500,
            tx_bytes_in_window: 0,
            tx_frames_in_window: 0,
        }
    }

    /// Reset all window counters, freeze anchors and bandwidth estimates at `now_ms`.
    pub fn reset(&mut self, now_ms: u64) {
        *self = Self::new(now_ms);
    }

    /// Return `true` when at least one inbound video packet or frame arrived in the current window.
    pub fn has_active_window(&self) -> bool {
        self.rx_pkts_in_window > 0 || self.rx_frames_in_window > 0
    }

    /// Record one parsed inbound RSM video packet at `now_ms`.
    ///
    /// Non-media packets (`header.pkg_seq == None`) are ignored.
    pub fn observe_packet(
        &mut self,
        header: &RsmPacketHeader,
        packet_bytes: usize,
        arq_recovered: bool,
        now_ms: u64,
    ) {
        let Some(seq16) = header.pkg_seq else {
            return;
        };
        if matches!(header.codec, VideoWireCodec::H264 | VideoWireCodec::H265) {
            self.last_rx_codec = header.codec;
        }
        self.ever_received_video = true;
        self.first_rx_video_ms.get_or_insert(now_ms);
        self.rx_pkts_in_window = self.rx_pkts_in_window.saturating_add(1);
        self.rx_bytes_in_window = self.rx_bytes_in_window.saturating_add(packet_bytes as u64);
        if arq_recovered {
            self.arq_recovered_in_window = self.arq_recovered_in_window.saturating_add(1);
        }
        if let Some(prev) = self.last_pkt_arrival_ms {
            let gap_ms = u32::try_from(now_ms.saturating_sub(prev)).unwrap_or(u32::MAX);
            self.max_pkt_gap_ms = self.max_pkt_gap_ms.max(gap_ms);
            if let Some(prev_gap) = self.prev_pkt_gap_ms {
                let diff = gap_ms.abs_diff(prev_gap);
                let cur = self.jitter_ms_x16 / 16;
                self.jitter_ms_x16 = self.jitter_ms_x16.saturating_sub(cur).saturating_add(diff);
            }
            self.prev_pkt_gap_ms = Some(gap_ms);
        }
        self.last_pkt_arrival_ms = Some(now_ms);

        match self.highest_seq {
            None => {
                let seq = u64::from(seq16);
                self.highest_seq = Some(seq);
                self.window_base_seq = Some(seq);
            }
            Some(highest) => {
                let delta = seq16.wrapping_sub(highest as u16) as i16;
                if delta > 0 {
                    let step = delta as u64;
                    let new_highest = highest.wrapping_add(step);
                    self.highest_seq = Some(new_highest);
                    if step > 256 {
                        // Reset baseline across large sequence discontinuities.
                        self.window_base_seq = Some(new_highest);
                    } else if self.window_base_seq.is_none() {
                        self.window_base_seq = Some(highest);
                    }
                }
            }
        }
    }

    /// Record one reassembled inbound video frame at `now_ms`.
    ///
    /// Undecodable P-frames arriving before `keyframe_synced` or during the post-drop IDR
    /// hold window count toward network `recv_fps`, not `play_fps` or stall recovery.
    pub fn observe_frame(
        &mut self,
        frame: &ReassembledVideoFrame,
        keyframe_synced: bool,
        now_ms: u64,
    ) {
        if matches!(frame.codec, VideoWireCodec::H264 | VideoWireCodec::H265) {
            self.last_rx_codec = frame.codec;
        }
        self.ever_received_video = true;
        self.first_rx_video_ms.get_or_insert(now_ms);
        self.rx_frames_in_window = self.rx_frames_in_window.saturating_add(1);

        if frame.is_keyframe {
            self.post_drop_hold_until_ms = None;
        } else if keyframe_synced && frame.prior_frames_dropped {
            self.post_drop_hold_until_ms
                .get_or_insert_with(|| now_ms.saturating_add(TELEMETRY_POST_DROP_IDR_HOLD_MS));
        }

        let holding_after_drop = match self.post_drop_hold_until_ms {
            Some(until) if now_ms < until => true,
            Some(_) => {
                self.post_drop_hold_until_ms = None;
                false
            }
            None => false,
        };
        if !keyframe_synced || holding_after_drop {
            return;
        }

        self.account_freeze_until(now_ms);
        self.last_frame_ms = Some(now_ms);
        self.freeze_cursor_ms = Some(now_ms);
        self.counted_light_stall = false;
        self.counted_heavy_stall = false;
        self.play_frames_in_window = self.play_frames_in_window.saturating_add(1);
        self.total_play_frames = self.total_play_frames.saturating_add(1);
    }

    /// Record one outbound video frame encoded into the current reporting interval.
    pub fn observe_tx_frame(&mut self, bytes: usize) {
        self.tx_bytes_in_window = self.tx_bytes_in_window.saturating_add(bytes as u64);
        self.tx_frames_in_window = self.tx_frames_in_window.saturating_add(1);
    }

    fn account_freeze_until(&mut self, now_ms: u64) {
        let Some(anchor) = self.last_frame_ms.or(self.first_rx_video_ms) else {
            return;
        };
        let total_gap_ms = u32::try_from(now_ms.saturating_sub(anchor)).unwrap_or(u32::MAX);
        let cursor = self.freeze_cursor_ms.unwrap_or(anchor);
        let slice_ms = u32::try_from(now_ms.saturating_sub(cursor)).unwrap_or(u32::MAX);
        if total_gap_ms >= 500 {
            if !self.counted_heavy_stall {
                self.heavy_freeze_cnt = self.heavy_freeze_cnt.saturating_add(1);
                self.counted_heavy_stall = true;
            }
            self.heavy_freeze_ms = self.heavy_freeze_ms.saturating_add(slice_ms);
        } else if total_gap_ms >= 200 {
            if !self.counted_light_stall {
                self.light_freeze_cnt = self.light_freeze_cnt.saturating_add(1);
                self.counted_light_stall = true;
            }
            self.light_freeze_ms = self.light_freeze_ms.saturating_add(slice_ms);
        }
        self.freeze_cursor_ms = Some(cursor.max(now_ms));
    }

    /// Close the current telemetry window at `now_ms`, advance window anchors and return
    /// a [`VideoP2sReport`] ready for [`crate::calls::video_packet::encode_p2s_report`].
    pub fn snapshot(
        &mut self,
        remote_member: u16,
        stream_id: u8,
        conn_id: u32,
        codec_support_mask: u32,
        now_ms: u64,
    ) -> VideoP2sReport {
        let window_ms = u32::try_from(now_ms.saturating_sub(self.window_start_ms))
            .unwrap_or(u32::MAX)
            .max(1);
        let send_kbps =
            u32::try_from(self.tx_bytes_in_window.saturating_mul(8) / u64::from(window_ms))
                .unwrap_or(u32::MAX);
        let send_fps = self
            .tx_frames_in_window
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);

        if !self.ever_received_video
            || (self.rx_pkts_in_window == 0 && self.rx_frames_in_window == 0)
        {
            // No inbound video packets arrived during this reporting window (either video
            // has not started or the remote camera is off/paused). Report idle instead of
            // 100% loss/heavy freeze, preserve `est_bw_kbps`, and clear per-stream gap
            // anchors so resuming video does not record a synthetic freeze or loss spike.
            self.window_start_ms = now_ms;
            self.rx_pkts_in_window = 0;
            self.rx_bytes_in_window = 0;
            self.arq_recovered_in_window = 0;
            self.highest_seq = None;
            self.window_base_seq = None;
            self.long_loss_ewma_x100 = 0;
            self.last_pkt_arrival_ms = None;
            self.prev_pkt_gap_ms = None;
            self.max_pkt_gap_ms = 0;
            self.jitter_ms_x16 = 0;
            self.rx_frames_in_window = 0;
            self.play_frames_in_window = 0;
            self.first_rx_video_ms = None;
            self.last_frame_ms = None;
            self.post_drop_hold_until_ms = None;
            self.freeze_cursor_ms = None;
            self.counted_light_stall = false;
            self.counted_heavy_stall = false;
            self.light_freeze_cnt = 0;
            self.heavy_freeze_cnt = 0;
            self.light_freeze_ms = 0;
            self.heavy_freeze_ms = 0;
            self.tx_bytes_in_window = 0;
            self.tx_frames_in_window = 0;
            return VideoP2sReport {
                remote_member_id: u32::from(remote_member),
                stream_id: u32::from(stream_id),
                conn_id,
                recv_data_interval_ms: 0,
                delay_ms: 0,
                est_bw_kbps: self.est_bw_kbps,
                send_kbps,
                recv_kbps: 0,
                send_fps,
                recv_fps: 0,
                capt_fps: send_fps,
                loss_pct: 0,
                codec_support_mask,
                loss_pkts: 0,
                decode_type: u32::from(self.last_rx_codec.wire()),
                play_fps: 0,
                play_frames: self.total_play_frames,
                light_freeze_cnt: 0,
                heavy_freeze_cnt: 0,
                light_freeze_time_ratio: 0,
                heavy_freeze_time_ratio: 0,
                short_loss_pct: 0,
                long_loss_pct: 0,
                recv_pkgs: 0,
                expected_member_num: 1,
                actual_member_num: 0,
                recv_arq_resp_pkgs: 0,
            };
        }

        self.account_freeze_until(now_ms);
        let recv_kbps =
            u32::try_from(self.rx_bytes_in_window.saturating_mul(8) / u64::from(window_ms))
                .unwrap_or(u32::MAX);
        let recv_fps = self
            .rx_frames_in_window
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);
        let play_fps = self
            .play_frames_in_window
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);

        let expected_pkts = match (self.window_base_seq, self.highest_seq) {
            (Some(base), Some(high)) if self.rx_pkts_in_window > 0 && high >= base => {
                let span = high.saturating_sub(base).saturating_add(1);
                if span <= 1024 {
                    (span as u32).max(self.rx_pkts_in_window)
                } else {
                    self.rx_pkts_in_window
                }
            }
            _ => self.rx_pkts_in_window,
        };
        let loss_pkts = expected_pkts.saturating_sub(self.rx_pkts_in_window);
        let short_loss_pct = if expected_pkts > 0 {
            loss_pkts
                .saturating_mul(100)
                .div_ceil(expected_pkts)
                .min(100)
        } else {
            0
        };
        // EWMA with alpha = 1/4 for smoothed long-term loss reporting.
        self.long_loss_ewma_x100 =
            (self.long_loss_ewma_x100 * 3 + short_loss_pct.saturating_mul(100)) / 4;
        let long_loss_pct = ((self.long_loss_ewma_x100 + 50) / 100).min(100);

        let stale_pkt_gap = self.last_pkt_arrival_ms.map_or(0, |t| {
            u32::try_from(now_ms.saturating_sub(t)).unwrap_or(u32::MAX)
        });
        let recv_data_interval_ms = self.max_pkt_gap_ms.max(stale_pkt_gap);
        let delay_ms = self.jitter_ms_x16 / 16;

        let light_ratio = self
            .light_freeze_ms
            .saturating_mul(100)
            .checked_div(window_ms)
            .unwrap_or(0)
            .min(100);
        let heavy_ratio = self
            .heavy_freeze_ms
            .saturating_mul(100)
            .checked_div(window_ms)
            .unwrap_or(0)
            .min(100);

        if short_loss_pct >= 5 || self.heavy_freeze_cnt > 0 || heavy_ratio >= 50 {
            let retention = 100u32.saturating_sub(short_loss_pct.min(70));
            let base = recv_kbps.max(self.est_bw_kbps / 2).max(100);
            self.est_bw_kbps = ((base.saturating_mul(retention)) / 100).clamp(100, 1500);
        } else if self.rx_pkts_in_window > 0 && self.play_frames_in_window > 0 {
            let target = (recv_kbps.saturating_mul(12) / 10).clamp(300, 1500);
            self.est_bw_kbps = if self.est_bw_kbps < target {
                self.est_bw_kbps.saturating_add(50).min(target)
            } else {
                target
            };
        }

        let report = VideoP2sReport {
            remote_member_id: u32::from(remote_member),
            stream_id: u32::from(stream_id),
            conn_id,
            recv_data_interval_ms,
            delay_ms,
            est_bw_kbps: self.est_bw_kbps,
            send_kbps,
            recv_kbps,
            send_fps,
            recv_fps,
            capt_fps: send_fps,
            loss_pct: short_loss_pct,
            codec_support_mask,
            loss_pkts,
            decode_type: u32::from(self.last_rx_codec.wire()),
            play_fps,
            play_frames: self.total_play_frames,
            light_freeze_cnt: self.light_freeze_cnt,
            heavy_freeze_cnt: self.heavy_freeze_cnt,
            light_freeze_time_ratio: light_ratio,
            heavy_freeze_time_ratio: heavy_ratio,
            short_loss_pct,
            long_loss_pct,
            recv_pkgs: self.rx_pkts_in_window,
            expected_member_num: 1,
            actual_member_num: u32::from(self.rx_pkts_in_window > 0),
            recv_arq_resp_pkgs: self.arq_recovered_in_window,
        };

        self.window_start_ms = now_ms;
        self.rx_pkts_in_window = 0;
        self.rx_bytes_in_window = 0;
        self.arq_recovered_in_window = 0;
        self.window_base_seq = self.highest_seq.map(|h| h.saturating_add(1));
        self.max_pkt_gap_ms = 0;
        self.rx_frames_in_window = 0;
        self.play_frames_in_window = 0;
        self.light_freeze_cnt = 0;
        self.heavy_freeze_cnt = 0;
        self.light_freeze_ms = 0;
        self.heavy_freeze_ms = 0;
        self.tx_bytes_in_window = 0;
        self.tx_frames_in_window = 0;
        report
    }
}

/// Reason a video keyframe request (`0x66 IRequest`) was scheduled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyframeRequestReason {
    /// Triggered by a dropped or unrecoverable frame assembly.
    FrameDrop,
    /// Triggered by an explicit local decoder or client request.
    LocalRequest,
    /// Triggered proactively before initial keyframe synchronization completes.
    InitialSync,
}

/// One ready-to-seal `0x66 IRequest` packet produced by [`VideoKeyframePolicy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledIRequest {
    /// Target member ID encoded in the `0x66` request payload.
    pub target_member: u16,
    /// Target video stream ID to pass to [`crate::calls::audio_packet::Live96::seal_video_rtcp`].
    pub stream_id: u8,
    /// `ullLostPointSeq` encoded into the request body.
    pub lost_point_seq: u64,
    /// Whether the `force` flag (`body[0x2c] & 1`) was set.
    pub force: bool,
    /// True when this is the companion stream-0 compatibility fallback packet.
    pub is_stream0_fallback: bool,
    /// Why this keyframe request was scheduled.
    pub reason: KeyframeRequestReason,
    /// Encoded 58-byte `0x66` `IRequest` payload ready for `Live96::seal_video_rtcp`.
    pub payload: Vec<u8>,
}

/// Stateful per-stream `0x66 IRequest` keyframe-sync and escalation policy.
///
/// Tracks codec parameter-set arrival, initial keyframe synchronization, `total_pkg_seq`
/// unwrapping, `force`-flag alternation, cooldowns, bounded local/proactive retry budgets
/// and stream-0 compatibility fallback packets.
pub struct VideoKeyframePolicy {
    connected_at_ms: u64,
    max_proactive_initial_requests: u8,
    initial_irequests_sent: u8,
    last_stream_id: Option<u8>,
    last_total_pkg_seq: Option<u64>,
    rx_video_seen: bool,
    codec_config_seen: bool,
    keyframe_seen: bool,
    irequest_streak: u32,
    last_irequest_ms: Option<u64>,
    drop_recovery_pending: bool,
    pending_local_retries: u8,
    local_target_override: Option<u16>,
}

impl VideoKeyframePolicy {
    /// Create a fresh keyframe recovery policy anchored at `connected_at_ms`.
    pub fn new(connected_at_ms: u64) -> Self {
        Self {
            connected_at_ms,
            max_proactive_initial_requests: 0,
            initial_irequests_sent: 0,
            last_stream_id: None,
            last_total_pkg_seq: None,
            rx_video_seen: false,
            codec_config_seen: false,
            keyframe_seen: false,
            irequest_streak: 0,
            last_irequest_ms: None,
            drop_recovery_pending: false,
            pending_local_retries: 0,
            local_target_override: None,
        }
    }

    /// Enable up to `max_attempts` proactive initial keyframe requests (used on direct video
    /// calls before the first self-contained keyframe arrives).
    pub fn with_proactive_initial_requests(mut self, max_attempts: u8) -> Self {
        self.max_proactive_initial_requests = max_attempts;
        self
    }

    /// Reset per-stream synchronization and retry state while keeping proactive configuration.
    pub fn reset(&mut self, connected_at_ms: u64) {
        let max_proactive = self.max_proactive_initial_requests;
        *self = Self::new(connected_at_ms).with_proactive_initial_requests(max_proactive);
    }

    /// Last video stream ID observed on an inbound RSM video packet or completed frame.
    pub fn last_stream_id(&self) -> Option<u8> {
        self.last_stream_id
    }

    /// Latest unwrapped 64-bit `total_pkg_seq` observed on inbound RSM video packets.
    pub fn last_total_pkg_seq(&self) -> Option<u64> {
        self.last_total_pkg_seq
    }

    /// True once at least one reassembled video frame has been observed on this stream.
    pub fn has_rx_video(&self) -> bool {
        self.rx_video_seen
    }

    /// True once codec parameter sets (VPS/SPS/PPS) have been observed on this stream.
    pub fn is_codec_config_seen(&self) -> bool {
        self.codec_config_seen
    }

    /// True once both codec parameter sets and a keyframe (IDR) have been observed.
    pub fn is_keyframe_synced(&self) -> bool {
        self.keyframe_seen
    }

    /// Recommended interval (in milliseconds) between `0x0b` MP P2S QoS heartbeats.
    ///
    /// Uses a faster 550 ms cadence while video is arriving prior to initial keyframe sync so
    /// WeChat's QoS server observes receiver stall promptly, then settles to 1000 ms.
    pub fn recommended_p2s_interval_ms(&self) -> u64 {
        if self.rx_video_seen && !self.keyframe_seen {
            PRESYNC_P2S_INTERVAL_MS
        } else {
            STEADY_P2S_INTERVAL_MS
        }
    }

    /// Determine whether the next `0x66 IRequest` should set `force = true`.
    ///
    /// Before initial keyframe sync or before any packet sequence is known, forces immediately.
    /// After sync, sends an unforced request first so encoder callbacks run, then alternates
    /// forced retries to bypass sender `lost_point_seq < prev_iframe_seq` gating.
    pub fn should_force_irequest(&self) -> bool {
        !self.keyframe_seen || self.last_total_pkg_seq.is_none() || self.irequest_streak % 2 == 1
    }

    /// Observe one authenticated inbound kind-3 RSM video packet header together with its outer
    /// envelope stream ID (`opened.packed_kind & 0x0f`).
    pub fn observe_packet(&mut self, channel_stream_id: u8, header: &RsmPacketHeader) {
        self.last_stream_id = Some(channel_stream_id & 0x0f);
        self.last_total_pkg_seq =
            Some(advance_seq16(self.last_total_pkg_seq, header.total_pkg_seq));
    }

    /// Observe one reassembled video frame and update codec-config/keyframe sync flags.
    ///
    /// Returns `true` when `frame.prior_frames_dropped && !frame.is_keyframe` flagged a drop
    /// needing recovery. Any keyframe clears pending recovery retries and streaks.
    pub fn observe_frame(&mut self, frame: &ReassembledVideoFrame) -> bool {
        self.last_stream_id = Some(frame.stream_id & 0x0f);
        self.rx_video_seen = true;
        if frame.has_codec_config {
            self.codec_config_seen = true;
        }
        if frame.is_keyframe && self.codec_config_seen {
            self.keyframe_seen = true;
        }
        if frame.is_keyframe {
            self.pending_local_retries = 0;
            self.local_target_override = None;
            self.irequest_streak = 0;
            self.drop_recovery_pending = false;
            false
        } else if frame.prior_frames_dropped {
            self.drop_recovery_pending = true;
            true
        } else {
            false
        }
    }

    /// Record an unrecovered frame assembly drop or ingest error needing a keyframe request.
    pub fn note_assembly_drop(&mut self) {
        self.drop_recovery_pending = true;
    }

    /// Queue a bounded local client/decoder keyframe request retry budget (for example `3`).
    pub fn queue_local_request(&mut self, retries: u8) {
        self.pending_local_retries = self.pending_local_retries.max(retries);
    }

    /// Queue a bounded local keyframe request targeted to `target_member` (used only for
    /// [`KeyframeRequestReason::LocalRequest`]; cleared once retries finish or a keyframe arrives).
    pub fn queue_local_request_for(&mut self, target_member: u16, retries: u8) {
        self.local_target_override = Some(target_member);
        self.pending_local_retries = self.pending_local_retries.max(retries);
    }

    /// Poll due `0x66 IRequest` packets at `now_ms`, applying cooldowns, `force` alternation
    /// and stream-0 compatibility fallbacks.
    pub fn poll_due(
        &mut self,
        local_member: u16,
        target_member: u16,
        now_ms: u64,
    ) -> Vec<ScheduledIRequest> {
        let mut out = Vec::new();

        if self.drop_recovery_pending {
            self.drop_recovery_pending = false;
            if self
                .last_irequest_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= DROP_IREQUEST_COOLDOWN_MS)
            {
                self.push_scheduled(
                    &mut out,
                    local_member,
                    target_member,
                    now_ms,
                    KeyframeRequestReason::FrameDrop,
                );
                return out;
            }
        }

        if self.pending_local_retries == 0
            && !self.keyframe_seen
            && self.initial_irequests_sent < self.max_proactive_initial_requests
            && now_ms.saturating_sub(self.connected_at_ms) >= INITIAL_IREQUEST_DELAY_MS
            && self
                .last_irequest_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= INITIAL_IREQUEST_INTERVAL_MS)
        {
            self.initial_irequests_sent = self.initial_irequests_sent.saturating_add(1);
            self.push_scheduled(
                &mut out,
                local_member,
                target_member,
                now_ms,
                KeyframeRequestReason::InitialSync,
            );
            return out;
        }

        if self.pending_local_retries > 0
            && self
                .last_irequest_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= LOCAL_IREQUEST_COOLDOWN_MS)
        {
            let local_target = self.local_target_override.unwrap_or(target_member);
            self.pending_local_retries = self.pending_local_retries.saturating_sub(1);
            if self.pending_local_retries == 0 {
                self.local_target_override = None;
            }
            self.push_scheduled(
                &mut out,
                local_member,
                local_target,
                now_ms,
                KeyframeRequestReason::LocalRequest,
            );
        }

        out
    }

    fn push_scheduled(
        &mut self,
        out: &mut Vec<ScheduledIRequest>,
        local_member: u16,
        target_member: u16,
        now_ms: u64,
        reason: KeyframeRequestReason,
    ) {
        let stream_id = self.last_stream_id.unwrap_or(1);
        let lost_point_seq = if self.keyframe_seen {
            self.last_total_pkg_seq.unwrap_or(0)
        } else {
            0
        };
        let force = self.should_force_irequest();
        out.push(ScheduledIRequest {
            target_member,
            stream_id,
            lost_point_seq,
            force,
            is_stream0_fallback: false,
            reason,
            payload: encode_irequest_with_force(
                local_member,
                target_member,
                stream_id,
                now_ms,
                lost_point_seq,
                force,
            ),
        });
        if self.irequest_streak >= 2 && self.irequest_streak.is_multiple_of(2) && stream_id != 0 {
            out.push(ScheduledIRequest {
                target_member,
                stream_id: 0,
                lost_point_seq,
                force,
                is_stream0_fallback: true,
                reason,
                payload: encode_irequest_with_force(
                    local_member,
                    target_member,
                    0,
                    now_ms,
                    lost_point_seq,
                    force,
                ),
            });
        }
        self.irequest_streak = self.irequest_streak.wrapping_add(1);
        self.last_irequest_ms = Some(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::video_packet::{
        parse_irequest, parse_rsm_packet_header, RsmVideoPacketizer, VideoWireCodec,
    };

    #[test]
    fn nack_tracker_handles_reorder_wraparound_recovery_and_expiry() {
        let mut tracker = VideoNackTracker::new();

        // Anchor near 16-bit wraparound and skip 0xffff and 0x0000.
        assert!(!tracker.observe_seq(0xfffe, 0));
        assert!(!tracker.observe_seq(0x0001, 5));
        assert!(tracker.poll_due_ranges(25).is_none());

        // 0xffff arrives during reorder grace -> only [0x0000, 0x0001) is NACKed.
        assert!(!tracker.observe_seq(0xffff, 28));
        let (seq1, ranges1) = tracker
            .poll_due_ranges(45)
            .expect("missing 0x0000 should trigger NACK after reorder delay");
        assert_eq!(seq1, 1);
        assert_eq!(ranges1, vec![(0x0000, 0x0001)]);

        // Retransmitted 0x0000 registers as ARQ-recovered.
        assert!(tracker.observe_seq(0x0000, 60));
        assert_eq!(tracker.arq_recovered_total(), 1);
        assert!(tracker.poll_due_ranges(200).is_none());

        // Contiguous range [0x0002, 0x0005) retries 3 times then stops emitting.
        assert!(!tracker.observe_seq(0x0005, 210));
        let (_, r_a) = tracker.poll_due_ranges(250).expect("attempt 1");
        assert_eq!(r_a, vec![(0x0002, 0x0005)]);
        assert!(tracker.poll_due_ranges(270).is_none());
        let (_, r_b) = tracker.poll_due_ranges(345).expect("attempt 2");
        assert_eq!(r_b, vec![(0x0002, 0x0005)]);
        let (_, r_c) = tracker.poll_due_ranges(440).expect("attempt 3");
        assert_eq!(r_c, vec![(0x0002, 0x0005)]);
        assert!(tracker.poll_due_ranges(540).is_none());
    }

    #[test]
    fn nack_tracker_emits_wraparound_range_and_counts_final_attempt_recovery() {
        let mut tracker = VideoNackTracker::new();
        assert!(!tracker.observe_seq(0xfffe, 0));
        assert!(!tracker.observe_seq(0x0001, 5));
        let (arq0, ranges0) = tracker
            .poll_due_ranges(45)
            .expect("wrap-straddling range [0xffff, 0x0001) should be emitted");
        assert_eq!(arq0, 1);
        assert_eq!(ranges0, vec![(0xffff, 0x0001)]);

        assert_eq!(tracker.poll_due_ranges(140).unwrap().0, 2);
        assert_eq!(tracker.poll_due_ranges(235).unwrap().0, 3);
        // Keep tracked until NACK_MAX_AGE_MS even after 3rd attempt so late ARQ replies count.
        assert!(tracker.poll_due_ranges(240).is_none());
        assert!(tracker.observe_seq(0xffff, 260));
        assert!(tracker.observe_seq(0x0000, 265));
        assert_eq!(tracker.arq_recovered_total(), 2);
    }

    #[test]
    fn nack_tracker_ignores_virtual_orientation_packets() {
        let mut tx = RsmVideoPacketizer::new(2, 1).with_orientation(1);
        let vf = tx
            .build_orientation_metadata_packet(VideoWireCodec::H264, 100, 15)
            .unwrap();
        let hdr = parse_rsm_packet_header(&vf).unwrap();
        assert!(hdr.is_virtual_frame);
        assert_eq!(hdr.pkg_seq, None);
        let mut tracker = VideoNackTracker::new();
        assert!(!tracker.observe_header(&hdr, 100));
        assert_eq!(tracker.pending_count(), 0);
    }

    #[test]
    fn p2s_accumulator_reports_loss_freezes_idle_windows_and_presync_stalls() {
        let mut tel = VideoP2sAccumulator::new(0);
        let mut pktz = RsmVideoPacketizer::new(7, 1).with_orientation(1);
        let p_frame = vec![0, 0, 0, 1, 0x41, 0x9a, 0x11, 0x22, 0x33, 0x44];
        let idr_frame = vec![
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0, 0, 0, 1, 0x68, 0xee, 0, 0, 0, 1, 0x65, 0x88,
        ];
        let mk_frame = |is_keyframe: bool, prior_frames_dropped: bool| ReassembledVideoFrame {
            codec: VideoWireCodec::H264,
            member_id: 7,
            stream_id: 1,
            frame_index: 1,
            timestamp_ms: 0,
            orientation: 1,
            prior_frames_dropped,
            fec_recovered: false,
            is_keyframe,
            has_codec_config: is_keyframe,
            payload: vec![],
        };

        // 9 H.264 P-frames over 560 ms BEFORE initial keyframe sync (`keyframe_synced = false`).
        for i in 0..9u32 {
            let at = 40 + u64::from(i) * 65;
            let pkts = pktz
                .packetize(VideoWireCodec::H264, &p_frame, i * 66, 15, 500)
                .unwrap();
            for pkt in &pkts {
                let hdr = parse_rsm_packet_header(pkt).unwrap();
                tel.observe_packet(&hdr, pkt.len(), false, at);
            }
            tel.observe_frame(&mk_frame(false, false), false, at);
        }
        let r1 = tel.snapshot(7, 1, 1, 0x1d, 600);
        assert_eq!(r1.decode_type, 9);
        assert_eq!(r1.recv_fps, 15);
        assert_eq!(r1.play_fps, 0);
        assert_eq!(r1.play_frames, 0);
        assert_eq!(r1.heavy_freeze_cnt, 1);
        assert!(r1.heavy_freeze_time_ratio >= 85);

        // Recovery IDR arrives at 1250 ms (`keyframe_synced = true`), followed by clean P-frames.
        let idr_pkts = pktz
            .packetize(VideoWireCodec::H264, &idr_frame, 1250, 15, 500)
            .unwrap();
        for pkt in &idr_pkts {
            let hdr = parse_rsm_packet_header(pkt).unwrap();
            tel.observe_packet(&hdr, pkt.len(), false, 1250);
        }
        tel.observe_frame(&mk_frame(true, false), true, 1250);
        for i in 1..15u32 {
            let at = 1250 + u64::from(i) * 66;
            let pkts = pktz
                .packetize(VideoWireCodec::H264, &p_frame, 1250 + i * 66, 15, 500)
                .unwrap();
            for pkt in &pkts {
                let hdr = parse_rsm_packet_header(pkt).unwrap();
                tel.observe_packet(&hdr, pkt.len(), false, at);
            }
            tel.observe_frame(&mk_frame(false, false), true, at);
        }
        let _ = tel.snapshot(7, 1, 1, 0x1d, 2250);

        // Clean 1s window has smooth play_fps and 0% freeze.
        for i in 0..15u32 {
            let at = 2300 + u64::from(i) * 66;
            let pkts = pktz
                .packetize(VideoWireCodec::H264, &p_frame, 2300 + i * 66, 15, 500)
                .unwrap();
            for pkt in &pkts {
                let hdr = parse_rsm_packet_header(pkt).unwrap();
                tel.observe_packet(&hdr, pkt.len(), false, at);
            }
            tel.observe_frame(&mk_frame(false, false), true, at);
        }
        let r_clean = tel.snapshot(7, 1, 1, 0x1d, 3250);
        assert_eq!(r_clean.play_fps, 15);
        assert_eq!(r_clean.heavy_freeze_cnt, 0);
        assert_eq!(r_clean.heavy_freeze_time_ratio, 0);
        let bw_before_pause = r_clean.est_bw_kbps;

        // Idle window (camera off) clears anchors without loss/freeze or bandwidth decay.
        let r_idle = tel.snapshot(7, 1, 1, 0x1d, 4250);
        assert_eq!(r_idle.loss_pct, 0);
        assert_eq!(r_idle.heavy_freeze_cnt, 0);
        assert_eq!(r_idle.actual_member_num, 0);
        assert_eq!(r_idle.est_bw_kbps, bw_before_pause);

        // Mid-stream unrecovered drop holds P-frames (`play_fps = 0`, heavy freeze).
        for i in 0..10u32 {
            let at = 4310 + u64::from(i) * 66;
            let pkts = pktz
                .packetize(VideoWireCodec::H264, &p_frame, 4310 + i * 66, 15, 500)
                .unwrap();
            for pkt in &pkts {
                let hdr = parse_rsm_packet_header(pkt).unwrap();
                tel.observe_packet(&hdr, pkt.len(), false, at);
            }
            tel.observe_frame(&mk_frame(false, i == 0), true, at);
        }
        let r_hold = tel.snapshot(7, 1, 1, 0x1d, 5000);
        assert!(r_hold.recv_fps > 0);
        assert_eq!(r_hold.play_fps, 0);
        assert_eq!(r_hold.heavy_freeze_cnt, 1);
    }

    #[test]
    fn keyframe_policy_handles_presync_proactive_escalation_stream0_fallback_and_sync_clear() {
        let mut policy = VideoKeyframePolicy::new(0).with_proactive_initial_requests(6);
        assert!(policy.should_force_irequest());
        assert_eq!(policy.recommended_p2s_interval_ms(), 1000);

        // Before 500 ms initial delay, no proactive IRequest is emitted.
        assert!(policy.poll_due(3, 7, 400).is_empty());

        // At 500 ms, first proactive IRequest fires forced with lost_point_seq = 0.
        let reqs0 = policy.poll_due(3, 7, 500);
        assert_eq!(reqs0.len(), 1);
        assert_eq!(reqs0[0].reason, KeyframeRequestReason::InitialSync);
        assert_eq!(reqs0[0].stream_id, 1);
        assert_eq!(reqs0[0].lost_point_seq, 0);
        assert!(reqs0[0].force);
        assert!(!reqs0[0].is_stream0_fallback);
        let parsed = parse_irequest(&reqs0[0].payload).unwrap();
        assert_eq!(parsed.local_member, 3);
        assert_eq!(parsed.target_member, 7);

        // Second proactive at 2000 ms (streak = 1) -> still forced, no stream-0 fallback yet.
        let reqs1 = policy.poll_due(3, 7, 2000);
        assert_eq!(reqs1.len(), 1);
        assert!(reqs1[0].force);

        // Third proactive at 3500 ms (streak = 2) -> emits primary stream 1 + stream-0 fallback!
        let reqs2 = policy.poll_due(3, 7, 3500);
        assert_eq!(reqs2.len(), 2);
        assert_eq!(reqs2[0].stream_id, 1);
        assert!(!reqs2[0].is_stream0_fallback);
        assert_eq!(reqs2[1].stream_id, 0);
        assert!(reqs2[1].is_stream0_fallback);

        // Observe a P-frame on stream 2 -> recommended P2S speeds up to 550 ms.
        let make_frame =
            |stream_id: u8, kf: bool, cfg: bool, dropped: bool| ReassembledVideoFrame {
                codec: VideoWireCodec::H264,
                member_id: 7,
                stream_id,
                frame_index: 1,
                timestamp_ms: 100,
                orientation: 2,
                prior_frames_dropped: dropped,
                fec_recovered: false,
                is_keyframe: kf,
                has_codec_config: cfg,
                payload: vec![],
            };
        assert!(!policy.observe_frame(&make_frame(2, false, false, false)));
        assert_eq!(policy.recommended_p2s_interval_ms(), 550);

        // Bare IDR before codec config does not satisfy keyframe_synced, though it resets streak.
        assert!(!policy.observe_frame(&make_frame(2, true, false, false)));
        assert!(!policy.is_keyframe_synced());

        // Separate parameter-set unit + subsequent bare IDR satisfies keyframe_synced.
        assert!(!policy.observe_frame(&make_frame(2, false, true, false)));
        assert!(!policy.is_keyframe_synced());
        assert!(!policy.observe_frame(&make_frame(2, true, false, false)));
        assert!(policy.is_keyframe_synced());
        assert_eq!(policy.recommended_p2s_interval_ms(), 1000);

        // Observe a packet with total_pkg_seq = 42, then simulate mid-stream drop:
        // first mid-stream request is unforced with lost_point_seq = 42, second alternates forced!
        let mut pktz = RsmVideoPacketizer::new(7, 2);
        let pkts = pktz
            .packetize(
                VideoWireCodec::H264,
                &[0, 0, 0, 1, 0x41, 0x9a],
                200,
                15,
                500,
            )
            .unwrap();
        let mut hdr = parse_rsm_packet_header(&pkts[0]).unwrap();
        hdr.total_pkg_seq = 42;
        policy.observe_packet(2, &hdr);

        assert!(policy.observe_frame(&make_frame(2, false, false, true)));
        let drop1 = policy.poll_due(3, 7, 5000);
        assert_eq!(drop1.len(), 1);
        assert_eq!(drop1[0].reason, KeyframeRequestReason::FrameDrop);
        assert_eq!(drop1[0].stream_id, 2);
        assert_eq!(drop1[0].lost_point_seq, 42);
        assert!(!drop1[0].force);

        policy.note_assembly_drop();
        let drop2 = policy.poll_due(3, 7, 6300);
        assert_eq!(drop2.len(), 1);
        assert!(drop2[0].force);
        assert_eq!(drop2[0].target_member, 7);

        // Local target override applies only to LocalRequest and clears once retries exhaust.
        policy.queue_local_request_for(9, 1);
        policy.note_assembly_drop();
        let drop3 = policy.poll_due(3, 7, 7600);
        assert_eq!(drop3[0].reason, KeyframeRequestReason::FrameDrop);
        assert_eq!(drop3[0].target_member, 7);

        let local1 = policy.poll_due(3, 7, 8700);
        assert_eq!(local1[0].reason, KeyframeRequestReason::LocalRequest);
        assert_eq!(local1[0].target_member, 9);

        policy.note_assembly_drop();
        let drop4 = policy.poll_due(3, 7, 10_000);
        assert_eq!(drop4[0].reason, KeyframeRequestReason::FrameDrop);
        assert_eq!(drop4[0].target_member, 7);
    }
}
