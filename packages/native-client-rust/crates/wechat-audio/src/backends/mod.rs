use crate::{AecStatus, AudioConfig, AudioError, AudioSession, PlaybackHandle, Resampler};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

const NATIVE_RATE: u32 = 48_000;

pub(crate) fn devices() -> Result<Vec<crate::AudioDevice>, AudioError> {
    #[cfg(target_os = "linux")]
    {
        linux::devices()
    }
    #[cfg(target_os = "windows")]
    {
        windows::devices()
    }
    #[cfg(target_os = "macos")]
    {
        macos::devices()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err(AudioError::Unsupported)
    }
}

trait DeviceBackend {
    fn aec_status(&self) -> AecStatus;
    fn capture_rate(&self) -> u32 {
        NATIVE_RATE
    }
    fn poll(&mut self) -> Result<(), AudioError>;
}

pub(crate) fn open(config: AudioConfig) -> Result<AudioSession, AudioError> {
    let playback = PlaybackHandle::new(NATIVE_RATE, config.playback_buffer_ms)?;
    let worker_playback = playback.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let (capture_tx, capture) = mpsc::sync_channel(16);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let sample_rate = config.sample_rate;
    let worker = std::thread::Builder::new()
        .name("wechat-audio".into())
        .spawn(move || {
            let result = create(&config, worker_playback, capture_tx);
            let mut backend = match result {
                Ok(backend) => backend,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return Ok(());
                }
            };
            let _ = ready_tx.send(Ok((backend.aec_status(), backend.capture_rate())));
            while !worker_stop.load(Ordering::Acquire) {
                if let Err(error) = backend.poll() {
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            // All native resources are dropped here, on their creating thread.
            Ok(())
        })
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let startup = ready_rx.recv_timeout(Duration::from_secs(6));
    let (aec, capture_rate) = match startup {
        Ok(Ok(info)) => info,
        error => {
            stop.store(true, Ordering::Release);
            playback.close();
            let _ = worker.join();
            return Err(match error {
                Ok(Err(error)) => error,
                _ => AudioError::Timeout,
            });
        }
    };
    let capture_resampler = if sample_rate != capture_rate {
        match Resampler::new(capture_rate, sample_rate) {
            Ok(resampler) => Some(resampler),
            Err(error) => {
                stop.store(true, Ordering::Release);
                playback.close();
                let _ = worker.join();
                return Err(error);
            }
        }
    } else {
        None
    };
    Ok(AudioSession {
        playback,
        capture,
        capture_resampler,
        stop,
        worker: Some(worker),
        native_rate: NATIVE_RATE,
        aec,
    })
}

fn create(
    config: &AudioConfig,
    playback: PlaybackHandle,
    capture: mpsc::SyncSender<Vec<f32>>,
) -> Result<Box<dyn DeviceBackend>, AudioError> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::Backend::new(config, playback, capture)?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::Backend::new(config, playback, capture)?))
    }
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::Backend::new(config, playback, capture)?))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (config, playback, capture);
        Err(AudioError::Unsupported)
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn parse_macos_device_id(raw: &str) -> Result<u32, AudioError> {
    let trimmed = raw.trim();
    let parsed: u32 = trimmed
        .parse()
        .map_err(|_| AudioError::Backend(format!("CoreAudio: invalid device ID '{trimmed}'")))?;
    if parsed == 0 {
        return Err(AudioError::Backend(
            "CoreAudio: device ID must be greater than zero".into(),
        ));
    }
    Ok(parsed)
}

#[cfg(target_os = "linux")]
fn wait_ready(mut poll: impl FnMut() -> Result<bool, AudioError>) -> Result<(), AudioError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if poll()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(AudioError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(test)]
mod tests {
    use super::parse_macos_device_id;

    #[test]
    fn macos_device_id_parser_validates_positive_u32_ids() {
        assert_eq!(parse_macos_device_id("42").unwrap(), 42);
        assert_eq!(parse_macos_device_id(" 108 ").unwrap(), 108);
        assert!(parse_macos_device_id("0").is_err());
        assert!(parse_macos_device_id("").is_err());
        assert!(parse_macos_device_id("-1").is_err());
        assert!(parse_macos_device_id("MacBook Pro Microphone").is_err());
    }
}
