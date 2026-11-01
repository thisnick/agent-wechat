//! Owned PCM and codec interfaces shared by native and hosted implementations.

use std::{error::Error, fmt};

/// Interleaved signed 16-bit PCM, grouped into fixed-duration frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioFormat {
    sample_rate: u32,
    channels: u8,
    frame_ms: u16,
}

impl AudioFormat {
    /// Validate a format supported by ordinary Opus frames.
    pub fn new(sample_rate: u32, channels: u8, frame_ms: u16) -> Result<Self, MediaError> {
        if !matches!(sample_rate, 8000 | 12000 | 16000 | 24000 | 48000)
            || !matches!(channels, 1 | 2)
            || !matches!(frame_ms, 10 | 20 | 40 | 60)
        {
            return Err(MediaError::InvalidFormat);
        }
        Ok(Self {
            sample_rate,
            channels,
            frame_ms,
        })
    }

    pub fn sample_rate(self) -> u32 {
        self.sample_rate
    }

    pub fn channels(self) -> u8 {
        self.channels
    }

    pub fn frame_ms(self) -> u16 {
        self.frame_ms
    }

    pub fn samples_per_channel(self) -> usize {
        (self.sample_rate as usize * self.frame_ms as usize) / 1000
    }

    pub fn samples_per_frame(self) -> usize {
        self.samples_per_channel() * self.channels as usize
    }

    pub fn validate_frame(self, samples: &[i16]) -> Result<(), MediaError> {
        if samples.len() != self.samples_per_frame() {
            return Err(MediaError::InvalidFrame);
        }
        Ok(())
    }
}

/// An owned frame. Generation fences queued output after interruption; timestamp
/// is the stream's sample-frame position per channel, not wall-clock time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioFrame {
    pub generation: u64,
    pub sequence: u64,
    pub timestamp: u64,
    pub samples: Vec<i16>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaError {
    InvalidFormat,
    InvalidFrame,
    InvalidPacket,
    InvalidConfiguration,
    StaleGeneration,
    WrongTrack,
    OutOfOrder,
    QueueFull,
    Capacity,
    Codec { code: i32 },
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => f.write_str("unsupported PCM format"),
            Self::InvalidFrame => f.write_str("PCM frame has the wrong number of samples"),
            Self::InvalidPacket => f.write_str("invalid or incompatible codec packet"),
            Self::InvalidConfiguration => f.write_str("invalid media configuration"),
            Self::WrongTrack => f.write_str("media track does not match this stream"),
            Self::StaleGeneration => f.write_str("media generation is no longer current"),
            Self::OutOfOrder => f.write_str("media sequence or timestamp is not increasing"),
            Self::QueueFull => f.write_str("media queue is full"),
            Self::Capacity => f.write_str("media counter or clock exhausted"),
            Self::Codec { code } => write!(f, "audio codec error ({code})"),
        }
    }
}

impl Error for MediaError {}

/// Encode exactly one configured PCM frame to one raw codec packet.
/// This method is called by a worker, never the audio device callback.
pub trait AudioEncoder {
    fn format(&self) -> AudioFormat;
    fn encode(&mut self, samples: &[i16]) -> Result<Vec<u8>, MediaError>;
    fn reset(&mut self) -> Result<(), MediaError>;
}

/// Decode only packets admitted by the authenticated transport. Empty packets
/// are rejected; concealment is a separate deliberate operation by the host.
pub trait AudioDecoder {
    fn format(&self) -> AudioFormat;
    fn decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, MediaError>;
    fn conceal(&mut self) -> Result<Vec<i16>, MediaError>;
    fn reset(&mut self) -> Result<(), MediaError>;
}
