//! Host-authorized, paced synthetic audio for the isolated duplex probe.
use super::*;
#[cfg(test)]
use crate::{audio::AudioDecoder, opus::OpusDecoder};
use crate::{
    audio::{AudioEncoder, AudioFormat},
    opus::OpusEncoder,
    playout::FramePacer,
    tone::PulseTone,
};
use wechat_protocol::calls::audio_packet::{Live96, MediaContext, TransmitScope};

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToneReport {
    pub enabled: bool,
    pub encoded_frames: u64,
    pub nonzero_frames: u64,
    pub silence_frames: u64,
    pub prepared_datagrams: u64,
    pub submitted_datagrams: u64,
    pub skipped_slots: u64,
}

pub(super) struct Tone {
    channel: Live96,
    encoder: OpusEncoder,
    source: PulseTone,
    pacer: FramePacer,
    pending: Option<Vec<u8>>,
    grouping: u8,
    sequence: u64,
    timestamp: u64,
    epoch: MediaEpoch,
    microphone_enabled: bool,
    pub report: ToneReport,
}
impl Tone {
    pub fn new(
        selected: &SelectedRelay,
        reference_member: u16,
        epoch: MediaEpoch,
        grouping: u8,
    ) -> Result<Self> {
        ensure!(
            matches!(grouping, 1 | 2),
            "Unsupported diagnostic audio grouping"
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
        // The host's duplex-probe mode grants this scope only after a fresh,
        // matching Join. This keeps the existing sender/room-derived start and
        // compatibility policy; it does not claim global nonce uniqueness.
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
            source: PulseTone::new(
                format,
                660.0,
                0.06,
                Duration::from_millis(250),
                Duration::from_millis(750),
            )?,
            pacer: FramePacer::new(format, Duration::ZERO),
            pending: None,
            grouping,
            sequence: 0,
            timestamp: 0,
            epoch,
            microphone_enabled: true,
            report: ToneReport {
                enabled: true,
                ..Default::default()
            },
        })
    }
    pub fn poll(&mut self, now: Duration, microphone_enabled: bool) -> Result<Option<Vec<u8>>> {
        if microphone_enabled != self.microphone_enabled {
            // Discard a buffered primary and codec history at the mute boundary
            // so neither can leak pre-mute sound. Keep clocks and nonce state.
            self.pending = None;
            self.encoder.reset()?;
            self.microphone_enabled = microphone_enabled;
        }
        if !self.pacer.poll(now)? {
            return Ok(None);
        }
        let skipped = self.pacer.skipped_slots() - self.report.skipped_slots;
        self.report.skipped_slots = self.pacer.skipped_slots();
        self.sequence = self
            .sequence
            .checked_add(skipped)
            .context("Diagnostic audio sequence exhausted")?;
        self.timestamp = self
            .timestamp
            .checked_add(
                skipped
                    .checked_mul(320)
                    .context("Diagnostic audio clock exhausted")?,
            )
            .context("Diagnostic audio clock exhausted")?;
        if skipped != 0 {
            self.pending = None;
            self.encoder.reset()?;
        }
        let sequence = u16::try_from(self.sequence)?;
        let timestamp = u32::try_from(self.timestamp)?;
        let mut pcm = self.source.next_frame()?;
        if !microphone_enabled {
            pcm.fill(0);
        }
        let nonzero = pcm.iter().any(|sample| *sample != 0);
        self.report.nonzero_frames += u64::from(nonzero);
        self.report.silence_frames += u64::from(!nonzero);
        let encoded = self.encoder.encode(&pcm)?;
        self.report.encoded_frames += 1;
        self.sequence += 1;
        self.timestamp += 320;
        let packet = if self.grouping == 1 {
            audio_packet::ordinary95_single(0, sequence, timestamp, &encoded)?
        } else if let Some(first) = self.pending.take() {
            audio_packet::ordinary95_pair(0, sequence, timestamp, &first, &encoded)?
        } else {
            self.pending = Some(encoded);
            return Ok(None);
        };
        let datagram =
            self.channel
                .seal_audio(self.epoch, u64::try_from(now.as_millis())?, &packet)?;
        self.report.prepared_datagrams += 1;
        Ok(Some(datagram))
    }
    pub fn finish(mut self) -> ToneReport {
        self.channel.close();
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected() -> SelectedRelay {
        SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:6000".parse().unwrap(),
            media_key: zeroize::Zeroizing::new([7; 32]),
            merge_mode: false,
        }
    }
    fn epoch() -> MediaEpoch {
        MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        }
    }
    #[test]
    fn generated_tone_and_silence_roundtrip_through_both_negotiated_groupings() {
        for grouping in [1, 2] {
            let mut tone = Tone::new(&selected(), 1, epoch(), grouping).unwrap();
            let mut receiver = Live96::new_receive_only(
                [7; 32],
                77,
                MediaContext {
                    room_id: 42,
                    own_member: 1,
                    peer_member: 0,
                    connection_id: 1,
                },
                epoch(),
            )
            .unwrap();
            let mut decoder = OpusDecoder::new(AudioFormat::new(16000, 1, 20).unwrap()).unwrap();
            let mut frames = 0;
            for index in 0..100 {
                if let Some(packet) = tone.poll(Duration::from_millis(index * 20), true).unwrap() {
                    let opened = receiver.open(epoch(), &packet).unwrap().unwrap();
                    let audio = audio_packet::parse_ordinary95(opened.payload()).unwrap();
                    assert_eq!(audio.primaries.len(), usize::from(grouping));
                    for primary in audio.primaries {
                        assert_eq!(decoder.decode(primary).unwrap().len(), 320);
                        frames += 1;
                    }
                }
            }
            let report = tone.finish();
            assert_eq!(frames, 100);
            assert!(report.silence_frames > 0);
            assert!(report.nonzero_frames > 0);
            assert_eq!(report.prepared_datagrams, 100 / u64::from(grouping));
        }
    }
    #[test]
    fn scheduler_stall_skips_slots_without_replaying_old_pcm_or_resetting_envelope() {
        let mut tone = Tone::new(&selected(), 1, epoch(), 1).unwrap();
        assert!(tone.poll(Duration::ZERO, true).unwrap().is_some());
        assert!(tone.poll(Duration::ZERO, true).unwrap().is_none());
        assert!(tone
            .poll(Duration::from_millis(300), true)
            .unwrap()
            .is_some());
        assert!(tone
            .poll(Duration::from_millis(300), true)
            .unwrap()
            .is_none());
        assert_eq!(tone.report.encoded_frames, 2);
        assert_eq!(tone.report.skipped_slots, 14);
    }

    #[test]
    fn microphone_mute_discards_pending_sound_and_sends_paced_silence() {
        let mut tone = Tone::new(&selected(), 1, epoch(), 2).unwrap();
        assert!(tone.poll(Duration::ZERO, true).unwrap().is_none());
        assert!(tone
            .poll(Duration::from_millis(20), false)
            .unwrap()
            .is_none());
        let packet = tone
            .poll(Duration::from_millis(40), false)
            .unwrap()
            .unwrap();
        let mut receiver = Live96::new_receive_only(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: 1,
                peer_member: 0,
                connection_id: 1,
            },
            epoch(),
        )
        .unwrap();
        let opened = receiver.open(epoch(), &packet).unwrap().unwrap();
        let audio = audio_packet::parse_ordinary95(opened.payload()).unwrap();
        let mut decoder = OpusDecoder::new(AudioFormat::new(16000, 1, 20).unwrap()).unwrap();
        assert_eq!(audio.primaries.len(), 2);
        for primary in audio.primaries {
            assert!(decoder
                .decode(primary)
                .unwrap()
                .iter()
                .all(|sample| sample.unsigned_abs() < 10));
        }
        assert_eq!(tone.report.silence_frames, 2);
        assert!(tone
            .poll(Duration::from_millis(60), true)
            .unwrap()
            .is_none());
        assert!(tone
            .poll(Duration::from_millis(80), true)
            .unwrap()
            .is_some());
        assert_eq!(tone.report.skipped_slots, 0);
    }
}
