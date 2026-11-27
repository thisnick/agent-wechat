//! Pure-Rust call media stream analysis (RMS, FFT dominant tone, video RGB/luma/motion),
//! WAV/raw video recording, and inspectable frame snapshots.

use crate::{
    audio::{SignalLevel, LEVEL_FLOOR_DB},
    video::{find_ffmpeg, DecodedFrame},
};
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, VecDeque},
    f64::consts::PI,
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use wechat_call_contracts::{
    call_types::{CallTrack, CallVideoCodec, CallVideoMode, CallView},
    pcm_envelope::{WireMediaFrame, FLAG_CODEC_CONFIG, FLAG_KEYFRAME},
};

const ACTIVE_AUDIO_THRESHOLD_DB: f64 = -45.0;
const FFT_SIZE: usize = 1024;
const MOTION_W: usize = 64;
const MOTION_H: usize = 48;

/// Pure-Rust 16-bit PCM RIFF/WAVE writer whose header can be flushed safely mid-call.
pub struct WavWriter {
    path: PathBuf,
    sample_rate: u32,
    channels: u16,
    data_bytes: u32,
    file: BufWriter<File>,
}

impl WavWriter {
    pub fn create(path: impl AsRef<Path>, sample_rate: u32, channels: u16) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = BufWriter::new(File::create(&path)?);
        file.write_all(&wav_header(sample_rate, channels, 0))?;
        Ok(Self {
            path,
            sample_rate,
            channels,
            data_bytes: 0,
            file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_samples(&mut self, samples: &[i16]) -> Result<()> {
        let bytes_len = u32::try_from(samples.len().saturating_mul(2)).unwrap_or(u32::MAX);
        if self.data_bytes.checked_add(bytes_len).is_none() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        self.file.write_all(&buf)?;
        self.data_bytes += bytes_len;
        Ok(())
    }

    pub fn flush_header(&mut self) -> Result<()> {
        self.file.flush()?;
        let inner = self.file.get_mut();
        inner.seek(SeekFrom::Start(0))?;
        inner.write_all(&wav_header(
            self.sample_rate,
            self.channels,
            self.data_bytes,
        ))?;
        inner.seek(SeekFrom::End(0))?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        self.flush_header()?;
        Ok(self.path)
    }
}

fn wav_header(sample_rate: u32, channels: u16, data_bytes: u32) -> [u8; 44] {
    let byte_rate = sample_rate * u32::from(channels) * 2;
    let block_align = channels * 2;
    let riff_size = 36u32.saturating_add(data_bytes);
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&riff_size.to_le_bytes());
    header[8..12].copy_from_slice(b"WAVE");
    header[12..16].copy_from_slice(b"fmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes()); // PCM chunk size
    header[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM format
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&block_align.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    header
}

/// Estimate the dominant frequency (Hz) and spectral purity (`0.0..=1.0`) of
/// PCM16 mono samples using a Hann-windowed 1024-point radix-2 FFT with
/// parabolic bin interpolation.
pub fn estimate_dominant_frequency(samples: &[i16], sample_rate: u32) -> Option<(f64, f64)> {
    if samples.len() < FFT_SIZE || sample_rate == 0 {
        return None;
    }
    let slice = &samples[samples.len() - FFT_SIZE..];
    let mean_sq = slice
        .iter()
        .map(|&s| {
            let v = f64::from(s) / 32768.0;
            v * v
        })
        .sum::<f64>()
        / FFT_SIZE as f64;
    let rms_db = if mean_sq > 0.0 {
        10.0 * mean_sq.log10()
    } else {
        LEVEL_FLOOR_DB
    };
    if rms_db < ACTIVE_AUDIO_THRESHOLD_DB {
        return None;
    }
    let mut re = [0.0f64; FFT_SIZE];
    let mut im = [0.0f64; FFT_SIZE];
    for (i, &s) in slice.iter().enumerate() {
        let window = 0.5 * (1.0 - (2.0 * PI * i as f64 / (FFT_SIZE - 1) as f64).cos());
        re[i] = (f64::from(s) / 32768.0) * window;
    }
    fft_radix2(&mut re, &mut im);

    let half = FFT_SIZE / 2;
    let mut mag_sq = vec![0.0f64; half];
    let mut total_energy = 0.0f64;
    let mut peak_bin = 2usize;
    let mut peak_val = 0.0f64;
    // Skip DC (bin 0) and sub-30Hz rumble (bin 1).
    for k in 2..half {
        let e = re[k] * re[k] + im[k] * im[k];
        mag_sq[k] = e;
        total_energy += e;
        if e > peak_val {
            peak_val = e;
            peak_bin = k;
        }
    }
    if total_energy <= 1e-12 {
        return None;
    }
    // Parabolic interpolation around peak_bin using log-magnitude for sub-bin accuracy.
    let delta = if peak_bin > 2 && peak_bin + 1 < half {
        let alpha = (mag_sq[peak_bin - 1].max(1e-20)).sqrt();
        let beta = (mag_sq[peak_bin].max(1e-20)).sqrt();
        let gamma = (mag_sq[peak_bin + 1].max(1e-20)).sqrt();
        let denom = alpha - 2.0 * beta + gamma;
        if denom.abs() > 1e-12 {
            (0.5 * (alpha - gamma) / denom).clamp(-0.5, 0.5)
        } else {
            0.0
        }
    } else {
        0.0
    };
    let bin_hz = f64::from(sample_rate) / FFT_SIZE as f64;
    let hz = (peak_bin as f64 + delta) * bin_hz;
    // Count energy in the main lobe (peak ± 2 bins) vs total spectrum.
    let lo = peak_bin.saturating_sub(2).max(2);
    let hi = (peak_bin + 2).min(half - 1);
    let lobe_energy: f64 = mag_sq[lo..=hi].iter().sum();
    let purity = (lobe_energy / total_energy).clamp(0.0, 1.0);
    Some((
        (hz * 10.0).round() / 10.0,
        (purity * 1000.0).round() / 1000.0,
    ))
}

fn fft_radix2(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two());
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / len as f64;
        let wlen_re = ang.cos();
        let wlen_im = ang.sin();
        for i in (0..n).step_by(len) {
            let mut w_re = 1.0f64;
            let mut w_im = 0.0f64;
            let half = len / 2;
            for k in 0..half {
                let u_re = re[i + k];
                let u_im = im[i + k];
                let v_re = re[i + k + half] * w_re - im[i + k + half] * w_im;
                let v_im = re[i + k + half] * w_im + im[i + k + half] * w_re;
                re[i + k] = u_re + v_re;
                im[i + k] = u_im + v_im;
                re[i + k + half] = u_re - v_re;
                im[i + k + half] = u_im - v_im;
                let next_re = w_re * wlen_re - w_im * wlen_im;
                let next_im = w_re * wlen_im + w_im * wlen_re;
                w_re = next_re;
                w_im = next_im;
            }
        }
        len <<= 1;
    }
}

/// JSON summary for one audio stream (send, mixed receive, or per-track receive).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrackReport {
    pub track_id: u32,
    pub generation: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub frames: u64,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_frame_age_ms: Option<u64>,
    pub rms_db: f64,
    pub peak_db: f64,
    pub active_frames: u64,
    pub active_ratio: f64,
    pub clipping_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_hz: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone_purity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_path: Option<String>,
}

pub struct AudioTrackAnalyzer {
    pub track_id: u32,
    pub generation: u32,
    pub participant_id: Option<String>,
    pub contact_id: Option<String>,
    sample_rate: u32,
    frames: u64,
    samples: u64,
    active_frames: u64,
    clipping_count: u64,
    level: SignalLevel,
    peak_db: f64,
    recent_rms_db: f64,
    ring: VecDeque<i16>,
    dominant_hz: Option<f64>,
    tone_purity: Option<f64>,
    last_frame_at: Option<Instant>,
    pub recording_path: Option<PathBuf>,
}

impl AudioTrackAnalyzer {
    pub fn new(
        track_id: u32,
        generation: u32,
        participant_id: Option<String>,
        sample_rate: u32,
    ) -> Self {
        Self {
            track_id,
            generation,
            participant_id,
            contact_id: None,
            sample_rate,
            frames: 0,
            samples: 0,
            active_frames: 0,
            clipping_count: 0,
            level: SignalLevel::default(),
            peak_db: LEVEL_FLOOR_DB,
            recent_rms_db: LEVEL_FLOOR_DB,
            ring: VecDeque::with_capacity(FFT_SIZE * 2),
            dominant_hz: None,
            tone_purity: None,
            last_frame_at: None,
            recording_path: None,
        }
    }

    pub fn observe(&mut self, samples: &[i16], now: Instant) {
        self.frames += 1;
        self.samples += samples.len() as u64;
        self.last_frame_at = Some(now);
        self.level.observe_i16(samples);
        self.level.advance();
        self.recent_rms_db = self.level.db();

        let mut frame_sq = 0.0f64;
        let mut frame_peak = 0.0f64;
        for &s in samples {
            let v = (f64::from(s) / 32768.0).clamp(-1.0, 1.0);
            frame_sq += v * v;
            frame_peak = frame_peak.max(v.abs());
            if s == i16::MAX || s == i16::MIN {
                self.clipping_count += 1;
            }
            if self.ring.len() >= FFT_SIZE {
                self.ring.pop_front();
            }
            self.ring.push_back(s);
        }
        let frame_rms_db = if !samples.is_empty() && frame_sq > 0.0 {
            (10.0 * (frame_sq / samples.len() as f64).log10()).max(LEVEL_FLOOR_DB)
        } else {
            LEVEL_FLOOR_DB
        };
        if frame_rms_db > ACTIVE_AUDIO_THRESHOLD_DB {
            self.active_frames += 1;
        }
        let frame_peak_db = if frame_peak > 0.0 {
            (20.0 * frame_peak.log10()).max(LEVEL_FLOOR_DB)
        } else {
            LEVEL_FLOOR_DB
        };
        self.peak_db = self.peak_db.max(frame_peak_db);

        // Refresh dominant tone estimate every ~5 frames (100 ms) once window is full.
        if self.ring.len() >= FFT_SIZE && self.frames.is_multiple_of(5) {
            let contiguous: Vec<i16> = self.ring.iter().copied().collect();
            if let Some((hz, purity)) = estimate_dominant_frequency(&contiguous, self.sample_rate) {
                self.dominant_hz = Some(hz);
                self.tone_purity = Some(purity);
            } else {
                self.dominant_hz = None;
                self.tone_purity = None;
            }
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn rms_db(&self) -> f64 {
        self.recent_rms_db
    }

    pub fn report(&self, now: Instant) -> AudioTrackReport {
        let duration_ms = if self.sample_rate > 0 {
            self.samples * 1_000 / u64::from(self.sample_rate)
        } else {
            0
        };
        let active_ratio = if self.frames > 0 {
            ((self.active_frames as f64 / self.frames as f64) * 1000.0).round() / 1000.0
        } else {
            0.0
        };
        AudioTrackReport {
            track_id: self.track_id,
            generation: self.generation,
            participant_id: self.participant_id.clone(),
            contact_id: self.contact_id.clone(),
            frames: self.frames,
            duration_ms,
            last_frame_age_ms: self
                .last_frame_at
                .map(|t| now.saturating_duration_since(t).as_millis() as u64),
            rms_db: (self.recent_rms_db * 10.0).round() / 10.0,
            peak_db: (self.peak_db * 10.0).round() / 10.0,
            active_frames: self.active_frames,
            active_ratio,
            clipping_count: self.clipping_count,
            dominant_hz: self.dominant_hz,
            tone_purity: self.tone_purity,
            recording_path: self
                .recording_path
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }
}

/// JSON summary for one video stream (send or receive).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoTrackReport {
    pub track_id: u32,
    pub generation: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub active: bool,
    pub codec: CallVideoCodec,
    pub mode: CallVideoMode,
    pub frames: u64,
    pub keyframes: u64,
    pub codec_configs: u64,
    pub bytes: u64,
    pub fps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_frame_age_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub rotation_degrees: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_rgb: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luma_mean: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luma_stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_frame_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording_path: Option<String>,
}

/// Metadata returned when saving an inspectable video snapshot image.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSnapshotReport {
    pub track_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact_id: Option<String>,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub rotation_degrees: u16,
    pub frame_index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean_rgb: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luma_stddev: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub motion_delta: Option<f64>,
}

pub struct VideoTrackAnalyzer {
    pub track_id: u32,
    pub generation: u32,
    pub participant_id: Option<String>,
    pub contact_id: Option<String>,
    pub active: bool,
    pub codec: CallVideoCodec,
    pub mode: CallVideoMode,
    frames: u64,
    keyframes: u64,
    codec_configs: u64,
    bytes: u64,
    recent_times: VecDeque<Instant>,
    last_frame_at: Option<Instant>,
    width: Option<u32>,
    height: Option<u32>,
    rotation_degrees: u16,
    mean_rgb: Option<[f64; 3]>,
    luma_mean: Option<f64>,
    luma_stddev: Option<f64>,
    motion_delta: Option<f64>,
    prev_grid: Option<Vec<u8>>,
    latest_decoded: Option<DecodedFrame>,
    pub latest_frame_path: Option<PathBuf>,
    pub recording_path: Option<PathBuf>,
}

impl VideoTrackAnalyzer {
    pub fn from_track(track: &CallTrack) -> Option<Self> {
        let format = track.video_format()?;
        let active = match track {
            CallTrack::Video { active, .. } => *active,
            CallTrack::Audio { .. } => false,
        };
        Some(Self {
            track_id: track.id(),
            generation: track.generation(),
            participant_id: track.participant_id().map(str::to_owned),
            contact_id: None,
            active,
            codec: format.codec,
            mode: format.mode,
            frames: 0,
            keyframes: 0,
            codec_configs: 0,
            bytes: 0,
            recent_times: VecDeque::with_capacity(64),
            last_frame_at: None,
            width: None,
            height: None,
            rotation_degrees: 0,
            mean_rgb: None,
            luma_mean: None,
            luma_stddev: None,
            motion_delta: None,
            prev_grid: None,
            latest_decoded: None,
            latest_frame_path: None,
            recording_path: None,
        })
    }

    pub fn update_track(&mut self, track: &CallTrack) {
        if let Some(format) = track.video_format() {
            self.codec = format.codec;
            self.mode = format.mode;
        }
        self.generation = track.generation();
        if let CallTrack::Video { active, .. } = track {
            self.active = *active;
        }
        if let Some(pid) = track.participant_id() {
            self.participant_id = Some(pid.to_owned());
        }
    }

    pub fn observe_wire(&mut self, wire: &WireMediaFrame<'_>, now: Instant) {
        self.active = true;
        self.frames += 1;
        if wire.flags & FLAG_KEYFRAME != 0 {
            self.keyframes += 1;
        }
        if wire.flags & FLAG_CODEC_CONFIG != 0 {
            self.codec_configs += 1;
        }
        self.bytes += wire.payload().len() as u64;
        self.rotation_degrees = wire.rotation_degrees();
        self.last_frame_at = Some(now);
        self.recent_times.push_back(now);
        while self
            .recent_times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > Duration::from_secs(2))
        {
            self.recent_times.pop_front();
        }
    }

    pub fn observe_sent(&mut self, bytes: usize, flags: u16, now: Instant) {
        self.active = true;
        self.frames += 1;
        if flags & FLAG_KEYFRAME != 0 {
            self.keyframes += 1;
        }
        if flags & FLAG_CODEC_CONFIG != 0 {
            self.codec_configs += 1;
        }
        self.bytes += bytes as u64;
        self.last_frame_at = Some(now);
        self.recent_times.push_back(now);
        while self
            .recent_times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) > Duration::from_secs(2))
        {
            self.recent_times.pop_front();
        }
    }

    pub fn observe_decoded(&mut self, frame: &DecodedFrame, now: Instant) {
        self.width = Some(frame.width);
        self.height = Some(frame.height);
        self.last_frame_at = Some(now);
        let stats = analyze_rgb24(frame);
        self.mean_rgb = Some(stats.mean_rgb);
        self.luma_mean = Some(stats.luma_mean);
        self.luma_stddev = Some(stats.luma_stddev);
        if let Some(prev) = &self.prev_grid {
            let diff: f64 = prev
                .iter()
                .zip(stats.grid.iter())
                .map(|(&a, &b)| f64::from(a.abs_diff(b)))
                .sum::<f64>()
                / stats.grid.len().max(1) as f64;
            self.motion_delta = Some((diff * 100.0).round() / 100.0);
        }
        self.prev_grid = Some(stats.grid);
        self.latest_decoded = Some(frame.clone());
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn latest_decoded(&self) -> Option<&DecodedFrame> {
        self.latest_decoded.as_ref()
    }

    pub fn save_snapshot(&mut self, path: &Path) -> Result<VideoSnapshotReport> {
        let frame = self
            .latest_decoded
            .as_ref()
            .context("no decoded video frame available for this track yet")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let img = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb.clone())
            .context("invalid decoded RGB buffer dimensions")?;
        let dynamic = image::DynamicImage::ImageRgb8(img);
        let fmt = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("png") => image::ImageFormat::Png,
            _ => image::ImageFormat::Jpeg,
        };
        dynamic
            .save_with_format(path, fmt)
            .with_context(|| format!("failed to write snapshot {}", path.display()))?;
        self.latest_frame_path = Some(path.to_path_buf());
        Ok(VideoSnapshotReport {
            track_id: self.track_id,
            participant_id: self.participant_id.clone(),
            contact_id: self.contact_id.clone(),
            path: path.display().to_string(),
            width: frame.width,
            height: frame.height,
            rotation_degrees: self.rotation_degrees,
            frame_index: self.frames,
            mean_rgb: self.mean_rgb,
            luma_stddev: self.luma_stddev,
            motion_delta: self.motion_delta,
        })
    }

    pub fn report(&self, now: Instant) -> VideoTrackReport {
        let fps = if self.recent_times.len() >= 2 {
            let span = self
                .recent_times
                .back()
                .unwrap()
                .saturating_duration_since(*self.recent_times.front().unwrap())
                .as_secs_f64();
            if span > 0.05 {
                (((self.recent_times.len() - 1) as f64 / span) * 10.0).round() / 10.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        VideoTrackReport {
            track_id: self.track_id,
            generation: self.generation,
            participant_id: self.participant_id.clone(),
            contact_id: self.contact_id.clone(),
            active: self.active,
            codec: self.codec,
            mode: self.mode,
            frames: self.frames,
            keyframes: self.keyframes,
            codec_configs: self.codec_configs,
            bytes: self.bytes,
            fps,
            last_frame_age_ms: self
                .last_frame_at
                .map(|t| now.saturating_duration_since(t).as_millis() as u64),
            width: self.width,
            height: self.height,
            rotation_degrees: self.rotation_degrees,
            mean_rgb: self.mean_rgb,
            luma_mean: self.luma_mean,
            luma_stddev: self.luma_stddev,
            motion_delta: self.motion_delta,
            latest_frame_path: self
                .latest_frame_path
                .as_ref()
                .map(|p| p.display().to_string()),
            recording_path: self
                .recording_path
                .as_ref()
                .map(|p| p.display().to_string()),
        }
    }
}

struct RgbAnalysis {
    mean_rgb: [f64; 3],
    luma_mean: f64,
    luma_stddev: f64,
    grid: Vec<u8>,
}

fn analyze_rgb24(frame: &DecodedFrame) -> RgbAnalysis {
    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 || frame.rgb.len() < w * h * 3 {
        return RgbAnalysis {
            mean_rgb: [0.0, 0.0, 0.0],
            luma_mean: 0.0,
            luma_stddev: 0.0,
            grid: vec![0; MOTION_W * MOTION_H],
        };
    }
    let mut sum_r = 0.0f64;
    let mut sum_g = 0.0f64;
    let mut sum_b = 0.0f64;
    let mut sum_y = 0.0f64;
    let mut sum_y2 = 0.0f64;
    let mut grid = Vec::with_capacity(MOTION_W * MOTION_H);
    let count = (MOTION_W * MOTION_H) as f64;
    for gy in 0..MOTION_H {
        let sy = (gy * h) / MOTION_H;
        for gx in 0..MOTION_W {
            let sx = (gx * w) / MOTION_W;
            let idx = (sy * w + sx) * 3;
            let r = f64::from(frame.rgb[idx]);
            let g = f64::from(frame.rgb[idx + 1]);
            let b = f64::from(frame.rgb[idx + 2]);
            let y = 0.299 * r + 0.587 * g + 0.114 * b;
            sum_r += r;
            sum_g += g;
            sum_b += b;
            sum_y += y;
            sum_y2 += y * y;
            grid.push(y.round().clamp(0.0, 255.0) as u8);
        }
    }
    let mean_y = sum_y / count;
    let var_y = (sum_y2 / count - mean_y * mean_y).max(0.0);
    let round1 = |v: f64| (v * 10.0).round() / 10.0;
    RgbAnalysis {
        mean_rgb: [
            round1(sum_r / count),
            round1(sum_g / count),
            round1(sum_b / count),
        ],
        luma_mean: round1(mean_y),
        luma_stddev: round1(var_y.sqrt()),
        grid,
    }
}

struct RawVideoWriter {
    codec: CallVideoCodec,
    raw_path: PathBuf,
    file: BufWriter<File>,
    frames: u64,
    started_at: Instant,
    last_at: Instant,
}

impl RawVideoWriter {
    fn create(dir: &Path, stem: &str, codec: CallVideoCodec) -> Result<Self> {
        let ext = match codec {
            CallVideoCodec::Jpeg => "mjpeg",
            CallVideoCodec::H265 => "h265",
            CallVideoCodec::H264 => "h264",
        };
        let raw_path = dir.join(format!("{stem}.{ext}"));
        let file = BufWriter::new(File::create(&raw_path)?);
        let now = Instant::now();
        Ok(Self {
            codec,
            raw_path,
            file,
            frames: 0,
            started_at: now,
            last_at: now,
        })
    }

    fn write_frame(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes)?;
        self.frames += 1;
        self.last_at = Instant::now();
        Ok(())
    }

    fn finish(mut self) -> Result<PathBuf> {
        self.file.flush()?;
        drop(self.file);
        if self.frames == 0 {
            return Ok(self.raw_path);
        }
        // Best-effort remux/transcode to .mp4 via FFmpeg for universal playback.
        if let Some(ffmpeg) = find_ffmpeg(None) {
            let span = self
                .last_at
                .saturating_duration_since(self.started_at)
                .as_secs_f64();
            let fps = if span > 0.25 && self.frames > 1 {
                ((self.frames - 1) as f64 / span).clamp(1.0, 30.0)
            } else {
                10.0
            };
            let abs_raw =
                std::fs::canonicalize(&self.raw_path).unwrap_or_else(|_| self.raw_path.clone());
            let mp4_path = self.raw_path.with_extension("mp4");
            let abs_mp4 = abs_raw.with_extension("mp4");
            let mut input_arg = std::ffi::OsString::from("file:");
            input_arg.push(abs_raw.as_os_str());
            let mut output_arg = std::ffi::OsString::from("file:");
            output_arg.push(abs_mp4.as_os_str());
            let input_fmt = match self.codec {
                CallVideoCodec::Jpeg => "mjpeg",
                CallVideoCodec::H265 => "hevc",
                CallVideoCodec::H264 => "h264",
            };
            let mut cmd = Command::new(ffmpeg);
            cmd.args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-r",
                &format!("{fps:.2}"),
                "-f",
                input_fmt,
                "-i",
            ])
            .arg(&input_arg);
            match self.codec {
                CallVideoCodec::Jpeg => {
                    cmd.args([
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-pix_fmt",
                        "yuv420p",
                        "-movflags",
                        "+faststart",
                        "-y",
                    ]);
                }
                CallVideoCodec::H265 | CallVideoCodec::H264 => {
                    cmd.args(["-c:v", "copy", "-movflags", "+faststart", "-y"]);
                }
            }
            cmd.arg(&output_arg)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x08000000);
            }
            if let Ok(mut child) = cmd.spawn() {
                let deadline = Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            if status.success() && mp4_path.is_file() {
                                return Ok(mp4_path);
                            }
                            break;
                        }
                        Ok(None) => {
                            if Instant::now() >= deadline {
                                let _ = child.kill();
                                let _ = child.wait();
                                let _ = std::fs::remove_file(&mp4_path);
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        Err(_) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            }
        }
        Ok(self.raw_path)
    }
}

/// Summary of finalized recording files for one call.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingReport {
    pub active: bool,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_tx: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_rx_mixed: Option<String>,
    pub audio_rx_tracks: BTreeMap<u32, String>,
    pub video_rx_tracks: BTreeMap<u32, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
}

/// Records audio (TX, RX mixed, per-track RX WAV) and per-track RX video.
pub struct CallRecorder {
    dir: PathBuf,
    sample_rate: u32,
    channels: u16,
    audio_tx: Option<WavWriter>,
    audio_rx_mixed: Option<WavWriter>,
    audio_rx_tracks: BTreeMap<u32, WavWriter>,
    video_rx_tracks: BTreeMap<u32, RawVideoWriter>,
    finalized_audio_rx: BTreeMap<u32, PathBuf>,
    finalized_video_rx: BTreeMap<u32, PathBuf>,
}

impl CallRecorder {
    pub fn start(dir: PathBuf, sample_rate: u32, channels: u16) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        let audio_tx = WavWriter::create(dir.join("audio-tx.wav"), sample_rate, channels)?;
        let audio_rx_mixed =
            WavWriter::create(dir.join("audio-rx-mixed.wav"), sample_rate, channels)?;
        Ok(Self {
            dir,
            sample_rate,
            channels,
            audio_tx: Some(audio_tx),
            audio_rx_mixed: Some(audio_rx_mixed),
            audio_rx_tracks: BTreeMap::new(),
            video_rx_tracks: BTreeMap::new(),
            finalized_audio_rx: BTreeMap::new(),
            finalized_video_rx: BTreeMap::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn record_tx_audio(&mut self, samples: &[i16]) {
        if let Some(writer) = self.audio_tx.as_mut() {
            let _ = writer.write_samples(samples);
        }
    }

    pub fn record_rx_mixed_audio(&mut self, samples: &[i16]) {
        if let Some(writer) = self.audio_rx_mixed.as_mut() {
            let _ = writer.write_samples(samples);
        }
    }

    pub fn record_rx_track_audio(
        &mut self,
        track_id: u32,
        label: Option<&str>,
        samples: &[i16],
    ) -> Option<PathBuf> {
        if !self.audio_rx_tracks.contains_key(&track_id) {
            let stem = track_file_stem("audio-rx-track", track_id, label);
            if let Ok(writer) = WavWriter::create(
                self.dir.join(format!("{stem}.wav")),
                self.sample_rate,
                self.channels,
            ) {
                self.audio_rx_tracks.insert(track_id, writer);
            }
        }
        if let Some(writer) = self.audio_rx_tracks.get_mut(&track_id) {
            let _ = writer.write_samples(samples);
            return Some(writer.path().to_path_buf());
        }
        None
    }

    pub fn record_rx_track_video(
        &mut self,
        track_id: u32,
        label: Option<&str>,
        codec: CallVideoCodec,
        bytes: &[u8],
    ) -> Option<PathBuf> {
        let needs_new = self
            .video_rx_tracks
            .get(&track_id)
            .is_none_or(|existing| existing.codec != codec);
        if needs_new {
            let codec_tag = match codec {
                CallVideoCodec::Jpeg => "mjpeg",
                CallVideoCodec::H265 => "h265",
                CallVideoCodec::H264 => "h264",
            };
            let base_stem = track_file_stem("video-rx-track", track_id, label);
            let stem = if self.video_rx_tracks.contains_key(&track_id) {
                format!("{base_stem}-{codec_tag}")
            } else {
                base_stem
            };
            if let Ok(writer) = RawVideoWriter::create(&self.dir, &stem, codec) {
                self.video_rx_tracks.insert(track_id, writer);
            }
        }
        if let Some(writer) = self.video_rx_tracks.get_mut(&track_id) {
            let _ = writer.write_frame(bytes);
            return Some(writer.raw_path.clone());
        }
        None
    }

    pub fn flush_headers(&mut self) {
        if let Some(w) = self.audio_tx.as_mut() {
            let _ = w.flush_header();
        }
        if let Some(w) = self.audio_rx_mixed.as_mut() {
            let _ = w.flush_header();
        }
        for w in self.audio_rx_tracks.values_mut() {
            let _ = w.flush_header();
        }
    }

    pub fn current_report(&self) -> RecordingReport {
        RecordingReport {
            active: true,
            directory: self.dir.display().to_string(),
            audio_tx: self
                .audio_tx
                .as_ref()
                .map(|w| w.path().display().to_string()),
            audio_rx_mixed: self
                .audio_rx_mixed
                .as_ref()
                .map(|w| w.path().display().to_string()),
            audio_rx_tracks: self
                .audio_rx_tracks
                .iter()
                .map(|(&id, w)| (id, w.path().display().to_string()))
                .collect(),
            video_rx_tracks: self
                .video_rx_tracks
                .iter()
                .map(|(&id, w)| (id, w.raw_path.display().to_string()))
                .collect(),
            manifest: None,
        }
    }

    pub fn finish(mut self) -> Result<RecordingReport> {
        let audio_tx = self
            .audio_tx
            .take()
            .and_then(|w| w.finish().ok())
            .map(|p| p.display().to_string());
        let audio_rx_mixed = self
            .audio_rx_mixed
            .take()
            .and_then(|w| w.finish().ok())
            .map(|p| p.display().to_string());
        for (id, writer) in std::mem::take(&mut self.audio_rx_tracks) {
            if let Ok(path) = writer.finish() {
                self.finalized_audio_rx.insert(id, path);
            }
        }
        for (id, writer) in std::mem::take(&mut self.video_rx_tracks) {
            if let Ok(path) = writer.finish() {
                self.finalized_video_rx.insert(id, path);
            }
        }
        let manifest_path = self.dir.join("manifest.json");
        let mut report = RecordingReport {
            active: false,
            directory: self.dir.display().to_string(),
            audio_tx,
            audio_rx_mixed,
            audio_rx_tracks: self
                .finalized_audio_rx
                .iter()
                .map(|(&id, p)| (id, p.display().to_string()))
                .collect(),
            video_rx_tracks: self
                .finalized_video_rx
                .iter()
                .map(|(&id, p)| (id, p.display().to_string()))
                .collect(),
            manifest: Some(manifest_path.display().to_string()),
        };
        if let Ok(json) = serde_json::to_vec_pretty(&report) {
            if fs::write(&manifest_path, json).is_err() {
                report.manifest = None;
            }
        }
        Ok(report)
    }
}

pub fn sanitize_filename_part(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    if cleaned.is_empty() {
        "item".into()
    } else {
        cleaned
    }
}

fn track_file_stem(prefix: &str, track_id: u32, label: Option<&str>) -> String {
    match label.map(sanitize_filename_part) {
        Some(tag) => format!("{prefix}-{track_id}-{tag}"),
        None => format!("{prefix}-{track_id}"),
    }
}

/// Complete live media telemetry snapshot returned by `session status` and `media.stats`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInspectionReport {
    pub audio_send: AudioTrackReport,
    pub audio_receive_mixed: AudioTrackReport,
    pub audio_receive: Vec<AudioTrackReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_send: Option<VideoTrackReport>,
    pub video_receive: Vec<VideoTrackReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingReport>,
}

/// Aggregates send/receive audio & video analysis and optional recording for one call.
pub struct CallMediaInspector {
    sample_rate: u32,
    audio_send: AudioTrackAnalyzer,
    audio_receive_mixed: AudioTrackAnalyzer,
    audio_receive: BTreeMap<u32, AudioTrackAnalyzer>,
    video_send: Option<VideoTrackAnalyzer>,
    video_receive: BTreeMap<u32, VideoTrackAnalyzer>,
    recorder: Option<CallRecorder>,
    last_recording: Option<RecordingReport>,
}

impl CallMediaInspector {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            audio_send: AudioTrackAnalyzer::new(1, 1, None, sample_rate),
            audio_receive_mixed: AudioTrackAnalyzer::new(0, 1, None, sample_rate),
            audio_receive: BTreeMap::new(),
            video_send: None,
            video_receive: BTreeMap::new(),
            recorder: None,
            last_recording: None,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn active_recording_dir(&self) -> Option<&Path> {
        self.recorder.as_ref().map(CallRecorder::dir)
    }

    pub fn start_recording(&mut self, dir: PathBuf) -> Result<RecordingReport> {
        ensure!(
            self.recorder.is_none(),
            "a recording is already active; stop or detach it before starting a new recording"
        );
        let recorder = CallRecorder::start(dir, self.sample_rate, 1)?;
        let report = recorder.current_report();
        self.recorder = Some(recorder);
        Ok(report)
    }

    /// Install a freshly started [`CallRecorder`], returning its initial report and any
    /// previously active recorder so callers can finalize the previous one off-thread.
    pub fn replace_recorder(
        &mut self,
        recorder: CallRecorder,
    ) -> (RecordingReport, Option<CallRecorder>) {
        let report = recorder.current_report();
        let previous = self.recorder.replace(recorder);
        (report, previous)
    }

    /// Take the active [`CallRecorder`] so callers in async runtimes can run
    /// [`CallRecorder::finish`] on a blocking thread pool, then pass the result
    /// back into [`Self::record_finished_report`].
    pub fn take_recorder(&mut self) -> Option<CallRecorder> {
        self.recorder.take()
    }

    pub fn record_finished_report(&mut self, report: RecordingReport) {
        for (&id, path) in &report.audio_rx_tracks {
            if let Some(track) = self.audio_receive.get_mut(&id) {
                track.recording_path = Some(PathBuf::from(path));
            }
        }
        for (&id, path) in &report.video_rx_tracks {
            if let Some(track) = self.video_receive.get_mut(&id) {
                track.recording_path = Some(PathBuf::from(path));
            }
        }
        self.last_recording = Some(report);
    }

    pub fn stop_recording(&mut self) -> Option<RecordingReport> {
        let recorder = self.take_recorder()?;
        let report = recorder.finish().ok()?;
        self.record_finished_report(report.clone());
        Some(report)
    }

    pub fn flush_recording(&mut self) {
        if let Some(rec) = self.recorder.as_mut() {
            rec.flush_headers();
        }
    }

    pub fn sync_send_tracks(&mut self, audio_track: &CallTrack, video_track: Option<&CallTrack>) {
        self.audio_send.track_id = audio_track.id();
        self.audio_send.generation = audio_track.generation();
        if let Some(vt) = video_track {
            match self.video_send.as_mut() {
                Some(existing) if existing.track_id == vt.id() => existing.update_track(vt),
                _ => self.video_send = VideoTrackAnalyzer::from_track(vt),
            }
        }
    }

    pub fn upsert_receive_track(&mut self, track: &CallTrack) {
        if track.is_video() {
            match self.video_receive.get_mut(&track.id()) {
                Some(existing) => existing.update_track(track),
                None => {
                    if let Some(analyzer) = VideoTrackAnalyzer::from_track(track) {
                        self.video_receive.insert(track.id(), analyzer);
                    }
                }
            }
        } else {
            let entry = self.audio_receive.entry(track.id()).or_insert_with(|| {
                AudioTrackAnalyzer::new(
                    track.id(),
                    track.generation(),
                    track.participant_id().map(str::to_owned),
                    self.sample_rate,
                )
            });
            entry.generation = track.generation();
            if let Some(pid) = track.participant_id() {
                entry.participant_id = Some(pid.to_owned());
            }
        }
    }

    pub fn apply_roster(&mut self, call: &CallView) {
        for track in self.audio_receive.values_mut() {
            if let Some(pid) = &track.participant_id {
                if let Some(p) = call.participants.iter().find(|p| &p.participant_id == pid) {
                    track.contact_id = Some(p.contact_id.clone());
                }
            }
        }
        for track in self.video_receive.values_mut() {
            if let Some(pid) = &track.participant_id {
                if let Some(p) = call.participants.iter().find(|p| &p.participant_id == pid) {
                    track.contact_id = Some(p.contact_id.clone());
                    if let Some(cam) = p.camera_enabled {
                        track.active = cam;
                    }
                }
            }
        }
    }

    pub fn observe_send_audio(&mut self, samples: &[i16], now: Instant) {
        self.audio_send.observe(samples, now);
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_tx_audio(samples);
        }
    }

    pub fn observe_receive_audio(
        &mut self,
        track_id: u32,
        participant_id: Option<&str>,
        samples: &[i16],
        now: Instant,
    ) {
        let track = self.audio_receive.entry(track_id).or_insert_with(|| {
            AudioTrackAnalyzer::new(
                track_id,
                1,
                participant_id.map(str::to_owned),
                self.sample_rate,
            )
        });
        if track.participant_id.is_none() {
            track.participant_id = participant_id.map(str::to_owned);
        }
        track.observe(samples, now);
        let label = track
            .contact_id
            .as_deref()
            .or(track.participant_id.as_deref());
        if let Some(rec) = self.recorder.as_mut() {
            if let Some(path) = rec.record_rx_track_audio(track_id, label, samples) {
                track.recording_path = Some(path);
            }
        }
    }

    pub fn observe_receive_mixed_audio(&mut self, samples: &[i16], now: Instant) {
        self.audio_receive_mixed.observe(samples, now);
        if let Some(rec) = self.recorder.as_mut() {
            rec.record_rx_mixed_audio(samples);
        }
    }

    pub fn observe_send_video(&mut self, bytes: usize, flags: u16, now: Instant) {
        if let Some(vs) = self.video_send.as_mut() {
            vs.observe_sent(bytes, flags, now);
        }
    }

    pub fn mark_receive_track_removed(&mut self, track_id: u32) {
        if let Some(track) = self.video_receive.get_mut(&track_id) {
            track.active = false;
        }
    }

    pub fn video_receive_track_ids(&self) -> Vec<u32> {
        self.video_receive
            .iter()
            .filter(|(_, t)| t.active)
            .map(|(&id, _)| id)
            .collect()
    }

    pub fn observe_receive_video_wire(&mut self, wire: &WireMediaFrame<'_>, now: Instant) {
        let track_id = wire.header.track_id;
        let codec = wire.kind.to_video_codec().unwrap_or(CallVideoCodec::Jpeg);
        let track = self
            .video_receive
            .entry(track_id)
            .or_insert_with(|| VideoTrackAnalyzer {
                track_id,
                generation: wire.header.generation,
                participant_id: None,
                contact_id: None,
                active: true,
                codec,
                mode: if codec == CallVideoCodec::Jpeg {
                    CallVideoMode::Transcoded
                } else {
                    CallVideoMode::Passthrough
                },
                frames: 0,
                keyframes: 0,
                codec_configs: 0,
                bytes: 0,
                recent_times: VecDeque::with_capacity(64),
                last_frame_at: None,
                width: None,
                height: None,
                rotation_degrees: 0,
                mean_rgb: None,
                luma_mean: None,
                luma_stddev: None,
                motion_delta: None,
                prev_grid: None,
                latest_decoded: None,
                latest_frame_path: None,
                recording_path: None,
            });
        track.observe_wire(wire, now);
        let label = track
            .contact_id
            .as_deref()
            .or(track.participant_id.as_deref());
        if let Some(rec) = self.recorder.as_mut() {
            if let Some(path) =
                rec.record_rx_track_video(track_id, label, track.codec, wire.payload())
            {
                track.recording_path = Some(path);
            }
        }
    }

    pub fn observe_receive_video_decoded(
        &mut self,
        track_id: u32,
        frame: &DecodedFrame,
        now: Instant,
    ) {
        if let Some(track) = self.video_receive.get_mut(&track_id) {
            track.observe_decoded(frame, now);
        }
    }

    pub fn total_rx_audio_frames(&self, track_id: Option<u32>) -> u64 {
        match track_id {
            Some(id) => self.audio_receive.get(&id).map_or(0, |t| t.frames()),
            None => self.audio_receive.values().map(|t| t.frames()).sum(),
        }
    }

    pub fn max_rx_audio_rms_db(&self, track_id: Option<u32>) -> f64 {
        match track_id {
            Some(id) => self
                .audio_receive
                .get(&id)
                .map_or(LEVEL_FLOOR_DB, |t| t.rms_db()),
            None => self
                .audio_receive
                .values()
                .map(|t| t.rms_db())
                .fold(LEVEL_FLOOR_DB, f64::max),
        }
    }

    pub fn total_rx_video_frames(&self, track_id: Option<u32>) -> u64 {
        match track_id {
            Some(id) => self.video_receive.get(&id).map_or(0, |t| t.frames()),
            None => self.video_receive.values().map(|t| t.frames()).sum(),
        }
    }

    pub fn save_snapshots(
        &mut self,
        track_id: Option<u32>,
        out: &Path,
        out_is_file: bool,
    ) -> Result<Vec<VideoSnapshotReport>> {
        let ids: Vec<u32> = match track_id {
            Some(id) => vec![id],
            None => self
                .video_receive
                .iter()
                .filter(|(_, t)| t.latest_decoded().is_some())
                .map(|(&id, _)| id)
                .collect(),
        };
        if ids.is_empty() {
            bail!("no decoded remote video frame is available yet");
        }
        if out_is_file && ids.len() > 1 {
            bail!("multiple video tracks active; pass --track ID or an output directory");
        }
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let track = self
                .video_receive
                .get_mut(&id)
                .context("video track not found")?;
            let target = if out_is_file {
                out.to_path_buf()
            } else {
                let label = track
                    .contact_id
                    .as_deref()
                    .or(track.participant_id.as_deref());
                let stem = track_file_stem("snapshot-track", id, label);
                out.join(format!("{stem}-{}.jpg", track.frames()))
            };
            results.push(track.save_snapshot(&target)?);
        }
        Ok(results)
    }

    pub fn report(&self, now: Instant) -> MediaInspectionReport {
        MediaInspectionReport {
            audio_send: self.audio_send.report(now),
            audio_receive_mixed: self.audio_receive_mixed.report(now),
            audio_receive: self.audio_receive.values().map(|t| t.report(now)).collect(),
            video_send: self.video_send.as_ref().map(|t| t.report(now)),
            video_receive: self.video_receive.values().map(|t| t.report(now)).collect(),
            recording: self
                .recorder
                .as_ref()
                .map(|r| r.current_report())
                .or_else(|| self.last_recording.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    fn sine_samples(hz: f64, amplitude: f64, count: usize) -> Vec<i16> {
        (0..count)
            .map(|i| {
                let t = i as f64 / 16_000.0;
                ((TAU * hz * t).sin() * amplitude * f64::from(i16::MAX)).round() as i16
            })
            .collect()
    }

    #[test]
    fn fft_detects_440_and_880_hz_tones_and_ignores_silence() {
        let s440 = sine_samples(440.0, 0.25, 1600);
        let (hz, purity) = estimate_dominant_frequency(&s440, 16_000).unwrap();
        assert!((hz - 440.0).abs() < 2.0, "expected ~440 Hz, got {hz}");
        assert!(purity > 0.85, "expected high purity, got {purity}");

        let s880 = sine_samples(880.0, 0.25, 1600);
        let (hz, purity) = estimate_dominant_frequency(&s880, 16_000).unwrap();
        assert!((hz - 880.0).abs() < 2.0, "expected ~880 Hz, got {hz}");
        assert!(purity > 0.85, "expected high purity, got {purity}");

        assert!(estimate_dominant_frequency(&vec![0i16; 1600], 16_000).is_none());
    }

    #[test]
    fn audio_analyzer_and_video_analyzer_compute_expected_metrics() {
        let now = Instant::now();
        let mut audio = AudioTrackAnalyzer::new(2, 1, None, 16_000);
        let tone = sine_samples(523.25, 0.3, 320 * 6);
        for chunk in tone.chunks_exact(320) {
            audio.observe(chunk, now);
        }
        let report = audio.report(now);
        assert_eq!(report.frames, 6);
        assert_eq!(report.duration_ms, 120);
        assert!(report.rms_db > -20.0);
        assert!(report.active_ratio > 0.9);
        assert!((report.dominant_hz.unwrap() - 523.25).abs() < 3.0);

        let track = CallTrack::Video {
            id: 65_538,
            generation: 1,
            direction: wechat_call_contracts::call_types::CallTrackDirection::Receive,
            format: wechat_call_contracts::call_types::CallVideoFormat {
                codec: CallVideoCodec::Jpeg,
                mode: CallVideoMode::Transcoded,
                max_width: 640,
                max_height: 480,
                max_fps: 15,
                bitrate_kbps: None,
            },
            participant_id: None,
            active: true,
        };
        let mut video = VideoTrackAnalyzer::from_track(&track).unwrap();
        let red = DecodedFrame {
            width: 64,
            height: 48,
            rgb: [220u8, 20, 20].repeat(64 * 48),
        };
        let green = DecodedFrame {
            width: 64,
            height: 48,
            rgb: [20u8, 220, 20].repeat(64 * 48),
        };
        video.observe_decoded(&red, now);
        let r1 = video.report(now);
        assert_eq!(r1.mean_rgb, Some([220.0, 20.0, 20.0]));
        assert_eq!(r1.luma_stddev, Some(0.0));
        assert!(r1.motion_delta.is_none());

        video.observe_decoded(&green, now + Duration::from_millis(100));
        let r2 = video.report(now + Duration::from_millis(100));
        assert_eq!(r2.mean_rgb, Some([20.0, 220.0, 20.0]));
        assert!(r2.motion_delta.unwrap() > 40.0);
    }

    #[test]
    fn recorder_finishes_safely_in_leading_dash_directory() {
        let base = std::env::temp_dir().join(format!(
            "wx-inspect-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let rec_dir = base.join("-dash-rec");
        let mut recorder = CallRecorder::start(rec_dir.clone(), 16_000, 1).unwrap();
        let samples = sine_samples(440.0, 0.25, 320);
        recorder.record_tx_audio(&samples);
        recorder.record_rx_mixed_audio(&samples);
        let img = image::RgbImage::from_fn(32, 24, |_, _| image::Rgb([10, 200, 30]));
        let mut jpeg = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        recorder.record_rx_track_video(65_538, Some("peer"), CallVideoCodec::Jpeg, jpeg.get_ref());
        let report = recorder.finish().unwrap();
        assert!(!report.active);
        assert!(Path::new(report.manifest.as_deref().unwrap()).is_file());
        let video_out = report.video_rx_tracks.get(&65_538).unwrap();
        assert!(Path::new(video_out).is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn start_recording_refuses_while_active_until_detached_or_stopped() {
        let base = std::env::temp_dir().join(format!(
            "wx-inspect-active-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let dir1 = base.join("rec-1");
        let dir2 = base.join("rec-2");
        let mut inspector = CallMediaInspector::new(16_000);
        inspector.start_recording(dir1.clone()).unwrap();
        assert_eq!(inspector.active_recording_dir(), Some(dir1.as_path()));
        let err = inspector.start_recording(dir2.clone()).unwrap_err();
        assert!(err.to_string().contains("already active"), "err: {err}");

        let detached = inspector.take_recorder().expect("recorder detached");
        assert!(inspector.active_recording_dir().is_none());
        inspector.start_recording(dir2.clone()).unwrap();
        assert_eq!(inspector.active_recording_dir(), Some(dir2.as_path()));

        let rep1 = detached.finish().unwrap();
        assert!(Path::new(rep1.manifest.as_deref().unwrap()).is_file());
        let rep2 = inspector.stop_recording().unwrap();
        assert!(Path::new(rep2.manifest.as_deref().unwrap()).is_file());
        let _ = std::fs::remove_dir_all(&base);
    }
}
