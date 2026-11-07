//! Generation-scoped PCM chunk admission and framing, independent of sockets.
use crate::audio::{AudioFormat, AudioFrame, MediaError};
use std::collections::VecDeque;

pub struct PcmOutput {
    format: AudioFormat,
    generation: Option<u32>,
    pending: VecDeque<u8>,
    limit: usize,
    sequence: u64,
    sample_offset: u64,
}
impl PcmOutput {
    pub fn new(format: AudioFormat, capacity_samples: usize) -> Result<Self, MediaError> {
        let limit = capacity_samples
            .checked_mul(2)
            .ok_or(MediaError::Capacity)?;
        if capacity_samples < format.samples_per_frame() {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self {
            format,
            generation: None,
            pending: VecDeque::new(),
            limit,
            sequence: 0,
            sample_offset: 0,
        })
    }
    pub fn start(&mut self, generation: u32, current: u32) -> Result<(), MediaError> {
        if generation == 0
            || generation > current
            || self.generation.is_some_and(|old| generation <= old)
        {
            return Err(MediaError::StaleGeneration);
        }
        self.generation = Some(generation);
        self.pending.clear();
        self.sequence = 0;
        self.sample_offset = 0;
        Ok(())
    }
    /// Admission is atomic. A full queue never accepts a prefix of a chunk.
    pub fn push(&mut self, generation: u32, current: u32, bytes: &[u8]) -> Result<(), MediaError> {
        if bytes.is_empty()
            || !bytes
                .len()
                .is_multiple_of(usize::from(self.format.channels()) * 2)
        {
            return Err(MediaError::InvalidFrame);
        }
        if generation == 0 || generation > current {
            return Err(MediaError::StaleGeneration);
        }
        if generation < current {
            self.discard_stale(current);
            return Ok(());
        }
        if self.generation != Some(generation) {
            return Err(MediaError::StaleGeneration);
        }
        if bytes.len() > self.limit.saturating_sub(self.pending.len()) {
            return Err(MediaError::QueueFull);
        }
        self.pending.extend(bytes);
        Ok(())
    }
    fn discard_stale(&mut self, current: u32) {
        if self
            .generation
            .is_some_and(|generation| generation < current)
        {
            self.pending.clear();
        }
    }
    pub fn pending_bytes(&self) -> usize {
        self.pending.len()
    }
    pub fn pop(&mut self, current: u32) -> Result<Option<AudioFrame>, MediaError> {
        self.discard_stale(current);
        let bytes = self.format.samples_per_frame() * 2;
        if self.generation != Some(current) || self.pending.len() < bytes {
            return Ok(None);
        }
        let next_sequence = self.sequence.checked_add(1).ok_or(MediaError::Capacity)?;
        let next_offset = self
            .sample_offset
            .checked_add(self.format.samples_per_channel() as u64)
            .ok_or(MediaError::Capacity)?;
        let payload = self.pending.drain(..bytes).collect::<Vec<_>>();
        let frame = AudioFrame {
            generation: u64::from(current),
            sequence: self.sequence,
            timestamp: self.sample_offset,
            samples: payload
                .chunks_exact(2)
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        };
        self.sequence = next_sequence;
        self.sample_offset = next_offset;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_chunk_admission_partial_frames_and_clear_generation() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let mut queue = PcmOutput::new(format, 640).unwrap();
        assert!(queue.push(1, 1, &[0, 0]).is_err());
        assert!(queue.start(2, 1).is_err());
        queue.start(1, 1).unwrap();
        assert!(queue.push(1, 1, &[0]).is_err());
        queue.push(1, 1, &[1, 0]).unwrap();
        assert!(queue.pop(1).unwrap().is_none());
        assert_eq!(queue.push(1, 1, &[0; 1280]), Err(MediaError::QueueFull));
        assert_eq!(queue.pending_bytes(), 2);
        queue.push(1, 1, &[0; 638]).unwrap();
        let frame = queue.pop(1).unwrap().unwrap();
        assert_eq!(
            (frame.sequence, frame.timestamp, frame.samples[0]),
            (0, 0, 1)
        );
        queue.push(1, 1, &[3; 640]).unwrap();
        assert!(queue.pop(2).unwrap().is_none());
        assert_eq!(queue.pending_bytes(), 0);
        queue.start(2, 2).unwrap();
        queue.push(1, 2, &[3; 640]).unwrap();
        assert_eq!(queue.pending_bytes(), 0);
        queue.push(2, 2, &[0; 640]).unwrap();
        assert_eq!(queue.pop(2).unwrap().unwrap().sequence, 0);
        assert!(queue.start(1, 2).is_err());
    }
}
