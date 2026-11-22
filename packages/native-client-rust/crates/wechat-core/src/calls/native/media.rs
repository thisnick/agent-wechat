//! Daemon authority, video pass-through/transcode bridge and event adapter for
//! the shared call-media runtime.
use super::*;
use crate::call_types::{
    CallTrack, CallTrackDirection, CallVideoCodec, CallVideoFormat, CallVideoMode,
    AUDIO_RECEIVE_TRACK_BASE, VIDEO_RECEIVE_TRACK_BASE,
};
use crate::calls::pcm_envelope::{video_rotation_flags, FLAG_CODEC_CONFIG, FLAG_KEYFRAME};
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use wechat_calls::{
    native_media::{
        self, ConferenceMediaHost, ConferenceReceiveTrack, DirectAudioFrame, DirectMediaHost,
        DirectMediaOptions, DirectVideoTx, MediaMetric,
    },
    video_codec::{TranscodeConfig, VideoCodec, VideoTranscoder},
};
use wechat_protocol::calls::video_packet::{ReassembledVideoFrame, VideoRotation, VideoWireCodec};

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

struct RxTrackBridge {
    generation: u32,
    sequence: u64,
    started_keyframe: bool,
    source_codec: Option<VideoCodec>,
    cached_codec_config: Option<Vec<u8>>,
    rotation: VideoRotation,
    transcoder: Option<VideoTranscoder>,
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
            source_codec: None,
            cached_codec_config: None,
            rotation: VideoRotation::Deg0,
            transcoder: None,
            pending_meta: VecDeque::with_capacity(16),
            frames_pushed_since_output: 0,
            last_timestamp_us: 0,
        }
    }

    fn reset_transcoder(&mut self) {
        self.transcoder = None;
        self.pending_meta.clear();
        self.started_keyframe = false;
        self.frames_pushed_since_output = 0;
    }
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

/// Shared inbound video state bridging WeChat RSM reassembly to either direct
/// pass-through or per-track FFmpeg transcoding on a dedicated worker thread
/// so video decoding never blocks the 20 ms UDP audio loop.
pub(super) struct VideoRxBridge {
    direct_peer_member: Option<u16>,
    keyframe_requests: Mutex<Receiver<u32>>,
    self_keyframe_tx: SyncSender<u32>,
    peer_keyframe_tx: Option<SyncSender<()>>,
    prefer_live98: bool,
    conference_tracks: ConferenceTrackUpdate,
    stop: Arc<AtomicBool>,
    frame_tx: Mutex<Option<SyncSender<ReassembledVideoFrame>>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl VideoRxBridge {
    pub(super) fn new(
        direct_peer_member: Option<u16>,
        format: Option<CallVideoFormat>,
        keyframe_rx: Receiver<u32>,
        self_keyframe_tx: SyncSender<u32>,
        peer_keyframe_tx: Option<SyncSender<()>>,
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
            prefer_live98,
            conference_tracks,
            stop,
            frame_tx: Mutex::new(frame_tx),
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
        let track_id = if self.direct_peer_member.is_some() {
            VIDEO_RECEIVE_TRACK_BASE
        } else {
            VIDEO_RECEIVE_TRACK_BASE + u32::from(frame.member_id)
        };
        if let Ok(guard) = self.frame_tx.lock() {
            if let Some(tx) = guard.as_ref() {
                if matches!(tx.try_send(frame), Err(mpsc::TrySendError::Full(_))) {
                    let _ = self.self_keyframe_tx.try_send(track_id);
                }
            }
        }
    }

    pub(super) fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut tx) = self.frame_tx.lock() {
            tx.take();
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
            (track_id == VIDEO_RECEIVE_TRACK_BASE).then_some(peer)
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

#[allow(clippy::too_many_arguments)]
fn run_video_rx_worker(
    is_conference: bool,
    target_format: CallVideoFormat,
    rx: Receiver<ReassembledVideoFrame>,
    conference_tracks: ConferenceTrackUpdate,
    self_keyframe_tx: SyncSender<u32>,
    sink: BackendSink,
    runtime: tokio::runtime::Handle,
    stop: Arc<AtomicBool>,
) {
    let mut tracks = BTreeMap::<u32, RxTrackBridge>::new();
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
                tracks.retain(|id, entry| keep.get(id) == Some(&entry.generation));
                for (id, generation) in keep {
                    tracks
                        .entry(id)
                        .or_insert_with(|| RxTrackBridge::new(generation));
                }
            }
        }

        match rx.recv_timeout(Duration::from_millis(8)) {
            Ok(frame) => {
                process_rx_video_frame(
                    is_conference,
                    target_format,
                    frame,
                    &mut tracks,
                    &self_keyframe_tx,
                    &sink,
                    &runtime,
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Continuously drain all ready transcoded output frames from FFmpeg stdout
        // without waiting for another inbound H.265/H.264 frame to arrive.
        for (&track_id, state) in &mut tracks {
            let generation = state.generation;
            while let Some(encoded) = state
                .transcoder
                .as_mut()
                .and_then(VideoTranscoder::try_recv)
            {
                state.frames_pushed_since_output = 0;
                let (ts_us, rot) = state
                    .pending_meta
                    .pop_front()
                    .unwrap_or((state.last_timestamp_us, state.rotation));
                let mut flags = video_rotation_flags(rot);
                if encoded.is_keyframe {
                    flags |= FLAG_KEYFRAME;
                }
                if encoded.has_codec_config {
                    flags |= FLAG_CODEC_CONFIG;
                }
                let seq = state.sequence;
                state.sequence = state.sequence.saturating_add(1);
                let _ = runtime.block_on(sink.video_frame(
                    track_id,
                    generation,
                    target_format.codec,
                    flags,
                    seq,
                    ts_us,
                    &encoded.bytes,
                ));
            }
            if state.frames_pushed_since_output >= MAX_STALLED_RX_TRANSCODE_FRAMES {
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

fn process_rx_video_frame(
    is_conference: bool,
    target_format: CallVideoFormat,
    frame: ReassembledVideoFrame,
    tracks: &mut BTreeMap<u32, RxTrackBridge>,
    self_keyframe_tx: &SyncSender<u32>,
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
        return;
    };
    let generation = state.generation;
    let timestamp_us = u64::from(frame.timestamp_ms) * 1_000;
    let rotation = VideoRotation::from_wire(frame.orientation, is_conference);
    state.last_timestamp_us = timestamp_us;

    if wire_call_codec == target_format.codec {
        let mut flags = video_rotation_flags(rotation);
        if frame.is_keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if frame.has_codec_config {
            flags |= FLAG_CODEC_CONFIG;
        }
        state.rotation = rotation;
        let seq = state.sequence;
        state.sequence = state.sequence.saturating_add(1);
        let _ = runtime.block_on(sink.video_frame(
            track_id,
            generation,
            target_format.codec,
            flags,
            seq,
            timestamp_us,
            &frame.payload,
        ));
        return;
    }

    // Transcoded receive path: keep the H.265/H.264 decoder running continuously at Deg0
    // across orientation changes and convey display rotation in WXCM flags so rotating a
    // mobile device never kills the decoder or drops P-frames waiting for an IDR keyframe.
    let src_codec = to_video_codec(wire_call_codec);
    let dst_codec = to_video_codec(target_format.codec);
    if state.source_codec != Some(src_codec) {
        if std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "[wechat-call-video] rx stream codec={:?} orient={} rot={}deg kf={} cfg={}",
                src_codec,
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
    if frame.has_codec_config {
        if let Some(params) = extract_codec_config_nalus(src_codec, &frame.payload) {
            state.cached_codec_config = Some(params);
        }
    }
    let mut prepended: Option<Vec<u8>> = None;
    if !state.started_keyframe {
        if !frame.is_keyframe {
            let _ = self_keyframe_tx.try_send(track_id);
            return;
        }
        if !frame.has_codec_config {
            let Some(params) = state.cached_codec_config.as_deref() else {
                // An IDR without inline VPS/SPS/PPS (and no prior parameter sets)
                // cannot initialize FFmpeg's raw elementary-stream decoder.
                let _ = self_keyframe_tx.try_send(track_id);
                return;
            };
            let mut combined = Vec::with_capacity(params.len() + frame.payload.len());
            combined.extend_from_slice(params);
            combined.extend_from_slice(&frame.payload);
            prepended = Some(combined);
        }
        state.started_keyframe = true;
    }
    if state.transcoder.is_none() {
        let Ok(transcoder) = VideoTranscoder::start(TranscodeConfig {
            from: src_codec,
            to: dst_codec,
            width: target_format.max_width,
            height: target_format.max_height,
            fps: target_format.max_fps,
            bitrate_kbps: target_format.bitrate_kbps.unwrap_or(500),
            rotation: VideoRotation::Deg0,
        }) else {
            state.started_keyframe = false;
            return;
        };
        state.transcoder = Some(transcoder);
    }
    let Some(transcoder) = state.transcoder.as_mut() else {
        return;
    };
    let push_bytes = prepended.as_deref().unwrap_or(&frame.payload);
    if transcoder.push(push_bytes).is_err() {
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
            let passthrough =
                send_format.mode == CallVideoMode::Passthrough && send_format.codec == wire_codec;
            let mut transcoder: Option<VideoTranscoder> = None;
            let mut transcoder_gen = 0u32;
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
                if !camera_on || (transcoder.is_some() && current_gen != transcoder_gen) {
                    transcoder = None;
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
                        let ts_ms = if passthrough && item.request.timestamp_micros > 0 {
                            u32::try_from(item.request.timestamp_micros / 1_000).unwrap_or(u32::MAX)
                        } else {
                            u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
                        };
                        if passthrough {
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
                        if transcoder_gen != current_gen || transcoder.is_none() {
                            transcoder = VideoTranscoder::start(TranscodeConfig {
                                from: to_video_codec(send_format.codec),
                                to: to_video_codec(wire_codec),
                                width: send_format.max_width,
                                height: send_format.max_height,
                                fps: send_format.max_fps,
                                bitrate_kbps: send_format.bitrate_kbps.unwrap_or(500),
                                rotation: VideoRotation::Deg0,
                            })
                            .ok();
                            transcoder_gen = current_gen;
                        }
                        if let Some(enc) = transcoder.as_mut() {
                            if enc.push(&item.request.payload).is_err() {
                                transcoder = None;
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
        endpoint: SocketAddrV4,
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
        config.prefer_live98,
        Some((sink.clone(), runtime.clone())),
    );
    let result = native_media::run_direct_media_with_video(
        &Host {
            worker,
            owner: Some((sink, runtime)),
            video: Some(&video_bridge),
        },
        options,
        media,
        audio,
        direct_video_rx,
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
        endpoint: SocketAddrV4,
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
}
