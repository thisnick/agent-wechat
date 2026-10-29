use anyhow::{ensure, Context, Result};

/// Check the top-level movie boxes before invoking the MOV-family demuxer.
/// Older QuickTime movies may omit `ftyp`; both layouts still need owned movie
/// metadata and media bytes. This only checks container structure; callers must
/// separately decode and validate streams before sending.
pub fn inspect_video_container(input: &[u8]) -> Result<&'static str> {
    let mut offset = 0;
    let mut boxes = 0;
    let mut movie = false;
    let mut media = false;
    let mut content_type = "video/quicktime";
    let mut file_type = false;
    while offset < input.len() {
        boxes += 1;
        ensure!(boxes <= 65_536, "invalid_video_source");
        let remaining = &input[offset..];
        ensure!(remaining.len() >= 8, "invalid_video_source");
        let size = u32::from_be_bytes(remaining[..4].try_into().unwrap());
        let (size, header) = match size {
            0 => (remaining.len(), 8),
            1 => {
                ensure!(remaining.len() >= 16, "invalid_video_source");
                let size = u64::from_be_bytes(remaining[8..16].try_into().unwrap());
                (usize::try_from(size).context("invalid_video_source")?, 16)
            }
            size => (size as usize, 8),
        };
        ensure!(
            size >= header && size <= remaining.len(),
            "invalid_video_source"
        );
        match &remaining[4..8] {
            b"ftyp" => {
                ensure!(
                    !file_type && size >= header + 8 && (size - header).is_multiple_of(4),
                    "invalid_video_source"
                );
                file_type = true;
                let brand = &remaining[header..header + 4];
                content_type = if brand == b"qt  " {
                    "video/quicktime"
                } else if brand.starts_with(b"3g2") {
                    "video/3gpp2"
                } else if brand.starts_with(b"3g") {
                    "video/3gpp"
                } else if matches!(brand, b"mjp2" | b"mj2s") {
                    "video/mj2"
                } else {
                    "video/mp4"
                };
            }
            b"moov" => movie = size > header,
            b"mdat" => media |= size > header,
            _ => {}
        }
        offset += size;
    }
    ensure!(movie && media, "invalid_video_source");
    Ok(content_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_container_checks_reject_unrelated_and_malformed_bytes() {
        fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
            let mut value = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
            value.extend_from_slice(kind);
            value.extend_from_slice(payload);
            value
        }
        let movie = [atom(b"moov", &[0; 8]), atom(b"mdat", &[1; 8])].concat();
        assert_eq!(inspect_video_container(&movie).unwrap(), "video/quicktime");
        let mut extended = 1u32.to_be_bytes().to_vec();
        extended.extend_from_slice(b"free");
        extended.extend_from_slice(&16u64.to_be_bytes());
        extended.extend_from_slice(&movie);
        assert!(inspect_video_container(&extended).is_ok());
        for malformed in [
            Vec::new(),
            b"RIFF\0\0\0\0AVI ".to_vec(),
            b"\x1a\x45\xdf\xa3webm".to_vec(),
            atom(b"ftyp", b"isom\0\0\0\0"),
            atom(b"moov", &[0; 8]),
            atom(b"mdat", &[0; 8]),
            [atom(b"ftyp", b"isom"), movie.clone()].concat(),
            movie[..movie.len() - 1].to_vec(),
            [movie.clone(), vec![0]].concat(),
            [
                1u32.to_be_bytes().as_slice(),
                b"mdat",
                &u64::MAX.to_be_bytes(),
            ]
            .concat(),
            [vec![0; 4], b"mdat".to_vec(), movie].concat(),
        ] {
            assert!(inspect_video_container(&malformed).is_err());
        }
        let excessive = atom(b"free", &[]).repeat(65_537);
        assert!(inspect_video_container(&excessive).is_err());
    }
}
