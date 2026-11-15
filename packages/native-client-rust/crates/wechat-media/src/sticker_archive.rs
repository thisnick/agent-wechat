//! Validate a supported pack ZIP in memory. No archive path is written to disk.
use anyhow::{ensure, Context, Result};
use md5::{Digest, Md5};
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};

/// One original and its paired thumbnail in canonical one-based order.
pub struct StickerArchiveItem {
    /// Original content MD5, checked against the supplied detail list.
    pub id: String,
    /// One-based ordinal parsed from the numeric archive filename.
    pub ordinal: u32,
    /// Verified original bytes, with their original format preserved.
    pub original: Vec<u8>,
    /// Verified still thumbnail.
    pub thumbnail: Vec<u8>,
    /// Native file type: PNG 1, GIF 2 or JPEG 3.
    pub file_type: i64,
}
/// Fully verified supported pack content. Membership is a separate operation.
pub struct StickerArchive {
    /// Independent package icon.
    pub icon: Vec<u8>,
    /// Complete paired content in numeric order.
    pub items: Vec<StickerArchiveItem>,
}
/// Decode a supported ZIP with exact product root, bounded expansion, regular
/// files, unique paths, CRC verification and MD5/detail ordinal correspondence.
pub fn decode_sticker_archive(
    bytes: &[u8],
    product: &str,
    ids: &[String],
) -> Result<StickerArchive> {
    ensure!(
        !bytes.is_empty()
            && bytes.len() <= 16 * 1024 * 1024
            && !ids.is_empty()
            && ids.len() <= 500
            && !product.is_empty()
            && !product.contains(['/', '\\', '\0']),
        "unsupported_sticker_archive"
    );
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(
        zip.len() <= ids.len() * 2 + 8,
        "sticker_archive_member_limit"
    );
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for index in 0..zip.len() {
        let mut member = zip.by_index(index)?;
        let name = member.name()?.into_owned();
        ensure!(
            !name.contains(['\\', '\0'])
                && member.enclosed_name().is_some()
                && name.split('/').all(|p| p != "." && p != "..")
                && name.starts_with(&format!("{product}/")),
            "unsafe_sticker_archive_path"
        );
        let mode = member.unix_mode().unwrap_or(0) & 0o170000;
        if member.is_dir() {
            ensure!(
                mode == 0 || mode == 0o040000,
                "unsupported_sticker_archive_member"
            );
            continue;
        }
        ensure!(
            mode == 0 || mode == 0o100000,
            "unsupported_sticker_archive_member"
        );
        ensure!(
            member.size() > 0 && member.size() <= 4 * 1024 * 1024,
            "sticker_archive_member_limit"
        );
        let mut content = Vec::new();
        member
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut content)?;
        ensure!(
            content.len() as u64 == member.size(),
            "sticker_archive_member_limit"
        );
        total += content.len();
        ensure!(total <= 64 * 1024 * 1024, "sticker_archive_expansion_limit");
        let relative = name
            .strip_prefix(&format!("{product}/"))
            .unwrap()
            .to_ascii_lowercase();
        ensure!(
            files.insert(relative, content).is_none(),
            "duplicate_sticker_archive_path"
        );
    }
    let icon = files
        .remove("icon.png")
        .context("unsupported_sticker_archive")?;
    ensure!(
        crate::inspect_image(&icon)?.content_type == "image/png",
        "unsupported_sticker_archive"
    );
    let mut originals = BTreeMap::new();
    let mut thumbs = BTreeMap::new();
    for (path, bytes) in files {
        let (folder, file) = path
            .split_once('/')
            .context("unsupported_sticker_archive")?;
        let (stem, extension) = file
            .rsplit_once('.')
            .context("unsupported_sticker_archive")?;
        ensure!(
            !stem.is_empty() && stem.bytes().all(|b| b.is_ascii_digit()),
            "unsupported_sticker_archive"
        );
        let ordinal: u32 = stem.parse()?;
        ensure!(
            ordinal > 0 && ordinal as usize <= ids.len(),
            "unsupported_sticker_archive"
        );
        let image = crate::inspect_image(&bytes)?;
        let file_type = match (extension, image.content_type.as_str()) {
            ("gif", "image/gif") => 2,
            ("png", "image/png") => 1,
            ("jpg", "image/jpeg") => 3,
            _ => anyhow::bail!("unsupported_sticker_archive"),
        };
        match folder {
            "large" => ensure!(
                originals.insert(ordinal, (bytes, file_type)).is_none(),
                "duplicate_sticker_archive_item"
            ),
            "thumb" => {
                ensure!(image.frames == 1, "unsupported_sticker_archive_thumbnail");
                ensure!(
                    thumbs.insert(ordinal, bytes).is_none(),
                    "duplicate_sticker_archive_item"
                );
            }
            _ => anyhow::bail!("unsupported_sticker_archive"),
        }
    }
    ensure!(
        originals.len() == ids.len() && thumbs.len() == ids.len(),
        "sticker_archive_pair_missing"
    );
    let mut items = vec![];
    for (index, id) in ids.iter().enumerate() {
        let ordinal = index as u32 + 1;
        let (original, file_type) = originals
            .remove(&ordinal)
            .context("sticker_archive_pair_missing")?;
        ensure!(
            id.len() == 32 && hex_digest(&original).eq_ignore_ascii_case(id),
            "sticker_archive_identity_mismatch"
        );
        items.push(StickerArchiveItem {
            id: id.to_ascii_lowercase(),
            ordinal,
            original,
            thumbnail: thumbs
                .remove(&ordinal)
                .context("sticker_archive_pair_missing")?,
            file_type,
        });
    }
    Ok(StickerArchive { icon, items })
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Md5::digest(bytes))
}

/// Encode verified pack content in the supported archive layout: the product
/// root holds `icon.png`, `large/<ordinal>.<ext>` originals and
/// `thumb/<ordinal>.<ext>` thumbnails. Members are stored uncompressed with a
/// fixed timestamp, so the same content always encodes to the same bytes.
/// Content assembled from per-item sources is kept in this layout so it is
/// stored and re-verified by [`decode_sticker_archive`] like a downloaded pack.
pub fn encode_sticker_archive(product: &str, archive: &StickerArchive) -> Result<Vec<u8>> {
    use std::io::Write;
    ensure!(
        !product.is_empty()
            && !product.contains(['/', '\\', '\0'])
            && !archive.items.is_empty()
            && archive.items.len() <= 500,
        "unsupported_sticker_archive"
    );
    let extension = |bytes: &[u8]| -> Result<&'static str> {
        Ok(match crate::inspect_image(bytes)?.content_type.as_str() {
            "image/gif" => "gif",
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => anyhow::bail!("unsupported_sticker_archive"),
        })
    };
    ensure!(
        extension(&archive.icon)? == "png",
        "unsupported_sticker_archive"
    );
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(format!("{product}/icon.png"), options)?;
    zip.write_all(&archive.icon)?;
    for item in &archive.items {
        ensure!(item.ordinal > 0, "unsupported_sticker_archive");
        zip.start_file(
            format!(
                "{product}/large/{}.{}",
                item.ordinal,
                extension(&item.original)?
            ),
            options,
        )?;
        zip.write_all(&item.original)?;
        zip.start_file(
            format!(
                "{product}/thumb/{}.{}",
                item.ordinal,
                extension(&item.thumbnail)?
            ),
            options,
        )?;
        zip.write_all(&item.thumbnail)?;
    }
    Ok(zip.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_zip_orders_numeric_members_and_rejects_wrong_product_or_identity() {
        let zip = include_bytes!("../fixtures/sticker-pack.zip");
        let ids = vec![
            "2c5f182a5005cfdeec4bd4071fad0e39".into(),
            "3cba29f7d86c1245ca196c29923a28eb".into(),
        ];
        let decoded = decode_sticker_archive(zip, "pack.fixture", &ids).unwrap();
        assert_eq!(
            decoded.items.iter().map(|i| i.ordinal).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(decoded.items[0].original.len(), 43);
        assert_eq!(decoded.items[1].thumbnail.len(), 69);
        assert!(decode_sticker_archive(zip, "another.pack", &ids).is_err());
        let mut reversed = ids.clone();
        reversed.reverse();
        assert!(decode_sticker_archive(zip, "pack.fixture", &reversed).is_err());
        assert!(decode_sticker_archive(&zip[..zip.len() - 8], "pack.fixture", &ids).is_err());
    }

    #[test]
    fn encoded_content_decodes_to_the_same_verified_items_deterministically() {
        let zip = include_bytes!("../fixtures/sticker-pack.zip");
        let ids: Vec<String> = vec![
            "2c5f182a5005cfdeec4bd4071fad0e39".into(),
            "3cba29f7d86c1245ca196c29923a28eb".into(),
        ];
        let decoded = decode_sticker_archive(zip, "pack.fixture", &ids).unwrap();
        let encoded = encode_sticker_archive("pack.fixture", &decoded).unwrap();
        assert_eq!(
            encoded,
            encode_sticker_archive("pack.fixture", &decoded).unwrap()
        );
        let again = decode_sticker_archive(&encoded, "pack.fixture", &ids).unwrap();
        assert_eq!(again.icon, decoded.icon);
        assert_eq!(again.items.len(), decoded.items.len());
        for (a, b) in again.items.iter().zip(&decoded.items) {
            assert_eq!(
                (&a.id, a.ordinal, &a.original, &a.thumbnail, a.file_type),
                (&b.id, b.ordinal, &b.original, &b.thumbnail, b.file_type)
            );
        }
        // The encoder keeps the product root, so another product still fails.
        assert!(decode_sticker_archive(&encoded, "another.pack", &ids).is_err());
        assert!(encode_sticker_archive("bad/product", &decoded).is_err());
    }
}
