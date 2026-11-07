//! One paced conference uplink. Application interruption never resets crypto
//! counters or the presentation clock. Missing application audio becomes silence.
use crate::{
    audio::{AudioEncoder, AudioFormat},
    opus::OpusEncoder,
    playout::FramePacer,
};
use anyhow::{ensure, Context, Result};
use std::time::Duration;
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, MediaEpoch, TransmitScope},
    relay::SelectedRelay,
};

pub struct PreparedAudio {
    pub generation: u32,
    pub packet: Vec<u8>,
}

pub struct ConferenceSender {
    channel: Live96,
    encoder: OpusEncoder,
    pacer: FramePacer,
    pending: Option<Vec<u8>>,
    grouping: u8,
    sequence: u64,
    sample_clock: u64,
    epoch: MediaEpoch,
    generation: u32,
    microphone_enabled: bool,
    skipped: u64,
}

impl ConferenceSender {
    pub fn new(
        selected: &SelectedRelay,
        reference_member: u16,
        epoch: MediaEpoch,
        grouping: u8,
        start: Duration,
    ) -> Result<Self> {
        ensure!(
            matches!(grouping, 1 | 2),
            "Unsupported conference packet grouping"
        );
        let mut channel = Live96::new(
            *selected.media_key,
            selected.room_key,
            MediaContext {
                room_id: selected.room_id,
                own_member: selected.own_member,
                peer_member: reference_member,
                connection_id: selected.connection_id,
            },
            epoch,
        )?;
        channel.authorize_transmit(
            epoch,
            TransmitScope::LegacyRelayAudio {
                key_generation: epoch.key_generation,
            },
        )?;
        let format = AudioFormat::new(16000, 1, 20)?;
        Ok(Self {
            channel,
            encoder: OpusEncoder::new(format, 20_000)?,
            pacer: FramePacer::new(format, start),
            pending: None,
            grouping,
            sequence: 0,
            sample_clock: 0,
            epoch,
            generation: 1,
            microphone_enabled: true,
            skipped: 0,
        })
    }

    /// The source is called only for one due slot. A late worker skips elapsed
    /// slots instead of requesting a catch-up burst. The host must recheck this
    /// returned generation under its physical UDP handoff gate.
    pub fn poll(
        &mut self,
        now: Duration,
        generation: u32,
        microphone_enabled: bool,
        source: impl FnOnce() -> Option<Vec<i16>>,
    ) -> Result<Option<PreparedAudio>> {
        ensure!(
            generation != 0 && generation >= self.generation,
            "Stale conference output generation"
        );
        if generation != self.generation || microphone_enabled != self.microphone_enabled {
            self.pending = None;
            self.encoder.reset()?;
            self.generation = generation;
            self.microphone_enabled = microphone_enabled;
        }
        if !self.pacer.poll(now)? {
            return Ok(None);
        }
        let skipped = self.pacer.skipped_slots() - self.skipped;
        self.skipped = self.pacer.skipped_slots();
        self.sequence = self
            .sequence
            .checked_add(skipped)
            .context("Conference RTP sequence exhausted")?;
        self.sample_clock = self
            .sample_clock
            .checked_add(
                skipped
                    .checked_mul(320)
                    .context("Conference clock exhausted")?,
            )
            .context("Conference clock exhausted")?;
        if skipped != 0 {
            self.pending = None;
            self.encoder.reset()?;
        }
        let mut samples = source().unwrap_or_else(|| vec![0; 320]);
        self.encoder.format().validate_frame(&samples)?;
        if !microphone_enabled {
            samples.fill(0);
        }
        let sequence = u16::try_from(self.sequence).context("Conference RTP lifetime reached")?;
        let timestamp =
            u32::try_from(self.sample_clock).context("Conference RTP clock exhausted")?;
        let encoded = self.encoder.encode(&samples)?;
        self.sequence += 1;
        self.sample_clock += 320;
        let payload = if self.grouping == 1 {
            audio_packet::ordinary95_single(0, sequence, timestamp, &encoded)?
        } else if let Some(first) = self.pending.take() {
            audio_packet::ordinary95_pair(0, sequence, timestamp, &first, &encoded)?
        } else {
            self.pending = Some(encoded);
            return Ok(None);
        };
        Ok(Some(PreparedAudio {
            generation,
            packet: self.channel.seal_audio(
                self.epoch,
                u64::try_from(now.as_millis())?,
                &payload,
            )?,
        }))
    }
}
impl Drop for ConferenceSender {
    fn drop(&mut self) {
        self.channel.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(grouping: u8) -> (ConferenceSender, Live96, MediaEpoch) {
        let selected = SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:6000".parse().unwrap(),
            media_key: zeroize::Zeroizing::new([7; 32]),
            merge_mode: false,
        };
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let receiver = Live96::new_receive_only(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        (
            ConferenceSender::new(&selected, 1, epoch, grouping, Duration::ZERO).unwrap(),
            receiver,
            epoch,
        )
    }
    #[test]
    fn silence_is_continuous_and_interruption_does_not_reset_packet_counters() {
        let (mut sender, mut receiver, epoch) = setup(1);
        let first = sender
            .poll(Duration::ZERO, 1, true, || Some(vec![1200; 320]))
            .unwrap()
            .unwrap();
        assert!(receiver.open(epoch, &first.packet).unwrap().is_some());
        let second = sender
            .poll(Duration::from_millis(20), 2, false, || {
                Some(vec![1200; 320])
            })
            .unwrap()
            .unwrap();
        let opened = receiver.open(epoch, &second.packet).unwrap().unwrap();
        let primary = audio_packet::parse_ordinary95(opened.payload()).unwrap();
        assert_eq!(primary.last_sequence, 1);
        assert_eq!(primary.last_timestamp, 320);
        assert_eq!(second.generation, 2);
        let third = sender
            .poll(Duration::from_millis(40), 2, true, || None)
            .unwrap()
            .unwrap();
        assert!(receiver.open(epoch, &third.packet).unwrap().is_some());
        assert!(sender
            .poll(Duration::from_millis(41), 1, true, || panic!(
                "stale source"
            ))
            .is_err());
    }
    #[test]
    fn interrupted_pair_discards_old_primary_and_delayed_slots_do_not_burst() {
        let (mut sender, mut receiver, epoch) = setup(2);
        assert!(sender
            .poll(Duration::ZERO, 1, true, || Some(vec![1000; 320]))
            .unwrap()
            .is_none());
        assert!(sender
            .poll(Duration::from_millis(20), 2, true, || None)
            .unwrap()
            .is_none());
        let packet = sender
            .poll(Duration::from_millis(40), 2, true, || None)
            .unwrap()
            .unwrap();
        let opened = receiver.open(epoch, &packet.packet).unwrap().unwrap();
        assert_eq!(
            audio_packet::parse_ordinary95(opened.payload())
                .unwrap()
                .last_sequence,
            2
        );
        assert!(sender
            .poll(Duration::from_millis(200), 2, true, || None)
            .unwrap()
            .is_none());
        assert!(sender
            .poll(Duration::from_millis(200), 2, true, || panic!(
                "no catch-up"
            ))
            .unwrap()
            .is_none());
    }
}
