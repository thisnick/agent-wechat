//! Binary `WXCM` v3 media envelopes shared between the daemon runtime and the
//! v3 `/media` WebSocket. These headers are separate from WeChat wire packets.
//!
//! All header integers are big endian; PCM samples are signed 16-bit little
//! endian; video frames carry raw Annex-B H.265/H.264 access units or complete
//! JPEG bytes. Decode borrows the payload and allocates nothing. Authentication
//! and socket ownership remain the daemon's responsibility before accepting any
//! media message.

use crate::call_types::CallVideoCodec;
use wechat_calls::audio::{AudioFormat, MediaError};

/// Fixed 36-byte big-endian `WXCM` v3 header length.
pub const HEADER_BYTES: usize = 36;
/// Maximum PCM payload bytes carried in one audio binary message.
pub const MAX_AUDIO_PAYLOAD_BYTES: usize = 64 * 1024;
/// Maximum video payload bytes carried in one video binary message.
pub const MAX_VIDEO_PAYLOAD_BYTES: usize = 512 * 1024;
/// Maximum total binary frame bytes (`HEADER_BYTES + MAX_VIDEO_PAYLOAD_BYTES`).
pub const MAX_FRAME_BYTES: usize = HEADER_BYTES + MAX_VIDEO_PAYLOAD_BYTES;
/// `WXCM` binary framing version (matches `CALL_PROTOCOL_VERSION`).
pub const VERSION: u8 = 3;
/// Video frame flag bit 0: frame is a keyframe (IDR / standalone JPEG).
pub const FLAG_KEYFRAME: u16 = 0x0001;
/// Video frame flag bit 1: Annex-B frame begins with inline parameter sets (`VPS`/`SPS`/`PPS`).
pub const FLAG_CODEC_CONFIG: u16 = 0x0002;
/// Video frame flag bits 2..3: rotate 90 degrees clockwise for display.
pub const FLAG_ROTATE_90: u16 = 0x0004;
/// Video frame flag bits 2..3: rotate 180 degrees clockwise for display.
pub const FLAG_ROTATE_180: u16 = 0x0008;
/// Video frame flag bits 2..3: rotate 270 degrees clockwise (90 degrees counter-clockwise) for display.
pub const FLAG_ROTATE_270: u16 = 0x000c;
/// Bitmask for the 2-bit clockwise display rotation field in video `WXCM` flags.
pub const FLAG_ROTATION_MASK: u16 = 0x000c;
const KNOWN_VIDEO_FLAGS: u16 = FLAG_KEYFRAME | FLAG_CODEC_CONFIG | FLAG_ROTATION_MASK;
const MAGIC: &[u8; 4] = b"WXCM";

/// Decode clockwise display rotation in degrees (`0`, `90`, `180`, or `270`) from `WXCM` video flags.
pub const fn video_rotation_degrees(flags: u16) -> u16 {
    match flags & FLAG_ROTATION_MASK {
        FLAG_ROTATE_90 => 90,
        FLAG_ROTATE_180 => 180,
        FLAG_ROTATE_270 => 270,
        _ => 0,
    }
}

/// Payload kind byte carried at offset 5 of the `WXCM` v3 header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MediaPayloadKind {
    /// Signed 16-bit little-endian PCM audio.
    PcmS16le = 1,
    /// Annex-B H.265 / HEVC video access unit.
    H265AnnexB = 2,
    /// Annex-B H.264 / AVC video access unit.
    H264AnnexB = 3,
    /// Complete standalone JPEG frame.
    Jpeg = 4,
}

impl MediaPayloadKind {
    /// Decode a wire payload kind byte.
    pub const fn from_byte(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::PcmS16le),
            2 => Some(Self::H265AnnexB),
            3 => Some(Self::H264AnnexB),
            4 => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// Return true when this payload kind carries video.
    pub const fn is_video(self) -> bool {
        !matches!(self, Self::PcmS16le)
    }

    /// Map a negotiated [`CallVideoCodec`] to its `WXCM` v3 payload kind.
    pub const fn from_video_codec(codec: CallVideoCodec) -> Self {
        match codec {
            CallVideoCodec::H265 => Self::H265AnnexB,
            CallVideoCodec::H264 => Self::H264AnnexB,
            CallVideoCodec::Jpeg => Self::Jpeg,
        }
    }

    /// Map a video payload kind back to [`CallVideoCodec`].
    pub const fn to_video_codec(self) -> Option<CallVideoCodec> {
        match self {
            Self::PcmS16le => None,
            Self::H265AnnexB => Some(CallVideoCodec::H265),
            Self::H264AnnexB => Some(CallVideoCodec::H264),
            Self::Jpeg => Some(CallVideoCodec::Jpeg),
        }
    }
}

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

/// Decoded zero-copy `WXCM` v3 binary WebSocket frame (audio or video).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireMediaFrame<'a> {
    pub kind: MediaPayloadKind,
    pub flags: u16,
    pub header: MediaHeader,
    payload: &'a [u8],
}

impl<'a> WireMediaFrame<'a> {
    /// Parse and validate any `WXCM` v3 binary frame without copying its payload.
    pub fn decode(input: &'a [u8]) -> Result<Self, MediaError> {
        if input.len() < HEADER_BYTES || input.len() > MAX_FRAME_BYTES {
            return Err(MediaError::InvalidPacket);
        }
        if &input[..4] != MAGIC || input[4] != VERSION {
            return Err(MediaError::InvalidPacket);
        }
        let kind = MediaPayloadKind::from_byte(input[5]).ok_or(MediaError::InvalidPacket)?;
        let flags = u16::from_be_bytes([input[6], input[7]]);
        match kind {
            MediaPayloadKind::PcmS16le => {
                if flags != 0 {
                    return Err(MediaError::InvalidPacket);
                }
            }
            _ => {
                if flags & !KNOWN_VIDEO_FLAGS != 0 {
                    return Err(MediaError::InvalidPacket);
                }
            }
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
        let max_payload = if kind.is_video() {
            MAX_VIDEO_PAYLOAD_BYTES
        } else {
            MAX_AUDIO_PAYLOAD_BYTES
        };
        if payload.is_empty() || payload.len() > max_payload || u32_at(32) as usize != payload.len()
        {
            return Err(MediaError::InvalidFrame);
        }
        Ok(Self {
            kind,
            flags,
            header,
            payload,
        })
    }

    pub fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Clockwise display rotation in degrees (`0`, `90`, `180`, or `270`) carried in `flags`.
    pub const fn rotation_degrees(&self) -> u16 {
        video_rotation_degrees(self.flags)
    }
}

/// A completely checked, borrowed PCM audio media message. Call `MediaStream::admit`
/// to check it against the current track generation and ordering before consuming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MediaFrame<'a> {
    pub header: MediaHeader,
    payload: &'a [u8],
}

impl<'a> MediaFrame<'a> {
    pub fn decode(input: &'a [u8], format: AudioFormat) -> Result<Self, MediaError> {
        let wire = WireMediaFrame::decode(input)?;
        if wire.kind != MediaPayloadKind::PcmS16le || wire.flags != 0 {
            return Err(MediaError::InvalidPacket);
        }
        if wire.payload.len() != format.samples_per_frame() * 2 {
            return Err(MediaError::InvalidFrame);
        }
        Ok(Self {
            header: wire.header,
            payload: wire.payload,
        })
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
    let mut output = encode_header(MediaPayloadKind::PcmS16le, 0, header, samples.len() * 2)?;
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
    let mut output = encode_header(MediaPayloadKind::PcmS16le, 0, header, payload.len())?;
    output.extend_from_slice(payload);
    Ok(output)
}

/// Encode variable-size client PCM bytes into a `WXCM` v3 binary audio frame.
pub fn encode_pcm_chunk(header: MediaHeader, payload: &[u8]) -> Result<Vec<u8>, MediaError> {
    if payload.is_empty()
        || payload.len() > MAX_AUDIO_PAYLOAD_BYTES
        || !payload.len().is_multiple_of(2)
    {
        return Err(MediaError::InvalidFrame);
    }
    let mut output = encode_header(MediaPayloadKind::PcmS16le, 0, header, payload.len())?;
    output.extend_from_slice(payload);
    Ok(output)
}

/// Encode one video frame (Annex-B H.265/H.264 or JPEG) into a `WXCM` v3 binary frame.
pub fn encode_video(
    kind: MediaPayloadKind,
    flags: u16,
    header: MediaHeader,
    payload: &[u8],
) -> Result<Vec<u8>, MediaError> {
    if !kind.is_video()
        || flags & !KNOWN_VIDEO_FLAGS != 0
        || payload.is_empty()
        || payload.len() > MAX_VIDEO_PAYLOAD_BYTES
    {
        return Err(MediaError::InvalidFrame);
    }
    let mut output = encode_header(kind, flags, header, payload.len())?;
    output.extend_from_slice(payload);
    Ok(output)
}

fn encode_header(
    kind: MediaPayloadKind,
    flags: u16,
    header: MediaHeader,
    payload_length: usize,
) -> Result<Vec<u8>, MediaError> {
    header.validate()?;
    let max_payload = if kind.is_video() {
        MAX_VIDEO_PAYLOAD_BYTES
    } else {
        MAX_AUDIO_PAYLOAD_BYTES
    };
    if payload_length == 0 || payload_length > max_payload {
        return Err(MediaError::InvalidFrame);
    }
    let length = HEADER_BYTES
        .checked_add(payload_length)
        .filter(|&length| length <= MAX_FRAME_BYTES)
        .ok_or(MediaError::InvalidFrame)?;
    let mut output = Vec::with_capacity(length);
    output.extend_from_slice(MAGIC);
    output.push(VERSION);
    output.push(kind as u8);
    output.extend_from_slice(&flags.to_be_bytes());
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
        // Independently authored header: magic/version(3)/type(1)/flags(0), track,
        // generation, sequence, microseconds, 160-byte payload length.
        let expected_header = [
            0x57, 0x58, 0x43, 0x4d, 3, 1, 0, 0, 1, 2, 3, 4, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0,
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
    fn video_wire_frame_roundtrip_and_flag_validation() {
        let payload = [0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xaa, 0xbb];
        let encoded = encode_video(
            MediaPayloadKind::H265AnnexB,
            FLAG_KEYFRAME | FLAG_CODEC_CONFIG | FLAG_ROTATE_90,
            header(),
            &payload,
        )
        .unwrap();
        let decoded = WireMediaFrame::decode(&encoded).unwrap();
        assert_eq!(decoded.kind, MediaPayloadKind::H265AnnexB);
        assert_eq!(
            decoded.flags,
            FLAG_KEYFRAME | FLAG_CODEC_CONFIG | FLAG_ROTATE_90
        );
        assert_eq!(decoded.rotation_degrees(), 90);
        assert_eq!(decoded.header, header());
        assert_eq!(decoded.payload(), &payload);

        for (flag, deg) in [
            (0, 0),
            (FLAG_ROTATE_90, 90),
            (FLAG_ROTATE_180, 180),
            (FLAG_ROTATE_270, 270),
        ] {
            let f = encode_video(MediaPayloadKind::Jpeg, flag, header(), &payload).unwrap();
            assert_eq!(WireMediaFrame::decode(&f).unwrap().rotation_degrees(), deg);
        }

        // Unknown flag bits are rejected.
        assert!(encode_video(MediaPayloadKind::H265AnnexB, 0x0010, header(), &payload).is_err());
        let mut bad = encoded.clone();
        bad[7] |= 0x10;
        assert!(WireMediaFrame::decode(&bad).is_err());
        // Video frame cannot decode as PCM audio.
        assert!(MediaFrame::decode(&encoded, format()).is_err());
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
}
