//! Version-three binary receive tracks and timed local playback. No account or device state.
use super::*;
use anyhow::ensure;
use std::collections::BTreeMap;
use wechat_calls::{
    audio::{AudioFormat, AudioFrame, MediaError},
    mixer::PcmMixer,
};
use wechat_core::call_types::{CallTrack, CallTrackDirection};
use wechat_core::calls::pcm_envelope::{MediaPayloadKind, WireMediaFrame};

pub(super) struct ReceiveAudio {
    format: AudioFormat,
    tracks: BTreeMap<u32, CallTrack>,
    retired: BTreeMap<u32, u32>,
    mixer: PcmMixer,
    alignment_delay: Duration,
    clock: Option<(u64, Duration)>,
    next_slot: Option<u64>,
    offset_bias: i64,
    consecutive_late: u32,
    silent_renders: u32,
}
impl ReceiveAudio {
    pub(super) fn new(format: AudioFormat, tracks: Vec<CallTrack>) -> Result<Self> {
        let mut value = Self {
            format,
            tracks: BTreeMap::new(),
            retired: BTreeMap::new(),
            mixer: PcmMixer::new(format, 64, 25, 0)?,
            alignment_delay: Duration::from_millis(60),
            clock: None,
            next_slot: None,
            offset_bias: 0,
            consecutive_late: 0,
            silent_renders: 0,
        };
        for track in tracks {
            if !track.is_video() {
                value.track(track)?;
            }
        }
        Ok(value)
    }
    fn track(&mut self, track: CallTrack) -> Result<()> {
        if track.is_video() {
            return Ok(());
        }
        let Some(audio_format) = track.audio_format() else {
            return Ok(());
        };
        let track_id = track.id();
        let generation = track.generation();
        ensure!(
            track_id != 0
                && generation != 0
                && track.direction() == CallTrackDirection::Receive
                && audio_format.sample_rate == self.format.sample_rate()
                && audio_format.channels == self.format.channels()
                && audio_format.frame_ms == self.format.frame_ms(),
            "unsupported receive track format"
        );
        if self
            .retired
            .get(&track_id)
            .is_some_and(|old| generation <= *old)
        {
            return Ok(());
        }
        if let Some(old) = self.tracks.get(&track_id) {
            if generation < old.generation() {
                return Ok(());
            }
            ensure!(
                generation != old.generation()
                    || old.participant_id().is_none()
                    || old.participant_id() == track.participant_id(),
                "receive track identity changed without a generation change"
            );
        }
        self.mixer.add_track(track_id, u64::from(generation))?;
        self.tracks.insert(track_id, track);
        Ok(())
    }
    pub(super) fn event(&mut self, event: &CallEvent, call_id: &str) -> Result<bool> {
        match event {
            CallEvent::TrackAdded { call_id: id, track }
            | CallEvent::TrackUpdated { call_id: id, track }
                if id == call_id
                    && !track.is_video()
                    && track.direction() == CallTrackDirection::Receive =>
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
                    .is_some_and(|track| track.generation() <= *generation)
                {
                    self.tracks.remove(track_id);
                    self.mixer.remove_track(*track_id);
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn binary_frame(
        &mut self,
        bytes: &[u8],
        now: Duration,
        levels: &mut levels::CallLevels,
    ) -> Result<()> {
        let frame = WireMediaFrame::decode(bytes)?;
        self.wire_frame(&frame, now, levels)
    }
    pub(super) fn wire_frame(
        &mut self,
        frame: &WireMediaFrame<'_>,
        now: Duration,
        levels: &mut levels::CallLevels,
    ) -> Result<()> {
        if frame.kind != MediaPayloadKind::PcmS16le {
            return Ok(());
        }
        let track_id = frame.header.track_id;
        let generation = frame.header.generation;
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
        if generation < track.generation() {
            return Ok(());
        }
        ensure!(
            generation == track.generation(),
            "audio arrived before its track generation"
        );
        let payload = frame.payload();
        ensure!(
            payload.len() == self.format.samples_per_frame() * 2,
            "invalid receive PCM frame length"
        );
        let samples: Vec<i16> = payload
            .chunks_exact(2)
            .map(|s| i16::from_le_bytes([s[0], s[1]]))
            .collect();
        let raw_offset =
            frame.header.timestamp_micros * u64::from(self.format.sample_rate()) / 1_000_000;
        let step = self.format.samples_per_channel() as u64;
        let mut offset = (raw_offset as i128 + i128::from(self.offset_bias)).max(0) as u64;
        if let Some(next) = self.next_slot {
            if offset < next {
                self.consecutive_late = self.consecutive_late.saturating_add(1);
                if self.consecutive_late >= 5 && self.silent_renders >= 3 {
                    // Re-anchor monotonically after sustained drift or DTX pause so
                    // live speech resumes cleanly without rewinding the mixer.
                    self.offset_bias = (next as i128 - raw_offset as i128) as i64;
                    offset = next;
                    self.clock = Some((next, now + self.alignment_delay));
                    self.consecutive_late = 0;
                    self.silent_renders = 0;
                }
            } else if offset > next.saturating_add(step.saturating_mul(25))
                && self.silent_renders >= 3
            {
                self.offset_bias = (next as i128 - raw_offset as i128) as i64;
                offset = next;
                self.clock = Some((next, now + self.alignment_delay));
                self.consecutive_late = 0;
                self.silent_renders = 0;
            } else {
                self.consecutive_late = 0;
            }
        }
        let sequence = frame.header.sequence;
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
        levels.participant_audio(track_id, track.participant_id(), &samples);
        if let Some((first, deadline)) = &mut self.clock {
            if now < *deadline {
                *first = (*first).min(offset);
            }
        } else {
            self.clock = Some((offset, now + self.alignment_delay));
        }
        Ok(())
    }
    #[cfg_attr(not(test), allow(dead_code))]
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
        let mixed = self.mixer.render(offset)?;
        if mixed.contributing_tracks.is_empty() {
            self.silent_renders = self.silent_renders.saturating_add(1);
        } else {
            self.silent_renders = 0;
        }
        Ok(Some(mixed.samples))
    }
    /// Render every due 20 ms playout slot monotonically from `next_slot` through
    /// the current wall-clock slot, never skipping a slot on minor timer jitter.
    pub(super) fn render_due(&mut self, now: Duration) -> Result<Vec<Vec<i16>>> {
        const MAX_CATCHUP_SLOTS: u64 = 4;
        let Some((origin, deadline)) = self.clock else {
            return Ok(Vec::new());
        };
        if now < deadline {
            return Ok(Vec::new());
        }
        let slots =
            u64::try_from((now - deadline).as_millis() / u128::from(self.format.frame_ms()))?;
        let step = self.format.samples_per_channel() as u64;
        let target = origin
            .checked_add(
                slots
                    .checked_mul(step)
                    .context("playback clock exhausted")?,
            )
            .context("playback clock exhausted")?;
        let mut cursor = self.next_slot.unwrap_or(origin);
        if target < cursor {
            return Ok(Vec::new());
        }
        let due_count = (target - cursor) / step + 1;
        if due_count > MAX_CATCHUP_SLOTS {
            // Skip stale frames only after a genuine multi-frame stall (> 80 ms)
            // so normal timer jitter never drops an intermediate speech frame.
            cursor = target.saturating_sub((MAX_CATCHUP_SLOTS - 1).saturating_mul(step));
        }
        let mut frames = Vec::with_capacity(due_count.min(MAX_CATCHUP_SLOTS) as usize);
        while cursor <= target {
            let mixed = self.mixer.render(cursor)?;
            if mixed.contributing_tracks.is_empty() {
                self.silent_renders = self.silent_renders.saturating_add(1);
            } else {
                self.silent_renders = 0;
            }
            frames.push(mixed.samples);
            cursor = cursor
                .checked_add(step)
                .context("playback clock exhausted")?;
            self.next_slot = Some(cursor);
        }
        Ok(frames)
    }
    pub(super) fn roster(&mut self, call: &CallView) -> Result<()> {
        for (&id, track) in &self.tracks {
            let muted = track
                .participant_id()
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
    use wechat_core::calls::pcm_envelope::{encode_pcm, MediaHeader};
    fn track(id: u32, generation: u32) -> CallTrack {
        CallTrack::Audio {
            id,
            generation,
            direction: CallTrackDirection::Receive,
            format: CallAudioFormat::default(),
            participant_id: Some(format!("person-{id}")),
        }
    }
    fn frame(id: u32, generation: u32, sequence: u64, offset: u64, sample: i16) -> Vec<u8> {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        encode_pcm(
            MediaHeader {
                track_id: id,
                generation,
                sequence,
                timestamp_micros: offset * 1_000_000 / 16000,
            },
            format,
            &[sample; 320],
        )
        .unwrap()
    }
    #[test]
    fn simultaneous_sources_mix_once_with_delay_and_separate_levels() {
        let mut rx = ReceiveAudio::new(
            AudioFormat::new(16000, 1, 20).unwrap(),
            vec![track(7, 1), track(8, 1)],
        )
        .unwrap();
        let mut levels = levels::CallLevels::for_test();
        rx.binary_frame(&frame(7, 1, 0, 3200, 1000), Duration::ZERO, &mut levels)
            .unwrap();
        rx.binary_frame(
            &frame(8, 1, 0, 3200, 2000),
            Duration::from_millis(5),
            &mut levels,
        )
        .unwrap();
        assert!(rx.render(Duration::from_millis(59)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![3000; 320]
        );
        assert!(rx.render(Duration::from_millis(61)).unwrap().is_none());
        assert_eq!(
            rx.render(Duration::from_millis(100)).unwrap().unwrap(),
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
        rx.binary_frame(&frame(7, 1, 0, 0, 1000), Duration::ZERO, &mut levels)
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
        rx.binary_frame(&frame(7, 1, 1, 320, 1000), Duration::ZERO, &mut levels)
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
        rx.binary_frame(&frame(7, 2, 0, 0, 2000), Duration::ZERO, &mut levels)
            .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![2000; 320]
        );
        rx.binary_frame(
            &frame(7, 2, 1, 0, 30000),
            Duration::from_millis(120),
            &mut levels,
        )
        .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(120)).unwrap().unwrap(),
            vec![0; 320]
        );
    }

    #[test]
    fn sustained_late_drift_after_silence_resyncs_monotonically() {
        let mut rx =
            ReceiveAudio::new(AudioFormat::new(16000, 1, 20).unwrap(), vec![track(2, 1)]).unwrap();
        let mut levels = levels::CallLevels::for_test();
        rx.binary_frame(&frame(2, 1, 0, 0, 1500), Duration::ZERO, &mut levels)
            .unwrap();
        assert_eq!(
            rx.render(Duration::from_millis(60)).unwrap().unwrap(),
            vec![1500; 320]
        );
        // Simulate a 2-second DTX silence where playback renders silence and advances
        // wall-clock slots far past offset 320.
        for slot in 1..=5u64 {
            assert_eq!(
                rx.render(Duration::from_millis(60 + slot * 20))
                    .unwrap()
                    .unwrap(),
                vec![0; 320]
            );
        }
        // Now a sender whose sample clock did not advance during silence resumes at
        // offsets 320..1920 (all behind next_slot = 1920). After 5 consecutive late
        // frames during silence, ReceiveAudio re-anchors monotonically and resumes audio.
        for i in 1..=5u64 {
            rx.binary_frame(
                &frame(2, 1, i, i * 320, 4000),
                Duration::from_millis(2000 + i * 20),
                &mut levels,
            )
            .unwrap();
        }
        assert_eq!(
            rx.render(Duration::from_millis(2160)).unwrap().unwrap(),
            vec![4000; 320]
        );
    }

    #[test]
    fn jittered_render_ticks_never_skip_slots_or_insert_gaps() {
        let mut rx =
            ReceiveAudio::new(AudioFormat::new(16000, 1, 20).unwrap(), vec![track(2, 1)]).unwrap();
        let mut levels = levels::CallLevels::for_test();
        for i in 0..6u64 {
            rx.binary_frame(
                &frame(2, 1, i, i * 320, (1000 + i * 100) as i16),
                Duration::from_millis(i * 20),
                &mut levels,
            )
            .unwrap();
        }
        let mut rendered = Vec::new();
        // Alternating early (19 ms) and late (21 ms) timer ticks around 20 ms cadence.
        for ms in [59, 60, 79, 101, 119, 141, 160] {
            for samples in rx.render_due(Duration::from_millis(ms)).unwrap() {
                rendered.push(samples[0]);
            }
        }
        assert_eq!(rendered, vec![1000, 1100, 1200, 1300, 1400, 1500]);
    }
}
