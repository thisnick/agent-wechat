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
pub const MAX_SAMPLES: usize = 60 * SAMPLE_RATE as usize;
pub fn encode(pcm: &[i16]) -> Result<Vec<u8>> {
    ensure!(
        !pcm.is_empty() && pcm.len() <= 50 * SAMPLE_RATE as usize,
        "voice_duration_limit"
    );
    let mut out = vec![0u8; 200 * 1024];
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
            && input.len() <= 200 * 1024
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
        ensure!(packets <= 3000, "voice_duration_limit");
    }
    ensure!(packets > 0, "empty_silk_stream");
    Ok(packets * 20)
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
