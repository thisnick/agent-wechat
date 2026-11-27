//! Headless synthetic, file, and optional OS-device audio sources and sinks.
//!
//! No OS audio device is ever opened unless `AudioSourceSpec::Device` or
//! `AudioSinkSpec::Device` is explicitly requested.

use crate::{
    audio::{AudioDeviceOptions, LocalAudio},
    video::find_ffmpeg,
};
use anyhow::{bail, ensure, Context, Result};
use std::{
    f64::consts::TAU,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use wechat_audio::AudioError;
use wechat_calls::{audio::AudioFormat, tone::PulseTone};

/// Configured outgoing audio source for a headless call session.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum AudioSourceSpec {
    /// Paced zero PCM frames; requires no hardware or FFmpeg.
    #[default]
    Silence,
    /// Continuous or pulsed sine wave (defaults: 440 Hz, 0.25 amplitude, continuous).
    Tone {
        hz: f64,
        amplitude: f64,
        on_ms: u32,
        off_ms: u32,
    },
    /// Audio file decoded to 16 kHz mono PCM16 (native WAV fast path, FFmpeg fallback).
    File { path: PathBuf, loop_forever: bool },
    /// Local OS microphone via `wechat-audio` (`None` = OS default).
    Device(Option<String>),
}

impl AudioSourceSpec {
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        ensure!(
            !trimmed.is_empty() && !trimmed.chars().any(char::is_control),
            "invalid audio source"
        );
        if trimmed.eq_ignore_ascii_case("silence")
            || trimmed.eq_ignore_ascii_case("null")
            || trimmed.eq_ignore_ascii_case("off")
        {
            return Ok(Self::Silence);
        }
        if trimmed.eq_ignore_ascii_case("device") {
            return Ok(Self::Device(None));
        }
        if let Some(id) = trimmed.strip_prefix("device:") {
            let id = id.trim();
            ensure!(!id.is_empty(), "audio device identifier is required");
            return Ok(Self::Device(Some(id.to_owned())));
        }
        if trimmed.eq_ignore_ascii_case("tone") {
            return Ok(Self::Tone {
                hz: 440.0,
                amplitude: 0.25,
                on_ms: 0,
                off_ms: 0,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("tone:") {
            let parts: Vec<&str> = rest.split(':').collect();
            ensure!(
                (1..=4).contains(&parts.len()) && parts.len() != 3,
                "tone format: tone[:HZ[:AMPLITUDE[:ON_MS:OFF_MS]]]"
            );
            let hz: f64 = parts[0].parse().context("invalid tone frequency Hz")?;
            ensure!(
                hz.is_finite() && (20.0..=7_900.0).contains(&hz),
                "tone frequency must be between 20 and 7900 Hz"
            );
            let amplitude: f64 = match parts.get(1) {
                Some(v) => v.parse().context("invalid tone amplitude")?,
                None => 0.25,
            };
            ensure!(
                amplitude.is_finite() && (0.0..=1.0).contains(&amplitude),
                "tone amplitude must be between 0.0 and 1.0"
            );
            let (on_ms, off_ms) = if parts.len() == 4 {
                let on: u32 = parts[2].parse().context("invalid tone on_ms")?;
                let off: u32 = parts[3].parse().context("invalid tone off_ms")?;
                ensure!(
                    on > 0 && off > 0 && on <= 60_000 && off <= 60_000,
                    "tone pulse on/off durations must be 1..=60000 ms"
                );
                (on, off)
            } else {
                (0, 0)
            };
            return Ok(Self::Tone {
                hz,
                amplitude,
                on_ms,
                off_ms,
            });
        }
        if let Some(rest) = trimmed.strip_prefix("file:") {
            let (path_str, loop_forever) = if let Some(p) = rest.strip_suffix(":once") {
                (p, false)
            } else if let Some(p) = rest.strip_suffix(":loop") {
                (p, true)
            } else {
                (rest, true)
            };
            ensure!(!path_str.trim().is_empty(), "audio file path is required");
            return Ok(Self::File {
                path: PathBuf::from(path_str),
                loop_forever,
            });
        }
        bail!(
            "unsupported audio source {trimmed:?}; expected silence, tone[:HZ[:AMP[:ON:OFF]]], file:PATH[:loop|:once], or device[:ID]"
        )
    }

    pub fn label(&self) -> String {
        match self {
            Self::Silence => "silence".into(),
            Self::Tone {
                hz,
                amplitude,
                on_ms,
                off_ms,
            } => {
                if *on_ms > 0 && *off_ms > 0 {
                    format!("tone:{hz}:{amplitude}:{on_ms}:{off_ms}")
                } else {
                    format!("tone:{hz}:{amplitude}")
                }
            }
            Self::File { path, loop_forever } => format!(
                "file:{}:{}",
                path.display(),
                if *loop_forever { "loop" } else { "once" }
            ),
            Self::Device(None) => "device".into(),
            Self::Device(Some(id)) => format!("device:{id}"),
        }
    }

    pub fn uses_device(&self) -> bool {
        matches!(self, Self::Device(_))
    }
}

/// Configured incoming audio sink for a headless call session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AudioSinkSpec {
    /// Discard after metering/analysis/recording; requires no OS sound server.
    #[default]
    Null,
    /// Play mixed audio to a local OS speaker via `wechat-audio`.
    Device(Option<String>),
}

impl AudioSinkSpec {
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        ensure!(
            !trimmed.is_empty() && !trimmed.chars().any(char::is_control),
            "invalid audio output sink"
        );
        if trimmed.eq_ignore_ascii_case("null") || trimmed.eq_ignore_ascii_case("off") {
            return Ok(Self::Null);
        }
        if trimmed.eq_ignore_ascii_case("device") {
            return Ok(Self::Device(None));
        }
        if let Some(id) = trimmed.strip_prefix("device:") {
            let id = id.trim();
            ensure!(!id.is_empty(), "audio output device identifier is required");
            return Ok(Self::Device(Some(id.to_owned())));
        }
        bail!("unsupported audio sink {trimmed:?}; expected null or device[:ID]")
    }

    pub fn label(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Device(None) => "device".into(),
            Self::Device(Some(id)) => format!("device:{id}"),
        }
    }

    pub fn uses_device(&self) -> bool {
        matches!(self, Self::Device(_))
    }
}

struct ContinuousTone {
    format: AudioFormat,
    frequency: f64,
    amplitude: f64,
    phase: f64,
    ramp_done: u64,
}

impl ContinuousTone {
    fn new(format: AudioFormat, frequency: f64, amplitude: f64) -> Self {
        Self {
            format,
            frequency,
            amplitude,
            phase: 0.0,
            ramp_done: 0,
        }
    }

    fn next_frame(&mut self) -> Vec<i16> {
        let mut out = Vec::with_capacity(self.format.samples_per_frame());
        let step = TAU * self.frequency / f64::from(self.format.sample_rate());
        let ramp_len = u64::from(self.format.sample_rate() / 200).max(1);
        for _ in 0..self.format.samples_per_channel() {
            let env = if self.ramp_done < ramp_len {
                self.ramp_done += 1;
                self.ramp_done as f64 / ramp_len as f64
            } else {
                1.0
            };
            let sample =
                (self.phase.sin() * self.amplitude * env * f64::from(i16::MAX)).round() as i16;
            out.extend(std::iter::repeat_n(sample, self.format.channels() as usize));
            self.phase = (self.phase + step) % TAU;
        }
        out
    }
}

enum FilePcmBackend {
    Memory {
        samples: Vec<i16>,
        cursor: usize,
        loop_forever: bool,
        finished_emitted: bool,
    },
    Ffmpeg {
        child: Child,
        stdout: std::process::ChildStdout,
        eof: bool,
        finished_emitted: bool,
    },
}

impl FilePcmBackend {
    fn open(path: &Path, format: AudioFormat, loop_forever: bool) -> Result<Self> {
        ensure!(path.is_file(), "audio file not found: {}", path.display());
        if let Some(samples) = try_read_matching_wav(path, format)? {
            return Ok(Self::Memory {
                samples,
                cursor: 0,
                loop_forever,
                finished_emitted: false,
            });
        }
        let ffmpeg = find_ffmpeg(None).context(
            "ffmpeg is required to decode non-16kHz-mono-WAV audio files; install ffmpeg or pass a 16kHz mono PCM16 WAV",
        )?;
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("failed to resolve audio file: {}", path.display()))?;
        let mut input = std::ffi::OsString::from("file:");
        input.push(canonical.as_os_str());
        let mut cmd = Command::new(ffmpeg);
        cmd.args(["-hide_banner", "-loglevel", "error"]);
        if loop_forever {
            cmd.args(["-stream_loop", "-1"]);
        }
        cmd.arg("-i").arg(input).args([
            "-vn",
            "-sn",
            "-dn",
            "-ac",
            &format.channels().to_string(),
            "-ar",
            &format.sample_rate().to_string(),
            "-f",
            "s16le",
            "pipe:1",
        ]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to start ffmpeg audio file reader")?;
        let stdout = child.stdout.take().context("missing ffmpeg audio stdout")?;
        Ok(Self::Ffmpeg {
            child,
            stdout,
            eof: false,
            finished_emitted: false,
        })
    }

    fn next_frame(&mut self, count: usize) -> (Vec<i16>, bool) {
        match self {
            Self::Memory {
                samples,
                cursor,
                loop_forever,
                finished_emitted,
            } => {
                let mut frame = vec![0i16; count];
                if samples.is_empty() {
                    let newly_finished = !*loop_forever && !*finished_emitted;
                    if newly_finished {
                        *finished_emitted = true;
                    }
                    return (frame, newly_finished);
                }
                let mut written = 0;
                while written < count {
                    if *cursor >= samples.len() {
                        if *loop_forever {
                            *cursor = 0;
                        } else {
                            let newly_finished = !*finished_emitted;
                            *finished_emitted = true;
                            return (frame, newly_finished);
                        }
                    }
                    let take = (count - written).min(samples.len() - *cursor);
                    frame[written..written + take]
                        .copy_from_slice(&samples[*cursor..*cursor + take]);
                    *cursor += take;
                    written += take;
                }
                (frame, false)
            }
            Self::Ffmpeg {
                stdout,
                eof,
                finished_emitted,
                ..
            } => {
                let mut raw = vec![0u8; count * 2];
                if *eof {
                    return (vec![0i16; count], false);
                }
                let mut filled = 0;
                while filled < raw.len() {
                    match stdout.read(&mut raw[filled..]) {
                        Ok(0) => {
                            *eof = true;
                            break;
                        }
                        Ok(n) => filled += n,
                        Err(_) => {
                            *eof = true;
                            break;
                        }
                    }
                }
                let samples = raw
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let newly_finished = *eof && !*finished_emitted;
                if newly_finished {
                    *finished_emitted = true;
                }
                (samples, newly_finished)
            }
        }
    }
}

impl Drop for FilePcmBackend {
    fn drop(&mut self) {
        if let Self::Ffmpeg { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read an uncompressed PCM16 RIFF/WAVE file natively when its sample rate and
/// channel count match `format`. Returns `Ok(None)` for non-WAV or mismatched
/// formats so FFmpeg handles resampling/transcoding.
fn try_read_matching_wav(path: &Path, format: AudioFormat) -> Result<Option<Vec<i16>>> {
    let meta = std::fs::metadata(path)?;
    // Cap pure-in-memory WAV load at 64 MiB (~33 minutes of 16 kHz mono PCM16).
    if meta.len() < 44 || meta.len() > 64 * 1024 * 1024 {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut pos = 12usize;
    let mut fmt_ok = false;
    let mut data_slice: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        pos += 8;
        let end = pos.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end - pos >= 16 {
            let audio_format = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]);
            let channels = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]);
            let sample_rate = u32::from_le_bytes([
                bytes[pos + 4],
                bytes[pos + 5],
                bytes[pos + 6],
                bytes[pos + 7],
            ]);
            let bits = u16::from_le_bytes([bytes[pos + 14], bytes[pos + 15]]);
            if audio_format == 1
                && channels == u16::from(format.channels())
                && sample_rate == format.sample_rate()
                && bits == 16
            {
                fmt_ok = true;
            } else {
                return Ok(None);
            }
        } else if id == b"data" {
            data_slice = Some(&bytes[pos..end]);
            break;
        }
        pos = end + (size % 2);
    }
    if !fmt_ok {
        return Ok(None);
    }
    let Some(data) = data_slice else {
        return Ok(None);
    };
    let samples = data
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(Some(samples))
}

enum ActiveProducer {
    Silence,
    Continuous(ContinuousTone),
    Pulsed(PulseTone),
    File(FilePcmBackend),
    Device,
}

/// Manages outgoing and incoming audio I/O for a headless or hybrid call session.
pub struct CallAudioIo {
    input_spec: AudioSourceSpec,
    output_spec: AudioSinkSpec,
    producer: ActiveProducer,
    local: Option<LocalAudio>,
}

impl CallAudioIo {
    pub async fn open(
        input: AudioSourceSpec,
        output: AudioSinkSpec,
        format: AudioFormat,
    ) -> Result<Self> {
        let local = if input.uses_device() || output.uses_device() {
            let opts = AudioDeviceOptions {
                input_device: match &input {
                    AudioSourceSpec::Device(id) => id.clone(),
                    _ => None,
                },
                output_device: match &output {
                    AudioSinkSpec::Device(id) => id.clone(),
                    _ => None,
                },
            };
            Some(LocalAudio::open(&opts).await?)
        } else {
            None
        };
        let producer = build_producer(&input, format)?;
        Ok(Self {
            input_spec: input,
            output_spec: output,
            producer,
            local,
        })
    }

    pub fn input_spec(&self) -> &AudioSourceSpec {
        &self.input_spec
    }

    pub fn output_spec(&self) -> &AudioSinkSpec {
        &self.output_spec
    }

    /// Hot-swap the outgoing audio source mid-call without dropping the call.
    pub async fn set_input(&mut self, input: AudioSourceSpec, format: AudioFormat) -> Result<()> {
        if input.uses_device() && self.local.is_none() {
            let opts = AudioDeviceOptions {
                input_device: match &input {
                    AudioSourceSpec::Device(id) => id.clone(),
                    _ => None,
                },
                output_device: match &self.output_spec {
                    AudioSinkSpec::Device(id) => id.clone(),
                    _ => None,
                },
            };
            self.local = Some(LocalAudio::open(&opts).await?);
        }
        let producer = build_producer(&input, format)?;
        self.producer = producer;
        self.input_spec = input;
        if !self.input_spec.uses_device() && !self.output_spec.uses_device() {
            if let Some(mut old) = self.local.take() {
                old.request_stop();
                let _ = old.stop().await;
            }
        }
        Ok(())
    }

    pub fn discard_capture(&mut self) {
        if let Some(local) = self.local.as_mut() {
            local.discard_capture();
        }
    }

    pub fn clear_playback(&mut self) -> Result<()> {
        if let Some(local) = self.local.as_mut() {
            local.playback_token = local.playback.clear()?;
        }
        Ok(())
    }

    /// Produce one outgoing PCM16 frame. Returns `(samples, finished_once)`.
    pub fn next_frame(&mut self, format: AudioFormat, muted: bool) -> Result<(Vec<i16>, bool)> {
        let count = format.samples_per_frame();
        let (mut samples, finished) = match &mut self.producer {
            ActiveProducer::Silence => (vec![0i16; count], false),
            ActiveProducer::Continuous(tone) => (tone.next_frame(), false),
            ActiveProducer::Pulsed(tone) => (tone.next_frame()?, false),
            ActiveProducer::File(file) => file.next_frame(count),
            ActiveProducer::Device => {
                let local = self
                    .local
                    .as_mut()
                    .context("local audio device not initialized")?;
                (local.frame(count, muted)?, false)
            }
        };
        if muted {
            samples.fill(0);
        }
        Ok((samples, finished))
    }

    pub fn enqueue_playback(&mut self, samples: &[i16], sample_rate: u32) -> Result<()> {
        if !self.output_spec.uses_device() {
            return Ok(());
        }
        let Some(local) = self.local.as_mut() else {
            return Ok(());
        };
        let converted: Vec<f32> = samples.iter().map(|s| f32::from(*s) / 32768.0).collect();
        match local
            .playback
            .enqueue(local.playback_token, &converted, sample_rate)
        {
            Ok(()) => Ok(()),
            Err(AudioError::QueueFull) => {
                local.playback_token = local.playback.clear()?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.producer = ActiveProducer::Silence;
        if let Some(mut local) = self.local.take() {
            local.request_stop();
            local.stop().await?;
        }
        Ok(())
    }
}

fn build_producer(spec: &AudioSourceSpec, format: AudioFormat) -> Result<ActiveProducer> {
    match spec {
        AudioSourceSpec::Silence => Ok(ActiveProducer::Silence),
        AudioSourceSpec::Tone {
            hz,
            amplitude,
            on_ms,
            off_ms,
        } => {
            if *on_ms > 0 && *off_ms > 0 {
                let tone = PulseTone::new(
                    format,
                    *hz,
                    *amplitude,
                    Duration::from_millis(u64::from(*on_ms)),
                    Duration::from_millis(u64::from(*off_ms)),
                )?;
                Ok(ActiveProducer::Pulsed(tone))
            } else {
                Ok(ActiveProducer::Continuous(ContinuousTone::new(
                    format, *hz, *amplitude,
                )))
            }
        }
        AudioSourceSpec::File { path, loop_forever } => Ok(ActiveProducer::File(
            FilePcmBackend::open(path, format, *loop_forever)?,
        )),
        AudioSourceSpec::Device(_) => Ok(ActiveProducer::Device),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspect::WavWriter;

    #[test]
    fn parses_audio_sources_and_sinks() {
        assert_eq!(
            AudioSourceSpec::parse("silence").unwrap(),
            AudioSourceSpec::Silence
        );
        assert_eq!(
            AudioSourceSpec::parse("tone").unwrap(),
            AudioSourceSpec::Tone {
                hz: 440.0,
                amplitude: 0.25,
                on_ms: 0,
                off_ms: 0,
            }
        );
        assert_eq!(
            AudioSourceSpec::parse("tone:880:0.4:300:100").unwrap(),
            AudioSourceSpec::Tone {
                hz: 880.0,
                amplitude: 0.4,
                on_ms: 300,
                off_ms: 100,
            }
        );
        assert_eq!(
            AudioSourceSpec::parse("file:/tmp/demo.wav:once").unwrap(),
            AudioSourceSpec::File {
                path: PathBuf::from("/tmp/demo.wav"),
                loop_forever: false,
            }
        );
        assert_eq!(
            AudioSourceSpec::parse("device:mic-1").unwrap(),
            AudioSourceSpec::Device(Some("mic-1".into()))
        );
        assert!(AudioSourceSpec::parse("tone:99999").is_err());
        assert!(AudioSourceSpec::parse("tone:440:0.5:100").is_err());
        assert!(AudioSourceSpec::parse("file:").is_err());
        assert_eq!(AudioSinkSpec::parse("null").unwrap(), AudioSinkSpec::Null);
        assert_eq!(
            AudioSinkSpec::parse("device:spk-1").unwrap(),
            AudioSinkSpec::Device(Some("spk-1".into()))
        );
    }

    #[tokio::test]
    async fn headless_audio_generates_tones_respects_mute_and_reads_wav_once() {
        let format = AudioFormat::new(16_000, 1, 20).unwrap();
        let mut io = CallAudioIo::open(
            AudioSourceSpec::parse("tone:660:0.3").unwrap(),
            AudioSinkSpec::Null,
            format,
        )
        .await
        .unwrap();
        let (frame, finished) = io.next_frame(format, false).unwrap();
        assert!(!finished);
        assert_eq!(frame.len(), 320);
        assert!(frame.iter().any(|&s| s.abs() > 2000));

        let (muted_frame, _) = io.next_frame(format, true).unwrap();
        assert!(muted_frame.iter().all(|&s| s == 0));

        // Write a 2-frame WAV and verify `:once` plays exact samples then reports finished.
        let dir = std::env::temp_dir().join(format!("wx-audio-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = dir.join("clip.wav");
        let mut writer = WavWriter::create(&wav_path, 16_000, 1).unwrap();
        writer.write_samples(&[1234i16; 320]).unwrap();
        writer.write_samples(&[-2345i16; 320]).unwrap();
        writer.finish().unwrap();

        io.set_input(
            AudioSourceSpec::File {
                path: wav_path.clone(),
                loop_forever: false,
            },
            format,
        )
        .await
        .unwrap();
        assert_eq!(
            io.next_frame(format, false).unwrap(),
            (vec![1234; 320], false)
        );
        assert_eq!(
            io.next_frame(format, false).unwrap(),
            (vec![-2345; 320], false)
        );
        assert_eq!(io.next_frame(format, false).unwrap(), (vec![0; 320], true));
        assert_eq!(io.next_frame(format, false).unwrap(), (vec![0; 320], false));

        io.stop().await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
