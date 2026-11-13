//! Complete pack rows used by checked profile operations. Source URLs stay private.
use crate::{ProfileError, ProfileReader, Result};
use serde::{Deserialize, Serialize};

/// Complete official package record, including independent content readiness.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct StickerPackRecord {
    /// Official product identity.
    pub id: String,
    /// Official package name.
    pub name: String,
    /// Retained payment state, never inferred from a localized price string.
    pub payment_status: i64,
    /// Retained native download state; verified cache availability is checked separately.
    pub download_status: i64,
    /// Retained installation timestamp; zero is not replaced with the current time.
    pub install_time: i64,
    /// Retained removal timestamp.
    pub remove_time: i64,
    /// Official package ordering value.
    pub sort_order: i64,
    /// Literal Linux introduction column; distinct from the wire display-description alias.
    pub introduction: String,
    /// Literal Linux full-description column.
    pub full_description: String,
    /// Official copyright text.
    pub copyright: String,
    /// Literal author column.
    pub author: String,
    /// Private native icon source.
    pub store_icon_url: String,
    /// Private native panel source.
    pub panel_url: String,
}
/// One item slice in the decrypted original and thumbnail aggregates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StickerPackFileRecord {
    /// Original content identity.
    pub id: String,
    /// Native file type, distinct from a message type.
    pub file_type: i64,
    /// Native ordinal in the pack, preserved from its supplied order.
    pub sort_order: i64,
    /// Plaintext original size.
    pub original_size: u32,
    /// Offset in the decrypted original aggregate.
    pub original_offset: u32,
    /// Plaintext thumbnail size.
    pub thumbnail_size: u32,
    /// Offset in the decrypted thumbnail aggregate.
    pub thumbnail_offset: u32,
}
/// One official localized caption, independent of a custom editable name.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StickerPackCaptionRecord {
    /// Original content identity.
    pub id: String,
    /// Official locale or literal `default` fallback.
    pub language: String,
    /// Official caption text.
    pub caption: String,
}
pub(crate) fn product(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control) {
        return Err(ProfileError::InvalidQuery);
    }
    Ok(())
}
impl ProfileReader {
    /// Read a complete canonical package row without contacting the server.
    /// Missing membership is NotCached, not a conclusion about catalog existence.
    pub fn sticker_pack_record(&self, pack: &str) -> Result<StickerPackRecord> {
        product(pack)?;
        let db = self.snapshot.open(self.emoticon_database()?)?;
        use rusqlite::OptionalExtension;
        db.query_row("SELECT package_id_,package_name_,payment_status_,download_status_,install_time_,remove_time_,sort_order_,introduction_,full_description_,copyright_,author_,store_icon_url_,panel_url_ FROM kStoreEmoticonPackageTable WHERE package_id_=?1",[pack],|r|Ok(StickerPackRecord {
            id:r.get(0)?,name:r.get(1)?,payment_status:r.get(2)?,download_status:r.get(3)?,install_time:r.get(4)?,remove_time:r.get(5)?,sort_order:r.get(6)?,introduction:r.get(7)?,full_description:r.get(8)?,copyright:r.get(9)?,author:r.get(10)?,store_icon_url:r.get(11)?,panel_url:r.get(12)?,
        })).optional()?.ok_or(ProfileError::NotCached)
    }
    /// Read bounded original/thumbnail slice descriptors in native ordinal order.
    /// The caller decrypts the complete aggregate before applying these offsets.
    pub fn sticker_pack_files(&self, pack: &str) -> Result<Vec<StickerPackFileRecord>> {
        product(pack)?;
        let db = self.snapshot.open(self.emoticon_database()?)?;
        let mut query=db.prepare("SELECT md5_,type_,sort_order_,emoticon_size_,emoticon_offset_,thumb_size_,thumb_offset_ FROM kStoreEmoticonFilesTable WHERE package_id_=?1 ORDER BY sort_order_,rowid LIMIT 1001")?;
        let rows = query
            .query_map([pack], |r| {
                Ok(StickerPackFileRecord {
                    id: r.get(0)?,
                    file_type: r.get(1)?,
                    sort_order: r.get(2)?,
                    original_size: r.get(3)?,
                    original_offset: r.get(4)?,
                    thumbnail_size: r.get(5)?,
                    thumbnail_offset: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > 1000
            || rows
                .iter()
                .any(|row| row.id.len() != 32 || !row.id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(ProfileError::InvalidMetadata);
        }
        Ok(rows)
    }
}

fn checked_database(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    options: &crate::WriteOptions,
) -> Result<rusqlite::Connection> {
    crate::projection::ownership(location, keys, options)?;
    let relative = crate::writer::find_database(location, "emoticon.db")?;
    let db = crate::writer::open_write(location, &relative, keys, options.allow_plaintext)?;
    crate::stickers::check_sticker_table(
        &db,
        &relative,
        "kNonStoreEmoticonTable",
        &[
            ("type", "INTEGER"),
            ("md5", "TEXT"),
            ("caption", "TEXT"),
            ("product_id", "TEXT"),
            ("aes_key", "TEXT"),
            ("thumb_url", "TEXT"),
            ("tp_url", "TEXT"),
            ("auth_key", "TEXT"),
            ("cdn_url", "TEXT"),
            ("extern_url", "TEXT"),
            ("extern_md5", "TEXT"),
            ("encrypt_url", "TEXT"),
            ("designer_id", "TEXT"),
            ("activity_id", "TEXT"),
        ],
        &["type", "md5"],
    )?;
    crate::stickers::check_sticker_table(
        &db,
        &relative,
        "kStoreEmoticonPackageTable",
        &[
            ("package_id_", "TEXT"),
            ("package_name_", "TEXT"),
            ("payment_status_", "INTEGER"),
            ("download_status_", "INTEGER"),
            ("install_time_", "INTEGER"),
            ("remove_time_", "INTEGER"),
            ("sort_order_", "INTEGER"),
            ("introduction_", "TEXT"),
            ("full_description_", "TEXT"),
            ("copyright_", "TEXT"),
            ("author_", "TEXT"),
            ("store_icon_url_", "TEXT"),
            ("panel_url_", "TEXT"),
        ],
        &["package_id_"],
    )?;
    crate::stickers::check_sticker_table(
        &db,
        &relative,
        "kStoreEmoticonFilesTable",
        &[
            ("package_id_", "TEXT"),
            ("md5_", "TEXT"),
            ("type_", "INTEGER"),
            ("sort_order_", "INTEGER"),
            ("emoticon_size_", "INTEGER"),
            ("emoticon_offset_", "INTEGER"),
            ("thumb_size_", "INTEGER"),
            ("thumb_offset_", "INTEGER"),
        ],
        &["package_id_", "md5_"],
    )?;
    crate::stickers::check_sticker_table(
        &db,
        &relative,
        "kStoreEmoticonCaptionsTable",
        &[
            ("package_id_", "TEXT"),
            ("md5_", "TEXT"),
            ("language_", "TEXT"),
            ("caption_", "TEXT"),
        ],
        &["package_id_", "md5_", "language_"],
    )?;
    Ok(db)
}
fn string(value: &str) -> Result<()> {
    if value.len() > 16384 || value.contains('\0') {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(())
}
fn item_id(value: &str) -> Result<()> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(())
}
/// Reconcile package membership from a completed authoritative account list.
/// New rows use complete supplied records with OR IGNORE; existing rows remain
/// unchanged, including their readiness, timestamps and ordering. This does not
/// implement the interactive manager's conditional full-record rank replacement.
/// Selected removals delete package/file rows and only orphaned captions, while
/// preserving every Saved/NonStore reference. The SQL changes use one transaction;
/// cache cleanup is separate and remains the caller's durable responsibility.
/// No media acquisition, purchase or account mutation is submitted here.
pub fn apply_sticker_pack_membership(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    records: &[StickerPackRecord],
    removed: &[String],
    options: &crate::WriteOptions,
) -> Result<()> {
    if records.len() > 1000 || removed.len() > 1000 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for record in records {
        product(&record.id)?;
        if !seen.insert(&record.id) || removed.contains(&record.id) {
            return Err(ProfileError::InvalidMetadata);
        }
        for value in [
            &record.name,
            &record.introduction,
            &record.full_description,
            &record.copyright,
            &record.author,
            &record.store_icon_url,
            &record.panel_url,
        ] {
            string(value)?;
        }
    }
    for id in removed {
        product(id)?;
    }
    let mut db = checked_database(location, keys, options)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for record in records {
        tx.execute("INSERT OR IGNORE INTO kStoreEmoticonPackageTable(package_id_,package_name_,payment_status_,download_status_,install_time_,remove_time_,sort_order_,introduction_,full_description_,copyright_,author_,store_icon_url_,panel_url_) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",rusqlite::params![record.id,record.name,record.payment_status,record.download_status,record.install_time,record.remove_time,record.sort_order,record.introduction,record.full_description,record.copyright,record.author,record.store_icon_url,record.panel_url])?;
    }
    for id in removed {
        tx.execute(
            "DELETE FROM kStoreEmoticonPackageTable WHERE package_id_=?1",
            [id],
        )?;
        tx.execute(
            "DELETE FROM kStoreEmoticonFilesTable WHERE package_id_=?1",
            [id],
        )?;
        let referenced: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kNonStoreEmoticonTable WHERE product_id=?1) OR EXISTS(SELECT 1 FROM kStoreEmoticonPackageTable WHERE package_id_=?1)",[id],|r|r.get(0))?;
        if !referenced {
            tx.execute(
                "DELETE FROM kStoreEmoticonCaptionsTable WHERE package_id_=?1",
                [id],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}
/// Register already verified, already published original/thumbnail aggregates.
/// Replaces only supplied file rows, then changes only the existing package's
/// download readiness. Other package fields and unspecified files are preserved.
/// Missing membership fails; callers must not fabricate it from cached media.
/// Filesystem publication precedes this SQL transaction and needs its own receipt.
pub fn apply_sticker_pack_content(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    pack: &str,
    files: &[StickerPackFileRecord],
    options: &crate::WriteOptions,
) -> Result<()> {
    product(pack)?;
    if files.is_empty() || files.len() > 1000 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for file in files {
        item_id(&file.id)?;
        if !seen.insert(&file.id)
            || !matches!(file.file_type, 1..=3)
            || file.sort_order < 1
            || file.original_size == 0
            || file.thumbnail_size == 0
            || file
                .original_offset
                .checked_add(file.original_size)
                .is_none_or(|n| n > 64 * 1024 * 1024)
            || file
                .thumbnail_offset
                .checked_add(file.thumbnail_size)
                .is_none_or(|n| n > 64 * 1024 * 1024)
        {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let mut db = checked_database(location, keys, options)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM kStoreEmoticonPackageTable WHERE package_id_=?1)",
        [pack],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(ProfileError::NotCached);
    }
    for file in files {
        tx.execute("INSERT OR REPLACE INTO kStoreEmoticonFilesTable(package_id_,md5_,type_,sort_order_,emoticon_size_,emoticon_offset_,thumb_size_,thumb_offset_) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",rusqlite::params![pack,file.id,file.file_type,file.sort_order,file.original_size,file.original_offset,file.thumbnail_size,file.thumbnail_offset])?;
    }
    tx.execute(
        "UPDATE kStoreEmoticonPackageTable SET download_status_=1 WHERE package_id_=?1",
        [pack],
    )?;
    tx.commit()?;
    Ok(())
}
/// Insert/replace supplied captions only for products still referenced by a pack
/// or any NonStore row. Unspecified languages remain intact. Empty language/text
/// pairs are ignored; query failures never count as proof of no references.
pub fn apply_sticker_pack_captions(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    pack: &str,
    captions: &[StickerPackCaptionRecord],
    options: &crate::WriteOptions,
) -> Result<()> {
    product(pack)?;
    if captions.len() > 10000 {
        return Err(ProfileError::ResourceLimit);
    }
    for caption in captions {
        item_id(&caption.id)?;
        string(&caption.language)?;
        string(&caption.caption)?;
    }
    let mut db = checked_database(location, keys, options)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let referenced: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM kNonStoreEmoticonTable WHERE product_id=?1) OR EXISTS(SELECT 1 FROM kStoreEmoticonPackageTable WHERE package_id_=?1)",[pack],|r|r.get(0))?;
    if referenced {
        for caption in captions {
            if !caption.language.is_empty() && !caption.caption.is_empty() {
                tx.execute("INSERT OR REPLACE INTO kStoreEmoticonCaptionsTable(package_id_,md5_,language_,caption_) VALUES(?1,?2,?3,?4)",rusqlite::params![pack,caption.id,caption.language,caption.caption])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}
