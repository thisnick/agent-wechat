//! Prefix codec for WeChat Channels videos.
//!
//! A Channels video is an ordinary MP4 whose first [`CHANNELS_PREFIX_BYTES`]
//! bytes are XORed with an ISAAC64 keystream. The keystream is seeded from the
//! decimal `decode_key` the authenticated detail reply returns. Bytes after the
//! prefix are stored unchanged.
//!
//! Generate the stream once per key with [`channels_prefix_keystream`], then
//! apply it with [`apply_channels_prefix`] at each chunk's absolute file offset.
//! Never restart the generator per chunk.

/// Number of leading video bytes covered by the keystream.
pub const CHANNELS_PREFIX_BYTES: usize = 128 * 1024;

const MASK_GOLDEN: u64 = 0x9e37_79b9_7f4a_7c13;
const WORDS: usize = 256;

/// The complete prefix keystream for one decode key.
pub struct ChannelsPrefixStream(Box<[u8; CHANNELS_PREFIX_BYTES]>);

impl ChannelsPrefixStream {
    /// The raw keystream bytes.
    pub fn as_bytes(&self) -> &[u8; CHANNELS_PREFIX_BYTES] {
        &self.0
    }
}

impl Drop for ChannelsPrefixStream {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for ChannelsPrefixStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelsPrefixStream(<redacted>)")
    }
}

fn mix(v: &mut [u64; 8]) {
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *v;
    a = a.wrapping_sub(e);
    f ^= h >> 9;
    h = h.wrapping_add(a);
    b = b.wrapping_sub(f);
    g ^= a << 9;
    a = a.wrapping_add(b);
    c = c.wrapping_sub(g);
    h ^= b >> 23;
    b = b.wrapping_add(c);
    d = d.wrapping_sub(h);
    a ^= c << 15;
    c = c.wrapping_add(d);
    e = e.wrapping_sub(a);
    b ^= d >> 14;
    d = d.wrapping_add(e);
    f = f.wrapping_sub(b);
    c ^= e << 20;
    e = e.wrapping_add(f);
    g = g.wrapping_sub(c);
    d ^= f >> 17;
    f = f.wrapping_add(g);
    h = h.wrapping_sub(d);
    e ^= g << 14;
    g = g.wrapping_add(h);
    *v = [a, b, c, d, e, f, g, h];
}

/// Derive the full prefix keystream for a Channels `decode_key`.
///
/// The seed occupies the first result word; the remaining 255 words start at
/// zero. Result words are consumed in descending order and emitted big-endian.
pub fn channels_prefix_keystream(seed: u64) -> ChannelsPrefixStream {
    let mut result = [0u64; WORDS];
    result[0] = seed;
    let mut memory = [0u64; WORDS];
    let mut state = [MASK_GOLDEN; 8];
    for _ in 0..4 {
        mix(&mut state);
    }
    for pass in 0..2 {
        for start in (0..WORDS).step_by(8) {
            for (j, word) in state.iter_mut().enumerate() {
                let source = if pass == 0 {
                    result[start + j]
                } else {
                    memory[start + j]
                };
                *word = word.wrapping_add(source);
            }
            mix(&mut state);
            memory[start..start + 8].copy_from_slice(&state);
        }
    }
    let (mut aa, mut bb, mut cc) = (0u64, 0u64, 0u64);
    let mut output = Box::new([0u8; CHANNELS_PREFIX_BYTES]);
    let mut written = 0;
    while written < CHANNELS_PREFIX_BYTES {
        cc = cc.wrapping_add(1);
        bb = bb.wrapping_add(cc);
        for i in 0..WORDS {
            let x = memory[i];
            let transformed = match i % 4 {
                0 => !(aa ^ (aa << 21)),
                1 => aa ^ (aa >> 5),
                2 => aa ^ (aa << 12),
                _ => aa ^ (aa >> 33),
            };
            aa = transformed.wrapping_add(memory[(i + 128) % WORDS]);
            let y = memory[((x >> 3) & 255) as usize]
                .wrapping_add(aa)
                .wrapping_add(bb);
            memory[i] = y;
            bb = memory[((y >> 11) & 255) as usize].wrapping_add(x);
            result[i] = bb;
        }
        for word in result.iter().rev() {
            if written == CHANNELS_PREFIX_BYTES {
                break;
            }
            output[written..written + 8].copy_from_slice(&word.to_be_bytes());
            written += 8;
        }
    }
    result.fill(0);
    memory.fill(0);
    ChannelsPrefixStream(output)
}

/// XOR the part of `data` that overlaps the encrypted prefix.
///
/// `offset` is the absolute file offset of `data[0]`. Bytes at or beyond
/// [`CHANNELS_PREFIX_BYTES`] are left unchanged, so the same call decrypts and
/// encrypts, and chunks may be any size.
pub fn apply_channels_prefix(data: &mut [u8], offset: u64, stream: &ChannelsPrefixStream) {
    let Ok(start) = usize::try_from(offset) else {
        return;
    };
    if start >= CHANNELS_PREFIX_BYTES {
        return;
    }
    let count = data.len().min(CHANNELS_PREFIX_BYTES - start);
    for (byte, key) in data[..count]
        .iter_mut()
        .zip(&stream.0[start..start + count])
    {
        *byte ^= key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn synthetic_vectors_match_the_reference_keystream() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/channels-prefix-vectors.json"
        ))
        .unwrap();
        assert_eq!(fixture["prefixBytes"], CHANNELS_PREFIX_BYTES as u64);
        let vectors = fixture["vectors"].as_array().unwrap();
        assert_eq!(vectors.len(), 5);
        for vector in vectors {
            let seed: u64 = vector["seed"].as_str().unwrap().parse().unwrap();
            let stream = channels_prefix_keystream(seed);
            let bytes = stream.as_bytes();
            assert_eq!(hex(&bytes[..64]), vector["first64Hex"], "seed {seed}");
            assert_eq!(
                hex(&bytes[2040..2064]),
                vector["bytes2040To2063Hex"],
                "seed {seed} batch boundary"
            );
            assert_eq!(
                hex(&Sha256::digest(&bytes[..])),
                vector["prefixSha256"],
                "seed {seed} full stream"
            );
        }
    }

    #[test]
    fn irregular_chunks_match_one_pass_and_leave_the_suffix_untouched() {
        let seed = 9_007_199_254_740_993;
        let stream = channels_prefix_keystream(seed);
        let plain: Vec<u8> = (0..CHANNELS_PREFIX_BYTES + 5000)
            .map(|i| (i * 31 % 251) as u8)
            .collect();
        let mut whole = plain.clone();
        apply_channels_prefix(&mut whole, 0, &stream);
        assert_eq!(
            &whole[CHANNELS_PREFIX_BYTES..],
            &plain[CHANNELS_PREFIX_BYTES..]
        );
        assert_ne!(&whole[..64], &plain[..64]);

        for sizes in [
            &[1usize, 7, 2048, 8191][..],
            &[CHANNELS_PREFIX_BYTES - 3, 10],
            &[65_536],
        ] {
            let mut chunked = plain.clone();
            let mut offset = 0usize;
            let mut i = 0;
            while offset < chunked.len() {
                let size = sizes[i % sizes.len()].min(chunked.len() - offset);
                apply_channels_prefix(&mut chunked[offset..offset + size], offset as u64, &stream);
                offset += size;
                i += 1;
            }
            assert_eq!(chunked, whole, "chunk sizes {sizes:?}");
        }

        let mut round_trip = whole.clone();
        apply_channels_prefix(&mut round_trip, 0, &stream);
        assert_eq!(round_trip, plain);
    }

    #[test]
    fn offsets_past_the_prefix_are_noops() {
        let stream = channels_prefix_keystream(1);
        let mut data = vec![7u8; 16];
        apply_channels_prefix(&mut data, CHANNELS_PREFIX_BYTES as u64, &stream);
        apply_channels_prefix(&mut data, u64::MAX, &stream);
        assert_eq!(data, vec![7u8; 16]);
        let mut edge = vec![0u8; 16];
        apply_channels_prefix(&mut edge, (CHANNELS_PREFIX_BYTES - 4) as u64, &stream);
        assert_eq!(&edge[..4], &stream.as_bytes()[CHANNELS_PREFIX_BYTES - 4..]);
        assert_eq!(&edge[4..], &[0u8; 12]);
        assert_eq!(format!("{stream:?}"), "ChannelsPrefixStream(<redacted>)");
    }
}
