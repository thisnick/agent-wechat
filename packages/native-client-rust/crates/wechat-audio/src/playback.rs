use crate::{resampler::Resampler, AudioError};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Captured once per response/stream. Clearing invalidates all older tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackToken(u64);

struct Shared {
    // Producers may lock queue while holding producer, never the reverse.
    producer: Mutex<ProducerState>,
    queue: Mutex<QueueState>,
}

struct ProducerState {
    generation: u64,
    resampler: Option<(u32, Resampler)>,
}

struct QueueState {
    generation: u64,
    closed: bool,
    native_rate: u32,
    max_samples: usize,
    samples: VecDeque<f32>,
}

impl QueueState {
    fn check_token(&self, token: PlaybackToken) -> Result<(), AudioError> {
        if self.closed {
            return Err(AudioError::Closed);
        }
        if token.0 != self.generation {
            return Err(AudioError::StalePlayback);
        }
        Ok(())
    }

    fn check_capacity(&self, count: usize) -> Result<(), AudioError> {
        if count > self.max_samples.saturating_sub(self.samples.len()) {
            return Err(AudioError::QueueFull);
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PlaybackHandle(Arc<Shared>);

impl PlaybackHandle {
    #[cfg(any(feature = "native-audio", test))]
    pub(crate) fn new(native_rate: u32, buffer_ms: u32) -> Result<Self, AudioError> {
        let max_samples = native_rate as usize * buffer_ms as usize / 1000;
        if max_samples == 0 {
            return Err(AudioError::InvalidConfig);
        }
        Ok(Self(Arc::new(Shared {
            producer: Mutex::new(ProducerState {
                generation: 0,
                resampler: None,
            }),
            queue: Mutex::new(QueueState {
                generation: 0,
                closed: false,
                native_rate,
                max_samples,
                samples: VecDeque::with_capacity(max_samples),
            }),
        })))
    }
    /// Begin a producer in the current generation. The token must stay with its
    /// source response, including tasks that are still computing audio.
    pub fn token(&self) -> Result<PlaybackToken, AudioError> {
        let state = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
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
        let mut producer = self.0.producer.lock().map_err(|_| AudioError::Closed)?;
        let (native_rate, generation) = {
            let queue = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
            queue.check_token(token)?;
            // Preflight before advancing streaming resampler state.
            let maximum_output = (samples.len() * queue.native_rate as usize)
                .div_ceil(sample_rate as usize)
                + if sample_rate == queue.native_rate {
                    0
                } else {
                    queue.native_rate as usize / 100
                };
            queue.check_capacity(maximum_output)?;
            (queue.native_rate, queue.generation)
        };
        if producer.generation != generation {
            producer.resampler = None;
            producer.generation = generation;
        }
        let output = if sample_rate == native_rate {
            // A rate change ends the old resampling stream, including its tail.
            producer.resampler = None;
            samples.to_vec()
        } else {
            if producer
                .resampler
                .as_ref()
                .is_none_or(|(rate, _)| *rate != sample_rate)
            {
                producer.resampler = Some((sample_rate, Resampler::new(sample_rate, native_rate)?));
            }
            producer
                .resampler
                .as_mut()
                .expect("resampler set")
                .1
                .process(samples)?
        };
        let mut queue = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
        queue.check_token(token)?;
        queue.check_capacity(output.len())?;
        queue.samples.extend(output);
        Ok(())
    }
    /// Drop queued audio and old resampling history. This cannot retract samples
    /// already submitted to an OS/hardware buffer; the backend flushes next poll.
    pub fn clear(&self) -> Result<PlaybackToken, AudioError> {
        let mut queue = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
        if queue.closed {
            return Err(AudioError::Closed);
        }
        queue.generation = queue.generation.checked_add(1).ok_or(AudioError::Closed)?;
        queue.samples.clear();
        let generation = queue.generation;
        drop(queue);

        // A producer may already have started resampling in the new generation.
        let mut producer = self.0.producer.lock().map_err(|_| AudioError::Closed)?;
        if producer.generation < generation {
            producer.resampler = None;
            producer.generation = generation;
        }
        Ok(PlaybackToken(generation))
    }
    pub(crate) fn close(&self) {
        if let Ok(mut queue) = self.0.queue.lock() {
            queue.closed = true;
            queue.samples.clear();
        }
        // Close admission before waiting for any producer to finish resampling.
        if let Ok(mut producer) = self.0.producer.lock() {
            producer.resampler = None;
        }
    }
    #[cfg(all(feature = "native-audio", target_os = "macos"))]
    pub(crate) fn render_callback(&self, output: &mut [f32]) {
        if let Ok(mut state) = self.0.queue.try_lock() {
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
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    pub(crate) fn drain(&self, output: &mut [f32]) -> Result<u64, AudioError> {
        let mut state = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
        for sample in output {
            *sample = if state.closed {
                0.0
            } else {
                state.samples.pop_front().unwrap_or(0.0)
            };
        }
        Ok(state.generation)
    }
    /// Pop up to `output.len()` currently queued samples without padding missing
    /// tail slots with synthetic silence.
    #[cfg(any(all(feature = "native-audio", target_os = "linux"), test))]
    pub(crate) fn drain_available(&self, output: &mut [f32]) -> Result<(usize, u64), AudioError> {
        let mut state = self.0.queue.lock().map_err(|_| AudioError::Closed)?;
        if state.closed {
            return Ok((0, state.generation));
        }
        let count = output.len().min(state.samples.len());
        for sample in &mut output[..count] {
            *sample = state.samples.pop_front().expect("bounded by queue length");
        }
        Ok((count, state.generation))
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
    use std::thread;
    use std::time::{Duration, Instant};

    fn wait_for_queue_fence(
        playback: &PlaybackHandle,
        fenced: impl Fn(&QueueState) -> bool,
    ) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        // Do not block on the queue: the worker might hold it while waiting for
        // the producer lock if the lock order regresses.
        while Instant::now() < deadline {
            if playback
                .0
                .queue
                .try_lock()
                .is_ok_and(|queue| fenced(&queue))
            {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        false
    }

    #[test]
    fn close_fences_queue_while_producer_is_held() {
        let playback = PlaybackHandle::new(48_000, 100).unwrap();
        let token = playback.token().unwrap();
        playback.enqueue(token, &[0.1; 160], 16_000).unwrap();
        let producer = playback.0.producer.lock().unwrap();
        assert!(producer.resampler.is_some());
        assert!(!playback.0.queue.lock().unwrap().samples.is_empty());

        let worker = playback.clone();
        let closer = thread::spawn(move || worker.close());
        let drained = wait_for_queue_fence(&playback, |queue| queue.closed)
            .then(|| playback.drain_available(&mut [0.0; 480]).unwrap());
        drop(producer);
        closer.join().unwrap();

        assert_eq!(drained, Some((0, token.0)));
        assert!(playback.0.producer.lock().unwrap().resampler.is_none());
        assert!(matches!(
            playback.enqueue(token, &[0.1; 160], 16_000),
            Err(AudioError::Closed)
        ));
    }

    #[test]
    fn clear_fences_queue_immediately_and_resets_resampler_once() {
        for new_producer_started in [false, true] {
            let playback = PlaybackHandle::new(48_000, 100).unwrap();
            let old = playback.token().unwrap();
            playback.enqueue(old, &[0.1; 160], 16_000).unwrap();
            let mut producer = playback.0.producer.lock().unwrap();
            assert!(producer.resampler.is_some());
            assert!(!playback.0.queue.lock().unwrap().samples.is_empty());

            let worker = playback.clone();
            let clearer = thread::spawn(move || worker.clear());
            let generation = old.0 + 1;
            let fenced = wait_for_queue_fence(&playback, |queue| {
                queue.generation == generation && queue.samples.is_empty()
            });
            if fenced && new_producer_started {
                // Model a new-generation enqueue winning the producer lock.
                let mut resampler = Resampler::new(16_000, 48_000).unwrap();
                assert!(resampler.process(&[0.2; 80]).unwrap().is_empty());
                producer.resampler = Some((16_000, resampler));
                producer.generation = generation;
            }
            drop(producer);
            let current = clearer.join().unwrap().unwrap();

            assert!(
                fenced,
                "clear must fence the queue before waiting for the producer"
            );
            assert_eq!(current, PlaybackToken(generation));
            assert_eq!(
                playback.0.producer.lock().unwrap().resampler.is_some(),
                new_producer_started
            );
            assert!(matches!(
                playback.enqueue(old, &[0.1; 80], 16_000),
                Err(AudioError::StalePlayback)
            ));
            playback.enqueue(current, &[0.3; 80], 16_000).unwrap();
            let (count, _) = playback.drain_available(&mut [0.0; 480]).unwrap();
            assert_eq!(count, if new_producer_started { 480 } else { 0 });
        }
    }

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
        assert!(queue
            .0
            .queue
            .lock()
            .unwrap()
            .samples
            .iter()
            .all(|s| *s == 0.3));
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
        assert!(queue.0.producer.lock().unwrap().resampler.is_none());
        queue.enqueue(token, &[0.1; 80], 16_000).unwrap();
        assert!(queue.0.producer.lock().unwrap().resampler.is_some());
    }
    #[test]
    fn resampling_preserves_history_across_chunks_and_clear_resets_it() {
        let queue = PlaybackHandle::new(48_000, 200).unwrap();
        let token = queue.token().unwrap();
        queue.enqueue(token, &[0.4; 80], 16_000).unwrap();
        assert!(queue.0.queue.lock().unwrap().samples.is_empty());
        queue.enqueue(token, &[0.4; 80], 16_000).unwrap();
        assert!(!queue.0.queue.lock().unwrap().samples.is_empty());
        queue.clear().unwrap();
        assert!(queue.0.queue.lock().unwrap().samples.is_empty());
        assert!(queue.0.producer.lock().unwrap().resampler.is_none());
    }
    #[test]
    fn resampling_preflight_uses_exact_remaining_space_after_clear() {
        let playback = PlaybackHandle::new(48_000, 20).unwrap();
        let token = playback.token().unwrap();
        // 160 inputs at 16 kHz need a 480-sample output plus 480 samples of
        // headroom. One queued native sample makes that preflight fail.
        playback.enqueue(token, &[0.2], 48_000).unwrap();
        assert!(matches!(
            playback.enqueue(token, &[0.4; 160], 16_000),
            Err(AudioError::QueueFull)
        ));
        assert!(playback.0.producer.lock().unwrap().resampler.is_none());

        let current = playback.clear().unwrap();
        assert!(matches!(
            playback.enqueue(token, &[0.4; 160], 16_000),
            Err(AudioError::StalePlayback)
        ));
        playback.enqueue(current, &[0.4; 160], 16_000).unwrap();
        // The drain path must remain usable while the producer lock is held.
        let producer = playback.0.producer.lock().unwrap();
        let producer_generation = producer.generation;
        let (done, drained) = std::sync::mpsc::channel();
        let consumer = playback.clone();
        let reader = std::thread::spawn(move || {
            let _ = done.send(consumer.drain_available(&mut [0.; 960]));
        });
        let result = drained.recv_timeout(std::time::Duration::from_secs(2));
        drop(producer);
        reader.join().unwrap();
        let (count, generation) = result.expect("drain must not wait for producer").unwrap();
        assert!(count > 0);
        assert_eq!(generation, current.0);
        assert_eq!(producer_generation, generation);
    }
    #[test]
    fn drain_available_returns_only_queued_samples_without_zero_padding() {
        let queue = PlaybackHandle::new(48_000, 100).unwrap();
        let token = queue.token().unwrap();
        queue.enqueue(token, &[0.25; 120], 48_000).unwrap();
        let mut out = [-1.0f32; 480];
        let (count, generation) = queue.drain_available(&mut out).unwrap();
        assert_eq!(count, 120);
        assert_eq!(generation, 0);
        assert!(out[..120].iter().all(|s| *s == 0.25));
        assert!(out[120..].iter().all(|s| *s == -1.0));
        // Second call with empty queue returns 0 instead of synthesizing silence.
        let (count2, _) = queue.drain_available(&mut out).unwrap();
        assert_eq!(count2, 0);
    }
}
