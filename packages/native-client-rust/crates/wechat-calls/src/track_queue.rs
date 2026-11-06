//! Independent audio budgets with ordered, non-droppable track controls.
//! The host supplies synchronization and wakeups; this queue opens no runtime.
use crate::audio::MediaError;
use std::collections::{BTreeMap, VecDeque};

pub enum Delivery<A, E> {
    Control(E),
    Audio(A),
}

struct Track<A> {
    generation: u32,
    audio: VecDeque<A>,
}

/// Controls are drained before audio. Audio pressure drops only that source's
/// oldest waiting frame; it cannot consume another source's budget or a control.
pub struct TrackQueue<A, E> {
    tracks: BTreeMap<u32, Track<A>>,
    retired: BTreeMap<u32, u32>,
    controls: VecDeque<E>,
    max_tracks: usize,
    frames_per_track: usize,
    max_controls: usize,
    cursor: u32,
    dropped: u64,
}

impl<A, E> TrackQueue<A, E> {
    pub fn new(
        max_tracks: usize,
        frames_per_track: usize,
        max_controls: usize,
    ) -> Result<Self, MediaError> {
        if !(1..=128).contains(&max_tracks)
            || !(1..=500).contains(&frames_per_track)
            || !(1..=4096).contains(&max_controls)
        {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(Self {
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            controls: VecDeque::new(),
            max_tracks,
            frames_per_track,
            max_controls,
            cursor: 0,
            dropped: 0,
        })
    }

    /// Install tracks already included in an atomic readiness snapshot. Later
    /// announcements must use add so they precede their audio on this queue.
    pub fn initial_track(&mut self, id: u32, generation: u32) -> Result<(), MediaError> {
        self.check_add(id, generation)?;
        self.tracks.insert(
            id,
            Track {
                generation,
                audio: VecDeque::new(),
            },
        );
        Ok(())
    }

    fn check_add(&self, id: u32, generation: u32) -> Result<(), MediaError> {
        if id == 0 || generation == 0 {
            return Err(MediaError::InvalidConfiguration);
        }
        if self
            .tracks
            .get(&id)
            .is_some_and(|track| generation <= track.generation)
            || self.retired.get(&id).is_some_and(|old| generation <= *old)
        {
            return Err(MediaError::StaleGeneration);
        }
        if !self.tracks.contains_key(&id) && self.tracks.len() == self.max_tracks {
            return Err(MediaError::QueueFull);
        }
        Ok(())
    }

    pub fn control(&mut self, event: E) -> Result<(), MediaError> {
        if self.controls.len() == self.max_controls {
            return Err(MediaError::QueueFull);
        }
        self.controls.push_back(event);
        Ok(())
    }

    pub fn add(&mut self, id: u32, generation: u32, event: E) -> Result<(), MediaError> {
        self.check_add(id, generation)?;
        self.control(event)?;
        self.tracks.insert(
            id,
            Track {
                generation,
                audio: VecDeque::new(),
            },
        );
        Ok(())
    }

    pub fn remove(&mut self, id: u32, generation: u32, event: E) -> Result<(), MediaError> {
        let track = self.tracks.get(&id).ok_or(MediaError::WrongTrack)?;
        if track.generation != generation {
            return Err(MediaError::StaleGeneration);
        }
        if !self.retired.contains_key(&id) && self.retired.len() == 4096 {
            return Err(MediaError::Capacity);
        }
        self.control(event)?;
        self.tracks.remove(&id);
        self.retired.insert(id, generation);
        Ok(())
    }

    pub fn push(&mut self, id: u32, generation: u32, audio: A) -> Result<(), MediaError> {
        let track = self.tracks.get_mut(&id).ok_or(MediaError::WrongTrack)?;
        if track.generation != generation {
            return Err(MediaError::StaleGeneration);
        }
        if track.audio.len() == self.frames_per_track {
            track.audio.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        track.audio.push_back(audio);
        Ok(())
    }

    pub fn pending_audio(&self) -> usize {
        self.tracks.values().map(|track| track.audio.len()).sum()
    }

    pub fn take_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.dropped)
    }

    pub fn pop(&mut self) -> Option<Delivery<A, E>> {
        if let Some(control) = self.controls.pop_front() {
            return Some(Delivery::Control(control));
        }
        let id = self
            .tracks
            .range((
                std::ops::Bound::Excluded(self.cursor),
                std::ops::Bound::Unbounded,
            ))
            .find(|(_, track)| !track.audio.is_empty())
            .map(|(&id, _)| id)
            .or_else(|| {
                self.tracks
                    .iter()
                    .find(|(_, track)| !track.audio.is_empty())
                    .map(|(&id, _)| id)
            })?;
        self.cursor = id;
        self.tracks
            .get_mut(&id)?
            .audio
            .pop_front()
            .map(Delivery::Audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_pressure_preserves_other_audio_and_track_controls() {
        let mut queue = TrackQueue::new(2, 2, 8).unwrap();
        queue.add(2, 1, "added-a").unwrap();
        queue.add(3, 1, "added-b").unwrap();
        queue.push(3, 1, 30).unwrap();
        for n in 0..10 {
            queue.push(2, 1, n).unwrap();
        }
        assert_eq!(queue.take_dropped(), 8);
        assert!(matches!(queue.pop(), Some(Delivery::Control("added-a"))));
        assert!(matches!(queue.pop(), Some(Delivery::Control("added-b"))));
        assert!(matches!(queue.pop(), Some(Delivery::Audio(8))));
        assert!(matches!(queue.pop(), Some(Delivery::Audio(30))));
        assert!(matches!(queue.pop(), Some(Delivery::Audio(9))));
    }
    #[test]
    fn removal_and_replacement_discard_stale_audio_without_resetting_generation() {
        let mut queue = TrackQueue::new(1, 2, 8).unwrap();
        queue.initial_track(2, 1).unwrap();
        queue.push(2, 1, "old").unwrap();
        queue.remove(2, 1, "removed").unwrap();
        assert!(queue.push(2, 1, "late").is_err());
        assert!(queue.add(2, 1, "stale-add").is_err());
        queue.add(2, 2, "replacement").unwrap();
        queue.push(2, 2, "new").unwrap();
        assert!(matches!(queue.pop(), Some(Delivery::Control("removed"))));
        assert!(matches!(
            queue.pop(),
            Some(Delivery::Control("replacement"))
        ));
        assert!(matches!(queue.pop(), Some(Delivery::Audio("new"))));
        assert!(queue.pop().is_none());
    }
    #[test]
    fn control_overflow_never_partially_changes_track_membership() {
        let mut queue = TrackQueue::<u8, _>::new(1, 2, 1).unwrap();
        queue.add(2, 1, "added").unwrap();
        assert!(queue.remove(2, 1, "removed").is_err());
        queue.push(2, 1, 9).unwrap();
        assert!(matches!(queue.pop(), Some(Delivery::Control("added"))));
        assert!(matches!(queue.pop(), Some(Delivery::Audio(9))));
    }
}
