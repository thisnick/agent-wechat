//! Caller-side pack export. Filesystem output never belongs in an account actor.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path};

/// One selected pack item, including a retrieval failure for partial exports.
pub struct StickerExportItem {
    /// Stable content identity.
    pub id: String,
    /// One-based official ordering.
    pub ordinal: u32,
    /// Optional official caption.
    pub caption: Option<String>,
    /// Original verified bytes, or the per-item acquisition failure.
    pub bytes: std::result::Result<Vec<u8>, String>,
}
/// Machine-readable index for exactly one pack, including partial outcomes.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StickerExportIndex {
    /// Export format version.
    pub version: u32,
    /// Official product identity.
    pub pack_id: String,
    /// Official pack name.
    pub name: String,
    /// All selected pack items, preserving identities and order.
    pub items: Vec<StickerExportEntry>,
}
/// One numbered output and its integrity/transformation record.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StickerExportEntry {
    /// Stable original content identity.
    pub id: String,
    /// One-based official ordering.
    pub ordinal: u32,
    /// Optional official caption.
    pub caption: Option<String>,
    /// Numbered GIF filename relative to this export directory.
    pub filename: String,
    /// Original bytes' SHA-256, when retrieval succeeded.
    pub original_sha256: Option<String>,
    /// Exported GIF SHA-256, when conversion succeeded.
    pub sha256: Option<String>,
    /// Applied shared-media transformations; empty for unchanged GIFs.
    pub transformations: Vec<String>,
    /// `pending`, `available` or `failed`.
    pub outcome: String,
    /// Per-item retrieval/conversion failure, if any.
    pub error: Option<String>,
}
/// Convert an original to GIF for export. Valid GIFs retain their exact bytes.
pub fn sticker_export_gif(bytes: &[u8]) -> Result<(Vec<u8>, Vec<String>)> {
    let image = crate::inspect_image(bytes)?;
    if image.content_type == "image/gif" {
        return Ok((bytes.to_vec(), vec![]));
    }
    let output = crate::prepare_sticker_image(bytes, &crate::StickerImageOptions::default())?;
    Ok((output.bytes, output.transformations))
}
/// Export one selected pack as numbered GIFs plus index.json. Resumption checks
/// product, ordered identities and checksums; unrelated files are never replaced.
pub fn export_sticker_pack(
    directory: &Path,
    pack_id: &str,
    name: &str,
    items: Vec<StickerExportItem>,
    resume: bool,
) -> Result<StickerExportIndex> {
    ensure!(
        !pack_id.is_empty() && !items.is_empty() && items.len() <= 500,
        "invalid_sticker_export"
    );
    if !directory.exists() {
        std::fs::create_dir_all(directory)?;
    }
    ensure!(
        std::fs::symlink_metadata(directory)?.is_dir(),
        "invalid_export_directory"
    );
    let index_path = directory.join("index.json");
    let previous: Option<StickerExportIndex> = if index_path.exists() {
        ensure!(
            resume && std::fs::symlink_metadata(&index_path)?.is_file(),
            "sticker_export_exists"
        );
        let bytes = std::fs::read(&index_path)?;
        ensure!(bytes.len() <= 1024 * 1024, "invalid_sticker_export_index");
        Some(serde_json::from_slice(&bytes)?)
    } else {
        None
    };
    if let Some(old) = &previous {
        ensure!(
            old.version == 1
                && old.pack_id == pack_id
                && old.items.len() == items.len()
                && old
                    .items
                    .iter()
                    .zip(&items)
                    .all(|(a, b)| a.id == b.id && a.ordinal == b.ordinal),
            "sticker_export_identity_mismatch"
        );
    }
    let mut index = StickerExportIndex {
        version: 1,
        pack_id: pack_id.into(),
        name: name.into(),
        items: vec![],
    };
    let mut outputs = vec![];
    for (position, item) in items.into_iter().enumerate() {
        ensure!(
            item.ordinal == position as u32 + 1 && !index.items.iter().any(|e| e.id == item.id),
            "invalid_sticker_export_order"
        );
        let filename = format!("{:03}.gif", item.ordinal);
        let path = directory.join(&filename);
        let mut entry = StickerExportEntry {
            id: item.id,
            ordinal: item.ordinal,
            caption: item.caption,
            filename,
            original_sha256: None,
            sha256: None,
            transformations: vec![],
            outcome: "pending".into(),
            error: None,
        };
        if let Some(old) = previous.as_ref().and_then(|p| p.items.get(position)) {
            ensure!(
                old.filename == entry.filename,
                "invalid_sticker_export_index"
            );
            if path.exists() {
                ensure!(
                    std::fs::symlink_metadata(&path)?.is_file(),
                    "sticker_export_file_conflict"
                );
                let bytes = std::fs::read(&path)?;
                ensure!(
                    bytes.len() <= crate::MAX_IMAGE_BYTES
                        && old.sha256.as_deref() == Some(sha(&bytes).as_str()),
                    "sticker_export_checksum_mismatch"
                );
                let mut old = old.clone();
                old.outcome = "available".into();
                old.error = None;
                index.items.push(old);
                outputs.push(None);
                continue;
            }
        }
        ensure!(!path.exists(), "sticker_export_file_conflict");
        let converted = match item.bytes {
            Ok(bytes) => {
                entry.original_sha256 = Some(sha(&bytes));
                sticker_export_gif(&bytes)
                    .map_err(|_| "sticker_export_conversion_failed".to_string())
            }
            Err(error) => Err(error),
        };
        match converted {
            Ok((bytes, transformations)) => {
                entry.sha256 = Some(sha(&bytes));
                entry.transformations = transformations;
                outputs.push(Some(bytes));
            }
            Err(error) => {
                entry.outcome = "failed".into();
                entry.error = Some(error);
                outputs.push(None);
            }
        }
        index.items.push(entry);
    }
    write_index(directory, &index, previous.is_some())?;
    for (position, bytes) in outputs.into_iter().enumerate() {
        let Some(bytes) = bytes else { continue };
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(directory.join(&index.items[position].filename))
            .map_err(|e| e.error)?;
        index.items[position].outcome = "available".into();
        write_index(directory, &index, true)?;
    }
    Ok(index)
}
fn write_index(directory: &Path, index: &StickerExportIndex, replace: bool) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, index)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    if replace {
        temporary
            .persist(directory.join("index.json"))
            .map_err(|e| e.error)?;
    } else {
        temporary
            .persist_noclobber(directory.join("index.json"))
            .map_err(|e| e.error)?;
    }
    Ok(())
}
fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn items() -> Vec<StickerExportItem> {
        let archive = crate::decode_sticker_archive(
            include_bytes!("../fixtures/sticker-pack.zip"),
            "pack.fixture",
            &[
                "2c5f182a5005cfdeec4bd4071fad0e39".into(),
                "3cba29f7d86c1245ca196c29923a28eb".into(),
            ],
        )
        .unwrap();
        archive
            .items
            .into_iter()
            .map(|item| StickerExportItem {
                id: item.id,
                ordinal: item.ordinal,
                caption: Some(format!("Caption {}", item.ordinal)),
                bytes: Ok(item.original),
            })
            .collect()
    }
    #[test]
    fn one_pack_export_preserves_gif_bytes_and_resumes_only_verified_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("keep.txt"), b"unrelated").unwrap();
        let originals = items();
        let expected = originals[0].bytes.as_ref().unwrap().clone();
        let index =
            export_sticker_pack(root.path(), "pack.fixture", "Fixture", originals, false).unwrap();
        assert!(index
            .items
            .iter()
            .all(|i| i.outcome == "available" && i.transformations.is_empty()));
        assert_eq!(
            std::fs::read(root.path().join("001.gif")).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read(root.path().join("keep.txt")).unwrap(),
            b"unrelated"
        );
        assert!(export_sticker_pack(root.path(), "other.pack", "Other", items(), true).is_err());
        assert!(export_sticker_pack(root.path(), "pack.fixture", "Fixture", items(), true).is_ok());
        std::fs::write(root.path().join("002.gif"), b"changed").unwrap();
        assert!(
            export_sticker_pack(root.path(), "pack.fixture", "Fixture", items(), true).is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("002.gif")).unwrap(),
            b"changed"
        );
    }
    #[test]
    fn partial_failure_keeps_success_and_retry_fills_only_the_missing_file() {
        let root = tempfile::tempdir().unwrap();
        let mut input = items();
        input[1].bytes = Err("offline".into());
        let index =
            export_sticker_pack(root.path(), "pack.fixture", "Fixture", input, false).unwrap();
        assert_eq!(index.items[0].outcome, "available");
        assert_eq!(index.items[1].outcome, "failed");
        let index =
            export_sticker_pack(root.path(), "pack.fixture", "Fixture", items(), true).unwrap();
        assert!(index.items.iter().all(|i| i.outcome == "available"));
    }
}
