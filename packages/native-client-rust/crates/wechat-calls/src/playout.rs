//! Queue and pacing policy without a timer or executor dependency.

use std::collections::VecDeque;
use std::time::Duration;

use crate::audio::{AudioFormat, AudioFrame, MediaError};

/// Overflow is chosen per stream. Incoming playback can discard stale frames;
/// outgoing audio rejects overflow so a producer cannot silently outrun pacing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverflowPolicy {
    RejectNewest,
    DropOldest,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueMetrics {
    pub dropped_frames: u64,
    pub underruns: u64,
    pub cleared_frames: u64,
}

/// Fixed-frame PCM queue. Clearing starts a new output generation, so delayed
/// producers cannot refill it with the interrupted response.
pub struct AudioQueue {
    format: AudioFormat,
    capacity: usize,
    policy: OverflowPolicy,
    generation: u64,
    frames: VecDeque<AudioFrame>,
    last: Option<(u64, u64)>,
    metrics: QueueMetrics,
}

impl AudioQueue {
    pub fn new(
        format: AudioFormat,
        capacity: usize,
        policy: OverflowPolicy,
        generation: u64,
    ) -> Result<Self, MediaError> {
        // This is an allocation budget, not a server limit. 500 frames can
        // represent at most 30s; callers should choose a much smaller live queue.
        if capacity == 0 || capacity > 500 {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self {
            format,
            capacity,
            policy,
            generation,
            frames: VecDeque::with_capacity(capacity),
            last: None,
            metrics: QueueMetrics::default(),
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn metrics(&self) -> QueueMetrics {
        self.metrics
    }

    pub fn push(&mut self, frame: AudioFrame) -> Result<(), MediaError> {
        self.format.validate_frame(&frame.samples)?;
        if frame.generation != self.generation {
            return Err(MediaError::StaleGeneration);
        }
        if self.last.is_some_and(|(sequence, timestamp)| {
            frame.sequence <= sequence || frame.timestamp <= timestamp
        }) {
            return Err(MediaError::OutOfOrder);
        }
        if self.frames.len() == self.capacity {
            if self.policy == OverflowPolicy::RejectNewest {
                return Err(MediaError::QueueFull);
            }
            self.frames.pop_front();
            self.metrics.dropped_frames = self.metrics.dropped_frames.saturating_add(1);
        }
        self.last = Some((frame.sequence, frame.timestamp));
        self.frames.push_back(frame);
        Ok(())
    }

    /// Call once for each due playback frame, not in a busy loop.
    pub fn pop(&mut self) -> Option<AudioFrame> {
        let frame = self.frames.pop_front();
        if frame.is_none() {
            self.metrics.underruns = self.metrics.underruns.saturating_add(1);
        }
        frame
    }

    /// Invalidate all queued and in-flight frames from the previous generation.
    /// Overflow leaves the current queue untouched and requires stream teardown.
    pub fn clear(&mut self) -> Result<u64, MediaError> {
        let next = self.generation.checked_add(1).ok_or(MediaError::Capacity)?;
        self.metrics.cleared_frames = self
            .metrics
            .cleared_frames
            .saturating_add(self.frames.len() as u64);
        self.frames.clear();
        self.generation = next;
        self.last = None;
        Ok(next)
    }
}

/// One frame per interval using host-supplied monotonic elapsed time. If the host
/// stalls, skip elapsed slots instead of transmitting a catch-up burst. This
/// schedules media; it does not implement network congestion control.
pub struct FramePacer {
    interval: Duration,
    next: Duration,
    last_now: Duration,
    skipped: u64,
}

impl FramePacer {
    pub fn new(format: AudioFormat, start: Duration) -> Self {
        Self {
            interval: Duration::from_millis(u64::from(format.frame_ms())),
            next: start,
            last_now: start,
            skipped: 0,
        }
    }

    pub fn poll(&mut self, now: Duration) -> Result<bool, MediaError> {
        if now < self.last_now {
            return Err(MediaError::OutOfOrder);
        }
        self.last_now = now;
        if now < self.next {
            return Ok(false);
        }
        let slots = (now - self.next).as_nanos() / self.interval.as_nanos();
        let skipped = u64::try_from(slots).map_err(|_| MediaError::Capacity)?;
        let remainder_nanos = (now - self.next).as_nanos() % self.interval.as_nanos();
        // Keep the original cadence even when callbacks arrive slightly late.
        let remaining = self.interval - Duration::from_nanos(remainder_nanos as u64);
        let next = now.checked_add(remaining).ok_or(MediaError::Capacity)?;
        self.skipped = self.skipped.saturating_add(skipped);
        self.next = next;
        Ok(true)
    }

    pub fn skipped_slots(&self) -> u64 {
        self.skipped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format() -> AudioFormat {
        AudioFormat::new(16000, 1, 20).unwrap()
    }
    fn frame(sequence: u64, generation: u64) -> AudioFrame {
        AudioFrame {
            generation,
            sequence,
            timestamp: sequence * 320,
            samples: vec![1; 320],
        }
    }

    #[test]
    fn interruption_fences_delayed_producer_and_allows_new_clock() {
        let mut queue = AudioQueue::new(format(), 2, OverflowPolicy::RejectNewest, 4).unwrap();
        queue.push(frame(100, 4)).unwrap();
        assert_eq!(queue.clear(), Ok(5));
        assert_eq!(queue.push(frame(101, 4)), Err(MediaError::StaleGeneration));
        queue.push(frame(0, 5)).unwrap();
        assert_eq!(queue.pop().unwrap().sequence, 0);
        assert_eq!(queue.metrics().cleared_frames, 1);
        assert!(queue.pop().is_none());
        assert_eq!(queue.metrics().underruns, 1);
    }

    #[test]
    fn receive_overflow_discards_old_audio_but_send_reports_backpressure() {
        let mut rx = AudioQueue::new(format(), 2, OverflowPolicy::DropOldest, 1).unwrap();
        for n in 0..3 {
            rx.push(frame(n, 1)).unwrap();
        }
        assert_eq!(rx.pop().unwrap().sequence, 1);
        assert_eq!(rx.metrics().dropped_frames, 1);
        let mut tx = AudioQueue::new(format(), 1, OverflowPolicy::RejectNewest, 1).unwrap();
        tx.push(frame(1, 1)).unwrap();
        assert_eq!(tx.push(frame(2, 1)), Err(MediaError::QueueFull));
        assert_eq!(tx.pop().unwrap().sequence, 1);
        tx.push(frame(2, 1)).unwrap();
        assert_eq!(tx.push(frame(1, 1)), Err(MediaError::OutOfOrder));
    }

    #[test]
    fn generation_exhaustion_preserves_current_queue() {
        let mut queue =
            AudioQueue::new(format(), 1, OverflowPolicy::RejectNewest, u64::MAX).unwrap();
        queue.push(frame(1, u64::MAX)).unwrap();
        assert_eq!(queue.clear(), Err(MediaError::Capacity));
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.generation(), u64::MAX);
    }

    #[test]
    fn late_clock_never_causes_a_burst_or_accepts_a_backward_clock() {
        let mut pacer = FramePacer::new(format(), Duration::ZERO);
        assert_eq!(pacer.poll(Duration::ZERO), Ok(true));
        assert_eq!(pacer.poll(Duration::from_millis(1)), Ok(false));
        assert_eq!(pacer.poll(Duration::from_millis(100)), Ok(true));
        assert_eq!(pacer.skipped_slots(), 4);
        assert_eq!(pacer.poll(Duration::from_millis(100)), Ok(false));
        assert_eq!(
            pacer.poll(Duration::from_millis(99)),
            Err(MediaError::OutOfOrder)
        );
        assert_eq!(pacer.poll(Duration::from_millis(120)), Ok(true));
    }
}
