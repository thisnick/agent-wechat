//! Authenticated conference source decoding. Devices, sockets and persistence
//! belong to the embedding runtime; no packet can announce its own participant.
use crate::{
    audio::{AudioDecoder, AudioFormat},
    opus::OpusDecoder,
    ordering::RtpOrder,
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, MediaEpoch},
    relay::SelectedRelay,
};

pub struct DecodedAudio {
    pub sequence: u64,
    pub sample_offset: u64,
    pub samples: Vec<i16>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamReport {
    pub authenticated_packets: u64,
    pub replayed_packets: u64,
    pub authenticated_nonce_reuses: BTreeMap<u8, u64>,
    pub invalid_packets: u64,
    pub invalid_while_inactive: u64,
    pub invalid_reasons: BTreeMap<&'static str, u64>,
    pub control_packets: u64,
    pub decoded_frames: u64,
    pub nonzero_frames: u64,
    pub peak_magnitude: u16,
    pub ordering_drops: u64,
    pub unsupported_audio: u64,
    pub feedback_packets: u64,
    pub unsupported_lengths: BTreeMap<usize, u64>,
    pub unsupported_reasons: BTreeMap<&'static str, u64>,
    pub payload_kinds: BTreeMap<u8, u64>,
    pub codec_markers: BTreeMap<String, u64>,
    pub aggregate_packets: u64,
    pub malformed_aggregates: u64,
    pub aggregate_member_hints: BTreeMap<u16, u64>,
    pub nonzero_second_bins: BTreeMap<u16, u64>,
    pub joined_events: u64,
    pub left_events: u64,
    pub identity_quarantined: bool,
    pub decode_errors: u64,
}
pub struct ConferenceReceiver {
    channel: Live96,
    decoder: OpusDecoder,
    order: RtpOrder,
    pub report: StreamReport,
    timeline: Option<(u32, u64)>,
    sequence: u64,
    decoding: bool,
}
impl ConferenceReceiver {
    pub fn new(selected: &SelectedRelay, member: u16, epoch: MediaEpoch) -> Result<Self> {
        Ok(Self {
            channel: Live96::new_legacy_receive_only(
                *selected.media_key,
                selected.room_key,
                MediaContext {
                    room_id: selected.room_id,
                    own_member: selected.own_member,
                    peer_member: member,
                    connection_id: selected.connection_id,
                },
                epoch,
            )?,
            decoder: OpusDecoder::new(AudioFormat::new(16000, 1, 20)?)?,
            order: RtpOrder::new(),
            report: StreamReport::default(),
            timeline: None,
            sequence: 0,
            decoding: true,
        })
    }
    pub fn consume(
        &mut self,
        epoch: MediaEpoch,
        packet: &[u8],
        elapsed_ms: u64,
        decode_audio: bool,
    ) -> Result<Vec<DecodedAudio>> {
        let result = self.channel.open(epoch, packet);
        self.report.authenticated_nonce_reuses = self.channel.received_nonce_reuses().clone();
        let opened = match result {
            Ok(Some(opened)) => opened,
            Ok(None) => {
                self.report.replayed_packets += 1;
                return Ok(vec![]);
            }
            Err(error) => {
                self.report.invalid_packets += 1;
                self.report.invalid_while_inactive += u64::from(!decode_audio);
                let reason = match error.to_string().as_str() {
                    "Call media authentication failed" => "authentication",
                    "Authenticated media nonce conflict" => "same_sender_nonce_conflict",
                    "Media generation changed or closed" => "closed_or_stale",
                    "Media room/member mismatch" => "room_or_clear_member",
                    "Protected media member mismatch or extent" => "protected_member",
                    "Unsupported Live96 packet family or extension" => "family_or_extension",
                    _ => "other",
                };
                *self.report.invalid_reasons.entry(reason).or_default() += 1;
                return Ok(vec![]);
            }
        };
        self.report.authenticated_packets += 1;
        if opened.packed_kind != 0x10 {
            self.report.control_packets += 1;
            return Ok(vec![]);
        }
        if let Some(kind) = opened.payload().first() {
            *self.report.payload_kinds.entry(*kind).or_default() += 1;
        }
        if opened.payload().first() == Some(&2) {
            self.report.feedback_packets += 1;
            return Ok(vec![]);
        }
        if opened.payload().first() == Some(&0x65) {
            match audio_packet::parse_aggregate_audio(opened.payload()) {
                Ok(aggregate) => {
                    self.report.aggregate_packets += 1;
                    for (member, _) in aggregate.members {
                        if self.report.aggregate_member_hints.len() < 128
                            || self.report.aggregate_member_hints.contains_key(&member)
                        {
                            *self
                                .report
                                .aggregate_member_hints
                                .entry(member)
                                .or_default() += 1;
                        }
                    }
                }
                Err(_) => self.report.malformed_aggregates += 1,
            }
            // One nested aggregate payload is not separate per-member PCM.
            return Ok(vec![]);
        }
        if self.decoding != decode_audio {
            self.decoder.reset()?;
            self.decoding = decode_audio;
        }
        if let Some(marker) = audio_packet::ordinary_audio_marker(opened.payload()) {
            let key = format!("{:02x}{:02x}", marker[0], marker[1]);
            if self.report.codec_markers.len() < 16 || self.report.codec_markers.contains_key(&key)
            {
                *self.report.codec_markers.entry(key).or_default() += 1;
            }
        }
        // Only roster-admitted ordinary audio reaches its source decoder.
        let primary = match audio_packet::parse_ordinary95(opened.payload()) {
            Ok(primary) => primary,
            Err(error) => {
                self.report.unsupported_audio += 1;
                let length = opened.payload().len();
                if self.report.unsupported_lengths.len() < 16
                    || self.report.unsupported_lengths.contains_key(&length)
                {
                    *self.report.unsupported_lengths.entry(length).or_default() += 1;
                }
                let reason = match error.to_string().as_str() {
                    "Unsupported ordinary95 audio shape" => "shape",
                    "Invalid ordinary95 primary count" => "primary_count",
                    "Invalid ordinary95 primary length" => "primary_length",
                    "Truncated ordinary95 primary" => "truncated_primary",
                    "Trailing ordinary95 bytes" => "trailing_bytes",
                    _ => "other",
                };
                *self.report.unsupported_reasons.entry(reason).or_default() += 1;
                return Ok(vec![]);
            }
        };
        let admitted = match self.order.admit(
            primary.last_sequence,
            primary.last_timestamp,
            primary.primaries.len(),
        ) {
            Ok(admitted) => admitted,
            Err(_) => {
                self.report.ordering_drops += 1;
                return Ok(vec![]);
            }
        };
        let count = primary.primaries.len();
        let first_timestamp = primary
            .last_timestamp
            .wrapping_sub((count as u32 - 1) * 320);
        let first_offset = (elapsed_ms / 20)
            .saturating_mul(320)
            .saturating_sub((count as u64 - 1) * 320);
        let (origin, offset) = *self.timeline.get_or_insert((first_timestamp, first_offset));
        let mut output = Vec::new();
        for (index, frame) in primary
            .primaries
            .into_iter()
            .enumerate()
            .skip(admitted.skip)
        {
            let timestamp = first_timestamp.wrapping_add(index as u32 * 320);
            let delta = timestamp.wrapping_sub(origin);
            let sequence = self.sequence;
            self.sequence = self
                .sequence
                .checked_add(1)
                .context("Conference PCM sequence exhausted")?;
            if !decode_audio || delta >= 1 << 31 {
                continue;
            }
            let sample_offset = offset
                .checked_add(u64::from(delta))
                .context("Conference presentation clock exhausted")?;
            let pcm = match self.decoder.decode(frame) {
                Ok(pcm) => pcm,
                Err(_) => {
                    self.report.decode_errors += 1;
                    self.decoder.reset()?;
                    continue;
                }
            };
            self.report.decoded_frames += 1;
            let peak = pcm
                .iter()
                .map(|sample| sample.unsigned_abs())
                .max()
                .unwrap_or(0);
            self.report.peak_magnitude = self.report.peak_magnitude.max(peak);
            self.report.nonzero_frames += u64::from(peak > 8);
            if peak > 8 {
                *self
                    .report
                    .nonzero_second_bins
                    .entry((elapsed_ms / 1000).min(u64::from(u16::MAX)) as u16)
                    .or_default() += 1;
            }
            output.push(DecodedAudio {
                sequence,
                sample_offset,
                samples: pcm,
            });
        }
        Ok(output)
    }

    pub fn close(&mut self) {
        self.channel.close();
    }

    /// New application track incarnation; keep authenticated replay/order state
    /// and the shared presentation clock, while discarding codec history.
    pub fn reset_decoder(&mut self) -> Result<()> {
        self.decoder.reset().map_err(Into::into)
    }
}
impl Drop for ConferenceReceiver {
    fn drop(&mut self) {
        self.close();
    }
}
