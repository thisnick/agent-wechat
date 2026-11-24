//! Daemon authority, video pass-through/transcode bridge and event adapter for
//! the shared call-media runtime.
use super::*;
use crate::call_types::{
    CallTrack, CallTrackDirection, CallVideoCodec, CallVideoFormat, CallVideoMode,
    AUDIO_RECEIVE_TRACK_BASE, VIDEO_RECEIVE_TRACK_BASE,
};
use crate::calls::pcm_envelope::{
    video_rotation_degrees, video_rotation_flags, FLAG_CODEC_CONFIG, FLAG_KEYFRAME,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use wechat_calls::{
    native_media::{
        self, ConferenceMediaHost, ConferenceReceiveTrack, DirectAudioFrame, DirectMediaHost,
        DirectMediaOptions, DirectVideoTx, MediaMetric, PreconnectedRelay,
    },
    video_codec::{TranscodeConfig, VideoCodec, VideoTranscoder},
};
use wechat_protocol::calls::video_packet::{ReassembledVideoFrame, VideoRotation, VideoWireCodec};

pub(super) fn select_relay_indices(candidates: &[(usize, usize, SocketAddr)]) -> (usize, usize) {
    candidates
        .iter()
        .find(|(_, _, endpoint)| udp_route_available(*endpoint))
        .or_else(|| candidates.first())
        .map_or((0, 0), |&(conn, udp, _)| (conn, udp))
}

fn udp_route_available(endpoint: SocketAddr) -> bool {
    let bind_addr: SocketAddr = match endpoint {
        SocketAddr::V4(_) => SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
    };
    UdpSocket::bind(bind_addr)
        .and_then(|socket| socket.connect(endpoint))
        .is_ok()
}

impl DirectAudioFrame for QueuedAudio {
    fn generation(&self) -> u32 {
        self.request.generation
    }
    fn samples(&self) -> &[i16] {
        &self.request.samples
    }
}

pub(super) fn to_video_codec(codec: CallVideoCodec) -> VideoCodec {
    match codec {
        CallVideoCodec::H265 => VideoCodec::H265,
        CallVideoCodec::H264 => VideoCodec::H264,
        CallVideoCodec::Jpeg => VideoCodec::Jpeg,
    }
}

pub(super) fn wire_codec_for_mask(video_dec_cap_mask: u32) -> CallVideoCodec {
    // Engine field 33 (`u32VDecCapMask`) packs per-codec decoder resolution tiers in nibbles:
    // HEVC (`Vcodec2` / `HW265`) occupies `0xF0F0_0F00`; H.264 (`SwH264` / `HW264`) occupies `0x0F0F_F000`.
    // Default to H.265 unless the negotiated mask explicitly advertises H.264 without H.265.
    let has_hevc = (video_dec_cap_mask & 0xF0F0_0F00) != 0;
    let has_h264 = (video_dec_cap_mask & 0x0F0F_F000) != 0;
    if !has_hevc && has_h264 {
        CallVideoCodec::H264
    } else {
        CallVideoCodec::H265
    }
}

fn wire_to_call_codec(wire: VideoWireCodec) -> Option<CallVideoCodec> {
    match wire {
        VideoWireCodec::H265 => Some(CallVideoCodec::H265),
        VideoWireCodec::H264 => Some(CallVideoCodec::H264),
        VideoWireCodec::Vcodec1 | VideoWireCodec::MixVcodec1 | VideoWireCodec::Unknown(_) => None,
    }
}

fn call_to_wire_codec(codec: CallVideoCodec) -> Option<VideoWireCodec> {
    match codec {
        CallVideoCodec::H265 => Some(VideoWireCodec::H265),
        CallVideoCodec::H264 => Some(VideoWireCodec::H264),
        CallVideoCodec::Jpeg => None,
    }
}

const MAX_STALLED_RX_TRANSCODE_FRAMES: u32 = 30;
/// Minimum uptime a newly spawned RX FFmpeg transcoder must reach before the
/// stalled-output heuristic may reset it (prevents killing a fresh decoder during
/// synchronous replay of cached startup frames before stdout has produced output).
const MIN_RX_TRANSCODE_STALL_ELAPSED: Duration = Duration::from_secs(2);
/// Maximum time to hold P-frames after a mid-stream drop while waiting for a recovery IDR.
/// If a peer's encoder does not emit an IDR within this window, resume feeding P-frames
/// while continuing periodic background keyframe requests so video does not stay frozen.
const POST_DROP_IDR_HOLD: Duration = Duration::from_millis(1200);
/// Cadence for background keyframe requests after resuming P-frames without a recovery IDR.
const RESUMED_BACKGROUND_IREQ_INTERVAL: Duration = Duration::from_millis(2000);

struct RxTrackBridge {
    generation: u32,
    sequence: u64,
    started_keyframe: bool,
    pending_gap_after_replay: bool,
    drop_hold_since: Option<Instant>,
    awaiting_idr_after_resume: bool,
    last_background_ireq: Option<Instant>,
    source_codec: Option<VideoCodec>,
    cached_codec_config: Option<Vec<u8>>,
    decoder_initialized: bool,
    rotation: VideoRotation,
    transcoder: Option<VideoTranscoder>,
    transcoder_started_at: Option<Instant>,
    pending_meta: VecDeque<(u64, VideoRotation)>,
    frames_pushed_since_output: u32,
    last_timestamp_us: u64,
}

impl RxTrackBridge {
    fn new(generation: u32) -> Self {
        Self {
            generation,
            sequence: 0,
            started_keyframe: false,
            pending_gap_after_replay: false,
            drop_hold_since: None,
            awaiting_idr_after_resume: false,
            last_background_ireq: None,
            source_codec: None,
            cached_codec_config: None,
            decoder_initialized: false,
            rotation: VideoRotation::Deg0,
            transcoder: None,
            transcoder_started_at: None,
            pending_meta: VecDeque::with_capacity(16),
            frames_pushed_since_output: 0,
            last_timestamp_us: 0,
        }
    }

    fn reset_transcoder(&mut self) {
        self.transcoder = None;
        self.transcoder_started_at = None;
        self.pending_meta.clear();
        self.started_keyframe = false;
        self.decoder_initialized = false;
        self.pending_gap_after_replay = false;
        self.drop_hold_since = None;
        self.awaiting_idr_after_resume = false;
        self.last_background_ireq = None;
        self.frames_pushed_since_output = 0;
    }
}

fn should_reset_stalled_transcoder(
    frames_pushed: u32,
    started_at: Option<Instant>,
    now: Instant,
) -> bool {
    frames_pushed >= MAX_STALLED_RX_TRANSCODE_FRAMES
        && started_at.is_some_and(|started| {
            now.saturating_duration_since(started) >= MIN_RX_TRANSCODE_STALL_ELAPSED
        })
}

/// Extract codec parameter-set NALUs (`VPS`/`SPS`/`PPS` for H.265, `SPS`/`PPS` for H.264)
/// from an Annex-B access unit so later IDR keyframes without inline headers can still
/// initialize the FFmpeg decoder.
fn extract_codec_config_nalus(codec: VideoCodec, annexb: &[u8]) -> Option<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        if annexb[i] == 0 && annexb[i + 1] == 0 {
            if annexb[i + 2] == 1 {
                starts.push((i, 3usize));
                i += 3;
                continue;
            }
            if i + 4 < annexb.len() && annexb[i + 2] == 0 && annexb[i + 3] == 1 {
                starts.push((i, 4usize));
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    let mut out = Vec::new();
    for idx in 0..starts.len() {
        let (start, sc_len) = starts[idx];
        let end = starts.get(idx + 1).map_or(annexb.len(), |&(next, _)| next);
        if start + sc_len >= end {
            continue;
        }
        let header = annexb[start + sc_len];
        let is_param = match codec {
            VideoCodec::H265 => matches!((header >> 1) & 0x3f, 32..=34),
            VideoCodec::H264 => matches!(header & 0x1f, 7 | 8),
            VideoCodec::Jpeg => false,
        };
        if is_param {
            out.extend_from_slice(&annexb[start..end]);
        }
    }
    (!out.is_empty()).then_some(out)
}

type ConferenceTrackUpdate = Arc<Mutex<Option<Vec<(u16, u32)>>>>;

struct RxFrameSender {
    tx: Option<SyncSender<ReassembledVideoFrame>>,
    pending_drops: BTreeSet<u32>,
}

impl RxFrameSender {
    fn send_or_mark_drop(&mut self, track_id: u32, mut frame: ReassembledVideoFrame) -> bool {
        let Some(tx) = self.tx.as_ref() else {
            return false;
        };
        if self.pending_drops.contains(&track_id) {
            frame.prior_frames_dropped = true;
        }
        match tx.try_send(frame) {
            Ok(()) => {
                self.pending_drops.remove(&track_id);
                false
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.pending_drops.insert(track_id);
                true
            }
            Err(mpsc::TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Shared inbound video state bridging WeChat RSM reassembly to either direct
/// pass-through or per-track FFmpeg transcoding on a dedicated worker thread
/// so video decoding never blocks the 20 ms UDP audio loop.
pub(super) struct VideoRxBridge {
    direct_peer_member: Option<u16>,
    keyframe_requests: Mutex<Receiver<u32>>,
    self_keyframe_tx: SyncSender<u32>,
    peer_keyframe_tx: Option<SyncSender<()>>,
    rx_video_seen: Option<Arc<AtomicBool>>,
    prefer_live98: bool,
    conference_tracks: ConferenceTrackUpdate,
    stop: Arc<AtomicBool>,
    frame_tx: Mutex<RxFrameSender>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl VideoRxBridge {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        direct_peer_member: Option<u16>,
        format: Option<CallVideoFormat>,
        keyframe_rx: Receiver<u32>,
        self_keyframe_tx: SyncSender<u32>,
        peer_keyframe_tx: Option<SyncSender<()>>,
        rx_video_seen: Option<Arc<AtomicBool>>,
        first_frame_delivered: Option<Arc<AtomicBool>>,
        prefer_live98: bool,
        owner: Option<(BackendSink, tokio::runtime::Handle)>,
    ) -> Self {
        let conference_tracks = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (frame_tx, worker) = match (format, owner) {
            (Some(target_format), Some((sink, runtime))) => {
                let (tx, rx) = mpsc::sync_channel::<ReassembledVideoFrame>(16);
                let is_conference = direct_peer_member.is_none();
                let worker_stop = stop.clone();
                let worker_tracks_update = conference_tracks.clone();
                let worker_kf_tx = self_keyframe_tx.clone();
                let handle = thread::Builder::new()
                    .name("wechat-video-rx".into())
                    .spawn(move || {
                        run_video_rx_worker(
                            is_conference,
                            target_format,
                            rx,
                            worker_tracks_update,
                            worker_kf_tx,
                            first_frame_delivered,
                            sink,
                            runtime,
                            worker_stop,
                        );
                    })
                    .ok();
                if handle.is_some() {
                    (Some(tx), handle)
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        };
        Self {
            direct_peer_member,
            keyframe_requests: Mutex::new(keyframe_rx),
            self_keyframe_tx,
            peer_keyframe_tx,
            rx_video_seen,
            prefer_live98,
            conference_tracks,
            stop,
            frame_tx: Mutex::new(RxFrameSender {
                tx: frame_tx,
                pending_drops: BTreeSet::new(),
            }),
            worker: Mutex::new(worker),
        }
    }

    pub(super) fn set_conference_tracks(&self, active: &[(u16, u32)]) {
        if let Ok(mut pending) = self.conference_tracks.lock() {
            *pending = Some(active.to_vec());
        }
    }

    /// Non-blocking handoff from the UDP media loop to the dedicated video RX worker.
    fn receive_frame(&self, frame: ReassembledVideoFrame) {
        if let Some(seen) = self.rx_video_seen.as_ref() {
            seen.store(true, Ordering::Release);
        }
        let track_id = if self.direct_peer_member.is_some() {
            VIDEO_RECEIVE_TRACK_BASE
        } else {
            VIDEO_RECEIVE_TRACK_BASE + u32::from(frame.member_id)
        };
        if let Ok(mut guard) = self.frame_tx.lock() {
            if guard.send_or_mark_drop(track_id, frame) {
                let _ = self.self_keyframe_tx.try_send(track_id);
            }
        }
    }

    pub(super) fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut guard) = self.frame_tx.lock() {
            guard.tx.take();
            guard.pending_drops.clear();
        }
        if let Ok(mut handle) = self.worker.lock() {
            if let Some(join) = handle.take() {
                let _ = join.join();
            }
        }
    }

    fn take_keyframe_member(&self) -> Option<u16> {
        let rx = self.keyframe_requests.lock().ok()?;
        let track_id = rx.try_recv().ok()?;
        if let Some(peer) = self.direct_peer_member {
            // Coalesce bursty direct-call keyframe requests so queued entries
            // from dropped P-frames do not keep triggering forced IRequests
            // after an IDR arrives.
            let mut matched = track_id == VIDEO_RECEIVE_TRACK_BASE;
            while let Ok(next_track) = rx.try_recv() {
                if next_track == VIDEO_RECEIVE_TRACK_BASE {
                    matched = true;
                }
            }
            matched.then_some(peer)
        } else if track_id >= VIDEO_RECEIVE_TRACK_BASE {
            u16::try_from(track_id - VIDEO_RECEIVE_TRACK_BASE).ok()
        } else {
            None
        }
    }
}

impl Drop for VideoRxBridge {
    fn drop(&mut self) {
        self.shutdown();
    }
}

const MAX_EARLY_CONFERENCE_TRACKS: usize = 16;
const MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK: usize = 128;
const MAX_EARLY_CONFERENCE_BYTES_PER_TRACK: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct EarlyConferenceTrackBuffer {
    frames: VecDeque<ReassembledVideoFrame>,
    bytes: usize,
    dropped_tail: bool,
}

/// Bounded buffer of initial conference frames (latest keyframe plus contiguous
/// trailing inter-frames) that arrive on the UDP relay before `subscribe_video`
/// announces the participant's video track.
struct EarlyConferenceFrameCache {
    tracks: BTreeMap<u32, EarlyConferenceTrackBuffer>,
}

impl EarlyConferenceFrameCache {
    fn new() -> Self {
        Self {
            tracks: BTreeMap::new(),
        }
    }

    fn push(&mut self, track_id: u32, mut frame: ReassembledVideoFrame) {
        if !self.tracks.contains_key(&track_id) && self.tracks.len() >= MAX_EARLY_CONFERENCE_TRACKS
        {
            if let Some(oldest) = self.tracks.keys().next().copied() {
                self.tracks.remove(&oldest);
            }
        }
        let buf = self.tracks.entry(track_id).or_default();
        if frame.is_keyframe {
            if !frame.has_codec_config {
                if let Some(existing) = buf.frames.front() {
                    if existing.is_keyframe
                        && existing.has_codec_config
                        && existing.codec == frame.codec
                    {
                        // Preserve SPS/PPS across a newer bare IDR so the cache resets cleanly
                        // onto the newest keyframe without losing codec configuration.
                        let inherited = wire_to_call_codec(frame.codec).and_then(|c| {
                            extract_codec_config_nalus(to_video_codec(c), &existing.payload)
                        });
                        if let Some(params) = inherited {
                            let mut combined =
                                Vec::with_capacity(params.len() + frame.payload.len());
                            combined.extend_from_slice(&params);
                            combined.extend_from_slice(&frame.payload);
                            frame.payload = combined;
                            frame.has_codec_config = true;
                        } else {
                            // Keep the existing self-contained keyframe intact if parameter-set
                            // extraction failed, and mark the tail discontinuous.
                            buf.dropped_tail = true;
                            return;
                        }
                    }
                }
            }
            buf.bytes = frame.payload.len();
            buf.frames.clear();
            buf.dropped_tail = false;
            buf.frames.push_back(frame);
        } else if !buf.frames.is_empty() {
            if !buf.dropped_tail
                && buf.frames.len() < MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK
                && buf.bytes.saturating_add(frame.payload.len())
                    <= MAX_EARLY_CONFERENCE_BYTES_PER_TRACK
            {
                buf.bytes = buf.bytes.saturating_add(frame.payload.len());
                buf.frames.push_back(frame);
            } else {
                // Once the bounded pre-announce buffer fills, stop appending so the
                // replayed prefix stays contiguous and signal the decoder boundary gap.
                buf.dropped_tail = true;
            }
        }
    }

    fn take(&mut self, track_id: u32) -> (Vec<ReassembledVideoFrame>, bool) {
        self.tracks
            .remove(&track_id)
            .map(|buf| (buf.frames.into_iter().collect(), buf.dropped_tail))
            .unwrap_or_else(|| (Vec::new(), false))
    }

    fn remove(&mut self, track_id: u32) {
        self.tracks.remove(&track_id);
    }
}

fn drain_rx_transcoder_output(
    track_id: u32,
    state: &mut RxTrackBridge,
    target_format: CallVideoFormat,
    first_frame_delivered: Option<&AtomicBool>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) {
    let generation = state.generation;
    while let Some(encoded) = state
        .transcoder
        .as_mut()
        .and_then(VideoTranscoder::try_recv)
    {
        state.frames_pushed_since_output = 0;
        // When JPEG transcoding drops older intermediate frames in its latest-frame
        // slot, align metadata to the newest pushed entry rather than lagging behind.
        let (ts_us, rot) = if target_format.codec == CallVideoCodec::Jpeg {
            let latest = state.pending_meta.back().copied();
            state.pending_meta.clear();
            latest.unwrap_or((state.last_timestamp_us, state.rotation))
        } else {
            state
                .pending_meta
                .pop_front()
                .unwrap_or((state.last_timestamp_us, state.rotation))
        };
        let mut flags = video_rotation_flags(rot);
        if encoded.is_keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if encoded.has_codec_config {
            flags |= FLAG_CODEC_CONFIG;
        }
        let seq = state.sequence;
        state.sequence = state.sequence.saturating_add(1);
        if runtime
            .block_on(sink.video_frame(
                track_id,
                generation,
                target_format.codec,
                flags,
                seq,
                ts_us,
                &encoded.bytes,
            ))
            .is_ok()
        {
            if let Some(seen) = first_frame_delivered {
                seen.store(true, Ordering::Release);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_video_rx_worker(
    is_conference: bool,
    target_format: CallVideoFormat,
    rx: Receiver<ReassembledVideoFrame>,
    conference_tracks: ConferenceTrackUpdate,
    self_keyframe_tx: SyncSender<u32>,
    first_frame_delivered: Option<Arc<AtomicBool>>,
    sink: BackendSink,
    runtime: tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
) {
    let mut tracks = BTreeMap::<u32, RxTrackBridge>::new();
    let mut early_conference_cache = EarlyConferenceFrameCache::new();
    if !is_conference {
        tracks.insert(VIDEO_RECEIVE_TRACK_BASE, RxTrackBridge::new(1));
    }

    while !stop.load(Ordering::Acquire) {
        if is_conference {
            let pending = conference_tracks
                .lock()
                .ok()
                .and_then(|mut guard| guard.take());
            if let Some(active) = pending {
                let keep: BTreeMap<u32, u32> = active
                    .into_iter()
                    .map(|(member, generation)| {
                        (VIDEO_RECEIVE_TRACK_BASE + u32::from(member), generation)
                    })
                    .collect();
                let removed_ids: Vec<u32> = tracks
                    .iter()
                    .filter(|(id, entry)| keep.get(id) != Some(&entry.generation))
                    .map(|(&id, _)| id)
                    .collect();
                tracks.retain(|id, entry| keep.get(id) == Some(&entry.generation));
                for id in removed_ids {
                    if !keep.contains_key(&id) {
                        early_conference_cache.remove(id);
                    }
                }
                let mut newly_added = Vec::new();
                for (id, generation) in keep {
                    if let std::collections::btree_map::Entry::Vacant(slot) = tracks.entry(id) {
                        slot.insert(RxTrackBridge::new(generation));
                        newly_added.push(id);
                    }
                }
                for id in newly_added {
                    let (cached_frames, dropped_tail) = early_conference_cache.take(id);
                    let replayed_any = !cached_frames.is_empty();
                    for cached in cached_frames {
                        process_rx_video_frame(
                            is_conference,
                            target_format,
                            cached,
                            &mut tracks,
                            &mut early_conference_cache,
                            &self_keyframe_tx,
                            first_frame_delivered.as_deref(),
                            &sink,
                            &runtime,
                        );
                        if let Some(state) = tracks.get_mut(&id) {
                            drain_rx_transcoder_output(
                                id,
                                state,
                                target_format,
                                first_frame_delivered.as_deref(),
                                &sink,
                                &runtime,
                            );
                        }
                    }
                    if replayed_any {
                        if let Some(state) = tracks.get_mut(&id) {
                            // Synchronous pre-announce replay pushes buffered frames before the
                            // worker drains FFmpeg stdout; do not count that burst toward stall
                            // detection.
                            state.frames_pushed_since_output = 0;
                            if dropped_tail {
                                state.pending_gap_after_replay = true;
                            }
                        }
                    }
                }
            }
        }

        let has_in_flight = tracks.values().any(|state| !state.pending_meta.is_empty());
        let wait = if has_in_flight {
            Duration::from_millis(4)
        } else {
            Duration::from_millis(10)
        };
        match rx.recv_timeout(wait) {
            Ok(first) => {
                process_rx_video_frame(
                    is_conference,
                    target_format,
                    first,
                    &mut tracks,
                    &mut early_conference_cache,
                    &self_keyframe_tx,
                    first_frame_delivered.as_deref(),
                    &sink,
                    &runtime,
                );
                // Drain a small burst of ready frames so the worker keeps pace with multi-stream arrivals.
                for _ in 0..3 {
                    let Ok(next) = rx.try_recv() else {
                        break;
                    };
                    process_rx_video_frame(
                        is_conference,
                        target_format,
                        next,
                        &mut tracks,
                        &mut early_conference_cache,
                        &self_keyframe_tx,
                        first_frame_delivered.as_deref(),
                        &sink,
                        &runtime,
                    );
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Continuously drain ready transcoded output frames from FFmpeg stdout
        // without waiting for another inbound H.265/H.264 frame to arrive.
        for (&track_id, state) in &mut tracks {
            drain_rx_transcoder_output(
                track_id,
                state,
                target_format,
                first_frame_delivered.as_deref(),
                &sink,
                &runtime,
            );
            if should_reset_stalled_transcoder(
                state.frames_pushed_since_output,
                state.transcoder_started_at,
                Instant::now(),
            ) {
                if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                    eprintln!(
                        "[wechat-call-video] rx transcoder reset track={track_id} stalled_pushes={}",
                        state.frames_pushed_since_output
                    );
                }
                state.reset_transcoder();
                let _ = self_keyframe_tx.try_send(track_id);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RxRecoveryDecision {
    Pass,
    Drop,
    ResumeAfterHold,
    PassWithBackgroundIReq,
}

/// Gate inter-frame P-frames before the first keyframe or briefly after packet loss.
///
/// Before initial codec configuration arrives (`cached_codec_config.is_none()`), all
/// P-frames are strictly dropped. Once a stream is initialized, a mid-stream drop holds
/// P-frames for at most [`POST_DROP_IDR_HOLD`] waiting for a clean IDR; if the peer does
/// not emit one in time, P-frames resume with periodic background keyframe requests until
/// an IDR arrives.
fn evaluate_rx_frame_recovery(
    state: &mut RxTrackBridge,
    is_keyframe: bool,
    prior_frames_dropped: bool,
    now: Instant,
) -> RxRecoveryDecision {
    if is_keyframe {
        state.drop_hold_since = None;
        state.awaiting_idr_after_resume = false;
        state.last_background_ireq = None;
        return RxRecoveryDecision::Pass;
    }
    if prior_frames_dropped {
        if !state.decoder_initialized || state.cached_codec_config.is_none() {
            state.started_keyframe = false;
            state.drop_hold_since = None;
            return RxRecoveryDecision::Drop;
        }
        if state.started_keyframe && !state.awaiting_idr_after_resume {
            state.started_keyframe = false;
            state.drop_hold_since = Some(now);
            return RxRecoveryDecision::Drop;
        }
        if !state.started_keyframe {
            state.drop_hold_since.get_or_insert(now);
        }
    }
    if !state.started_keyframe {
        if state.decoder_initialized && state.cached_codec_config.is_some() {
            if let Some(since) = state.drop_hold_since {
                if now.saturating_duration_since(since) >= POST_DROP_IDR_HOLD {
                    state.started_keyframe = true;
                    state.drop_hold_since = None;
                    state.awaiting_idr_after_resume = true;
                    state.last_background_ireq = Some(now);
                    return RxRecoveryDecision::ResumeAfterHold;
                }
            }
        }
        return RxRecoveryDecision::Drop;
    }
    if state.awaiting_idr_after_resume
        && state
            .last_background_ireq
            .is_none_or(|t| now.saturating_duration_since(t) >= RESUMED_BACKGROUND_IREQ_INTERVAL)
    {
        state.last_background_ireq = Some(now);
        return RxRecoveryDecision::PassWithBackgroundIReq;
    }
    RxRecoveryDecision::Pass
}

enum RxFramePrep {
    DropRequestKeyframe {
        missing_initial_config: bool,
    },
    Deliver {
        prepended: Option<Vec<u8>>,
        has_codec_config: bool,
        request_background_keyframe: bool,
        resumed_after_hold: bool,
    },
}

fn prepare_rx_video_frame_at(
    state: &mut RxTrackBridge,
    src_codec: VideoCodec,
    is_passthrough: bool,
    frame: &ReassembledVideoFrame,
    now: Instant,
) -> RxFramePrep {
    if frame.has_codec_config {
        if let Some(params) = extract_codec_config_nalus(src_codec, &frame.payload) {
            state.cached_codec_config = Some(params);
        }
    }
    let recovery =
        evaluate_rx_frame_recovery(state, frame.is_keyframe, frame.prior_frames_dropped, now);
    let (request_background_keyframe, resumed_after_hold) = match recovery {
        RxRecoveryDecision::Drop => {
            return RxFramePrep::DropRequestKeyframe {
                missing_initial_config: false,
            };
        }
        RxRecoveryDecision::ResumeAfterHold => (true, true),
        RxRecoveryDecision::PassWithBackgroundIReq => (true, false),
        RxRecoveryDecision::Pass => (false, false),
    };
    let mut prepended: Option<Vec<u8>> = None;
    let mut has_codec_config = frame.has_codec_config;
    if !state.started_keyframe || (is_passthrough && frame.is_keyframe) {
        if !frame.has_codec_config {
            if let Some(params) = state.cached_codec_config.as_deref() {
                let mut combined = Vec::with_capacity(params.len() + frame.payload.len());
                combined.extend_from_slice(params);
                combined.extend_from_slice(&frame.payload);
                prepended = Some(combined);
                has_codec_config = true;
            } else if !state.started_keyframe {
                return RxFramePrep::DropRequestKeyframe {
                    missing_initial_config: true,
                };
            }
        }
        state.started_keyframe = true;
        state.decoder_initialized = true;
    }
    RxFramePrep::Deliver {
        prepended,
        has_codec_config,
        request_background_keyframe,
        resumed_after_hold,
    }
}

#[allow(clippy::too_many_arguments)]
fn process_rx_video_frame(
    is_conference: bool,
    target_format: CallVideoFormat,
    mut frame: ReassembledVideoFrame,
    tracks: &mut BTreeMap<u32, RxTrackBridge>,
    early_conference_cache: &mut EarlyConferenceFrameCache,
    self_keyframe_tx: &SyncSender<u32>,
    first_frame_delivered: Option<&AtomicBool>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) {
    let Some(wire_call_codec) = wire_to_call_codec(frame.codec) else {
        // Proprietary codec (e.g. Vcodec1): ignore without breaking the call.
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[wechat-call-video] rx unsupported wire codec={:?} member={} stream={}",
                frame.codec, frame.member_id, frame.stream_id
            );
        }
        return;
    };
    let track_id = if !is_conference {
        VIDEO_RECEIVE_TRACK_BASE
    } else {
        VIDEO_RECEIVE_TRACK_BASE + u32::from(frame.member_id)
    };
    let Some(state) = tracks.get_mut(&track_id) else {
        if is_conference {
            early_conference_cache.push(track_id, frame);
        }
        return;
    };
    if state.pending_gap_after_replay {
        state.pending_gap_after_replay = false;
        if !frame.is_keyframe {
            frame.prior_frames_dropped = true;
        }
    }
    let generation = state.generation;
    let timestamp_us = u64::from(frame.timestamp_ms) * 1_000;
    let rotation = VideoRotation::from_wire(frame.orientation, is_conference);
    state.last_timestamp_us = timestamp_us;

    let src_codec = to_video_codec(wire_call_codec);
    let dst_codec = to_video_codec(target_format.codec);
    if state.source_codec != Some(src_codec) {
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[wechat-call-video] rx stream codec={:?} target={:?} orient={} rot={}deg kf={} cfg={}",
                src_codec,
                dst_codec,
                frame.orientation,
                rotation.degrees(),
                frame.is_keyframe,
                frame.has_codec_config
            );
        }
        state.reset_transcoder();
        state.cached_codec_config = None;
        state.source_codec = Some(src_codec);
    } else if state.rotation != rotation
        && std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1")
    {
        eprintln!(
            "[wechat-call-video] rx rotation change orient={} rot={}deg (no decoder restart)",
            frame.orientation,
            rotation.degrees()
        );
    }
    state.rotation = rotation;
    let is_passthrough = wire_call_codec == target_format.codec;
    let was_started = state.started_keyframe;
    let (prepended, effective_has_codec_config) = match prepare_rx_video_frame_at(
        state,
        src_codec,
        is_passthrough,
        &frame,
        Instant::now(),
    ) {
        RxFramePrep::DropRequestKeyframe {
            missing_initial_config,
        } => {
            if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                if missing_initial_config {
                    eprintln!(
                            "[wechat-call-video] rx dropping initial IDR without codec config track={track_id} codec={:?}; requesting IDR",
                            src_codec
                        );
                } else if frame.prior_frames_dropped {
                    eprintln!(
                            "[wechat-call-video] rx holding P-frames after drop on track={track_id}; requesting IDR"
                        );
                }
            }
            let _ = self_keyframe_tx.try_send(track_id);
            return;
        }
        RxFramePrep::Deliver {
            prepended,
            has_codec_config,
            request_background_keyframe,
            resumed_after_hold,
        } => {
            if resumed_after_hold && std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                eprintln!(
                    "[wechat-call-video] rx resuming P-frames after {}ms IDR hold on track={track_id} fidx={}; continuing background IDR requests",
                    POST_DROP_IDR_HOLD.as_millis(),
                    frame.frame_index
                );
            }
            if request_background_keyframe {
                let _ = self_keyframe_tx.try_send(track_id);
            }
            (prepended, has_codec_config)
        }
    };

    let push_bytes = prepended.as_deref().unwrap_or(&frame.payload);
    if is_passthrough {
        let mut flags = video_rotation_flags(rotation);
        if frame.is_keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if effective_has_codec_config {
            flags |= FLAG_CODEC_CONFIG;
        }
        let seq = state.sequence;
        state.sequence = state.sequence.saturating_add(1);
        if runtime
            .block_on(sink.video_frame(
                track_id,
                generation,
                target_format.codec,
                flags,
                seq,
                timestamp_us,
                push_bytes,
            ))
            .is_ok()
        {
            if let Some(seen) = first_frame_delivered {
                seen.store(true, Ordering::Release);
            }
        } else if !was_started {
            state.started_keyframe = false;
            state.decoder_initialized = false;
            let _ = self_keyframe_tx.try_send(track_id);
        }
        return;
    }

    // Transcoded receive path: keep the H.265/H.264 decoder running continuously at Deg0
    // across orientation changes and convey display rotation in WXCM flags so rotating a
    // mobile device never kills the decoder or drops P-frames waiting for an IDR keyframe.
    if state.transcoder.is_none() {
        match VideoTranscoder::start(TranscodeConfig {
            from: src_codec,
            to: dst_codec,
            width: target_format.max_width,
            height: target_format.max_height,
            fps: target_format.max_fps,
            bitrate_kbps: target_format.bitrate_kbps.unwrap_or(500),
            rotation: VideoRotation::Deg0,
        }) {
            Ok(transcoder) => {
                state.transcoder = Some(transcoder);
                state.transcoder_started_at = Some(Instant::now());
            }
            Err(error) => {
                if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
                    eprintln!(
                        "[wechat-call-video] rx transcoder start failed track={track_id}: {error:#}"
                    );
                }
                state.started_keyframe = false;
                state.decoder_initialized = false;
                return;
            }
        }
    }
    let Some(transcoder) = state.transcoder.as_mut() else {
        return;
    };
    if let Err(error) = transcoder.push(push_bytes) {
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!("[wechat-call-video] rx transcoder push failed track={track_id}: {error:#}");
        }
        state.reset_transcoder();
        let _ = self_keyframe_tx.try_send(track_id);
        return;
    }
    state.frames_pushed_since_output = state.frames_pushed_since_output.saturating_add(1);
    if state.pending_meta.len() >= 16 {
        state.pending_meta.pop_front();
    }
    state.pending_meta.push_back((timestamp_us, rotation));
}

/// Compute the clockwise rotation the outbound transcoder must apply so wire
/// frames are always encoded 90° CCW (matching wire orientation `1`, +90° CW
/// display rotation expected by mobile and conference receivers).
fn tx_wire_rotation(flags: u16) -> VideoRotation {
    match video_rotation_degrees(flags) {
        90 => VideoRotation::Deg0,
        180 => VideoRotation::Deg90,
        270 => VideoRotation::Deg180,
        _ => VideoRotation::Deg270,
    }
}

fn tx_transcode_dimensions(format: CallVideoFormat) -> (u16, u16) {
    // Outgoing wire frames are always rotated 90° CCW relative to the upright negotiated
    // `(max_width, max_height)` regardless of whether the client or daemon performed the
    // quarter turn, so preserve the negotiated long edge in both axes.
    let max_edge = format.max_width.max(format.max_height).clamp(64, 1080);
    (max_edge, max_edge)
}

fn contains_idr_nalu(codec: VideoCodec, annexb: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 <= annexb.len() {
        let sc_len = if annexb[i] == 0 && annexb[i + 1] == 0 {
            if annexb[i + 2] == 1 {
                3
            } else if i + 4 < annexb.len() && annexb[i + 2] == 0 && annexb[i + 3] == 1 {
                4
            } else {
                0
            }
        } else {
            0
        };
        if sc_len > 0 {
            if let Some(&header) = annexb.get(i + sc_len) {
                let is_idr = match codec {
                    VideoCodec::H265 => matches!((header >> 1) & 0x3f, 16..=21),
                    VideoCodec::H264 => (header & 0x1f) == 5,
                    VideoCodec::Jpeg => true,
                };
                if is_idr {
                    return true;
                }
            }
            i += sc_len;
        } else {
            i += 1;
        }
    }
    false
}

/// Spawn the outbound video pass-through/transcoder adapter thread feeding `DirectVideoTx`
/// into the WeChat UDP media loop.
pub(super) fn spawn_video_tx_bridge(
    worker: Arc<Worker>,
    send_format: CallVideoFormat,
    wire_codec: CallVideoCodec,
    video_rx: Receiver<QueuedVideo>,
    peer_keyframe_rx: Receiver<()>,
    sink: BackendSink,
    runtime: tokio::runtime::Handle,
) -> Result<(Receiver<DirectVideoTx>, thread::JoinHandle<()>)> {
    let wire = call_to_wire_codec(wire_codec).context("Unsupported wire video codec")?;
    let (tx, rx) = mpsc::sync_channel::<DirectVideoTx>(8);
    let handle = thread::Builder::new()
        .name("wechat-video-tx".into())
        .spawn(move || {
            let codec_matches_wire =
                send_format.mode == CallVideoMode::Passthrough && send_format.codec == wire_codec;
            let src_codec = to_video_codec(send_format.codec);
            let dst_codec = to_video_codec(wire_codec);
            let mut transcoder: Option<VideoTranscoder> = None;
            let mut transcoder_gen = 0u32;
            let mut transcoder_rotation = VideoRotation::Deg0;
            let mut tx_started_keyframe = false;
            let mut tx_cached_codec_config: Option<Vec<u8>> = None;
            let mut failed_transcode: Option<(u32, VideoRotation)> = None;
            let mut last_kf_forward: Option<Instant> = None;
            let started = Instant::now();
            while worker.current() && !worker.stop.load(Ordering::Acquire) {
                let mut keyframe_requested = false;
                while peer_keyframe_rx.try_recv().is_ok() {
                    keyframe_requested = true;
                }
                if keyframe_requested
                    && last_kf_forward
                        .is_none_or(|last| last.elapsed() >= Duration::from_millis(1500))
                {
                    last_kf_forward = Some(Instant::now());
                    // Natural 1 s GOP with inline parameter sets already satisfies routine S2P
                    // probes without killing and respawning the transcoder pipeline; notify the
                    // client at a bounded cadence so it can respond if needed.
                    let _ = runtime.block_on(sink.peer_keyframe_requested());
                }
                let current_gen = worker.video_generation.load(Ordering::Acquire);
                let camera_on = worker.camera_enabled.load(Ordering::Acquire);
                if !camera_on || (transcoder_gen != 0 && current_gen != transcoder_gen) {
                    transcoder = None;
                    tx_started_keyframe = false;
                    tx_cached_codec_config = None;
                    failed_transcode = None;
                }
                if let Some(enc) = transcoder.as_mut() {
                    while let Some(out) = enc.try_recv() {
                        if !worker.camera_enabled.load(Ordering::Acquire)
                            || worker.video_generation.load(Ordering::Acquire) != transcoder_gen
                        {
                            break;
                        }
                        let ts_ms =
                            u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
                        let _ = tx.try_send(DirectVideoTx {
                            generation: transcoder_gen,
                            codec: wire,
                            timestamp_ms: ts_ms,
                            fps: send_format.max_fps,
                            kbps: u16::try_from(send_format.bitrate_kbps.unwrap_or(500))
                                .unwrap_or(500),
                            annexb: out.bytes,
                        });
                    }
                }
                match video_rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(item) => {
                        let current_gen = worker.video_generation.load(Ordering::Acquire);
                        if !worker.camera_enabled.load(Ordering::Acquire)
                            || !item.request.is_current()
                            || item.request.generation != current_gen
                            || item.queued.elapsed() > Duration::from_millis(1000)
                        {
                            continue;
                        }
                        let needed_rotation = tx_wire_rotation(item.request.flags);
                        let passthrough =
                            codec_matches_wire && needed_rotation == VideoRotation::Deg0;
                        let ts_ms = if passthrough && item.request.timestamp_micros > 0 {
                            u32::try_from(item.request.timestamp_micros / 1_000).unwrap_or(u32::MAX)
                        } else {
                            u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
                        };
                        if passthrough {
                            if transcoder.is_some() {
                                transcoder = None;
                                tx_started_keyframe = false;
                                tx_cached_codec_config = None;
                                failed_transcode = None;
                            }
                            let _ = tx.try_send(DirectVideoTx {
                                generation: current_gen,
                                codec: wire,
                                timestamp_ms: ts_ms,
                                fps: send_format.max_fps,
                                kbps: u16::try_from(send_format.bitrate_kbps.unwrap_or(500))
                                    .unwrap_or(500),
                                annexb: item.request.payload,
                            });
                            continue;
                        }
                        if transcoder_gen != current_gen || transcoder_rotation != needed_rotation {
                            transcoder = None;
                            tx_started_keyframe = false;
                            if transcoder_gen != current_gen {
                                tx_cached_codec_config = None;
                            }
                            transcoder_gen = current_gen;
                            transcoder_rotation = needed_rotation;
                        }
                        if failed_transcode == Some((current_gen, needed_rotation)) {
                            if send_format.codec == wire_codec {
                                let _ = tx.try_send(DirectVideoTx {
                                    generation: current_gen,
                                    codec: wire,
                                    timestamp_ms: ts_ms,
                                    fps: send_format.max_fps,
                                    kbps: u16::try_from(send_format.bitrate_kbps.unwrap_or(500))
                                        .unwrap_or(500),
                                    annexb: item.request.payload,
                                });
                            }
                            continue;
                        }
                        let mut prepended: Option<Vec<u8>> = None;
                        if src_codec != VideoCodec::Jpeg {
                            let inline_params =
                                extract_codec_config_nalus(src_codec, &item.request.payload);
                            let has_inline_params = inline_params.is_some();
                            if let Some(params) = inline_params {
                                tx_cached_codec_config = Some(params);
                            }
                            if !tx_started_keyframe {
                                let is_keyframe = (item.request.flags & FLAG_KEYFRAME) != 0
                                    || contains_idr_nalu(src_codec, &item.request.payload);
                                if !is_keyframe {
                                    if last_kf_forward.is_none_or(|last| {
                                        last.elapsed() >= Duration::from_millis(1500)
                                    }) {
                                        last_kf_forward = Some(Instant::now());
                                        let _ = runtime.block_on(sink.peer_keyframe_requested());
                                    }
                                    continue;
                                }
                                if !has_inline_params {
                                    let Some(params) = tx_cached_codec_config.as_deref() else {
                                        if last_kf_forward.is_none_or(|last| {
                                            last.elapsed() >= Duration::from_millis(1500)
                                        }) {
                                            last_kf_forward = Some(Instant::now());
                                            let _ =
                                                runtime.block_on(sink.peer_keyframe_requested());
                                        }
                                        continue;
                                    };
                                    let mut combined = Vec::with_capacity(
                                        params.len() + item.request.payload.len(),
                                    );
                                    combined.extend_from_slice(params);
                                    combined.extend_from_slice(&item.request.payload);
                                    prepended = Some(combined);
                                }
                                tx_started_keyframe = true;
                            }
                        }
                        if transcoder.is_none() {
                            let (width, height) = tx_transcode_dimensions(send_format);
                            match VideoTranscoder::start(TranscodeConfig {
                                from: src_codec,
                                to: dst_codec,
                                width,
                                height,
                                fps: send_format.max_fps,
                                bitrate_kbps: send_format.bitrate_kbps.unwrap_or(500),
                                rotation: needed_rotation,
                            }) {
                                Ok(enc) => {
                                    transcoder = Some(enc);
                                }
                                Err(_) => {
                                    tx_started_keyframe = false;
                                    failed_transcode = Some((current_gen, needed_rotation));
                                    if send_format.codec == wire_codec {
                                        let _ = tx.try_send(DirectVideoTx {
                                            generation: current_gen,
                                            codec: wire,
                                            timestamp_ms: ts_ms,
                                            fps: send_format.max_fps,
                                            kbps: u16::try_from(
                                                send_format.bitrate_kbps.unwrap_or(500),
                                            )
                                            .unwrap_or(500),
                                            annexb: item.request.payload,
                                        });
                                    }
                                    continue;
                                }
                            }
                        }
                        if let Some(enc) = transcoder.as_mut() {
                            let push_bytes = prepended.as_deref().unwrap_or(&item.request.payload);
                            if enc.push(push_bytes).is_err() {
                                transcoder = None;
                                tx_started_keyframe = false;
                                continue;
                            }
                            while let Some(out) = enc.try_recv() {
                                let out_ts_ms = u32::try_from(started.elapsed().as_millis())
                                    .unwrap_or(u32::MAX);
                                let _ = tx.try_send(DirectVideoTx {
                                    generation: current_gen,
                                    codec: wire,
                                    timestamp_ms: out_ts_ms,
                                    fps: send_format.max_fps,
                                    kbps: u16::try_from(send_format.bitrate_kbps.unwrap_or(500))
                                        .unwrap_or(500),
                                    annexb: out.bytes,
                                });
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            drop(transcoder);
            // Drain any remaining try_recv quietly on shutdown.
            while !matches!(
                video_rx.try_recv(),
                Err(TryRecvError::Empty | TryRecvError::Disconnected)
            ) {}
        })?;
    Ok((rx, handle))
}

pub(super) struct Host<'a> {
    pub(super) worker: &'a Worker,
    pub(super) owner: Option<(&'a BackendSink, &'a tokio::runtime::Handle)>,
    pub(super) video: Option<&'a VideoRxBridge>,
}
impl DirectMediaHost for Host<'_> {
    type Frame = QueuedAudio;
    fn current(&self) -> bool {
        self.worker.current() && !self.worker.stop.load(Ordering::Acquire)
    }
    fn generation(&self) -> u32 {
        self.worker.generation.load(Ordering::Acquire)
    }
    fn frame_current(&self, frame: &QueuedAudio) -> bool {
        audio_current(self.worker, frame)
    }
    fn connected(&self) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime
            .block_on(sink.connected())
            .map_err(|_| anyhow::anyhow!("Call owner unavailable"))
    }
    fn receive_pcm(&self, samples: &[i16]) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime
            .block_on(sink.audio(samples))
            .map_err(|_| anyhow::anyhow!("Call audio receiver unavailable"))
    }
    fn receive_pcm_at(&self, sample_offset: u64, samples: &[i16]) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime
            .block_on(sink.audio_at(sample_offset, samples))
            .map_err(|_| anyhow::anyhow!("Call audio receiver unavailable"))
    }
    fn record(&self, metric: MediaMetric, count: u64) {
        let metrics = &self.worker.metrics;
        let target = match metric {
            MediaMetric::InvalidOrder => &metrics.rx_invalid_order,
            MediaMetric::SkippedFrames => &metrics.rx_skipped_frames,
            MediaMetric::Gaps => &metrics.rx_gaps,
            MediaMetric::DeliveredFrames => &metrics.rx_delivered_frames,
            MediaMetric::StaleFrames => &metrics.tx_stale_frames,
            MediaMetric::InputUnderruns => &metrics.tx_input_underruns,
        };
        target.fetch_add(count, Ordering::Relaxed);
    }
    fn submit(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddr,
        packet: &[u8],
        frames: &[&QueuedAudio],
    ) -> Result<bool> {
        send_datagram(socket, endpoint, packet, self.worker, frames)
    }
    fn video_generation(&self) -> u32 {
        if self.worker.camera_enabled.load(Ordering::Acquire) {
            self.worker.video_generation.load(Ordering::Acquire)
        } else {
            0
        }
    }
    fn receive_video(&self, frame: ReassembledVideoFrame) -> Result<()> {
        if let Some(video) = self.video {
            video.receive_frame(frame);
        }
        Ok(())
    }
    fn on_peer_keyframe_requested(&self) {
        if let Some(tx) = self.video.and_then(|v| v.peer_keyframe_tx.as_ref()) {
            let _ = tx.try_send(());
        } else if let Some((sink, runtime)) = self.owner {
            let _ = runtime.block_on(sink.peer_keyframe_requested());
        }
    }
    fn take_local_keyframe_request(&self) -> Option<u16> {
        self.video.and_then(VideoRxBridge::take_keyframe_member)
    }
    fn prefer_live98_tx(&self) -> bool {
        self.video.is_some_and(|v| v.prefer_live98)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    config: &NativeCallConfig,
    worker: &Arc<Worker>,
    media: wechat_calls::session::NegotiatedMedia,
    audio: Receiver<QueuedAudio>,
    video_rx: Option<Receiver<QueuedVideo>>,
    keyframe_rx: Receiver<u32>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    run_with_preconnected_relay(
        config,
        worker,
        media,
        audio,
        video_rx,
        keyframe_rx,
        None,
        sink,
        runtime,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_with_preconnected_relay(
    config: &NativeCallConfig,
    worker: &Arc<Worker>,
    media: wechat_calls::session::NegotiatedMedia,
    audio: Receiver<QueuedAudio>,
    video_rx: Option<Receiver<QueuedVideo>>,
    keyframe_rx: Receiver<u32>,
    preconnected: Option<PreconnectedRelay>,
    sink: &BackendSink,
    runtime: &tokio::runtime::Handle,
) -> Result<()> {
    let options = DirectMediaOptions {
        epoch: MediaEpoch {
            account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
            logout_epoch: 0,
            call_generation: nonzero64(worker.identity.call_id().as_bytes()),
            key_generation: 1,
        },
        peer_idle_timeout: config.peer_idle_timeout,
    };
    let wire_codec = wire_codec_for_mask(media.video_dec_cap_mask);
    if media.video_enabled {
        let _ = runtime.block_on(sink.set_video_wire_codec(wire_codec));
    }
    let (send_fmt, recv_fmt) = if media.video_enabled {
        sink.negotiated_video_formats().unwrap_or((None, None))
    } else {
        (None, None)
    };
    let (peer_kf_tx, peer_kf_rx) = mpsc::sync_channel::<()>(4);
    let tx_bridge = match (media.video_enabled, send_fmt, video_rx) {
        (true, Some(fmt), Some(rx)) => Some(spawn_video_tx_bridge(
            worker.clone(),
            fmt,
            wire_codec,
            rx,
            peer_kf_rx,
            sink.clone(),
            runtime.clone(),
        )?),
        _ => None,
    };
    let (direct_video_rx, tx_handle) = match tx_bridge {
        Some((rx, handle)) => (Some(rx), Some(handle)),
        None => (None, None),
    };
    let video_bridge = VideoRxBridge::new(
        Some(media.peer_member),
        recv_fmt,
        keyframe_rx,
        worker.video_keyframe_requests.clone(),
        tx_handle.is_some().then_some(peer_kf_tx),
        Some(worker.rx_video_packets_seen.clone()),
        Some(worker.rx_video_keyframe_seen.clone()),
        config.prefer_live98,
        Some((sink.clone(), runtime.clone())),
    );
    let result = native_media::run_direct_media_with_preconnected_relay(
        &Host {
            worker,
            owner: Some((sink, runtime)),
            video: Some(&video_bridge),
        },
        options,
        media,
        audio,
        direct_video_rx,
        preconnected,
    );
    worker.media_stop.store(true, Ordering::Release);
    video_bridge.shutdown();
    if let Some(handle) = tx_handle {
        let _ = handle.join();
    }
    result
}

impl ConferenceMediaHost for Host<'_> {
    fn receive_tracks(&self, tracks: Vec<ConferenceReceiveTrack>) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime.block_on(
            sink.receive_tracks(
                tracks
                    .into_iter()
                    .map(|track| CallTrack::Audio {
                        id: u32::from(track.member) + AUDIO_RECEIVE_TRACK_BASE,
                        generation: track.generation,
                        direction: CallTrackDirection::Receive,
                        format: CallAudioFormat::default(),
                        participant_id: Some(format!("member-{}", track.member)),
                    })
                    .collect(),
            ),
        )?;
        Ok(())
    }
    fn receive_track_pcm(
        &self,
        member: u16,
        generation: u32,
        frame: wechat_calls::conference_media::DecodedAudio,
    ) -> Result<()> {
        let (sink, runtime) = self.owner.context("Call owner unavailable")?;
        runtime.block_on(sink.track_audio(
            super::super::pcm_envelope::MediaHeader {
                track_id: u32::from(member) + AUDIO_RECEIVE_TRACK_BASE,
                generation,
                sequence: frame.sequence,
                timestamp_micros: frame.sample_offset * 1_000_000 / 16000,
            },
            &frame.samples,
        ))?;
        Ok(())
    }
    fn submit_generation(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddr,
        packet: &[u8],
        frames: &[&QueuedAudio],
        generation: u32,
    ) -> Result<bool> {
        send_datagram_generation(
            socket,
            endpoint,
            packet,
            self.worker,
            frames,
            Some(generation),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_codec_config_nalus_separates_parameter_sets_from_slices() {
        let h265 = [
            0, 0, 0, 1, 0x40, 0x01, 0xaa, // VPS (nal type 32)
            0, 0, 0, 1, 0x42, 0x01, 0xbb, // SPS (nal type 33)
            0, 0, 0, 1, 0x44, 0x01, 0xcc, // PPS (nal type 34)
            0, 0, 0, 1, 0x26, 0x01, 0xdd, // IDR slice (nal type 19)
        ];
        assert_eq!(
            extract_codec_config_nalus(VideoCodec::H265, &h265),
            Some(h265[..21].to_vec())
        );

        let h264 = [
            0, 0, 0, 1, 0x67, 0x42, // SPS (nal type 7)
            0, 0, 0, 1, 0x68, 0xce, // PPS (nal type 8)
            0, 0, 0, 1, 0x65, 0x88, // IDR slice (nal type 5)
        ];
        assert_eq!(
            extract_codec_config_nalus(VideoCodec::H264, &h264),
            Some(h264[..12].to_vec())
        );

        let bare_idr = [0, 0, 0, 1, 0x26, 0x01, 0xdd];
        assert_eq!(
            extract_codec_config_nalus(VideoCodec::H265, &bare_idr),
            None
        );
    }

    #[test]
    fn tx_wire_rotation_targets_90_ccw_for_mobile_and_conference_compatibility() {
        use crate::calls::pcm_envelope::{FLAG_ROTATE_180, FLAG_ROTATE_270, FLAG_ROTATE_90};
        assert_eq!(tx_wire_rotation(0), VideoRotation::Deg270);
        assert_eq!(tx_wire_rotation(FLAG_KEYFRAME), VideoRotation::Deg270);
        assert_eq!(tx_wire_rotation(FLAG_ROTATE_90), VideoRotation::Deg0);
        assert_eq!(
            tx_wire_rotation(FLAG_KEYFRAME | FLAG_ROTATE_90),
            VideoRotation::Deg0
        );
        assert_eq!(tx_wire_rotation(FLAG_ROTATE_180), VideoRotation::Deg90);
        assert_eq!(tx_wire_rotation(FLAG_ROTATE_270), VideoRotation::Deg180);
    }

    #[test]
    fn tx_transcode_dimensions_preserves_long_edge_across_quarter_turn() {
        let fmt = CallVideoFormat {
            codec: CallVideoCodec::H264,
            mode: CallVideoMode::Transcoded,
            max_width: 640,
            max_height: 480,
            max_fps: 15,
            bitrate_kbps: Some(500),
        };
        assert_eq!(tx_transcode_dimensions(fmt), (640, 640));
    }

    #[test]
    fn contains_idr_nalu_detects_hevc_and_avc_keyframes() {
        let hevc_idr = [0, 0, 0, 1, 0x40, 0x01, 0, 0, 0, 1, 0x26, 0x01, 0xdd];
        let hevc_p = [0, 0, 0, 1, 0x02, 0x01, 0xdd];
        assert!(contains_idr_nalu(VideoCodec::H265, &hevc_idr));
        assert!(!contains_idr_nalu(VideoCodec::H265, &hevc_p));

        let h264_idr = [0, 0, 1, 0x65, 0x88];
        let h264_p = [0, 0, 1, 0x41, 0x88];
        assert!(contains_idr_nalu(VideoCodec::H264, &h264_idr));
        assert!(!contains_idr_nalu(VideoCodec::H264, &h264_p));
    }

    #[test]
    fn rx_loss_recovery_holds_briefly_then_resumes_with_background_idr_requests() {
        let t0 = Instant::now();
        let mut state = RxTrackBridge::new(1);

        // Before the first keyframe / codec config, P-frames are strictly gated out.
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, false, false, t0),
            RxRecoveryDecision::Drop
        );
        assert!(!state.started_keyframe);
        // Even after a long wait without codec config, P-frames stay dropped.
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, false, false, t0 + Duration::from_secs(5)),
            RxRecoveryDecision::Drop
        );

        // An IDR keyframe passes through, allowing the caller to latch started_keyframe and config.
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, true, false, t0),
            RxRecoveryDecision::Pass
        );
        state.started_keyframe = true;
        state.decoder_initialized = true;
        state.cached_codec_config = Some(vec![0, 0, 0, 1, 0x67, 0x42]);

        // Normal P-frames pass through once latched.
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, false, false, t0 + Duration::from_millis(66)),
            RxRecoveryDecision::Pass
        );

        // Packet/queue loss before a P-frame enters bounded hold.
        let drop_t = t0 + Duration::from_millis(132);
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, false, true, drop_t),
            RxRecoveryDecision::Drop
        );
        assert!(!state.started_keyframe);

        // Within the 1200 ms hold window, subsequent P-frames stay dropped even if another drop occurs.
        assert_eq!(
            evaluate_rx_frame_recovery(
                &mut state,
                false,
                true,
                drop_t + Duration::from_millis(500)
            ),
            RxRecoveryDecision::Drop
        );

        // Once POST_DROP_IDR_HOLD elapses without an IDR, resume feeding P-frames instead of
        // staying permanently frozen, and keep requesting background IDRs every 2000 ms.
        let resume_t = drop_t + POST_DROP_IDR_HOLD;
        assert_eq!(
            evaluate_rx_frame_recovery(&mut state, false, false, resume_t),
            RxRecoveryDecision::ResumeAfterHold
        );
        assert!(state.started_keyframe);
        assert!(state.awaiting_idr_after_resume);

        // Further P-frames (even with additional loss) stay live while awaiting a real IDR.
        assert_eq!(
            evaluate_rx_frame_recovery(
                &mut state,
                false,
                true,
                resume_t + Duration::from_millis(500)
            ),
            RxRecoveryDecision::Pass
        );
        assert_eq!(
            evaluate_rx_frame_recovery(
                &mut state,
                false,
                false,
                resume_t + RESUMED_BACKGROUND_IREQ_INTERVAL
            ),
            RxRecoveryDecision::PassWithBackgroundIReq
        );

        // A real IDR clears the resumed state cleanly.
        assert_eq!(
            evaluate_rx_frame_recovery(
                &mut state,
                true,
                true,
                resume_t + RESUMED_BACKGROUND_IREQ_INTERVAL + Duration::from_millis(100)
            ),
            RxRecoveryDecision::Pass
        );
        assert!(!state.awaiting_idr_after_resume);
    }

    #[test]
    fn prepare_rx_video_frame_gates_and_prepends_codec_config_in_passthrough() {
        let t0 = Instant::now();
        let mut state = RxTrackBridge::new(1);
        let sps_pps = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
        let idr_slice = [0, 0, 0, 1, 0x65, 0x88, 0x99];
        let p_slice = [0, 0, 0, 1, 0x41, 0x11, 0x22];
        let mut idr_with_cfg = Vec::new();
        idr_with_cfg.extend_from_slice(&sps_pps);
        idr_with_cfg.extend_from_slice(&idr_slice);

        let make = |is_keyframe: bool,
                    has_codec_config: bool,
                    prior_frames_dropped: bool,
                    payload: &[u8]| ReassembledVideoFrame {
            codec: VideoWireCodec::H264,
            member_id: 1,
            stream_id: 1,
            frame_index: 1,
            timestamp_ms: 0,
            orientation: 0,
            prior_frames_dropped,
            fec_recovered: false,
            is_keyframe,
            has_codec_config,
            payload: payload.to_vec(),
        };

        // 1. P-frame before first keyframe is dropped and requests a keyframe.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                true,
                &make(false, false, false, &p_slice),
                t0
            ),
            RxFramePrep::DropRequestKeyframe {
                missing_initial_config: false
            }
        ));

        // 2. Initial bare IDR without SPS/PPS is dropped with missing_initial_config = true.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                true,
                &make(true, false, false, &idr_slice),
                t0
            ),
            RxFramePrep::DropRequestKeyframe {
                missing_initial_config: true
            }
        ));
        assert!(!state.started_keyframe);

        // 3. IDR with SPS/PPS starts the stream and caches parameter sets.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                true,
                &make(true, true, false, &idr_with_cfg),
                t0
            ),
            RxFramePrep::Deliver {
                prepended: None,
                has_codec_config: true,
                request_background_keyframe: false,
                resumed_after_hold: false,
            }
        ));
        assert!(state.started_keyframe);
        assert_eq!(
            state.cached_codec_config.as_deref(),
            Some(sps_pps.as_slice())
        );

        // 4. Subsequent P-frame passes through unchanged.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                true,
                &make(false, false, false, &p_slice),
                t0 + Duration::from_millis(66)
            ),
            RxFramePrep::Deliver {
                prepended: None,
                has_codec_config: false,
                request_background_keyframe: false,
                resumed_after_hold: false,
            }
        ));

        // 5. Packet loss drops P-frame and re-arms keyframe recovery.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                true,
                &make(false, false, true, &p_slice),
                t0 + Duration::from_millis(132)
            ),
            RxFramePrep::DropRequestKeyframe {
                missing_initial_config: false
            }
        ));
        assert!(!state.started_keyframe);

        // 6. Recovery IDR without inline SPS/PPS gets cached SPS/PPS prepended and FLAG_CODEC_CONFIG set.
        match prepare_rx_video_frame_at(
            &mut state,
            VideoCodec::H264,
            true,
            &make(true, false, false, &idr_slice),
            t0 + Duration::from_millis(200),
        ) {
            RxFramePrep::Deliver {
                prepended: Some(bytes),
                has_codec_config: true,
                ..
            } => assert_eq!(bytes, idr_with_cfg),
            _ => panic!("expected prepended SPS/PPS on bare recovery IDR"),
        }
        assert!(state.started_keyframe);

        // 7. Even while already started, a later bare IDR in passthrough gets cached SPS/PPS prepended.
        match prepare_rx_video_frame_at(
            &mut state,
            VideoCodec::H264,
            true,
            &make(true, false, false, &idr_slice),
            t0 + Duration::from_millis(300),
        ) {
            RxFramePrep::Deliver {
                prepended: Some(bytes),
                has_codec_config: true,
                ..
            } => assert_eq!(bytes, idr_with_cfg),
            _ => panic!("expected prepended SPS/PPS on subsequent passthrough bare IDR"),
        }
    }

    #[test]
    fn rx_frame_sender_marks_drop_at_exact_stream_position() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut sender = RxFrameSender {
            tx: Some(tx),
            pending_drops: BTreeSet::new(),
        };
        let make_frame = |frame_index: u32, is_keyframe: bool| ReassembledVideoFrame {
            codec: VideoWireCodec::H265,
            member_id: 1,
            stream_id: 0,
            frame_index,
            timestamp_ms: frame_index * 66,
            orientation: 0,
            prior_frames_dropped: false,
            fec_recovered: false,
            is_keyframe,
            has_codec_config: is_keyframe,
            payload: vec![0, 0, 0, 1, 0x26, 0x01],
        };

        // Frame 1 (IDR) enters the 1-slot channel without drop flag.
        assert!(!sender.send_or_mark_drop(VIDEO_RECEIVE_TRACK_BASE, make_frame(1, true)));
        // Frame 2 overflows and is dropped -> records pending drop for the track.
        assert!(sender.send_or_mark_drop(VIDEO_RECEIVE_TRACK_BASE, make_frame(2, false)));

        // Consumer drains pre-drop frame 1 cleanly (still prior_frames_dropped == false).
        let f1 = rx.try_recv().unwrap();
        assert_eq!(f1.frame_index, 1);
        assert!(!f1.prior_frames_dropped);

        // Next post-drop frame 3 carries prior_frames_dropped == true at the exact gap.
        assert!(!sender.send_or_mark_drop(VIDEO_RECEIVE_TRACK_BASE, make_frame(3, false)));
        let f3 = rx.try_recv().unwrap();
        assert_eq!(f3.frame_index, 3);
        assert!(f3.prior_frames_dropped);

        // Once delivered, pending drop clears so frame 4 is unmarked.
        assert!(!sender.send_or_mark_drop(VIDEO_RECEIVE_TRACK_BASE, make_frame(4, false)));
        let f4 = rx.try_recv().unwrap();
        assert_eq!(f4.frame_index, 4);
        assert!(!f4.prior_frames_dropped);
    }

    #[test]
    fn select_relay_indices_prefers_first_routable_candidate() {
        let v4: SocketAddr = "127.0.0.1:6000".parse().unwrap();
        assert_eq!(select_relay_indices(&[]), (0, 0));
        assert_eq!(select_relay_indices(&[(2, 1, v4)]), (2, 1));
        if udp_route_available("[::1]:6000".parse().unwrap()) {
            let v6: SocketAddr = "[::1]:6000".parse().unwrap();
            assert_eq!(select_relay_indices(&[(0, 0, v6), (0, 1, v4)]), (0, 0));
        }
        // If an IPv6 candidate is unroutable on an IPv4-only host, select_relay_indices
        // falls through to the routable IPv4 candidate; on dual-stack hosts it picks index 0.
        let doc_v6: SocketAddr = "[2001:db8::1]:6000".parse().unwrap();
        let picked = select_relay_indices(&[(0, 0, doc_v6), (1, 2, v4)]);
        if udp_route_available(doc_v6) {
            assert_eq!(picked, (0, 0));
        } else {
            assert_eq!(picked, (1, 2));
        }
    }

    #[test]
    fn take_keyframe_member_coalesces_direct_bursts() {
        let (tx, rx) = mpsc::sync_channel(8);
        let direct =
            VideoRxBridge::new(Some(7), None, rx, tx.clone(), None, None, None, false, None);
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE).unwrap();
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE).unwrap();
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE).unwrap();
        assert_eq!(direct.take_keyframe_member(), Some(7));
        assert_eq!(direct.take_keyframe_member(), None);

        // Non-matching tracks mixed with the direct track still resolve once and drain.
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE + 5).unwrap();
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE).unwrap();
        assert_eq!(direct.take_keyframe_member(), Some(7));
        assert_eq!(direct.take_keyframe_member(), None);

        // Unrelated track IDs alone do not trigger a direct keyframe request.
        tx.try_send(VIDEO_RECEIVE_TRACK_BASE + 5).unwrap();
        assert_eq!(direct.take_keyframe_member(), None);

        // Conference mode preserves distinct per-member requests in order.
        let (conf_tx, conf_rx) = mpsc::sync_channel(8);
        let conference = VideoRxBridge::new(
            None,
            None,
            conf_rx,
            conf_tx.clone(),
            None,
            None,
            None,
            false,
            None,
        );
        conf_tx.try_send(VIDEO_RECEIVE_TRACK_BASE + 4).unwrap();
        conf_tx.try_send(VIDEO_RECEIVE_TRACK_BASE + 9).unwrap();
        assert_eq!(conference.take_keyframe_member(), Some(4));
        assert_eq!(conference.take_keyframe_member(), Some(9));
        assert_eq!(conference.take_keyframe_member(), None);
    }

    #[test]
    fn early_conference_cache_preserves_initial_idr_and_p_frames_until_track_added() {
        let mut cache = EarlyConferenceFrameCache::new();
        let sps_pps = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
        let make_frame = |fidx: u32, kf: bool, cfg: bool| {
            let mut payload = Vec::new();
            if cfg {
                payload.extend_from_slice(&sps_pps);
            }
            payload.extend_from_slice(&[0, 0, 0, 1, if kf { 0x65 } else { 0x41 }, fidx as u8]);
            ReassembledVideoFrame {
                codec: VideoWireCodec::H264,
                member_id: 0,
                stream_id: 1,
                frame_index: fidx,
                timestamp_ms: fidx * 66,
                orientation: 1,
                prior_frames_dropped: false,
                fec_recovered: false,
                is_keyframe: kf,
                has_codec_config: cfg,
                payload,
            }
        };
        let track = VIDEO_RECEIVE_TRACK_BASE;

        // Leading P-frame before any IDR is ignored.
        cache.push(track, make_frame(1, false, false));
        assert_eq!(cache.take(track), (Vec::new(), false));

        // 20-frame startup burst (exceeding the old 8-frame cap) stays contiguous with zero drops.
        cache.push(track, make_frame(1, true, true));
        for fidx in 2..=20 {
            cache.push(track, make_frame(fidx, false, false));
        }
        let (replayed, dropped_tail) = cache.take(track);
        assert!(!dropped_tail);
        assert_eq!(replayed.len(), 20);
        assert_eq!(replayed.first().unwrap().frame_index, 1);
        assert_eq!(replayed.last().unwrap().frame_index, 20);
        assert_eq!(cache.take(track), (Vec::new(), false));

        // A bare IDR after a self-contained IDR inherits SPS/PPS and resets the cached GOP cleanly.
        cache.push(track, make_frame(1, true, true));
        cache.push(track, make_frame(2, false, false));
        cache.push(track, make_frame(4, true, false));
        let (replayed, dropped_tail) = cache.take(track);
        assert!(!dropped_tail);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].frame_index, 4);
        assert!(replayed[0].is_keyframe);
        assert!(replayed[0].has_codec_config);
        assert!(replayed[0].payload.starts_with(&sps_pps));

        // Overflowing MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK keeps the contiguous prefix and
        // reports dropped_tail so the receiver requests a recovery IDR instead of decoding across a gap.
        cache.push(track, make_frame(1, true, true));
        for fidx in 2..=(MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK as u32 + 5) {
            cache.push(track, make_frame(fidx, false, false));
        }
        let (replayed, dropped_tail) = cache.take(track);
        assert!(dropped_tail);
        assert_eq!(replayed.len(), MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK);
        assert_eq!(
            replayed.last().unwrap().frame_index,
            MAX_EARLY_CONFERENCE_FRAMES_PER_TRACK as u32
        );
    }

    #[test]
    fn stalled_transcoder_reset_requires_startup_grace_elapsed() {
        let t0 = Instant::now();
        // No transcoder started yet -> never resets.
        assert!(!should_reset_stalled_transcoder(
            MAX_STALLED_RX_TRANSCODE_FRAMES + 10,
            None,
            t0
        ));
        // Synchronous startup replay burst (e.g. 40 frames in < 10 ms) must not reset
        // a newly spawned transcoder before MIN_RX_TRANSCODE_STALL_ELAPSED.
        assert!(!should_reset_stalled_transcoder(
            MAX_STALLED_RX_TRANSCODE_FRAMES + 10,
            Some(t0),
            t0 + Duration::from_millis(5)
        ));
        assert!(!should_reset_stalled_transcoder(
            MAX_STALLED_RX_TRANSCODE_FRAMES + 10,
            Some(t0),
            t0 + MIN_RX_TRANSCODE_STALL_ELAPSED - Duration::from_millis(1)
        ));
        // Once the startup grace has elapsed, >= MAX_STALLED_RX_TRANSCODE_FRAMES with no
        // output triggers a reset, while fewer frames do not.
        assert!(!should_reset_stalled_transcoder(
            MAX_STALLED_RX_TRANSCODE_FRAMES - 1,
            Some(t0),
            t0 + MIN_RX_TRANSCODE_STALL_ELAPSED
        ));
        assert!(should_reset_stalled_transcoder(
            MAX_STALLED_RX_TRANSCODE_FRAMES,
            Some(t0),
            t0 + MIN_RX_TRANSCODE_STALL_ELAPSED
        ));
    }

    #[test]
    fn post_drop_resume_disabled_after_transcoder_reset_until_fresh_idr() {
        let t0 = Instant::now();
        let mut state = RxTrackBridge::new(1);
        let sps_pps = vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce];
        let mut idr_with_cfg = sps_pps.clone();
        idr_with_cfg.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        let bare_idr = vec![0, 0, 0, 1, 0x65, 0x99];
        let p_frame = vec![0, 0, 0, 1, 0x41, 0x01];

        let make = |fidx: u32, kf: bool, cfg: bool, dropped: bool, payload: Vec<u8>| {
            ReassembledVideoFrame {
                codec: VideoWireCodec::H264,
                member_id: 1,
                stream_id: 1,
                frame_index: fidx,
                timestamp_ms: fidx * 66,
                orientation: 0,
                prior_frames_dropped: dropped,
                fec_recovered: false,
                is_keyframe: kf,
                has_codec_config: cfg,
                payload,
            }
        };

        // 1. Initial self-contained IDR initializes decoder and caches SPS/PPS.
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                false,
                &make(1, true, true, false, idr_with_cfg),
                t0,
            ),
            RxFramePrep::Deliver { .. }
        ));
        assert!(state.decoder_initialized);
        assert!(state.cached_codec_config.is_some());

        // 2. Transcoder reset clears decoder_initialized while keeping cached_codec_config.
        state.reset_transcoder();
        assert!(!state.decoder_initialized);
        assert!(state.cached_codec_config.is_some());

        // 3. Post-drop P-frames must stay dropped even past POST_DROP_IDR_HOLD (never ResumeAfterHold).
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                false,
                &make(2, false, false, true, p_frame.clone()),
                t0 + Duration::from_millis(100),
            ),
            RxFramePrep::DropRequestKeyframe { .. }
        ));
        assert!(matches!(
            prepare_rx_video_frame_at(
                &mut state,
                VideoCodec::H264,
                false,
                &make(3, false, false, false, p_frame),
                t0 + POST_DROP_IDR_HOLD + Duration::from_secs(1),
            ),
            RxFramePrep::DropRequestKeyframe { .. }
        ));
        assert!(!state.decoder_initialized);

        // 4. A subsequent bare IDR restarts cleanly with cached SPS/PPS prepended.
        match prepare_rx_video_frame_at(
            &mut state,
            VideoCodec::H264,
            false,
            &make(4, true, false, false, bare_idr.clone()),
            t0 + POST_DROP_IDR_HOLD + Duration::from_secs(2),
        ) {
            RxFramePrep::Deliver {
                prepended: Some(bytes),
                has_codec_config: true,
                resumed_after_hold: false,
                ..
            } => {
                assert!(bytes.starts_with(&sps_pps));
                assert!(bytes.ends_with(&bare_idr));
            }
            _ => panic!("expected bare IDR to restart transcoder with cached SPS/PPS"),
        }
        assert!(state.decoder_initialized);
    }

    #[test]
    fn early_conference_cache_does_not_inherit_config_across_codecs() {
        let mut cache = EarlyConferenceFrameCache::new();
        let track = VIDEO_RECEIVE_TRACK_BASE + 3;
        // H.264 IDR with SPS/PPS and a slice byte 0x41 (whose H.265 header bits match NAL 32/VPS).
        let h264_idr = ReassembledVideoFrame {
            codec: VideoWireCodec::H264,
            member_id: 3,
            stream_id: 1,
            frame_index: 1,
            timestamp_ms: 100,
            orientation: 0,
            prior_frames_dropped: false,
            fec_recovered: false,
            is_keyframe: true,
            has_codec_config: true,
            payload: vec![
                0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88, 0, 0, 0, 1,
                0x41, 0x99,
            ],
        };
        let bare_h265_idr = ReassembledVideoFrame {
            codec: VideoWireCodec::H265,
            member_id: 3,
            stream_id: 1,
            frame_index: 2,
            timestamp_ms: 200,
            orientation: 0,
            prior_frames_dropped: false,
            fec_recovered: false,
            is_keyframe: true,
            has_codec_config: false,
            payload: vec![0, 0, 0, 1, 0x26, 0x01, 0xaa],
        };
        cache.push(track, h264_idr);
        cache.push(track, bare_h265_idr.clone());
        let (replayed, dropped_tail) = cache.take(track);
        assert!(!dropped_tail);
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].codec, VideoWireCodec::H265);
        assert!(!replayed[0].has_codec_config);
        assert_eq!(replayed[0].payload, bare_h265_idr.payload);
    }
}
