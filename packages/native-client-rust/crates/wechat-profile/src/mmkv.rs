//! Read-only identity fields from the inspected MMKV v4 layout. The format key
//! is caller-provided; no artifact-derived key or account material is shipped.
use crate::{ProfileError, Result};
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub struct ProfileIdentity {
    pub username: String,
    pub uin: u32,
    pub device_id: Vec<u8>,
}
fn var(input: &[u8], at: &mut usize) -> Result<u64> {
    let mut value = 0;
    for i in 0..10 {
        let byte = *input.get(*at).ok_or(ProfileError::InvalidMetadata)?;
        *at += 1;
        if i == 9 && byte > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        value |= ((byte & 127) as u64) << (i * 7);
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(ProfileError::InvalidMetadata)
}
fn span<'a>(input: &'a [u8], at: &mut usize) -> Result<&'a [u8]> {
    let n = usize::try_from(var(input, at)?).map_err(|_| ProfileError::ResourceLimit)?;
    let end = at.checked_add(n).ok_or(ProfileError::ResourceLimit)?;
    let value = input.get(*at..end).ok_or(ProfileError::InvalidMetadata)?;
    *at = end;
    Ok(value)
}
fn string(input: &[u8]) -> Result<Vec<u8>> {
    let mut at = 0;
    let value = span(input, &mut at)?.to_vec();
    if at != input.len() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(value)
}
/// CRC32 checks the captured ciphertext span; it is not authentication. Identity
/// values must also match the independently authenticated native account.
pub fn read_mmkv_identity(
    data: &[u8],
    metadata: &[u8],
    format_key: &[u8],
) -> Result<ProfileIdentity> {
    if format_key.len() != 16
        || data.len() < 4
        || data.len() > 4 * 1024 * 1024 + 4
        || metadata.len() < 40
        || metadata.len() > 4096
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let number = |offset| {
        u32::from_le_bytes(
            metadata[offset..offset + 4]
                .try_into()
                .expect("metadata bound"),
        )
    };
    if number(4) != 4 || metadata.len() >= 108 && number(104) != 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let actual = number(28) as usize;
    let ciphertext = data
        .get(4..4 + actual)
        .ok_or(ProfileError::InvalidMetadata)?;
    if crc32fast::hash(ciphertext) != number(0) {
        return Err(ProfileError::InvalidMetadata);
    }
    let plain = Zeroizing::new(
        openssl::symm::decrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            format_key,
            Some(&metadata[12..28]),
            ciphertext,
        )
        .map_err(|_| ProfileError::InvalidMetadata)?,
    );
    let mut at = 0;
    var(&plain, &mut at)?;
    let mut entries = BTreeMap::new();
    let mut count = 0;
    while at < plain.len() {
        let key = std::str::from_utf8(span(&plain, &mut at)?)
            .map_err(|_| ProfileError::InvalidMetadata)?;
        if key.is_empty() || key.len() > 4096 || key.chars().any(char::is_control) {
            return Err(ProfileError::InvalidMetadata);
        }
        let value = span(&plain, &mut at)?;
        if value.is_empty() {
            entries.remove(key);
        } else {
            entries.insert(key, value);
        }
        count += 1;
        if count > 65536 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    let get = |name: &str| {
        entries
            .get(name)
            .copied()
            .ok_or(ProfileError::InvalidMetadata)
    };
    let username = String::from_utf8(string(get("mmkv_key_user_name")?)?)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let mut at = 0;
    let raw = get("mmkv_key_latest_login_uin")?;
    let uin = u32::try_from(var(raw, &mut at)?).map_err(|_| ProfileError::InvalidMetadata)?;
    if at != raw.len()
        || uin == 0
        || username.is_empty()
        || username.len() > 256
        || username.chars().any(char::is_control)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let device_id = string(get("key_wechat_device_id")?)?;
    if device_id.len() < 16 || device_id.len() > 256 {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(ProfileIdentity {
        username,
        uin,
        device_id,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn append(value: u64, out: &mut Vec<u8>) {
        let mut value = value;
        while value >= 128 {
            out.push((value as u8 & 127) | 128);
            value >>= 7;
        }
        out.push(value as u8);
    }
    fn framed(value: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        append(value.len() as u64, &mut out);
        out.extend(value);
        out
    }
    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let mut plain = vec![0];
        for (key, value) in [
            ("mmkv_key_user_name", framed(b"synthetic_self")),
            ("mmkv_key_latest_login_uin", vec![7]),
            ("key_wechat_device_id", framed(b"synthetic-device-16")),
        ] {
            plain.extend(framed(key.as_bytes()));
            plain.extend(framed(&value));
        }
        let cipher = openssl::symm::encrypt(
            openssl::symm::Cipher::aes_128_cfb128(),
            &[9; 16],
            Some(&[3; 16]),
            &plain,
        )
        .unwrap();
        let mut metadata = vec![0; 112];
        metadata[..4].copy_from_slice(&crc32fast::hash(&cipher).to_le_bytes());
        metadata[4..8].copy_from_slice(&4u32.to_le_bytes());
        metadata[12..28].copy_from_slice(&[3; 16]);
        metadata[28..32].copy_from_slice(&(cipher.len() as u32).to_le_bytes());
        let mut data = vec![0; 4];
        data.extend(cipher);
        (data, metadata)
    }
    #[test]
    fn identity_requires_consistent_version_crc_key_and_complete_scalar() {
        let (mut data, mut metadata) = fixture();
        let id = read_mmkv_identity(&data, &metadata, &[9; 16]).unwrap();
        assert_eq!(id.username, "synthetic_self");
        assert_eq!(id.uin, 7);
        assert!(read_mmkv_identity(&data, &metadata, &[8; 16]).is_err());
        data[4] ^= 1;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
        data[4] ^= 1;
        metadata[104] = 1;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
        metadata[104] = 0;
        metadata[4] = 5;
        assert!(read_mmkv_identity(&data, &metadata, &[9; 16]).is_err());
    }
}
