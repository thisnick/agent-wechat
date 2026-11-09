//! Version-two receive tracks and timed local playback. No account or device state.
use super::*;
use anyhow::ensure;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::collections::BTreeMap;
use wechat_calls::{
    audio::{AudioFormat, AudioFrame, MediaError},
    mixer::PcmMixer,
};
use wechat_core::call_types::{CallAudioTrack, CallMediaCommand, CallTrackDirection};

pub(super) struct ReceiveAudio {
    format: AudioFormat,
    tracks: BTreeMap<u32, CallAudioTrack>,
    retired: BTreeMap<u32, u32>,
    mixer: PcmMixer,
    clock: Option<(u64, Duration)>,
    next_slot: Option<u64>,
}
impl ReceiveAudio {
    pub(super) fn new(format: AudioFormat, tracks: Vec<CallAudioTrack>) -> Result<Self> {
        let mut value = Self {
            format,
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            mixer: PcmMixer::new(format, 64, 10, 0)?,
            clock: None,
            next_slot: None,
        };
        for track in tracks {
            value.track(track)?;
        }
        Ok(value)
    }
    fn track(&mut self, track: CallAudioTrack) -> Result<()> {
        ensure!(
            track.id != 0
                && track.generation != 0
                && track.direction == CallTrackDirection::Receive
                && track.format.sample_rate == self.format.sample_rate()
                && track.format.channels == self.format.channels()
                && track.format.frame_ms == self.format.frame_ms(),
            "unsupported receive track format"
        );
        if self
            .retired
            .get(&track.id)
            .is_some_and(|old| track.generation <= *old)
        {
            return Ok(());
        }
        if let Some(old) = self.tracks.get(&track.id) {
            if track.generation < old.generation {
                return Ok(());
            }
            ensure!(
                track.generation != old.generation
                    || old.participant_id.is_none()
                    || old.participant_id == track.participant_id,
                "receive track identity changed without a generation change"
            );
        }
        self.mixer
            .add_track(track.id, u64::from(track.generation))?;
        self.tracks.insert(track.id, track);
        Ok(())
    }
    pub(super) fn event(&mut self, event: &CallEvent, call_id: &str) -> Result<bool> {
        match event {
            CallEvent::TrackAdded { call_id: id, track }
            | CallEvent::TrackUpdated { call_id: id, track }
                if id == call_id && track.direction == CallTrackDirection::Receive =>
            {
                self.track(track.clone())?;
                Ok(true)
            }
            CallEvent::TrackRemoved {
                call_id: id,
                track_id,
                generation,
                ..
            } if id == call_id => {
                if self.retired.len() >= 256 && !self.retired.contains_key(track_id) {
                    bail!("receive track history limit reached");
                }
                self.retired
                    .entry(*track_id)
                    .and_modify(|old| *old = (*old).max(*generation))
                    .or_insert(*generation);
                if self
                    .tracks
                    .get(track_id)
                    .is_some_and(|track| track.generation <= *generation)
                {
                    self.tracks.remove(track_id);
                    self.mixer.remove_track(*track_id);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    pub(super) fn chunk(
        &mut self,
        command: CallMediaCommand,
        now: Duration,
        levels: &mut levels::CallLevels,
    ) -> Result<()> {
        let CallMediaCommand::AudioChunk {
            track_id,
            generation,
            sequence,
            sample_offset,
            pcm_base64,
        } = command
        else {
            bail!("unexpected media command");
        };
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return Ok(());
        }
        let track = self
            .tracks
            .get(&track_id)
            .context("audio arrived before its track announcement")?;
        if generation < track.generation {
            return Ok(());
        }
        ensure!(
            generation == track.generation,
            "audio arrived before its track generation"
        );
        ensure!(pcm_base64.len() <= 87384, "call PCM chunk too large");
        let bytes = BASE64.decode(pcm_base64)?;
        ensure!(
            bytes.len() == self.format.samples_per_frame() * 2,
            "invalid receive PCM frame length"
        );
        let samples: Vec<i16> = bytes
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect();
        let offset = sample_offset.context("receive audio has no presentation clock")?;
        let sequence = sequence.context("receive audio has no sequence")?;
        match self.mixer.push(
            track_id,
            AudioFrame {
                generation: u64::from(generation),
                sequence,
                timestamp: offset,
                samples: samples.clone(),
            },
        ) {
            Ok(()) => {}
            Err(MediaError::OutOfOrder) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        levels.participant_audio(track_id, track.participant_id.as_deref(), &samples);
        if let Some((first, deadline)) = &mut self.clock {
            if now < *deadline {
                *first = (*first).min(offset);
            }
        } else {
            self.clock = Some((offset, now + Duration::from_millis(40)));
        }
        Ok(())
    }
    pub(super) fn render(&mut self, now: Duration) -> Result<Option<Vec<i16>>> {
        let Some((origin, deadline)) = self.clock else {
            return Ok(None);
        };
        if now < deadline {
            return Ok(None);
        }
        let slots =
            u64::try_from((now - deadline).as_millis() / u128::from(self.format.frame_ms()))?;
        let step = self.format.samples_per_channel() as u64;
        let offset = origin
            .checked_add(
                slots
                    .checked_mul(step)
                    .context("playback clock exhausted")?,
            )
            .context("playback clock exhausted")?;
        if self.next_slot.is_some_and(|next| offset < next) {
            return Ok(None);
        }
        self.next_slot = Some(
            offset
                .checked_add(step)
                .context("playback clock exhausted")?,
        );
        Ok(Some(self.mixer.render(offset)?.samples))
    }
    pub(super) fn roster(&mut self, call: &CallView) -> Result<()> {
        for (&id, track) in &self.tracks {
            let muted = track
                .participant_id
                .as_ref()
                .and_then(|id| call.participants.iter().find(|p| p.participant_id == *id))
                .is_some_and(|p| {
                    p.microphone_enabled == Some(false)
                        || p.membership == wechat_core::call_types::CallMembership::Left
                });
            self.mixer.set_muted(id, muted)?;
        }
        Ok(())
    }
    pub(super) fn track_count(&self) -> usize {
        self.tracks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: u32, generation: u32) -> CallAudioTrack {
        CallAudioTrack {
            id,
            generation,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some(format!("person-{id}")),
        }
    }
    fn chunk(
        id: u32,
        generation: u32,
        sequence: u64,
        offset: u64,
        sample: i16,
    ) -> CallMediaCommand {
        CallMediaCommand::AudioChunk {
            track_id: id,
            generation,
            sequence: Some(sequence),
            sample_offset: Some(offset),
            pcm_base64: BASE64.encode(sample.to_le_bytes().repeat(320)),
        }
    }
    #[test]
    fn simultaneous_sources_mix_once_with_delay_and_separate_levels() {
        let mut rx = ReceiveAudio::new(
            AudioFormat::new(16000, 1, 20).unwrap(),
            vec![track(7, 1), track(8, 1)],
        )
        .unwrap();
        let mut levels = levels::CallLevels::for_test();
        rx.chunk(chunk(7, 1, 0, 3200, 1000), Duration::ZERO, &mut levels)
            .unwrap();
        rx.chunk(
            chunk(8, 1, 0, 3200, 2000),
            Duration::from_millis(5),
            &mut levels,
        )
        .unwrap();
        assert!(rx.render(Duration::from_millis(39)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(40)).unwrap().unwrap(),
            vec![3000; 320]
        );
        assert!(rx.render(Duration::from_millis(41)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(80)).unwrap().unwrap(),
            vec![0; 320]
        );
        levels.refresh();
        assert!(levels.participant_level("person-8") > levels.participant_level("person-7"));
    }
    #[test]
    fn retirement_replacement_and_late_frames_never_restore_old_audio() {
        let mut rx =
            ReceiveAudio::new(AudioFormat::new(16000, 1, 20).unwrap(), vec![track(7, 1)]).unwrap();
        let mut levels = levels::CallLevels::for_test();
        rx.chunk(chunk(7, 1, 0, 0, 1000), Duration::ZERO, &mut levels)
            .unwrap();
        rx.event(
            &CallEvent::TrackRemoved {
                call_id: "test".into(),
                track_id: 7,
                generation: 1,
                reason: "left".into(),
            },
            "test",
        )
        .unwrap();
        rx.chunk(chunk(7, 1, 1, 320, 1000), Duration::ZERO, &mut levels)
            .unwrap();
        rx.event(
            &CallEvent::TrackAdded {
                call_id: "test".into(),
                track: track(7, 1),
            },
            "test",
        )
        .unwrap();
        assert_eq!(rx.track_count(), 0);
        rx.event(
            &CallEvent::TrackAdded {
                call_id: "test".into(),
                track: track(7, 2),
            },
            "test",
        )
        .unwrap();
        rx.chunk(chunk(7, 2, 0, 0, 2000), Duration::ZERO, &mut levels)
            .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(40)).unwrap().unwrap(),
            vec![2000; 320]
        );
        rx.chunk(
            chunk(7, 2, 1, 0, 30000),
            Duration::from_millis(100),
            &mut levels,
        )
        .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(100)).unwrap().unwrap(),
            vec![0; 320]
        );
    }
}
