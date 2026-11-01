//! Version 1 daemon/client PCM frames. This is our API format, not WeChat wire.
//!
//! All header integers are big endian; PCM samples are signed 16-bit little
//! endian. One message carries exactly one negotiated audio frame. Decode borrows
//! the payload and allocates nothing. Authentication and socket ownership remain
//! the host's responsibility before accepting a media message.

use crate::audio::{AudioFormat, MediaError};

pub const HEADER_BYTES: usize = 36;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAGIC: &[u8; 4] = b"WXCM";
const VERSION: u8 = 1;
const PCM_S16LE: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaHeader {
    pub track_id: u32,
    pub generation: u32,
    pub sequence: u64,
    /// Sender's monotonic stream time in microseconds, not wall-clock time.
    pub timestamp_micros: u64,
}

impl MediaHeader {
    fn validate(self) -> Result<(), MediaError> {
        if self.track_id == 0 || self.generation == 0 {
            return Err(MediaError::InvalidConfiguration);
        }
        Ok(())
    }
}

/// A completely checked, borrowed media message. Call `MediaStream::admit` to
/// check it against the current track generation and ordering before consuming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaFrame<'a> {
    pub header: MediaHeader,
    payload: &'a [u8],
}

impl<'a> MediaFrame<'a> {
    pub fn decode(input: &'a [u8], format: AudioFormat) -> Result<Self, MediaError> {
        if input.len() < HEADER_BYTES || input.len() > MAX_FRAME_BYTES {
            return Err(MediaError::InvalidPacket);
        }
        if &input[..4] != MAGIC
            || input[4] != VERSION
            || input[5] != PCM_S16LE
            || input[6..8] != [0, 0]
        {
            return Err(MediaError::InvalidPacket);
        }
        let u32_at = |offset| {
            u32::from_be_bytes(
                input[offset..offset + 4]
                    .try_into()
                    .expect("checked header"),
            )
        };
        let u64_at = |offset| {
            u64::from_be_bytes(
                input[offset..offset + 8]
                    .try_into()
                    .expect("checked header"),
            )
        };
        let header = MediaHeader {
            track_id: u32_at(8),
            generation: u32_at(12),
            sequence: u64_at(16),
            timestamp_micros: u64_at(24),
        };
        header.validate()?;
        let payload = &input[HEADER_BYTES..];
        if u32_at(32) as usize != payload.len() || payload.len() != format.samples_per_frame() * 2 {
            return Err(MediaError::InvalidFrame);
        }
        Ok(Self { header, payload })
    }

    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    pub fn samples(&self) -> impl ExactSizeIterator<Item = i16> + '_ {
        self.payload
            .chunks_exact(2)
            .map(|sample| i16::from_le_bytes(sample.try_into().expect("validated two-byte sample")))
    }
}

/// Encode signed PCM without exposing platform-native byte order.
pub fn encode_pcm(
    header: MediaHeader,
    format: AudioFormat,
    samples: &[i16],
) -> Result<Vec<u8>, MediaError> {
    format.validate_frame(samples)?;
    let mut output = encode_header(header, samples.len() * 2)?;
    for &sample in samples {
        output.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(output)
}

/// Encode already little-endian PCM bytes (for host adapters).
pub fn encode_payload(
    header: MediaHeader,
    format: AudioFormat,
    payload: &[u8],
) -> Result<Vec<u8>, MediaError> {
    if payload.len() != format.samples_per_frame() * 2 {
        return Err(MediaError::InvalidFrame);
    }
    let mut output = encode_header(header, payload.len())?;
    output.extend_from_slice(payload);
    Ok(output)
}

fn encode_header(header: MediaHeader, payload_length: usize) -> Result<Vec<u8>, MediaError> {
    header.validate()?;
    let length = HEADER_BYTES
        .checked_add(payload_length)
        .filter(|&length| length <= MAX_FRAME_BYTES)
        .ok_or(MediaError::InvalidFrame)?;
    let mut output = Vec::with_capacity(length);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&[VERSION, PCM_S16LE, 0, 0]);
    output.extend_from_slice(&header.track_id.to_be_bytes());
    output.extend_from_slice(&header.generation.to_be_bytes());
    output.extend_from_slice(&header.sequence.to_be_bytes());
    output.extend_from_slice(&header.timestamp_micros.to_be_bytes());
    output.extend_from_slice(&(payload_length as u32).to_be_bytes());
    Ok(output)
}

/// Per-track stream guard. Create one only after the owner has attached to an
/// announced track. Update its generation only following authenticated control.
/// This does not replace authentication, call ownership or packet replay checks
/// on the underlying WeChat transport.
pub struct MediaStream {
    track_id: u32,
    generation: u32,
    last: Option<(u64, u64)>,
}

impl MediaStream {
    pub fn new(track_id: u32, generation: u32) -> Result<Self, MediaError> {
        MediaHeader {
            track_id,
            generation,
            sequence: 0,
            timestamp_micros: 0,
        }
        .validate()?;
        Ok(Self {
            track_id,
            generation,
            last: None,
        })
    }

    pub fn admit(&mut self, frame: &MediaFrame<'_>) -> Result<(), MediaError> {
        if frame.header.track_id != self.track_id {
            return Err(MediaError::WrongTrack);
        }
        if frame.header.generation != self.generation {
            return Err(MediaError::StaleGeneration);
        }
        if self.last.is_some_and(|(sequence, timestamp)| {
            frame.header.sequence <= sequence || frame.header.timestamp_micros <= timestamp
        }) {
            return Err(MediaError::OutOfOrder);
        }
        self.last = Some((frame.header.sequence, frame.header.timestamp_micros));
        Ok(())
    }

    pub fn advance_generation(&mut self, generation: u32) -> Result<(), MediaError> {
        if generation <= self.generation {
            return Err(MediaError::StaleGeneration);
        }
        self.generation = generation;
        self.last = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format() -> AudioFormat {
        AudioFormat::new(8000, 1, 10).unwrap()
    }
    fn header() -> MediaHeader {
        MediaHeader {
            track_id: 0x01020304,
            generation: 2,
            sequence: 3,
            timestamp_micros: 10_000,
        }
    }

    #[test]
    fn literal_header_and_little_endian_samples_are_not_native_layout() {
        let mut pcm = vec![0; 80];
        pcm[0] = -32768;
        pcm[1] = 32767;
        pcm[2] = -1;
        let encoded = encode_pcm(header(), format(), &pcm).unwrap();
        // Independently authored header: magic/version/type/reserved, track,
        // generation, sequence, microseconds, 160-byte payload length.
        let expected_header = [
            0x57, 0x58, 0x43, 0x4d, 1, 1, 0, 0, 1, 2, 3, 4, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0,
            0, 0, 0, 0, 0, 0x27, 0x10, 0, 0, 0, 0xa0,
        ];
        assert_eq!(&encoded[..HEADER_BYTES], &expected_header);
        assert_eq!(
            &encoded[HEADER_BYTES..HEADER_BYTES + 6],
            &[0, 0x80, 0xff, 0x7f, 0xff, 0xff]
        );
        let decoded = MediaFrame::decode(&encoded, format()).unwrap();
        assert_eq!(decoded.header, header());
        assert_eq!(decoded.payload.as_ptr(), encoded[HEADER_BYTES..].as_ptr());
        assert_eq!(decoded.samples().collect::<Vec<_>>(), pcm);
        assert_eq!(
            encode_payload(header(), format(), decoded.payload).unwrap(),
            encoded
        );
    }

    #[test]
    fn rejects_truncation_trailing_bytes_bad_version_reserved_and_lengths() {
        let original = encode_pcm(header(), format(), &[0; 80]).unwrap();
        for length in 0..original.len() {
            assert!(MediaFrame::decode(&original[..length], format()).is_err());
        }
        let mut trailing = original.clone();
        trailing.push(0);
        assert!(MediaFrame::decode(&trailing, format()).is_err());
        for index in [0, 4, 5, 6, 7, 35] {
            let mut corrupt = original.clone();
            corrupt[index] ^= 0xff;
            assert!(
                MediaFrame::decode(&corrupt, format()).is_err(),
                "index {index}"
            );
        }
        assert!(MediaFrame::decode(&vec![0; MAX_FRAME_BYTES + 1], format()).is_err());
        assert!(encode_payload(header(), format(), &[0; 159]).is_err());
        let stereo = AudioFormat::new(8000, 2, 10).unwrap();
        assert!(MediaFrame::decode(&original, stereo).is_err());
    }

    #[test]
    fn track_generation_and_order_fail_before_consumption() {
        let bytes = encode_pcm(header(), format(), &[0; 80]).unwrap();
        let frame = MediaFrame::decode(&bytes, format()).unwrap();
        let mut stream = MediaStream::new(header().track_id, 2).unwrap();
        let mut wrong = frame;
        wrong.header.track_id += 1;
        assert_eq!(stream.admit(&wrong), Err(MediaError::WrongTrack));
        wrong = frame;
        wrong.header.generation = 1;
        assert_eq!(stream.admit(&wrong), Err(MediaError::StaleGeneration));
        stream.admit(&frame).unwrap();
        assert_eq!(stream.admit(&frame), Err(MediaError::OutOfOrder));
        wrong = frame;
        wrong.header.sequence += 1;
        assert_eq!(stream.admit(&wrong), Err(MediaError::OutOfOrder));
        assert_eq!(
            stream.advance_generation(2),
            Err(MediaError::StaleGeneration)
        );
        stream.advance_generation(3).unwrap();
        wrong = frame;
        wrong.header.generation = 3;
        wrong.header.sequence = 0;
        wrong.header.timestamp_micros = 0;
        stream.admit(&wrong).unwrap();
        assert_eq!(stream.admit(&frame), Err(MediaError::StaleGeneration));
    }

    #[test]
    fn zero_track_or_generation_is_not_a_live_stream() {
        assert!(MediaStream::new(0, 1).is_err());
        assert!(MediaStream::new(1, 0).is_err());
        let mut h = header();
        h.generation = 0;
        assert!(encode_pcm(h, format(), &[0; 80]).is_err());
    }
}
