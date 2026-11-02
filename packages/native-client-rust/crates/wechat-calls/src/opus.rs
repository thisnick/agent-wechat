//! Optional native libopus implementation of the portable audio interfaces.
//!
//! Each instance has independent encoder/decoder state. Input packets must have
//! passed transport authentication before reaching this adapter. FEC and DTX are
//! disabled initially; a negotiated mode must opt into either separately.

use crate::audio::{AudioDecoder, AudioEncoder, AudioFormat, MediaError};
use opusic_c::{Application, Bitrate, Channels, Decoder, Encoder, InbandFec, SampleRate};

/// Local allocation/input budget for a single raw Opus packet.
pub const MAX_OPUS_PACKET_BYTES: usize = 4000;

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
}
