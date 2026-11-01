//! Bounded independent SILK SDK wrapper. Public source/license pins are under
//! vendor/silk; no WeChat codec library is linked or loaded.
use anyhow::{ensure, Result};
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
/// Largest stream accepted by framing validation and decoding.
pub const MAX_STREAM_BYTES: usize = 512 * 1024;
/// Encoded-size bound for one maximum-length note from the bundled profile:
/// the 10-byte header plus, per 20 ms packet, a 2-byte length prefix and the
/// 40 kbit/s payload budget (100 bytes) with 1.5x headroom. SILK rate control
/// is soft, so single packets may exceed the budget; the observed 60-second
/// worst case (full-scale white noise) averages about 2.2 KB/s, or 131 KB.
pub const MAX_NOTE_BYTES: usize = 10
    + (MAX_NOTE_MS / FRAME_MS) as usize * (2 + (BIT_RATE / 8 * FRAME_MS / 1000) as usize * 3 / 2);
const _: () = assert!(MAX_NOTE_BYTES <= MAX_STREAM_BYTES);
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
pub fn validate_framing(input: &[u8]) -> Result<u32> {
    ensure!(
        input.len() >= 12
            && input.len() <= MAX_STREAM_BYTES
            && input[0] <= 3
            && input[1..10] == *b"#!SILK_V3",
        "invalid_silk_stream"
    );
    let mut p = 10;
    let mut packets = 0u32;
    while p < input.len() {
        ensure!(p + 2 <= input.len(), "invalid_silk_packet");
        let n = i16::from_le_bytes(input[p..p + 2].try_into()?) as i32;
        p += 2;
        ensure!(
            n > 0 && n <= 5120 && n as usize <= input.len() - p,
            "invalid_silk_packet"
        );
        p += n as usize;
        packets += 1;
        ensure!(packets <= MAX_NOTE_MS / FRAME_MS, "voice_duration_limit");
    }
    ensure!(packets > 0, "empty_silk_stream");
    Ok(packets * FRAME_MS)
}
pub fn decode(input: &[u8]) -> Result<Vec<i16>> {
    validate_framing(input)?;
    let mut pcm = vec![0i16; MAX_SAMPLES];
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
pub fn wav(pcm: &[i16]) -> Result<Vec<u8>> {
    ensure!(pcm.len() <= MAX_SAMPLES, "voice_duration_limit");
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
    #[test]
    fn malformed_packet_refused_before_ffi() {
        for input in [
            vec![],
            [vec![2], b"#!SILK_V3".to_vec(), vec![0, 0]].concat(),
            [vec![2], b"#!SILK_V3".to_vec(), vec![255, 255]].concat(),
        ] {
            assert!(decode(&input).is_err());
        }
    }
}
