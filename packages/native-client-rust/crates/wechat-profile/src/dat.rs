use crate::{ImageDatKey, ProfileError, Result};
use aes::Aes128;
use cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAGIC: &[u8; 6] = b"\x07\x08V2\x08\x07";
pub const MAX_DAT_PLAINTEXT_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatLayout {
    pub aes_plain_bytes: u32,
    pub xor_bytes: u32,
    pub control_byte: u8,
}
impl DatLayout {
    pub fn observed(plain_len: usize) -> Result<Self> {
        if !(1024..=MAX_DAT_PLAINTEXT_BYTES).contains(&plain_len) {
            return Err(ProfileError::InvalidDat);
        }
        Ok(Self {
            aes_plain_bytes: 1024,
            xor_bytes: (plain_len - 1024).min(1024 * 1024) as u32,
            control_byte: 1,
        })
    }
    fn cipher_bytes(self) -> Result<usize> {
        if self.aes_plain_bytes == 0
            || self.aes_plain_bytes as usize > MAX_DAT_PLAINTEXT_BYTES
            || self.xor_bytes as usize > MAX_DAT_PLAINTEXT_BYTES
            || self.control_byte != 1
        {
            return Err(ProfileError::InvalidDat);
        }
        Ok((self.aes_plain_bytes as usize / 16 + 1) * 16)
    }
}
pub fn parse_dat_v2(data: &[u8]) -> Result<DatLayout> {
    if data.len() < 15
        || data.len() > MAX_DAT_PLAINTEXT_BYTES + 31
        || data.get(..6) != Some(MAGIC.as_slice())
    {
        return Err(ProfileError::InvalidDat);
    }
    let mut h = [0u8; 4];
    h.copy_from_slice(&data[6..10]);
    let mut t = [0u8; 4];
    t.copy_from_slice(&data[10..14]);
    let layout = DatLayout {
        aes_plain_bytes: u32::from_le_bytes(h),
        xor_bytes: u32::from_le_bytes(t),
        control_byte: data[14],
    };
    let cipher = layout.cipher_bytes()?;
    if data.len() < 15 + cipher + layout.xor_bytes as usize {
        return Err(ProfileError::InvalidDat);
    }
    Ok(layout)
}
/// Independent V2 framing decode. ECB/XOR is not authenticated encryption;
/// callers must separately validate image format/dimensions before rendering.
pub fn decode_dat_v2(data: &[u8], key: &ImageDatKey) -> Result<(Vec<u8>, DatLayout)> {
    let layout = parse_dat_v2(data)?;
    let end = 15 + layout.cipher_bytes()?;
    let cipher = Aes128::new_from_slice(key.aes.expose()).map_err(|_| ProfileError::InvalidDat)?;
    let mut head = Zeroizing::new(data[15..end].to_vec());
    for b in head.chunks_exact_mut(16) {
        cipher.decrypt_block(GenericArray::from_mut_slice(b));
    }
    let pad = usize::from(*head.last().ok_or(ProfileError::InvalidDat)?);
    if !(1..=16).contains(&pad)
        || pad > head.len()
        || head[head.len() - pad..]
            .iter()
            .any(|b| usize::from(*b) != pad)
    {
        return Err(ProfileError::InvalidDat);
    }
    let unpadded = head.len() - pad;
    head.truncate(unpadded);
    if head.len() != layout.aes_plain_bytes as usize {
        return Err(ProfileError::InvalidDat);
    }
    let tail = data.len() - layout.xor_bytes as usize;
    let output_len = head.len() + tail - end + layout.xor_bytes as usize;
    if output_len > MAX_DAT_PLAINTEXT_BYTES {
        return Err(ProfileError::ResourceLimit);
    }
    let mut output = Vec::with_capacity(output_len);
    output.extend_from_slice(&head);
    output.extend_from_slice(&data[end..tail]);
    output.extend(data[tail..].iter().map(|b| b ^ key.xor));
    Ok((output, layout))
}
pub fn encode_dat_v2(
    plaintext: &[u8],
    key: &ImageDatKey,
    layout: Option<DatLayout>,
) -> Result<Vec<u8>> {
    if plaintext.len() > MAX_DAT_PLAINTEXT_BYTES {
        return Err(ProfileError::ResourceLimit);
    }
    let layout = match layout {
        Some(l) => l,
        None => DatLayout::observed(plaintext.len())?,
    };
    let cipher_bytes = layout.cipher_bytes()?;
    let h = layout.aes_plain_bytes as usize;
    let t = layout.xor_bytes as usize;
    if h.checked_add(t).is_none_or(|v| v > plaintext.len()) {
        return Err(ProfileError::InvalidDat);
    }
    let cipher = Aes128::new_from_slice(key.aes.expose()).map_err(|_| ProfileError::InvalidDat)?;
    let mut head = Zeroizing::new(plaintext[..h].to_vec());
    head.resize(cipher_bytes, (cipher_bytes - h) as u8);
    for b in head.chunks_exact_mut(16) {
        cipher.encrypt_block(GenericArray::from_mut_slice(b));
    }
    let mut output = Vec::with_capacity(15 + cipher_bytes + plaintext.len() - h);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&layout.aes_plain_bytes.to_le_bytes());
    output.extend_from_slice(&layout.xor_bytes.to_le_bytes());
    output.push(layout.control_byte);
    output.extend_from_slice(&head);
    output.extend_from_slice(&plaintext[h..plaintext.len() - t]);
    output.extend(plaintext[plaintext.len() - t..].iter().map(|b| b ^ key.xor));
    Ok(output)
}
