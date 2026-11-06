//! PCM mixing on a shared presentation clock. This module owns no timer or
//! device: the consumer supplies the due sample offset after its alignment delay.
use std::collections::BTreeMap;

use crate::audio::{AudioFormat, AudioFrame, MediaError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MixerMetrics {
    pub late_frames: u64,
    pub overflow_frames: u64,
    pub missing_frames: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MixedFrame {
    pub sample_offset: u64,
    pub samples: Vec<i16>,
    /// Tracks with a frame in this slot, not a claim that a person spoke.
    pub contributing_tracks: Vec<u32>,
}

struct Track {
    generation: u64,
    muted: bool,
    last_sequence: Option<u64>,
    frames: BTreeMap<u64, AudioFrame>,
}

/// A fixed-format mixer with separate queue budgets for each receive track.
/// Missing/muted tracks contribute silence. Rendering never waits for a source.
pub struct PcmMixer {
    format: AudioFormat,
    track_capacity: usize,
    frame_capacity: usize,
    next_offset: u64,
    tracks: BTreeMap<u32, Track>,
    metrics: MixerMetrics,
}

impl PcmMixer {
    pub fn new(
        format: AudioFormat,
        track_capacity: usize,
        frames_per_track: usize,
        start_offset: u64,
    ) -> Result<Self, MediaError> {
        if !(1..=64).contains(&track_capacity)
            || !(1..=500).contains(&frames_per_track)
            || !start_offset.is_multiple_of(format.samples_per_channel() as u64)
        {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self {
            format,
            track_capacity,
            frame_capacity: frames_per_track,
            next_offset: start_offset,
            tracks: BTreeMap::new(),
            metrics: MixerMetrics::default(),
        })
    }

    /// Announce a receive source. A newer generation replaces queued PCM but
    /// does not reset the call clock or any transport cryptographic state.
    pub fn add_track(&mut self, track_id: u32, generation: u64) -> Result<(), MediaError> {
        if let Some(track) = self.tracks.get_mut(&track_id) {
            if generation < track.generation {
                return Err(MediaError::StaleGeneration);
            }
            if generation > track.generation {
                track.generation = generation;
                track.frames.clear();
                track.last_sequence = None;
            }
            return Ok(());
        }
        if self.tracks.len() == self.track_capacity {
            return Err(MediaError::QueueFull);
        }
        self.tracks.insert(
            track_id,
            Track {
                generation,
                muted: false,
                last_sequence: None,
                frames: BTreeMap::new(),
            },
        );
        Ok(())
    }

    pub fn remove_track(&mut self, track_id: u32) -> bool {
        self.tracks.remove(&track_id).is_some()
    }

    pub fn set_muted(&mut self, track_id: u32, muted: bool) -> Result<(), MediaError> {
        let track = self
            .tracks
            .get_mut(&track_id)
            .ok_or(MediaError::WrongTrack)?;
        track.muted = muted;
        if muted {
            track.frames.clear();
        }
        Ok(())
    }

    pub fn push(&mut self, track_id: u32, frame: AudioFrame) -> Result<(), MediaError> {
        self.format.validate_frame(&frame.samples)?;
        if !frame
            .timestamp
            .is_multiple_of(self.format.samples_per_channel() as u64)
        {
            return Err(MediaError::InvalidFrame);
        }
        let track = self
            .tracks
            .get_mut(&track_id)
            .ok_or(MediaError::WrongTrack)?;
        if frame.generation != track.generation {
            return Err(MediaError::StaleGeneration);
        }
        if track
            .last_sequence
            .is_some_and(|last| frame.sequence <= last)
            || track.frames.contains_key(&frame.timestamp)
        {
            return Err(MediaError::OutOfOrder);
        }
        track.last_sequence = Some(frame.sequence);
        if frame.timestamp < self.next_offset {
            self.metrics.late_frames = self.metrics.late_frames.saturating_add(1);
            return Ok(());
        }
        if track.muted {
            return Ok(());
        }
        track.frames.insert(frame.timestamp, frame);
        if track.frames.len() > self.frame_capacity {
            track.frames.pop_first();
            self.metrics.overflow_frames = self.metrics.overflow_frames.saturating_add(1);
        }
        Ok(())
    }

    /// Mix one due slot. Advancing past skipped slots drops old PCM rather than
    /// building a catch-up backlog. Sequence/generation checks remain per track.
    pub fn render(&mut self, sample_offset: u64) -> Result<MixedFrame, MediaError> {
        let step = self.format.samples_per_channel() as u64;
        if sample_offset < self.next_offset || !sample_offset.is_multiple_of(step) {
            return Err(MediaError::OutOfOrder);
        }
        let next = sample_offset
            .checked_add(step)
            .ok_or(MediaError::Capacity)?;
        let mut mixed = vec![0i64; self.format.samples_per_frame()];
        let mut contributing_tracks = Vec::new();
        for (&id, track) in &mut self.tracks {
            while track
                .frames
                .first_key_value()
                .is_some_and(|(&offset, _)| offset < sample_offset)
            {
                track.frames.pop_first();
                self.metrics.late_frames = self.metrics.late_frames.saturating_add(1);
            }
            if track.muted {
                continue;
            }
            if let Some(frame) = track.frames.remove(&sample_offset) {
                for (sum, sample) in mixed.iter_mut().zip(frame.samples) {
                    *sum += i64::from(sample);
                }
                contributing_tracks.push(id);
            } else {
                self.metrics.missing_frames = self.metrics.missing_frames.saturating_add(1);
            }
        }
        let peak = mixed.iter().map(|sample| sample.abs()).max().unwrap_or(0);
        let samples = mixed
            .into_iter()
            .map(|sample| {
                if peak > i64::from(i16::MAX) {
                    (sample * i64::from(i16::MAX) / peak) as i16
                } else {
                    sample as i16
                }
            })
            .collect();
        self.next_offset = next;
        Ok(MixedFrame {
            sample_offset,
            samples,
            contributing_tracks,
        })
    }

    pub fn metrics(&self) -> MixerMetrics {
        self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format() -> AudioFormat {
        AudioFormat::new(16000, 1, 20).unwrap()
    }
    fn frame(generation: u64, sequence: u64, offset: u64, sample: i16) -> AudioFrame {
        AudioFrame {
            generation,
            sequence,
            timestamp: offset,
            samples: vec![sample; 320],
        }
    }

    #[test]
    fn simultaneous_tracks_overlap_and_missing_sources_are_silence() {
        let mut mixer = PcmMixer::new(format(), 3, 4, 0).unwrap();
        mixer.add_track(1, 1).unwrap();
        mixer.add_track(2, 1).unwrap();
        mixer.add_track(3, 1).unwrap();
        mixer.push(1, frame(1, 0, 0, 100)).unwrap();
        mixer.push(2, frame(1, 0, 0, 200)).unwrap();
        let mixed = mixer.render(0).unwrap();
        assert_eq!(mixed.samples, vec![300; 320]);
        assert_eq!(mixed.contributing_tracks, vec![1, 2]);
        assert_eq!(mixer.render(320).unwrap().samples, vec![0; 320]);
        assert_eq!(mixer.metrics().missing_frames, 4);
    }

    #[test]
    fn noisy_track_cannot_evict_another_tracks_audio() {
        let mut mixer = PcmMixer::new(format(), 2, 2, 0).unwrap();
        mixer.add_track(1, 1).unwrap();
        mixer.add_track(2, 1).unwrap();
        mixer.push(2, frame(1, 0, 0, 200)).unwrap();
        for sequence in 0..8 {
            mixer
                .push(1, frame(1, sequence, sequence * 320, 100))
                .unwrap();
        }
        assert_eq!(mixer.render(0).unwrap().samples, vec![200; 320]);
        assert_eq!(mixer.metrics().overflow_frames, 6);
    }

    #[test]
    fn mute_replacement_and_removal_never_replay_queued_speech() {
        let mut mixer = PcmMixer::new(format(), 2, 4, 0).unwrap();
        mixer.add_track(1, 1).unwrap();
        mixer.push(1, frame(1, 0, 0, 123)).unwrap();
        mixer.set_muted(1, true).unwrap();
        mixer.set_muted(1, false).unwrap();
        assert_eq!(mixer.render(0).unwrap().samples, vec![0; 320]);
        mixer.push(1, frame(1, 1, 320, 123)).unwrap();
        mixer.add_track(1, 2).unwrap();
        assert_eq!(
            mixer.push(1, frame(1, 2, 320, 999)),
            Err(MediaError::StaleGeneration)
        );
        mixer.push(1, frame(2, 0, 320, 456)).unwrap();
        assert_eq!(mixer.render(320).unwrap().samples, vec![456; 320]);
        mixer.push(1, frame(2, 1, 640, 789)).unwrap();
        assert!(mixer.remove_track(1));
        assert_eq!(mixer.render(640).unwrap().samples, vec![0; 320]);
    }

    #[test]
    fn skipped_slots_and_late_arrivals_do_not_build_playback_lag() {
        let mut mixer = PcmMixer::new(format(), 1, 4, 0).unwrap();
        mixer.add_track(1, 1).unwrap();
        mixer.push(1, frame(1, 0, 0, 111)).unwrap();
        mixer.push(1, frame(1, 1, 960, 333)).unwrap();
        assert_eq!(mixer.render(640).unwrap().samples, vec![0; 320]);
        mixer.push(1, frame(1, 2, 320, 222)).unwrap();
        assert_eq!(mixer.render(960).unwrap().samples, vec![333; 320]);
        assert_eq!(mixer.metrics().late_frames, 2);
    }

    #[test]
    fn peak_limiter_scales_whole_frame_without_integer_wrap() {
        let mut mixer = PcmMixer::new(format(), 2, 2, 0).unwrap();
        for id in [1, 2] {
            mixer.add_track(id, 1).unwrap();
            let mut samples = frame(1, 0, 0, 30000);
            samples.samples[1] = 15000;
            samples.samples[2] = -30000;
            mixer.push(id, samples).unwrap();
        }
        let samples = mixer.render(0).unwrap().samples;
        assert_eq!(&samples[..3], &[32767, 16383, -32767]);
    }

    #[test]
    fn invalid_budget_frames_and_clock_do_not_mutate_playout() {
        assert!(PcmMixer::new(format(), 0, 2, 0).is_err());
        assert!(PcmMixer::new(format(), 1, 0, 0).is_err());
        let mut mixer = PcmMixer::new(format(), 1, 2, 0).unwrap();
        mixer.add_track(1, 1).unwrap();
        assert_eq!(mixer.add_track(2, 1), Err(MediaError::QueueFull));
        assert_eq!(
            mixer.push(1, frame(1, 0, 1, 1)),
            Err(MediaError::InvalidFrame)
        );
        assert_eq!(mixer.render(1), Err(MediaError::OutOfOrder));
        mixer.push(1, frame(1, 0, 0, 1)).unwrap();
        assert_eq!(mixer.render(0).unwrap().samples, vec![1; 320]);
        assert_eq!(mixer.render(0), Err(MediaError::OutOfOrder));
    }
}
