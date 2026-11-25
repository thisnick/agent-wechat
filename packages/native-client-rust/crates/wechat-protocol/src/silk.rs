//! Bounded independent SILK SDK wrapper. Public source/license pins are under
//! vendor/silk; no WeChat codec library is linked or loaded.
//!
//! Framing constants, [`is_silk`], [`validate_framing`], and [`wav`] are always
//! available. PCM encoding and decoding (`encode` and `decode`) require the
//! default-enabled `voice-silk` Cargo feature.
use anyhow::{ensure, Result};

#[cfg(feature = "voice-silk")]
unsafe extern "C" {
    fn aw_silk_encode(
        pcm: *const i16,
        samples: i32,
        out: *mut u8,
        capacity: i32,
        length: *mut i32,
    ) -> i32;
    fn aw_silk_decode(
        input: *const u8,
        length: i32,
        pcm: *mut i16,
        capacity: i32,
        samples: *mut i32,
    ) -> i32;
}
pub const SAMPLE_RATE: u32 = 16000;
/// Target bit rate of the single bundled encoder profile (`silk_bridge.c`).
/// SILK's wideband range tops out here; the previous 16 kbit/s profile was
/// audibly destructive for anything richer than speech, and the rate control is
/// soft, so the actual stream lands well under this ceiling.
pub const BIT_RATE: u32 = 40000;
/// Duration of one encoded SILK packet in the bundled profile.
pub const FRAME_MS: u32 = 20;
/// Longest single voice note: the official 60-second voice-message limit.
pub const MAX_NOTE_MS: u32 = 60_000;
pub const MAX_SAMPLES: usize = (MAX_NOTE_MS / 1000 * SAMPLE_RATE) as usize;
/// Largest encoded stream buffer produced by [`encode`] for outgoing notes.
pub const MAX_STREAM_BYTES: usize = 512 * 1024;
/// Longest incoming voice note accepted by SILK decoding (5 minutes, exceeding
/// mobile clients' 70-second recording ceiling while staying bounded).
const MAX_DECODE_MS: u32 = 300_000;
const MAX_DECODE_SAMPLES: usize = (MAX_DECODE_MS / 1000 * SAMPLE_RATE) as usize;
const MAX_DECODE_STREAM_BYTES: usize = 4 * 1024 * 1024;
/// Encoded-size bound for one maximum-length note from the bundled profile:
/// the 10-byte header plus, per 20 ms packet, a 2-byte length prefix and the
/// 40 kbit/s payload budget (100 bytes) with 1.5x headroom. SILK rate control
/// is soft, so single packets may exceed the budget; the observed 60-second
/// worst case (full-scale white noise) averages about 2.2 KB/s, or 131 KB.
pub const MAX_NOTE_BYTES: usize = 10
    + (MAX_NOTE_MS / FRAME_MS) as usize * (2 + (BIT_RATE / 8 * FRAME_MS / 1000) as usize * 3 / 2);
const _: () = assert!(MAX_NOTE_BYTES <= MAX_STREAM_BYTES);

fn header_offset(input: &[u8]) -> Option<usize> {
    if input.len() >= 10 && input[1..10] == *b"#!SILK_V3" {
        Some(10)
    } else if input.len() >= 9 && input[0..9] == *b"#!SILK_V3" {
        Some(9)
    } else {
        None
    }
}

/// Returns true when `input` begins with a WeChat SILK v3 header (either with
/// its leading byte prefix or bare `#!SILK_V3`).
pub fn is_silk(input: &[u8]) -> bool {
    header_offset(input).is_some()
}

/// Encode mono 16 kHz signed 16-bit PCM into a framed `#!SILK_V3` voice note
/// using the bundled SILK C encoder. Requires the `voice-silk` feature.
#[cfg(feature = "voice-silk")]
pub fn encode(pcm: &[i16]) -> Result<Vec<u8>> {
    ensure!(
        !pcm.is_empty() && pcm.len() <= MAX_SAMPLES,
        "voice_duration_limit"
    );
    let mut out = vec![0u8; MAX_STREAM_BYTES];
    let mut length = 0i32;
    // SAFETY: C receives immutable aligned samples and an explicitly sized output;
    // the wrapper validates frame/output bounds before every copy.
    let status = unsafe {
        aw_silk_encode(
            pcm.as_ptr(),
            pcm.len() as i32,
            out.as_mut_ptr(),
            out.len() as i32,
            &mut length,
        )
    };
    ensure!(
        status == 0 && length >= 12 && length as usize <= out.len(),
        "silk_encode_failed"
    );
    out.truncate(length as usize);
    validate_framing(&out)?;
    Ok(out)
}

/// Validate a WeChat-prefixed `#!SILK_V3` stream's header, packet lengths and
/// duration bounds, returning the total duration in milliseconds.
pub fn validate_framing(input: &[u8]) -> Result<u32> {
    let Some(mut p) = header_offset(input) else {
        anyhow::bail!("invalid_silk_stream");
    };
    ensure!(
        input.len() >= p + 2 && input.len() <= MAX_DECODE_STREAM_BYTES,
        "invalid_silk_stream"
    );
    let mut packets = 0u32;
    while p < input.len() {
        ensure!(p + 2 <= input.len(), "invalid_silk_packet");
        let n = i16::from_le_bytes(input[p..p + 2].try_into()?) as i32;
        p += 2;
        // Negative signed lengths (such as 0xFFFF = -1) mark end-of-stream.
        if n < 0 {
            break;
        }
        // Length 0 (0x0000) is a valid DTX silence / comfort-noise frame.
        ensure!(
            n <= 5120 && n as usize <= input.len() - p,
            "invalid_silk_packet"
        );
        p += n as usize;
        packets += 1;
        ensure!(packets <= MAX_DECODE_MS / FRAME_MS, "voice_duration_limit");
    }
    ensure!(packets > 0, "empty_silk_stream");
    Ok(packets * FRAME_MS)
}

/// Decode a validated `#!SILK_V3` voice note into mono 16 kHz signed 16-bit PCM
/// using the bundled SILK C decoder. Requires the `voice-silk` feature.
#[cfg(feature = "voice-silk")]
pub fn decode(input: &[u8]) -> Result<Vec<i16>> {
    let duration_ms = validate_framing(input)?;
    let packets = (duration_ms / FRAME_MS) as usize;
    // Each SILK packet carries up to 5 internal 20 ms frames (1600 samples at 16 kHz).
    let capacity = packets.saturating_mul(1600).clamp(480, MAX_DECODE_SAMPLES);
    let mut pcm = vec![0i16; capacity];
    let mut samples = 0i32;
    // SAFETY: Framing and total sizes were checked above. The C bridge bounds each
    // decoder frame and copy to the supplied PCM capacity.
    let status = unsafe {
        aw_silk_decode(
            input.as_ptr(),
            input.len() as i32,
            pcm.as_mut_ptr(),
            pcm.len() as i32,
            &mut samples,
        )
    };
    ensure!(
        status == 0 && samples > 0 && samples as usize <= pcm.len(),
        "silk_decode_failed"
    );
    pcm.truncate(samples as usize);
    Ok(pcm)
}

/// Wrap mono 16 kHz signed 16-bit PCM samples in an uncompressed RIFF/WAVE
/// container.
pub fn wav(pcm: &[i16]) -> Result<Vec<u8>> {
    ensure!(pcm.len() <= MAX_DECODE_SAMPLES, "voice_duration_limit");
    let n = u32::try_from(pcm.len() * 2)?;
    let mut out = Vec::with_capacity(44 + n as usize);
    out.extend(b"RIFF");
    out.extend((36 + n).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(SAMPLE_RATE.to_le_bytes());
    out.extend((SAMPLE_RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(n.to_le_bytes());
    for s in pcm {
        out.extend(s.to_le_bytes());
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_validation_and_wav_work_without_c_codec() {
        let mut valid = vec![2];
        valid.extend_from_slice(b"#!SILK_V3");
        valid.extend_from_slice(&4i16.to_le_bytes());
        valid.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(validate_framing(&valid).unwrap(), FRAME_MS);

        valid.extend_from_slice(&2i16.to_le_bytes());
        valid.extend_from_slice(&[5, 6]);
        assert_eq!(validate_framing(&valid).unwrap(), FRAME_MS * 2);

        let mut too_many_packets = vec![2];
        too_many_packets.extend_from_slice(b"#!SILK_V3");
        for _ in 0..=(MAX_DECODE_MS / FRAME_MS) {
            too_many_packets.extend_from_slice(&1i16.to_le_bytes());
            too_many_packets.push(0);
        }
        assert!(validate_framing(&too_many_packets).is_err());

        // Headerless #!SILK_V3, alternate leading byte, DTX 0x0000, and 0xFFFF EOF are valid.
        assert!(is_silk(&[b"#!SILK_V3".as_slice(), &[1, 0, 0]].concat()));
        assert_eq!(
            validate_framing(&[vec![4], b"#!SILK_V3".to_vec(), vec![1, 0, 0]].concat()).unwrap(),
            FRAME_MS
        );
        assert_eq!(
            validate_framing(&[vec![2], b"#!SILK_V3".to_vec(), vec![0, 0]].concat()).unwrap(),
            FRAME_MS
        );
        assert_eq!(
            validate_framing(&[vec![2], b"#!SILK_V3".to_vec(), vec![1, 0, 0, 255, 255]].concat())
                .unwrap(),
            FRAME_MS
        );

        for bad in [
            vec![],
            b"not-silk".to_vec(),
            [vec![2], b"#!SILK_V3".to_vec()].concat(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![255, 255]].concat(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![5, 0, 1, 2]].concat(),
            [
                vec![2],
                b"#!SILK_V3".to_vec(),
                5121i16.to_le_bytes().to_vec(),
            ]
            .concat(),
        ] {
            assert!(validate_framing(&bad).is_err());
        }

        let wav_bytes = wav(&[0, 1000, -1000, i16::MAX]).unwrap();
        assert_eq!(&wav_bytes[..4], b"RIFF");
        assert_eq!(&wav_bytes[8..12], b"WAVE");
        assert_eq!(wav_bytes.len(), 44 + 8);
        assert!(wav(&vec![0i16; MAX_DECODE_SAMPLES + 1]).is_err());
    }

    #[cfg(feature = "voice-silk")]
    #[test]
    fn independent_fresh_tones_encode_decode() {
        let pcm = (0..16000)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 10000.0) as i16)
            .collect::<Vec<_>>();
        let encoded = encode(&pcm).unwrap();
        assert_eq!(validate_framing(&encoded).unwrap(), 1000);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.len(), pcm.len());
        assert!(decoded.iter().any(|s| s.abs() > 1000));
        assert_eq!(&wav(&decoded).unwrap()[..4], b"RIFF");
    }

    #[cfg(feature = "voice-silk")]
    #[test]
    fn maximum_note_stays_within_derived_byte_bound() {
        assert_eq!(MAX_SAMPLES, 960_000);
        assert_eq!(MAX_NOTE_BYTES, 456_010);
        // Full-scale white noise is the least compressible input; a harmonic
        // signal with a moving pitch exercises the voiced path.
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let noise = (0..MAX_SAMPLES)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 48) as u16 as i16
            })
            .collect::<Vec<_>>();
        let harmonic = (0..MAX_SAMPLES)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE as f64;
                let f0 = 120.0 + 40.0 * (t * 3.0).sin();
                let v = (1..30)
                    .map(|h| (t * f0 * h as f64 * std::f64::consts::TAU).sin() / h as f64)
                    .sum::<f64>();
                (v * (t * 4.0).sin().abs() * 8000.0) as i16
            })
            .collect::<Vec<_>>();
        for pcm in [noise, harmonic] {
            let encoded = encode(&pcm).unwrap();
            assert!(encoded.len() <= MAX_NOTE_BYTES, "{}", encoded.len());
            assert_eq!(validate_framing(&encoded).unwrap(), MAX_NOTE_MS);
        }
        let too_long = vec![0i16; MAX_SAMPLES + 1];
        assert!(encode(&too_long).is_err());
    }

    #[cfg(feature = "voice-silk")]
    #[test]
    fn malformed_packet_refused_before_ffi() {
        for input in [
            vec![],
            b"not-silk-at-all".to_vec(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![255, 255]].concat(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![10, 0, 1, 2]].concat(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![1]].concat(),
        ] {
            assert!(decode(&input).is_err(), "expected error for {input:?}");
        }
    }

    #[cfg(feature = "voice-silk")]
    #[test]
    fn dtx_eof_headerless_and_over_sixty_second_streams_decode() {
        let tone = (0..3200)
            .map(|i| ((i as f64 * 440.0 * std::f64::consts::TAU / 16000.0).sin() * 8000.0) as i16)
            .collect::<Vec<_>>();
        let encoded = encode(&tone).unwrap();

        // Interleave DTX silence frames (0x0000) and append a -1 (0xFFFF) EOF trailer.
        let mut with_dtx_and_eof = encoded[..10].to_vec();
        with_dtx_and_eof.extend([0u8, 0]);
        with_dtx_and_eof.extend(&encoded[10..]);
        with_dtx_and_eof.extend([0u8, 0, 0, 0]);
        with_dtx_and_eof.extend([0xffu8, 0xff]);
        assert_eq!(validate_framing(&with_dtx_and_eof).unwrap(), 260);
        let decoded = decode(&with_dtx_and_eof).unwrap();
        assert_eq!(decoded.len(), 260 * 16);
        assert!(decoded.iter().any(|s| s.abs() > 1000));

        // Leading byte outside 0..=3 is tolerated when #!SILK_V3 is at offset 1.
        let mut alt_prefix = encoded.clone();
        alt_prefix[0] = 7;
        assert_eq!(decode(&alt_prefix).unwrap().len(), tone.len());

        // Headerless #!SILK_V3 at offset 0 decodes cleanly.
        let headerless = encoded[1..].to_vec();
        assert!(is_silk(&headerless));
        assert_eq!(decode(&headerless).unwrap().len(), tone.len());

        // Streams longer than 60 seconds (e.g., 65 seconds = 3250 frames) decode to PCM and WAV.
        let mut long_stream = encoded[..10].to_vec();
        for _ in 0..325 {
            long_stream.extend(&encoded[10..]);
        }
        assert_eq!(validate_framing(&long_stream).unwrap(), 65_000);
        let long_pcm = decode(&long_stream).unwrap();
        assert_eq!(long_pcm.len(), 65 * 16000);
        assert_eq!(&wav(&long_pcm).unwrap()[..4], b"RIFF");
    }
}
