//! Shared native call audio capture, playback, multi-track mixing, and RMS metering.

use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, time::timeout};
use wechat_audio::{
    AecStatus, AudioConfig, AudioError, AudioSession, PlaybackHandle, PlaybackToken,
};
use wechat_call_contracts::{
    call_types::{CallEvent, CallMembership, CallTrack, CallTrackDirection, CallView},
    pcm_envelope::{MediaPayloadKind, WireMediaFrame},
};
use wechat_calls::{
    audio::{AudioFormat, AudioFrame, MediaError},
    mixer::PcmMixer,
};

pub const LEVEL_FLOOR_DB: f64 = -60.0;

/// Selected audio device identifiers; `None` requests the operating system default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioDeviceOptions {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
}

/// Signal-only RMS and peak meter with fast attack and 8 dB/tick release decay.
#[derive(Clone, Debug)]
pub struct SignalLevel {
    squares: f64,
    count: usize,
    peak: f64,
    db: f64,
    clip_ticks: u8,
}

impl Default for SignalLevel {
    fn default() -> Self {
        Self {
            squares: 0.0,
            count: 0,
            peak: 0.0,
            db: LEVEL_FLOOR_DB,
            clip_ticks: 0,
        }
    }
}

impl SignalLevel {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe_f64(&mut self, samples: impl Iterator<Item = f64>) {
        for sample in samples {
            let value = sample.clamp(-1.0, 1.0);
            self.squares += value * value;
            self.peak = self.peak.max(value.abs());
            self.count += 1;
        }
    }

    pub fn observe_i16(&mut self, samples: &[i16]) {
        self.observe_f64(samples.iter().map(|v| f64::from(*v) / 32768.0));
    }

    pub fn observe_f32(&mut self, samples: &[f32]) {
        self.observe_f64(samples.iter().map(|v| f64::from(*v)));
    }

    pub fn advance(&mut self) {
        let rms = if self.count == 0 {
            0.0
        } else {
            (self.squares / self.count as f64).sqrt()
        };
        let db = if rms > 0.0 {
            (20.0 * rms.log10()).max(LEVEL_FLOOR_DB)
        } else {
            LEVEL_FLOOR_DB
        };
        // Immediate attack; a quiet signal falls 8 dB per refresh tick.
        self.db = db.max(self.db - 8.0);
        self.clip_ticks = if self.peak >= 0.999 {
            10
        } else {
            self.clip_ticks.saturating_sub(1)
        };
        self.squares = 0.0;
        self.count = 0;
        self.peak = 0.0;
    }

    pub fn db(&self) -> f64 {
        self.db
    }

    /// Normalized `[0.0, 1.0]` meter value suitable for UI level bars.
    pub fn normalized(&self) -> f32 {
        (((self.db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0)) as f32
    }

    pub fn clip_ticks(&self) -> u8 {
        self.clip_ticks
    }

    pub fn is_clipping(&self) -> bool {
        self.clip_ticks > 0
    }
}

/// Elastic microphone capture ring that pre-buffers two 20 ms frames before
/// starting playout so 10 ms OS capture chunks arriving slightly out of phase
/// with the 20 ms send tick never inject 10 ms zero-padded notches into speech.
pub struct CaptureRing {
    queued: VecDeque<i16>,
    primed: bool,
}

impl Default for CaptureRing {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureRing {
    pub fn new() -> Self {
        Self {
            queued: VecDeque::with_capacity(2048),
            primed: false,
        }
    }

    pub fn clear(&mut self) {
        self.queued.clear();
        self.primed = false;
    }

    pub fn push_f32(&mut self, samples: &[f32], frame_samples: usize) -> Result<()> {
        for &sample in samples {
            if !sample.is_finite() {
                bail!("invalid microphone sample");
            }
            self.queued
                .push_back((sample.clamp(-1.0, 1.0) * 32767.0).round() as i16);
        }
        let max_samples = frame_samples.saturating_mul(6).max(1920);
        let target_trim = frame_samples.saturating_mul(3).max(960);
        if self.queued.len() > max_samples {
            let excess = self.queued.len() - target_trim;
            self.queued.drain(..excess);
        }
        if !self.primed && self.queued.len() >= frame_samples.saturating_mul(2) {
            self.primed = true;
        }
        Ok(())
    }

    pub fn pop_frame(&mut self, count: usize, muted: bool) -> Vec<i16> {
        let mut frame = vec![0i16; count];
        if self.primed {
            if self.queued.len() >= count {
                for sample in &mut frame {
                    *sample = self.queued.pop_front().expect("checked length");
                }
            } else {
                // Re-prime cleanly instead of splicing partial audio with trailing zeros.
                self.primed = false;
            }
        }
        if muted {
            frame.fill(0);
        }
        frame
    }
}

/// Threaded local microphone capture and speaker playback session built on [`wechat_audio`].
pub struct LocalAudio {
    pub options: AudioDeviceOptions,
    pub aec_status: AecStatus,
    pub startup_summary: String,
    pub playback: PlaybackHandle,
    pub playback_token: PlaybackToken,
    capture: mpsc::Receiver<Vec<f32>>,
    ring: CaptureRing,
    last_capture: Instant,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<Result<()>>>,
}

impl LocalAudio {
    pub async fn open(options: &AudioDeviceOptions) -> Result<Self> {
        let config = AudioConfig {
            input_device: options.input_device.clone(),
            output_device: options.output_device.clone(),
            playback_buffer_ms: 300,
            ..AudioConfig::default()
        };
        let session = timeout(
            Duration::from_secs(12),
            tokio::task::spawn_blocking(move || -> Result<_> {
                let mut session = AudioSession::open(config)?;
                let until = Instant::now() + Duration::from_secs(3);
                loop {
                    match session.recv_timeout(Duration::from_millis(100)) {
                        Ok(samples) if !samples.is_empty() => break,
                        Ok(_) | Err(AudioError::Timeout) if Instant::now() < until => {}
                        Ok(_) | Err(AudioError::Timeout) => {
                            bail!("microphone produced no samples; no call was placed or answered")
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                Ok(session)
            }),
        )
        .await
        .context("audio device startup timed out; no call was placed or answered")???;
        let clean = |text: &str| {
            text.chars()
                .filter(|c| !c.is_control())
                .take(96)
                .collect::<String>()
        };
        let aec_status = session.aec_status();
        let startup_summary = format!(
            "Microphone: {}\nSpeaker: {}\nAudio AEC status: {:?}; cancellation quality is unmeasured. A headset is recommended.",
            clean(options.input_device.as_deref().unwrap_or("OS default")),
            clean(options.output_device.as_deref().unwrap_or("OS default")),
            aec_status,
        );
        let playback = session.playback();
        let playback_token = playback.token()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (sender, capture) = mpsc::channel(32);
        let worker = std::thread::Builder::new()
            .name("wx-call-audio".into())
            .spawn(move || {
                let mut session = session;
                while !worker_stop.load(Ordering::Acquire) {
                    match session.recv_timeout(Duration::from_millis(20)) {
                        Ok(samples) if !samples.is_empty() => {
                            let _ = sender.try_send(samples);
                        }
                        Ok(_) | Err(AudioError::Timeout) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                session.stop()?;
                Ok(())
            })?;
        Ok(Self {
            options: options.clone(),
            aec_status,
            startup_summary,
            playback,
            playback_token,
            capture,
            ring: CaptureRing::new(),
            last_capture: Instant::now(),
            stop,
            worker: Some(worker),
        })
    }

    pub fn request_stop(&self) {
        if !self.stop.swap(true, Ordering::AcqRel) {
            let _ = self.playback.clear();
        }
    }

    pub fn discard_capture(&mut self) {
        while self.capture.try_recv().is_ok() {}
        self.ring.clear();
        self.last_capture = Instant::now();
    }

    pub fn frame(&mut self, count: usize, muted: bool) -> Result<Vec<i16>> {
        loop {
            match self.capture.try_recv() {
                Ok(samples) => {
                    self.last_capture = Instant::now();
                    self.ring.push_f32(&samples, count)?;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => bail!("microphone disconnected"),
            }
        }
        if self.last_capture.elapsed() > Duration::from_secs(2) {
            bail!("microphone stopped producing samples");
        }
        Ok(self.ring.pop_frame(count, muted))
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            timeout(
                Duration::from_secs(3),
                tokio::task::spawn_blocking(move || {
                    worker
                        .join()
                        .map_err(|_| anyhow::anyhow!("audio worker panicked"))?
                }),
            )
            .await
            .context("audio device shutdown timed out")???;
        }
        Ok(())
    }
}

impl Drop for LocalAudio {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Version-three binary audio receive tracks, multi-party [`PcmMixer`], and
/// monotonic 20 ms playout clock with DTX silence re-anchoring.
pub struct ReceiveAudio {
    format: AudioFormat,
    tracks: BTreeMap<u32, CallTrack>,
    retired: BTreeMap<u32, u32>,
    mixer: PcmMixer,
    alignment_delay: Duration,
    clock: Option<(u64, Duration)>,
    next_slot: Option<u64>,
    offset_bias: i64,
    consecutive_late: u32,
    silent_renders: u32,
}

impl ReceiveAudio {
    pub fn new(format: AudioFormat, tracks: Vec<CallTrack>) -> Result<Self> {
        let mut value = Self {
            format,
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            mixer: PcmMixer::new(format, 64, 25, 0)?,
            alignment_delay: Duration::from_millis(60),
            clock: None,
            next_slot: None,
            offset_bias: 0,
            consecutive_late: 0,
            silent_renders: 0,
        };
        for track in tracks {
            if !track.is_video() {
                value.track(track)?;
            }
        }
        Ok(value)
    }

    fn track(&mut self, track: CallTrack) -> Result<()> {
        if track.is_video() {
            return Ok(());
        }
        let Some(audio_format) = track.audio_format() else {
            return Ok(());
        };
        let track_id = track.id();
        let generation = track.generation();
        ensure!(
            track_id != 0
                && generation != 0
                && track.direction() == CallTrackDirection::Receive
                && audio_format.sample_rate == self.format.sample_rate()
                && audio_format.channels == self.format.channels()
                && audio_format.frame_ms == self.format.frame_ms(),
            "unsupported receive track format"
        );
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return Ok(());
        }
        if let Some(old) = self.tracks.get(&track_id) {
            if generation < old.generation() {
                return Ok(());
            }
            ensure!(
                generation != old.generation()
                    || old.participant_id().is_none()
                    || old.participant_id() == track.participant_id(),
                "receive track identity changed without a generation change"
            );
        }
        self.mixer.add_track(track_id, u64::from(generation))?;
        self.tracks.insert(track_id, track);
        Ok(())
    }

    pub fn event(&mut self, event: &CallEvent, call_id: &str) -> Result<bool> {
        match event {
            CallEvent::TrackAdded { call_id: id, track }
            | CallEvent::TrackUpdated { call_id: id, track }
                if id == call_id
                    && !track.is_video()
                    && track.direction() == CallTrackDirection::Receive =>
            {
                self.track(track.clone())?;
                Ok(true)
            }
            CallEvent::TrackRemoved {
                call_id: id,
                track_id,
                generation,
                ..
            } if id == call_id => {
                if self.retired.len() >= 256 && !self.retired.contains_key(track_id) {
                    bail!("receive track history limit reached");
                }
                self.retired
                    .entry(*track_id)
                    .and_modify(|old| *old = (*old).max(*generation))
                    .or_insert(*generation);
                if self
                    .tracks
                    .get(track_id)
                    .is_some_and(|track| track.generation() <= *generation)
                {
                    self.tracks.remove(track_id);
                    self.mixer.remove_track(*track_id);
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    pub fn binary_frame(
        &mut self,
        bytes: &[u8],
        now: Duration,
        on_samples: impl FnMut(u32, Option<&str>, &[i16]),
    ) -> Result<()> {
        let frame = WireMediaFrame::decode(bytes)?;
        self.wire_frame(&frame, now, on_samples)
    }

    pub fn wire_frame(
        &mut self,
        frame: &WireMediaFrame<'_>,
        now: Duration,
        mut on_samples: impl FnMut(u32, Option<&str>, &[i16]),
    ) -> Result<()> {
        if frame.kind != MediaPayloadKind::PcmS16le {
            return Ok(());
        }
        let track_id = frame.header.track_id;
        let generation = frame.header.generation;
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return Ok(());
        }
        let track = self
            .tracks
            .get(&track_id)
            .context("audio arrived before its track announcement")?;
        if generation < track.generation() {
            return Ok(());
        }
        ensure!(
            generation == track.generation(),
            "audio arrived before its track generation"
        );
        let payload = frame.payload();
        ensure!(
            payload.len() == self.format.samples_per_frame() * 2,
            "invalid receive PCM frame length"
        );
        let samples: Vec<i16> = payload
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect();
        let raw_offset = u64::try_from(
            u128::from(frame.header.timestamp_micros) * u128::from(self.format.sample_rate())
                / 1_000_000,
        )
        .context("receive audio timestamp overflow")?;
        let step = self.format.samples_per_channel() as u64;
        let mut offset =
            u64::try_from((i128::from(raw_offset) + i128::from(self.offset_bias)).max(0))
                .context("receive audio clock offset overflow")?;
        if let Some(next) = self.next_slot {
            if offset < next {
                self.consecutive_late = self.consecutive_late.saturating_add(1);
                if self.consecutive_late >= 5 && self.silent_renders >= 3 {
                    // Re-anchor monotonically after sustained drift or DTX pause so
                    // live speech resumes cleanly without rewinding the mixer.
                    self.offset_bias = i64::try_from(i128::from(next) - i128::from(raw_offset))
                        .context("receive audio bias overflow")?;
                    offset = next;
                    self.clock = Some((next, now + self.alignment_delay));
                    self.consecutive_late = 0;
                    self.silent_renders = 0;
                }
            } else if offset > next.saturating_add(step.saturating_mul(25))
                && self.silent_renders >= 3
            {
                self.offset_bias = i64::try_from(i128::from(next) - i128::from(raw_offset))
                    .context("receive audio bias overflow")?;
                offset = next;
                self.clock = Some((next, now + self.alignment_delay));
                self.consecutive_late = 0;
                self.silent_renders = 0;
            } else {
                self.consecutive_late = 0;
            }
        }
        let sequence = frame.header.sequence;
        let participant_id = track.participant_id().map(str::to_owned);
        match self.mixer.push(
            track_id,
            AudioFrame {
                generation: u64::from(generation),
                sequence,
                timestamp: offset,
                samples: samples.clone(),
            },
        ) {
            Ok(()) => {}
            Err(MediaError::OutOfOrder) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        on_samples(track_id, participant_id.as_deref(), &samples);
        if let Some((first, deadline)) = &mut self.clock {
            if now < *deadline {
                *first = (*first).min(offset);
            }
        } else {
            self.clock = Some((offset, now + self.alignment_delay));
        }
        Ok(())
    }

    pub fn render(&mut self, now: Duration) -> Result<Option<Vec<i16>>> {
        let Some((origin, deadline)) = self.clock else {
            return Ok(None);
        };
        if now < deadline {
            return Ok(None);
        }
        let slots =
            u64::try_from((now - deadline).as_millis() / u128::from(self.format.frame_ms()))?;
        let step = self.format.samples_per_channel() as u64;
        let offset = origin
            .checked_add(
                slots
                    .checked_mul(step)
                    .context("playback clock exhausted")?,
            )
            .context("playback clock exhausted")?;
        if self.next_slot.is_some_and(|next| offset < next) {
            return Ok(None);
        }
        self.next_slot = Some(
            offset
                .checked_add(step)
                .context("playback clock exhausted")?,
        );
        let mixed = self.mixer.render(offset)?;
        if mixed.contributing_tracks.is_empty() {
            self.silent_renders = self.silent_renders.saturating_add(1);
        } else {
            self.silent_renders = 0;
        }
        Ok(Some(mixed.samples))
    }

    /// Render every due 20 ms playout slot monotonically from `next_slot` through
    /// the current wall-clock slot, never skipping a slot on minor timer jitter.
    pub fn render_due(&mut self, now: Duration) -> Result<Vec<Vec<i16>>> {
        const MAX_CATCHUP_SLOTS: u64 = 4;
        let Some((origin, deadline)) = self.clock else {
            return Ok(Vec::new());
        };
        if now < deadline {
            return Ok(Vec::new());
        }
        let slots =
            u64::try_from((now - deadline).as_millis() / u128::from(self.format.frame_ms()))?;
        let step = self.format.samples_per_channel() as u64;
        let target = origin
            .checked_add(
                slots
                    .checked_mul(step)
                    .context("playback clock exhausted")?,
            )
            .context("playback clock exhausted")?;
        let mut cursor = self.next_slot.unwrap_or(origin);
        if target < cursor {
            return Ok(Vec::new());
        }
        let due_count = (target - cursor) / step + 1;
        if due_count > MAX_CATCHUP_SLOTS {
            // Skip stale frames only after a genuine multi-frame stall (> 80 ms)
            // so normal timer jitter never drops an intermediate speech frame.
            cursor = target.saturating_sub((MAX_CATCHUP_SLOTS - 1).saturating_mul(step));
        }
        let mut frames = Vec::with_capacity(due_count.min(MAX_CATCHUP_SLOTS) as usize);
        while cursor <= target {
            let mixed = self.mixer.render(cursor)?;
            if mixed.contributing_tracks.is_empty() {
                self.silent_renders = self.silent_renders.saturating_add(1);
            } else {
                self.silent_renders = 0;
            }
            frames.push(mixed.samples);
            cursor = cursor
                .checked_add(step)
                .context("playback clock exhausted")?;
            self.next_slot = Some(cursor);
        }
        Ok(frames)
    }

    pub fn roster(&mut self, call: &CallView) -> Result<()> {
        for (&id, track) in &self.tracks {
            let muted = track
                .participant_id()
                .and_then(|id| call.participants.iter().find(|p| p.participant_id == *id))
                .is_some_and(|p| {
                    p.microphone_enabled == Some(false) || p.membership == CallMembership::Left
                });
            self.mixer.set_muted(id, muted)?;
        }
        Ok(())
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_call_contracts::{
        call_types::CallAudioFormat,
        pcm_envelope::{encode_pcm, MediaHeader},
    };

    #[test]
    fn signal_level_tracks_rms_decay_and_clipping() {
        let mut level = SignalLevel::default();
        level.observe_i16(&[16384; 320]);
        level.advance();
        assert!((level.db() + 6.02).abs() < 0.1);
        assert!(level.normalized() > 0.85);
        assert!(!level.is_clipping());

        level.observe_f32(&[1.0; 320]);
        level.advance();
        assert_eq!(level.clip_ticks(), 10);
        assert!(level.is_clipping());

        for _ in 0..10 {
            level.advance();
        }
        assert_eq!(level.db(), LEVEL_FLOOR_DB);
        assert_eq!(level.clip_ticks(), 0);
    }

    #[test]
    fn capture_ring_absorbs_10ms_chunk_phase_jitter_without_zero_notches() {
        let mut ring = CaptureRing::new();
        let chunk_10ms = vec![0.5f32; 160];
        for _ in 0..3 {
            ring.push_f32(&chunk_10ms, 320).unwrap();
            assert!(ring.pop_frame(320, false).iter().all(|&s| s == 0));
        }
        ring.push_f32(&chunk_10ms, 320).unwrap();
        for cycle in 0..10 {
            let frame = ring.pop_frame(320, false);
            assert!(
                frame.iter().all(|&s| s > 10_000),
                "unexpected zero notch at cycle {cycle}"
            );
            let chunks_this_tick = if cycle % 2 == 0 { 1 } else { 3 };
            for _ in 0..chunks_this_tick {
                ring.push_f32(&chunk_10ms, 320).unwrap();
            }
        }
    }

    #[test]
    fn receive_audio_mixes_simultaneous_tracks_and_emits_per_track_samples() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let track = |id| CallTrack::Audio {
            id,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some(format!("person-{id}")),
        };
        let mut rx = ReceiveAudio::new(format, vec![track(7), track(8)]).unwrap();
        let mut seen = Vec::new();
        for (id, sample) in [(7, 1000i16), (8, 2000i16)] {
            let bytes = encode_pcm(
                MediaHeader {
                    track_id: id,
                    generation: 1,
                    sequence: 0,
                    timestamp_micros: 200_000,
                },
                format,
                &[sample; 320],
            )
            .unwrap();
            rx.binary_frame(&bytes, Duration::ZERO, |tid, pid, pcm| {
                seen.push((tid, pid.map(str::to_owned), pcm[0]));
            })
            .unwrap();
        }
        assert_eq!(
            seen,
            vec![
                (7, Some("person-7".into()), 1000),
                (8, Some("person-8".into()), 2000),
            ]
        );
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![3000; 320]
        );
    }

    #[test]
    fn retirement_replacement_and_late_frames_never_restore_old_audio() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let make_track = |id, generation| CallTrack::Audio {
            id,
            generation,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some(format!("person-{id}")),
        };
        let make_frame = |id: u32, generation: u32, sequence: u64, offset: u64, sample: i16| {
            encode_pcm(
                MediaHeader {
                    track_id: id,
                    generation,
                    sequence,
                    timestamp_micros: offset * 1_000_000 / 16000,
                },
                format,
                &[sample; 320],
            )
            .unwrap()
        };
        let mut rx = ReceiveAudio::new(format, vec![make_track(7, 1)]).unwrap();
        rx.binary_frame(&make_frame(7, 1, 0, 0, 1000), Duration::ZERO, |_, _, _| {})
            .unwrap();
        rx.event(
            &CallEvent::TrackRemoved {
                call_id: "test".into(),
                track_id: 7,
                generation: 1,
                reason: "left".into(),
            },
            "test",
        )
        .unwrap();
        rx.binary_frame(
            &make_frame(7, 1, 1, 320, 1000),
            Duration::ZERO,
            |_, _, _| {},
        )
        .unwrap();
        rx.event(
            &CallEvent::TrackAdded {
                call_id: "test".into(),
                track: make_track(7, 1),
            },
            "test",
        )
        .unwrap();
        assert_eq!(rx.track_count(), 0);
        rx.event(
            &CallEvent::TrackAdded {
                call_id: "test".into(),
                track: make_track(7, 2),
            },
            "test",
        )
        .unwrap();
        rx.binary_frame(&make_frame(7, 2, 0, 0, 2000), Duration::ZERO, |_, _, _| {})
            .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![2000; 320]
        );
        rx.binary_frame(
            &make_frame(7, 2, 1, 0, 30000),
            Duration::from_millis(120),
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(120)).unwrap().unwrap(),
            vec![0; 320]
        );
    }

    #[test]
    fn sustained_late_drift_after_silence_resyncs_monotonically() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let track = CallTrack::Audio {
            id: 2,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some("person-2".into()),
        };
        let make_frame = |sequence: u64, offset: u64, sample: i16| {
            encode_pcm(
                MediaHeader {
                    track_id: 2,
                    generation: 1,
                    sequence,
                    timestamp_micros: offset * 1_000_000 / 16000,
                },
                format,
                &[sample; 320],
            )
            .unwrap()
        };
        let mut rx = ReceiveAudio::new(format, vec![track]).unwrap();
        rx.binary_frame(&make_frame(0, 0, 1500), Duration::ZERO, |_, _, _| {})
            .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![1500; 320]
        );
        for slot in 1..=5u64 {
            assert_eq!(
                rx.render(Duration::from_millis(60 + slot * 20))
                    .unwrap()
                    .unwrap(),
                vec![0; 320]
            );
        }
        for i in 1..=5u64 {
            rx.binary_frame(
                &make_frame(i, i * 320, 4000),
                Duration::from_millis(2000 + i * 20),
                |_, _, _| {},
            )
            .unwrap();
        }
        assert_eq!(
            rx.render(Duration::from_millis(2160)).unwrap().unwrap(),
            vec![4000; 320]
        );
    }

    #[test]
    fn jittered_render_ticks_never_skip_slots_or_insert_gaps() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let track = CallTrack::Audio {
            id: 2,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some("person-2".into()),
        };
        let mut rx = ReceiveAudio::new(format, vec![track]).unwrap();
        for i in 0..6u64 {
            let bytes = encode_pcm(
                MediaHeader {
                    track_id: 2,
                    generation: 1,
                    sequence: i,
                    timestamp_micros: i * 320 * 1_000_000 / 16000,
                },
                format,
                &[(1000 + i * 100) as i16; 320],
            )
            .unwrap();
            rx.binary_frame(&bytes, Duration::from_millis(i * 20), |_, _, _| {})
                .unwrap();
        }
        let mut rendered = Vec::new();
        for ms in [59, 60, 79, 101, 119, 141, 160] {
            for samples in rx.render_due(Duration::from_millis(ms)).unwrap() {
                rendered.push(samples[0]);
            }
        }
        assert_eq!(rendered, vec![1000, 1100, 1200, 1300, 1400, 1500]);
    }

    #[test]
    fn receive_audio_handles_large_timestamps_without_u64_overflow_panic() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let track = CallTrack::Audio {
            id: 2,
            generation: 1,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some("person-2".into()),
        };
        let mut rx = ReceiveAudio::new(format, vec![track]).unwrap();
        // Exceeds u64::MAX / 16000 (~1.15e15) while remaining 20 ms frame-aligned.
        let large_aligned_us = (u64::MAX / 20_000) * 20_000;
        let bytes = encode_pcm(
            MediaHeader {
                track_id: 2,
                generation: 1,
                sequence: 0,
                timestamp_micros: large_aligned_us,
            },
            format,
            &[1000i16; 320],
        )
        .unwrap();
        rx.binary_frame(&bytes, Duration::ZERO, |_, _, _| {})
            .unwrap();
        // Extreme unaligned u64::MAX also returns an error cleanly instead of panicking.
        let unaligned = encode_pcm(
            MediaHeader {
                track_id: 2,
                generation: 1,
                sequence: 1,
                timestamp_micros: u64::MAX,
            },
            format,
            &[1000i16; 320],
        )
        .unwrap();
        let _ = rx.binary_frame(&unaligned, Duration::ZERO, |_, _, _| {});
    }
}
