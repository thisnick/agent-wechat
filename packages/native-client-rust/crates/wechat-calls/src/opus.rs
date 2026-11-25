//! Optional native libopus implementation of the portable audio interfaces.
//!
//! Each instance has independent encoder/decoder state. Input packets must have
//! passed transport authentication before reaching this adapter. FEC and DTX are
//! disabled initially; a negotiated mode must opt into either separately.

use crate::audio::{AudioDecoder, AudioEncoder, AudioFormat, MediaError};
use opusic_c::{Application, Bitrate, Channels, Decoder, Encoder, InbandFec, SampleRate};

/// Local allocation/input budget for a single raw Opus packet.
pub const MAX_OPUS_PACKET_BYTES: usize = 4000;

/// Header magic for WeChat's `#!OPUS_RAW_V1` length-prefixed Opus voice container.
pub const RAW_OPUS_V1_MAGIC: &[u8; 13] = b"#!OPUS_RAW_V1";

const RAW_OPUS_V1_HEADER_BYTES: usize = 16;
const RAW_OPUS_V1_RATES: [u32; 5] = [8000, 12000, 16000, 24000, 48000];
const MAX_RAW_OPUS_V1_DURATION_SECS: usize = 300;
const MAX_RAW_OPUS_V1_PACKETS: usize = 15_000;

/// Decoded PCM audio extracted from a `#!OPUS_RAW_V1` voice container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedOpusAudio {
    /// Interleaved signed 16-bit PCM samples.
    pub samples: Vec<i16>,
    /// Output sample rate in Hz (`8000`, `12000`, `16000`, `24000`, or `48000`).
    pub sample_rate: u32,
    /// Channel count (`1` for mono, `2` for stereo).
    pub channels: u16,
}

/// Decode a WeChat `#!OPUS_RAW_V1` voice container into signed 16-bit PCM.
///
/// The container consists of a 16-byte header (`#!OPUS_RAW_V1`, 1-byte sample
/// rate index `0..=4`, 1-byte channel count `1..=2`, and a zero byte) followed
/// by `[u16 little-endian length][Opus packet]` frames. Zero-length frames are
/// concealed via Opus packet-loss concealment (20 ms).
pub fn decode_raw_opus_v1(input: &[u8]) -> Result<DecodedOpusAudio, MediaError> {
    if input.len() < RAW_OPUS_V1_HEADER_BYTES
        || &input[..RAW_OPUS_V1_MAGIC.len()] != RAW_OPUS_V1_MAGIC
        || input[15] != 0
    {
        return Err(MediaError::InvalidPacket);
    }
    let sample_rate = *RAW_OPUS_V1_RATES
        .get(usize::from(input[13]))
        .ok_or(MediaError::InvalidConfiguration)?;
    let channel_count = u16::from(input[14]);
    let opus_channels = match channel_count {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        _ => return Err(MediaError::InvalidConfiguration),
    };
    let opus_rate = match sample_rate {
        8000 => SampleRate::Hz8000,
        12000 => SampleRate::Hz12000,
        16000 => SampleRate::Hz16000,
        24000 => SampleRate::Hz24000,
        48000 => SampleRate::Hz48000,
        _ => unreachable!(),
    };
    let mut decoder = Decoder::new(opus_channels, opus_rate).map_err(codec_error)?;
    let ch = usize::from(channel_count);
    // An Opus packet can carry up to 120 ms per frame.
    let max_frame_samples = (sample_rate as usize * 120 / 1000) * ch;
    let conceal_frame_samples = (sample_rate as usize * 20 / 1000) * ch;
    let max_total_samples = (sample_rate as usize)
        .saturating_mul(MAX_RAW_OPUS_V1_DURATION_SECS)
        .saturating_mul(ch);
    let mut scratch = vec![0u16; max_frame_samples];
    let mut samples = Vec::new();
    let mut pos = RAW_OPUS_V1_HEADER_BYTES;
    let mut packets = 0usize;
    while pos + 2 <= input.len() {
        let len = usize::from(u16::from_le_bytes([input[pos], input[pos + 1]]));
        pos += 2;
        if len > MAX_OPUS_PACKET_BYTES {
            return Err(MediaError::InvalidPacket);
        }
        if len > input.len() - pos {
            // Stop cleanly with decoded audio on trailing truncation; fail if nothing decoded yet.
            if samples.is_empty() {
                return Err(MediaError::InvalidPacket);
            }
            break;
        }
        packets += 1;
        if packets > MAX_RAW_OPUS_V1_PACKETS {
            return Err(MediaError::InvalidPacket);
        }
        let decoded_per_channel = if len == 0 {
            decoder
                .decode_to_slice(&[], &mut scratch[..conceal_frame_samples], false)
                .map_err(codec_error)?
        } else {
            let packet = &input[pos..pos + len];
            pos += len;
            decoder
                .decode_to_slice(packet, &mut scratch, false)
                .map_err(codec_error)?
        };
        let decoded_total = decoded_per_channel.saturating_mul(ch);
        if decoded_total == 0
            || decoded_total > scratch.len()
            || samples.len().saturating_add(decoded_total) > max_total_samples
        {
            return Err(MediaError::InvalidPacket);
        }
        samples.extend(scratch[..decoded_total].iter().map(|&s| s as i16));
    }
    if samples.is_empty() {
        return Err(MediaError::InvalidPacket);
    }
    Ok(DecodedOpusAudio {
        samples,
        sample_rate,
        channels: channel_count,
    })
}

fn codec_error(error: opusic_c::ErrorCode) -> MediaError {
    MediaError::Codec { code: error as i32 }
}

fn channels(format: AudioFormat) -> Channels {
    match format.channels() {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        _ => unreachable!("validated AudioFormat"),
    }
}

fn rate(format: AudioFormat) -> SampleRate {
    match format.sample_rate() {
        8000 => SampleRate::Hz8000,
        12000 => SampleRate::Hz12000,
        16000 => SampleRate::Hz16000,
        24000 => SampleRate::Hz24000,
        48000 => SampleRate::Hz48000,
        _ => unreachable!("validated AudioFormat"),
    }
}

pub struct OpusEncoder {
    format: AudioFormat,
    encoder: Encoder,
    pcm: Vec<u16>,
}

impl OpusEncoder {
    /// Construct a VOIP encoder. Bitrate is an application-selected value;
    /// passing this range check does not establish remote negotiation.
    pub fn new(format: AudioFormat, bitrate: u32) -> Result<Self, MediaError> {
        if !(6000..=128000).contains(&bitrate) {
            return Err(MediaError::InvalidConfiguration);
        }
        let mut encoder =
            Encoder::new(channels(format), rate(format), Application::Voip).map_err(codec_error)?;
        encoder
            .set_bitrate(Bitrate::Value(bitrate))
            .map_err(codec_error)?;
        encoder
            .set_inband_fec(InbandFec::Off)
            .map_err(codec_error)?;
        encoder.set_dtx(false).map_err(codec_error)?;
        Ok(Self {
            format,
            encoder,
            pcm: vec![0; format.samples_per_frame()],
        })
    }
}

impl AudioEncoder for OpusEncoder {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>, MediaError> {
        self.format.validate_frame(samples)?;
        for (dest, &sample) in self.pcm.iter_mut().zip(samples) {
            // opusic-c's u16 API passes these exact bits to libopus's i16 API.
            *dest = sample as u16;
        }
        let mut output = vec![0; MAX_OPUS_PACKET_BYTES];
        let written = self
            .encoder
            .encode_to_slice(&self.pcm, &mut output)
            .map_err(codec_error)?;
        if written == 0 || written > output.len() {
            return Err(MediaError::InvalidPacket);
        }
        output.truncate(written);
        Ok(output)
    }

    fn reset(&mut self) -> Result<(), MediaError> {
        self.encoder.reset().map_err(codec_error)
    }
}

pub struct OpusDecoder {
    format: AudioFormat,
    decoder: Decoder,
    pcm: Vec<u16>,
}

impl OpusDecoder {
    pub fn new(format: AudioFormat) -> Result<Self, MediaError> {
        Ok(Self {
            format,
            decoder: Decoder::new(channels(format), rate(format)).map_err(codec_error)?,
            pcm: vec![0; format.samples_per_frame()],
        })
    }

    fn decode_inner(&mut self, packet: &[u8]) -> Result<Vec<i16>, MediaError> {
        let samples = self
            .decoder
            .decode_to_slice(packet, &mut self.pcm, false)
            .map_err(codec_error)?;
        if samples != self.format.samples_per_channel() {
            return Err(MediaError::InvalidPacket);
        }
        Ok(self.pcm.iter().map(|&sample| sample as i16).collect())
    }
}

impl AudioDecoder for OpusDecoder {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, MediaError> {
        if packet.is_empty() || packet.len() > MAX_OPUS_PACKET_BYTES {
            return Err(MediaError::InvalidPacket);
        }
        // Reject duration changes before they can mutate decoder prediction state.
        let samples = self.decoder.get_nb_samples(packet).map_err(codec_error)?;
        if samples != self.format.samples_per_channel() {
            return Err(MediaError::InvalidPacket);
        }
        self.decode_inner(packet)
    }

    fn conceal(&mut self) -> Result<Vec<i16>, MediaError> {
        self.decode_inner(&[])
    }

    fn reset(&mut self) -> Result<(), MediaError> {
        self.decoder.reset().map_err(codec_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::PulseTone;
    use std::time::Duration;

    #[test]
    fn stereo_decode_keeps_every_interleaved_sample() {
        let format = AudioFormat::new(48000, 2, 20).unwrap();
        let mut encoder = OpusEncoder::new(format, 64000).unwrap();
        let mut decoder = OpusDecoder::new(format).unwrap();
        let mut tone = PulseTone::new(
            format,
            440.0,
            0.2,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let packet = encoder.encode(&tone.next_frame().unwrap()).unwrap();
        let pcm = decoder.decode(&packet).unwrap();
        assert_eq!(pcm.len(), 1920);
        assert!(pcm[960..].iter().any(|&sample| sample != 0));
    }

    #[test]
    fn reset_restores_independent_encoder_and_decoder_state() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let input = (0..320)
            .map(|i| ((i % 40) * 200 - 4000) as i16)
            .collect::<Vec<_>>();
        let mut encoder = OpusEncoder::new(format, 20000).unwrap();
        let mut decoder = OpusDecoder::new(format).unwrap();
        let initial = encoder.encode(&input).unwrap();
        let initial_pcm = decoder.decode(&initial).unwrap();
        encoder.encode(&input).unwrap();
        decoder.conceal().unwrap();
        encoder.reset().unwrap();
        decoder.reset().unwrap();
        let reset = encoder.encode(&input).unwrap();
        assert_eq!(initial, reset);
        assert_eq!(initial_pcm, decoder.decode(&reset).unwrap());
    }

    #[test]
    fn malformed_bounds_and_duration_change_fail_without_implicit_plc() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let mut decoder = OpusDecoder::new(format).unwrap();
        assert_eq!(decoder.decode(&[]), Err(MediaError::InvalidPacket));
        assert!(decoder.decode(&vec![0; MAX_OPUS_PACKET_BYTES + 1]).is_err());
        assert!(decoder.decode(&[0xff]).is_err());
        let long = AudioFormat::new(16000, 1, 60).unwrap();
        let mut encoder = OpusEncoder::new(long, 20000).unwrap();
        let packet = encoder.encode(&vec![0; long.samples_per_frame()]).unwrap();
        assert_eq!(decoder.decode(&packet), Err(MediaError::InvalidPacket));
        assert_eq!(encoder.encode(&[0]), Err(MediaError::InvalidFrame));
    }

    #[test]
    fn raw_opus_v1_container_decodes_mono_stereo_plc_and_variable_frame_durations() {
        let build = |rate_idx: u8, channels: u8, packets: &[Vec<u8>]| -> Vec<u8> {
            let mut out = Vec::from(*RAW_OPUS_V1_MAGIC);
            out.extend([rate_idx, channels, 0]);
            for pkt in packets {
                out.extend((pkt.len() as u16).to_le_bytes());
                out.extend(pkt);
            }
            out
        };

        // 16 kHz mono with 20 ms frames + a zero-length PLC frame.
        let mono_fmt = AudioFormat::new(16000, 1, 20).unwrap();
        let mut mono_enc = OpusEncoder::new(mono_fmt, 32000).unwrap();
        let mut tone = PulseTone::new(
            mono_fmt,
            440.0,
            0.3,
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap();
        let p1 = mono_enc.encode(&tone.next_frame().unwrap()).unwrap();
        let p2 = mono_enc.encode(&tone.next_frame().unwrap()).unwrap();
        let container = build(2, 1, &[p1, vec![], p2]);
        let decoded = decode_raw_opus_v1(&container).unwrap();
        assert_eq!(decoded.sample_rate, 16000);
        assert_eq!(decoded.channels, 1);
        assert_eq!(decoded.samples.len(), 320 * 3);
        assert!(decoded.samples.iter().any(|&s| s.abs() > 500));

        // 48 kHz stereo with 40 ms frame.
        let stereo_fmt = AudioFormat::new(48000, 2, 40).unwrap();
        let mut stereo_enc = OpusEncoder::new(stereo_fmt, 64000).unwrap();
        let stereo_pkt = stereo_enc
            .encode(&vec![1500i16; stereo_fmt.samples_per_frame()])
            .unwrap();
        let stereo_container = build(4, 2, &[stereo_pkt]);
        let decoded_stereo = decode_raw_opus_v1(&stereo_container).unwrap();
        assert_eq!(decoded_stereo.sample_rate, 48000);
        assert_eq!(decoded_stereo.channels, 2);
        assert_eq!(decoded_stereo.samples.len(), 1920 * 2);

        // Malformed headers, empty containers, and mid-stream oversize packet lengths are rejected.
        assert!(decode_raw_opus_v1(b"not-opus").is_err());
        assert!(decode_raw_opus_v1(&build(2, 1, &[])).is_err());
        let mut bad_reserved = build(2, 1, &[vec![1, 2]]);
        bad_reserved[15] = 1;
        assert!(decode_raw_opus_v1(&bad_reserved).is_err());
        assert!(decode_raw_opus_v1(&build(9, 1, &[vec![1, 2]])).is_err());
        let mut midstream_oversize = build(
            2,
            1,
            &[mono_enc.encode(&tone.next_frame().unwrap()).unwrap()],
        );
        midstream_oversize.extend(((MAX_OPUS_PACKET_BYTES + 1) as u16).to_le_bytes());
        midstream_oversize.extend(vec![0u8; MAX_OPUS_PACKET_BYTES + 1]);
        assert_eq!(
            decode_raw_opus_v1(&midstream_oversize),
            Err(MediaError::InvalidPacket)
        );
    }
}
