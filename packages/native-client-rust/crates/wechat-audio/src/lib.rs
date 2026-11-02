//! Native audio devices, kept separate from protocol and headless call transport.
//!
//! Device access requires `native-audio` and an explicit [`AudioSession::open`].
//! Tests exercise queue/resampling behavior without opening a device. This adapter
//! derives from the MIT-licensed sys-voice library; see THIRD-PARTY.md.

#[cfg(feature = "native-audio")]
mod backends;
mod playback;
mod resampler;

pub use playback::{PlaybackHandle, PlaybackToken};
use resampler::Resampler;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;
use std::time::Duration;
use thiserror::Error;

/// AEC setup status, not a measurement of acoustic cancellation quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AecStatus {
    Unavailable,
    /// The voice-processing backend was selected; effectiveness is unmeasured.
    Requested,
    /// The sound server may apply AEC, but this adapter cannot establish it.
    Unknown,
}

/// User-selected devices; None asks the operating system for its defaults.
#[derive(Clone, Debug)]
pub struct AudioConfig {
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Mono f32 capture delivered by recv_timeout, and default playback rate.
    pub sample_rate: u32,
    /// Maximum queued playback, excluding the OS/hardware buffer.
    pub playback_buffer_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            input_device: None,
            output_device: None,
            sample_rate: 16_000,
            playback_buffer_ms: 200,
        }
    }
}

impl AudioConfig {
    fn validate(&self) -> Result<(), AudioError> {
        if !resampler::valid_rate(self.sample_rate)
            || !(20..=1_000).contains(&self.playback_buffer_ms)
            || [&self.input_device, &self.output_device]
                .into_iter()
                .flatten()
                .any(|s| s.is_empty() || s.len() > 1_024 || s.chars().any(char::is_control))
        {
            return Err(AudioError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("native audio support is not enabled for this platform")]
    Unsupported,
    #[error("invalid audio configuration")]
    InvalidConfig,
    #[error("audio backend: {0}")]
    Backend(String),
    #[error("audio playback queue is full")]
    QueueFull,
    #[error("playback belongs to an interrupted generation")]
    StalePlayback,
    #[error("audio session is closed")]
    Closed,
    #[error("audio capture timed out")]
    Timeout,
}

/// Owns the device worker. Dropping or stopping it fences all playback handles,
/// asks the backend to disconnect, and joins the worker before returning.
pub struct AudioSession {
    playback: PlaybackHandle,
    capture: mpsc::Receiver<Vec<f32>>,
    capture_resampler: Option<Resampler>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), AudioError>>>,
    native_rate: u32,
    aec: AecStatus,
}

impl AudioSession {
    pub fn open(config: AudioConfig) -> Result<Self, AudioError> {
        config.validate()?;
        #[cfg(feature = "native-audio")]
        {
            backends::open(config)
        }
        #[cfg(not(feature = "native-audio"))]
        {
            Err(AudioError::Unsupported)
        }
    }

    pub fn playback(&self) -> PlaybackHandle {
        self.playback.clone()
    }
    pub fn native_sample_rate(&self) -> u32 {
        self.native_rate
    }
    pub fn aec_status(&self) -> AecStatus {
        self.aec
    }

    /// A timeout does not establish device failure. Capture overflows drop a new
    /// chunk instead of blocking an OS callback; applications must keep draining.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Result<Vec<f32>, AudioError> {
        let samples = self
            .capture
            .recv_timeout(timeout)
            .map_err(|err| match err {
                mpsc::RecvTimeoutError::Timeout => AudioError::Timeout,
                mpsc::RecvTimeoutError::Disconnected => AudioError::Closed,
            })?;
        if let Some(resampler) = &mut self.capture_resampler {
            resampler.process(&samples)
        } else {
            Ok(samples)
        }
    }

    pub fn stop(&mut self) -> Result<(), AudioError> {
        self.playback.close();
        self.stop.store(true, Ordering::Release);
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| AudioError::Backend("device worker panicked".into()))?,
            None => Ok(()),
        }
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_joins_the_owner_and_fences_all_cloned_producers() {
        let playback = PlaybackHandle::new(48_000, 100).unwrap();
        let producer = playback.clone();
        let token = producer.token().unwrap();
        let (tx, capture) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_observed = observed.clone();
        let worker = std::thread::spawn(move || {
            let _capture = tx;
            while !worker_stop.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            worker_observed.store(true, Ordering::Release);
            Ok(())
        });
        let mut session = AudioSession {
            playback,
            capture,
            capture_resampler: None,
            stop,
            worker: Some(worker),
            native_rate: 48_000,
            aec: AecStatus::Unknown,
        };
        session.stop().unwrap();
        assert!(observed.load(Ordering::Acquire));
        assert!(matches!(
            producer.enqueue(token, &[0.1; 480], 48_000),
            Err(AudioError::Closed)
        ));
        session.stop().unwrap();
    }
    #[test]
    fn rejects_invalid_config_without_opening_any_device() {
        let config = AudioConfig {
            sample_rate: 0,
            ..AudioConfig::default()
        };
        assert!(matches!(
            AudioSession::open(config),
            Err(AudioError::InvalidConfig)
        ));
    }
}
