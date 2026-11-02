use crate::{resampler::Resampler, AudioError};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Captured once per response/stream. Clearing invalidates all older tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackToken(u64);

struct State {
    generation: u64,
    closed: bool,
    native_rate: u32,
    max_samples: usize,
    samples: VecDeque<f32>,
    resampler: Option<(u32, Resampler)>,
}

#[derive(Clone)]
pub struct PlaybackHandle(Arc<Mutex<State>>);

impl PlaybackHandle {
    #[cfg(any(feature = "native-audio", test))]
    pub(crate) fn new(native_rate: u32, buffer_ms: u32) -> Result<Self, AudioError> {
        let max_samples = native_rate as usize * buffer_ms as usize / 1000;
        if max_samples == 0 {
            return Err(AudioError::InvalidConfig);
        }
        Ok(Self(Arc::new(Mutex::new(State {
            generation: 0,
            closed: false,
            native_rate,
            max_samples,
            samples: VecDeque::with_capacity(max_samples),
            resampler: None,
        }))))
    }
    /// Begin a producer in the current generation. The token must stay with its
    /// source response, including tasks that are still computing audio.
    pub fn token(&self) -> Result<PlaybackToken, AudioError> {
        let state = self.0.lock().map_err(|_| AudioError::Closed)?;
        if state.closed {
            return Err(AudioError::Closed);
        }
        Ok(PlaybackToken(state.generation))
    }
    pub fn enqueue(
        &self,
        token: PlaybackToken,
        samples: &[f32],
        sample_rate: u32,
    ) -> Result<(), AudioError> {
        if samples.is_empty()
            || samples.len() > 192_000
            || samples.iter().any(|s| !s.is_finite())
            || !crate::resampler::valid_rate(sample_rate)
        {
            return Err(AudioError::InvalidConfig);
        }
        let mut state = self.0.lock().map_err(|_| AudioError::Closed)?;
        if state.closed {
            return Err(AudioError::Closed);
        }
        if token.0 != state.generation {
            return Err(AudioError::StalePlayback);
        }
        // Preflight before advancing streaming resampler state.
        let maximum_output = (samples.len() * state.native_rate as usize)
            .div_ceil(sample_rate as usize)
            + if sample_rate == state.native_rate {
                0
            } else {
                state.native_rate as usize / 100
            };
        if maximum_output > state.max_samples.saturating_sub(state.samples.len()) {
            return Err(AudioError::QueueFull);
        }
        let output = if sample_rate == state.native_rate {
            // A rate change ends the old resampling stream, including its tail.
            state.resampler = None;
            samples.to_vec()
        } else {
            if state
                .resampler
                .as_ref()
                .is_none_or(|(rate, _)| *rate != sample_rate)
            {
                state.resampler =
                    Some((sample_rate, Resampler::new(sample_rate, state.native_rate)?));
            }
            state
                .resampler
                .as_mut()
                .expect("resampler set")
                .1
                .process(samples)?
        };
        if output.len() > state.max_samples.saturating_sub(state.samples.len()) {
            return Err(AudioError::QueueFull);
        }
        state.samples.extend(output);
        Ok(())
    }
    /// Drop queued audio and old resampling history. This cannot retract samples
    /// already submitted to an OS/hardware buffer; the backend flushes next poll.
    pub fn clear(&self) -> Result<PlaybackToken, AudioError> {
        let mut state = self.0.lock().map_err(|_| AudioError::Closed)?;
        if state.closed {
            return Err(AudioError::Closed);
        }
        state.generation = state.generation.checked_add(1).ok_or(AudioError::Closed)?;
        state.samples.clear();
        state.resampler = None;
        Ok(PlaybackToken(state.generation))
    }
    pub(crate) fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
            state.samples.clear();
            state.resampler = None;
        }
    }
    #[cfg(all(feature = "native-audio", target_os = "macos"))]
    pub(crate) fn render_callback(&self, output: &mut [f32]) {
        if let Ok(mut state) = self.0.try_lock() {
            for sample in output {
                *sample = if state.closed {
                    0.0
                } else {
                    state.samples.pop_front().unwrap_or(0.0)
                };
            }
        } else {
            output.fill(0.0);
        }
    }
    #[cfg(all(
        feature = "native-audio",
        any(target_os = "linux", target_os = "windows")
    ))]
    pub(crate) fn drain(&self, output: &mut [f32]) -> Result<u64, AudioError> {
        let mut state = self.0.lock().map_err(|_| AudioError::Closed)?;
        for sample in output {
            *sample = if state.closed {
                0.0
            } else {
                state.samples.pop_front().unwrap_or(0.0)
            };
        }
        Ok(state.generation)
    }
    #[cfg(all(
        feature = "native-audio",
        any(target_os = "linux", target_os = "windows")
    ))]
    pub(crate) fn generation(&self) -> Result<u64, AudioError> {
        Ok(self.token()?.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_producer_cannot_refill_after_clear_or_close() {
        let queue = PlaybackHandle::new(48_000, 100).unwrap();
        let old = queue.token().unwrap();
        queue.enqueue(old, &[0.1; 480], 48_000).unwrap();
        let current = queue.clear().unwrap();
        assert!(matches!(
            queue.enqueue(old, &[0.2; 480], 48_000),
            Err(AudioError::StalePlayback)
        ));
        queue.enqueue(current, &[0.3; 480], 48_000).unwrap();
        assert!(queue.0.lock().unwrap().samples.iter().all(|s| *s == 0.3));
        queue.close();
        assert!(matches!(
            queue.enqueue(current, &[0.1; 480], 48_000),
            Err(AudioError::Closed)
        ));
    }
    #[test]
    fn queue_full_rejects_before_advancing_resampler() {
        let queue = PlaybackHandle::new(48_000, 20).unwrap();
        let token = queue.token().unwrap();
        assert!(matches!(
            queue.enqueue(token, &[0.1; 400], 16_000),
            Err(AudioError::QueueFull)
        ));
        assert!(queue.0.lock().unwrap().resampler.is_none());
        queue.enqueue(token, &[0.1; 80], 16_000).unwrap();
        assert!(queue.0.lock().unwrap().resampler.is_some());
    }
    #[test]
    fn resampling_preserves_history_across_chunks_and_clear_resets_it() {
        let queue = PlaybackHandle::new(48_000, 200).unwrap();
        let token = queue.token().unwrap();
        queue.enqueue(token, &[0.4; 80], 16_000).unwrap();
        assert!(queue.0.lock().unwrap().samples.is_empty());
        queue.enqueue(token, &[0.4; 80], 16_000).unwrap();
        assert!(!queue.0.lock().unwrap().samples.is_empty());
        queue.clear().unwrap();
        let state = queue.0.lock().unwrap();
        assert!(state.samples.is_empty());
        assert!(state.resampler.is_none());
    }
}
