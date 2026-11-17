//! Account-bound persisted sticker caches. Database/media identities are separate.
use crate::{ProfileError, ProfileLocation, Result, WriteOptions};
use md5::{Digest, Md5};
use openssl::symm::{decrypt, encrypt, Cipher};
use zeroize::Zeroizing;

fn cache_key(uin: u32, username: &str) -> Result<Zeroizing<Vec<u8>>> {
    crate::projection::identity(username)?;
    if uin == 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(Zeroizing::new(
        Md5::digest(format!("{uin}{username}EMOTICON").as_bytes()).to_vec(),
    ))
}
fn saved_path(id: &str) -> Result<String> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProfileError::InvalidMetadata);
    }
    let id = id.to_ascii_lowercase();
    Ok(format!("business/emoticon/Persist/{}/{id}", &id[..2]))
}
fn message_path(month: &str, id: &str) -> Result<String> {
    saved_path(id)?;
    if month.len() != 7
        || !month.is_ascii()
        || month.as_bytes()[4] != b'-'
        || !month[..4].bytes().all(|b| b.is_ascii_digit())
        || !matches!(month[5..].parse::<u8>(), Ok(1..=12))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(format!("cache/{month}/Emoticon/{}/{id}", &id[..2]))
}
/// Read an original GIF retained in the official local-month message cache.
/// Membership and product caches are independent of this chat-rendering copy.
pub fn read_message_sticker_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    month: &str,
    id: &str,
) -> Result<Vec<u8>> {
    let path = message_path(month, id)?;
    let encrypted = crate::media::read_cache(
        location.root(),
        std::path::Path::new(&path),
        wechat_media::MAX_IMAGE_BYTES + 16,
    )?;
    let key = cache_key(uin, username)?;
    let bytes = decrypt(Cipher::aes_128_cbc(), &key, Some(&key), &encrypted)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    if format!("{:x}", Md5::digest(&bytes)) != id
        || wechat_media::inspect_image(&bytes)
            .map_err(|_| ProfileError::InvalidMetadata)?
            .content_type
            != "image/gif"
    {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(bytes)
}
/// Publish one verified outgoing GIF in the native message asset cache. The
/// caller records the local calendar month before submission; no Saved or pack
/// membership/metadata is created. Conflicting existing bytes are preserved.
pub fn apply_message_sticker_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    month: &str,
    id: &str,
    bytes: &[u8],
    options: &WriteOptions,
) -> Result<bool> {
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    let path = message_path(month, id)?;
    if bytes.is_empty()
        || bytes.len() > wechat_media::MAX_IMAGE_BYTES
        || format!("{:x}", Md5::digest(bytes)) != id
        || wechat_media::inspect_image(bytes)
            .map_err(|_| ProfileError::InvalidMetadata)?
            .content_type
            != "image/gif"
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let key = cache_key(uin, username)?;
    let encrypted = encrypt(Cipher::aes_128_cbc(), &key, Some(&key), bytes)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    crate::media::apply_checked_cache_file(
        location,
        &path,
        &encrypted,
        wechat_media::MAX_IMAGE_BYTES + 16,
    )
}
/// Read and verify a Saved original from its official encrypted cache. WXGF
/// alternatives must match the separately supplied external representation MD5.
/// This never downloads media or writes to the profile.
pub fn read_saved_sticker_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    id: &str,
    external_md5: Option<&str>,
) -> Result<Vec<u8>> {
    let relative = saved_path(id)?;
    let encrypted = crate::media::read_cache(
        location.root(),
        std::path::Path::new(&relative),
        wechat_media::MAX_IMAGE_BYTES + 16,
    )?;
    let key = cache_key(uin, username)?;
    let bytes = decrypt(Cipher::aes_128_cbc(), &key, Some(&key), &encrypted)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let inspected =
        wechat_media::inspect_image(&bytes).map_err(|_| ProfileError::InvalidMetadata)?;
    let expected = if inspected.content_type == "image/x-wxgf" {
        external_md5.ok_or(ProfileError::InvalidMetadata)?
    } else {
        id
    };
    if !format!("{:x}", Md5::digest(&bytes)).eq_ignore_ascii_case(expected) {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(bytes)
}
/// Publish verified original bytes at the account's official persisted Saved
/// cache path. Existing different bytes are preserved and reported as a conflict.
/// No membership/order change, thumbnail synthesis or network access occurs.
pub fn apply_saved_sticker_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    id: &str,
    bytes: &[u8],
    options: &WriteOptions,
) -> Result<bool> {
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    let relative = saved_path(id)?;
    if bytes.is_empty()
        || bytes.len() > wechat_media::MAX_IMAGE_BYTES
        || !format!("{:x}", Md5::digest(bytes)).eq_ignore_ascii_case(id)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    wechat_media::inspect_image(bytes).map_err(|_| ProfileError::InvalidMetadata)?;
    let key = cache_key(uin, username)?;
    let encrypted = encrypt(Cipher::aes_128_cbc(), &key, Some(&key), bytes)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    crate::media::apply_checked_cache_file(
        location,
        &relative,
        &encrypted,
        wechat_media::MAX_IMAGE_BYTES + 16,
    )
}
/// Persisted aggregate or icon file of one official pack.
#[derive(Clone, Copy, Debug)]
pub enum StickerPackCacheKind {
    /// Concatenated original media, with SQL plaintext offsets.
    Original,
    /// Concatenated thumbnails, with separate SQL plaintext offsets.
    Thumbnail,
    /// Pack icon, stored outside the thumbnail aggregate.
    Icon,
}
fn pack_path(pack: &str, kind: StickerPackCacheKind) -> Result<String> {
    crate::sticker_packs::product(pack)?;
    let hash = format!("{:x}", Md5::digest(pack.as_bytes()));
    let (directory, suffix) = match kind {
        StickerPackCacheKind::Original => ("PersistStore", ""),
        StickerPackCacheKind::Thumbnail => ("ThumbStore", ""),
        StickerPackCacheKind::Icon => ("ThumbStore", ".icon"),
    };
    Ok(format!(
        "business/emoticon/{directory}/{}/{hash}{suffix}",
        &hash[..2]
    ))
}
/// Read and decrypt a bounded official pack aggregate/icon. This does not prove
/// individual item integrity: callers must check each SQL slice against its MD5
/// and inspect image bytes before describing them as available.
pub fn read_sticker_pack_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    pack: &str,
    kind: StickerPackCacheKind,
) -> Result<Vec<u8>> {
    let path = pack_path(pack, kind)?;
    let encrypted = crate::media::read_cache(
        location.root(),
        std::path::Path::new(&path),
        64 * 1024 * 1024 + 16,
    )?;
    let key = cache_key(uin, username)?;
    let bytes = decrypt(Cipher::aes_128_cbc(), &key, Some(&key), &encrypted)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    Ok(bytes)
}
/// Publish one already verified pack aggregate/icon at its native encrypted path.
/// Never replaces conflicting existing bytes. The original and thumbnail writes
/// are separate; callers retain their inputs and finish SQL only after both pass.
/// The caller verifies account identity, item bounds/hashes and image formats.
pub fn apply_sticker_pack_cache(
    location: &ProfileLocation,
    uin: u32,
    username: &str,
    pack: &str,
    kind: StickerPackCacheKind,
    bytes: &[u8],
    options: &WriteOptions,
) -> Result<bool> {
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let path = pack_path(pack, kind)?;
    let key = cache_key(uin, username)?;
    let encrypted = encrypt(Cipher::aes_128_cbc(), &key, Some(&key), bytes)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    crate::media::apply_checked_cache_file(location, &path, &encrypted, 64 * 1024 * 1024 + 16)
}
/// Remove only one pack's three known persisted caches, after membership SQL
/// removal. Other files and every custom Saved cache are preserved. Files are
/// unlinked separately in icon/thumbnail/original order; a partial error is
/// reported for local replay, which never contacts the server. Missing files
/// are satisfied no-ops; links and special files are refused.
pub fn apply_remove_sticker_pack_cache(
    location: &ProfileLocation,
    pack: &str,
    options: &WriteOptions,
) -> Result<()> {
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    for kind in [
        StickerPackCacheKind::Icon,
        StickerPackCacheKind::Thumbnail,
        StickerPackCacheKind::Original,
    ] {
        let relative = pack_path(pack, kind)?;
        let target = location.root().join(&relative);
        match crate::media::read_cache(
            location.root(),
            std::path::Path::new(&relative),
            64 * 1024 * 1024 + 16,
        ) {
            Ok(_) => {}
            Err(ProfileError::NotCached) => continue,
            Err(error) => return Err(error),
        }
        std::fs::remove_file(&target)?;
        wechat_platform::sync_dir(target.parent().ok_or(ProfileError::InvalidProfile)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cipher_matches_an_independent_synthetic_vector() {
        let key = cache_key(42, "wxid_sticker_fixture").unwrap();
        let bytes = b"GIF89a synthetic fixture";
        let cipher = encrypt(Cipher::aes_128_cbc(), &key, Some(&key), bytes).unwrap();
        assert_eq!(
            hex::encode(&cipher),
            "303d04a01dfd672b8dee048ddf080f1325264418affe4ac6010267c81aea3fd9"
        );
        assert_eq!(
            decrypt(Cipher::aes_128_cbc(), &key, Some(&key), &cipher).unwrap(),
            bytes
        );
        assert!(cache_key(0, "wxid_fixture").is_err());
        assert!(saved_path("../escape").is_err());
    }
}
