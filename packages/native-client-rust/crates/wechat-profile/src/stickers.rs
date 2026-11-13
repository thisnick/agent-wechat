//! Saved and installed-pack reads from the official emoticon database.
use crate::{DatabaseState, ProfileError, ProfileReader, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_ITEMS: usize = 10_000;

/// One item in the account's Saved collection. Transport URLs remain private.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedSticker {
    /// Official content identity, retained as lowercase hexadecimal.
    pub id: String,
    /// Optional official caption; an empty caption does not imply a name.
    pub caption: Option<String>,
}

/// Ordered Saved membership from one consistent official-profile read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedStickers {
    /// Items in picker order.
    pub items: Vec<SavedSticker>,
    /// Digest of ordered identities, suitable for a collection precondition.
    pub revision: String,
}

/// A pack present in the official installed-pack table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledStickerPack {
    /// Official package identity.
    pub id: String,
    /// Official display name when retained locally.
    pub name: Option<String>,
    /// Retained download status. It is not interpreted as membership.
    pub download_status: Option<i64>,
}

/// One official pack item and its localized captions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPackSticker {
    /// Official content identity.
    pub id: String,
    /// Official sort key within this pack.
    pub sort_order: i64,
    /// Caption language and text pairs, sorted by language.
    pub captions: Vec<(String, String)>,
}

fn identity(value: String) -> Result<String> {
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(value.to_ascii_lowercase())
}

impl ProfileReader {
    pub(crate) fn emoticon_database(&self) -> Result<&str> {
        let mut found = self.inventory().iter().filter(|db| {
            db.relative_path.rsplit('/').next() == Some("emoticon.db")
                && db.state == DatabaseState::Readable
        });
        let db = found.next().ok_or_else(|| {
            ProfileError::UnsupportedSchema("readable emoticon database required".into())
        })?;
        if found.next().is_some() {
            return Err(ProfileError::UnsupportedSchema(
                "ambiguous emoticon database".into(),
            ));
        }
        Ok(&db.relative_path)
    }

    /// Read Saved membership in the supported Linux picker order. This does
    /// not fetch metadata or alter the account's collection.
    pub fn saved_stickers(&self) -> Result<SavedStickers> {
        let db = self.snapshot.open(self.emoticon_database()?)?;
        let mut query = db.prepare(
            "SELECT o.md5, (SELECT NULLIF(e.caption,'') FROM kNonStoreEmoticonTable e \
             WHERE e.md5=o.md5 AND e.type=3) FROM kFavEmoticonOrderTable o \
             ORDER BY o.rowid DESC LIMIT 10001",
        )?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut items = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            let (id, caption) = row?;
            let id = identity(id)?;
            if items.len() >= MAX_ITEMS || !seen.insert(id.clone()) {
                return Err(ProfileError::ResourceLimit);
            }
            items.push(SavedSticker { id, caption });
        }
        let revision = hex::encode(Sha256::digest(
            serde_json::to_vec(&items.iter().map(|item| &item.id).collect::<Vec<_>>())
                .map_err(|_| ProfileError::InvalidMetadata)?,
        ));
        Ok(SavedStickers { items, revision })
    }

    /// Read the private type-3 transport record for one Saved identity. The
    /// caller must keep its URLs and keys out of public representations.
    pub fn saved_sticker_record(&self, id: &str) -> Result<SavedStickerRecord> {
        let id = identity(id.to_owned())?;
        let db = self.snapshot.open(self.emoticon_database()?)?;
        use rusqlite::OptionalExtension;
        db.query_row("SELECT md5,caption,product_id,aes_key,thumb_url,tp_url,auth_key,cdn_url,extern_url,extern_md5,encrypt_url,designer_id,activity_id FROM kNonStoreEmoticonTable WHERE md5=?1 AND type=3", [id], |r| Ok(SavedStickerRecord {
            id:r.get(0)?,caption:r.get(1)?,product_id:r.get(2)?,aes_key:r.get(3)?,thumb_url:r.get(4)?,tp_url:r.get(5)?,auth_key:r.get(6)?,cdn_url:r.get(7)?,extern_url:r.get(8)?,extern_md5:r.get(9)?,encrypt_url:r.get(10)?,designer_id:r.get(11)?,activity_id:r.get(12)?,
        })).optional()?.ok_or(ProfileError::NotCached)
    }

    /// Read every locally installed pack, including packs absent from a current
    /// store feed. Membership is established by package records, not cache files.
    pub fn installed_sticker_packs(&self) -> Result<Vec<InstalledStickerPack>> {
        let db = self.snapshot.open(self.emoticon_database()?)?;
        let mut query = db.prepare(
            "SELECT package_id_,NULLIF(package_name_,''),download_status_ \
             FROM kStoreEmoticonPackageTable ORDER BY sort_order_ DESC,rowid LIMIT 10001",
        )?;
        let items = query
            .query_map([], |row| {
                Ok(InstalledStickerPack {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    download_status: row.get(2)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if items.len() > MAX_ITEMS
            || items
                .iter()
                .any(|item| item.id.is_empty() || item.id.len() > 1024)
        {
            return Err(ProfileError::ResourceLimit);
        }
        Ok(items)
    }

    /// Read ordered file identities and captions from an installed pack. No
    /// network acquisition is initiated by this read.
    pub fn installed_pack_stickers(&self, pack_id: &str) -> Result<Vec<InstalledPackSticker>> {
        if pack_id.is_empty() || pack_id.len() > 1024 {
            return Err(ProfileError::InvalidQuery);
        }
        let db = self.snapshot.open(self.emoticon_database()?)?;
        let mut query = db.prepare(
            "SELECT md5_,sort_order_ FROM kStoreEmoticonFilesTable \
             WHERE package_id_=?1 ORDER BY sort_order_,rowid LIMIT 10001",
        )?;
        let rows = query
            .query_map([pack_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > MAX_ITEMS {
            return Err(ProfileError::ResourceLimit);
        }
        let mut captions = db.prepare(
            "SELECT language_,caption_ FROM kStoreEmoticonCaptionsTable \
             WHERE package_id_=?1 AND md5_=?2 AND caption_<>'' ORDER BY language_ LIMIT 101",
        )?;
        let mut seen = std::collections::BTreeSet::new();
        rows.into_iter()
            .map(|(id, sort_order)| {
                let id = identity(id)?;
                if !seen.insert(id.clone()) {
                    return Err(ProfileError::InvalidMetadata);
                }
                let captions = captions
                    .query_map([pack_id, &id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if captions.len() > 100 {
                    return Err(ProfileError::ResourceLimit);
                }
                Ok(InstalledPackSticker {
                    id,
                    sort_order,
                    captions,
                })
            })
            .collect()
    }
}

/// A complete official NonStore record. Transport keys and URLs are private
/// profile data and must never be returned through a public sticker API.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SavedStickerRecord {
    /// Content identity.
    pub id: String,
    /// Official optional caption, empty when absent.
    pub caption: String,
    /// Retained product/source identity.
    pub product_id: String,
    /// Retained encrypted-representation key.
    pub aes_key: String,
    /// Thumbnail source.
    pub thumb_url: String,
    /// Retained alternate thumbnail/source URL.
    pub tp_url: String,
    /// Retained source authorization.
    pub auth_key: String,
    /// Original source.
    pub cdn_url: String,
    /// External representation source.
    pub extern_url: String,
    /// External representation identity.
    pub extern_md5: String,
    /// Encrypted representation source.
    pub encrypt_url: String,
    /// Official designer identity.
    pub designer_id: String,
    /// Official activity identity.
    pub activity_id: String,
}
/// Receipt-derived full Saved order and narrowly selected metadata changes.
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedStickerProjection {
    /// Complete authoritative list, newest first. Partial pages are not valid input.
    pub order: Vec<String>,
    /// Exact type-3 metadata identities selected for removal; unrelated orphan
    /// metadata is preserved rather than inferred absent from collection order.
    pub removed_metadata: Vec<String>,
    /// Complete records supplied by the verified metadata/object reconciliation.
    pub records: Vec<SavedStickerRecord>,
}
pub(crate) fn check_sticker_table(
    db: &rusqlite::Connection,
    relative: &str,
    name: &str,
    columns: &[(&str, &str)],
    unique: &[&str],
) -> Result<()> {
    let schema = crate::schema::inspect(db, relative)?;
    let table = schema
        .tables
        .iter()
        .find(|table| table.name == name)
        .ok_or_else(|| ProfileError::UnsupportedSchema(relative.into()))?;
    let valid = table.columns.len() == columns.len()
        && table.triggers.is_empty()
        && table.columns.iter().all(|column| {
            columns.iter().any(|(name, kind)| {
                column.name == *name && column.declared_type.eq_ignore_ascii_case(kind)
            }) && !column.not_null
                && column.default_sql.is_none()
                && column.primary_key_position == 0
                && column.hidden == 0
        })
        && table.indexes.iter().filter(|index| index.unique).count() == 1
        && table.indexes.iter().any(|index| {
            index.unique
                && !index.partial
                && index
                    .columns
                    .iter()
                    .map(|column| column.as_deref())
                    .eq(unique.iter().copied().map(Some))
        });
    if !valid {
        return Err(ProfileError::UnsupportedSchema(relative.into()));
    }
    Ok(())
}
/// Apply a complete Saved reconciliation in one emoticon-database transaction.
/// Rebuilds favorite order, removes only supplied type-3 metadata keys and
/// inserts/replaces supplied complete records. Other types, packs, caches and
/// unknown files are preserved. Replay is idempotent for these logical values;
/// SQLite rowids are not stable identities. Caller owns exclusive access and a
/// durable receipt, and must fence stale receipts before invoking this writer.
pub fn apply_saved_stickers(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    projection: &SavedStickerProjection,
    options: &crate::WriteOptions,
) -> Result<()> {
    crate::projection::ownership(location, keys, options)?;
    if projection.order.len() > MAX_ITEMS
        || projection.removed_metadata.len() > MAX_ITEMS
        || projection.records.len() > MAX_ITEMS
    {
        return Err(ProfileError::ResourceLimit);
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in &projection.order {
        if identity(id.clone())? != *id || !seen.insert(id) {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let mut removed = std::collections::BTreeSet::new();
    for id in &projection.removed_metadata {
        if identity(id.clone())? != *id || seen.contains(id) || !removed.insert(id) {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let mut records = std::collections::BTreeSet::new();
    for record in &projection.records {
        if identity(record.id.clone())? != record.id
            || !records.insert(&record.id)
            || removed.contains(&record.id)
        {
            return Err(ProfileError::InvalidMetadata);
        }
        for value in [
            &record.caption,
            &record.product_id,
            &record.aes_key,
            &record.thumb_url,
            &record.tp_url,
            &record.auth_key,
            &record.cdn_url,
            &record.extern_url,
            &record.extern_md5,
            &record.encrypt_url,
            &record.designer_id,
            &record.activity_id,
        ] {
            if value.len() > 16384 || value.contains('\0') {
                return Err(ProfileError::ResourceLimit);
            }
        }
    }
    let relative = crate::writer::find_database(location, "emoticon.db")?;
    let mut db = crate::writer::open_write(location, &relative, keys, options.allow_plaintext)?;
    check_sticker_table(
        &db,
        &relative,
        "kFavEmoticonOrderTable",
        &[("md5", "TEXT")],
        &["md5"],
    )?;
    check_sticker_table(
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
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM kFavEmoticonOrderTable", [])?;
    for id in projection.order.iter().rev() {
        tx.execute("INSERT INTO kFavEmoticonOrderTable(md5) VALUES(?1)", [id])?;
    }
    for id in &projection.removed_metadata {
        tx.execute(
            "DELETE FROM kNonStoreEmoticonTable WHERE type=3 AND md5=?1",
            [id],
        )?;
    }
    for record in &projection.records {
        tx.execute("INSERT OR REPLACE INTO kNonStoreEmoticonTable(type,md5,caption,product_id,aes_key,thumb_url,tp_url,auth_key,cdn_url,extern_url,extern_md5,encrypt_url,designer_id,activity_id) VALUES(3,?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            rusqlite::params![record.id,record.caption,record.product_id,record.aes_key,record.thumb_url,record.tp_url,record.auth_key,record.cdn_url,record.extern_url,record.extern_md5,record.encrypt_url,record.designer_id,record.activity_id])?;
    }
    tx.commit()?;
    Ok(())
}

/// Create only a missing emoticon database in a caller-owned supported profile.
/// Existing files and unknown schemas are never replaced or migrated. The
/// returned key must be published in the caller's protected key manifest before
/// use. Caller retains the durable initialization intent across publication;
/// a crash after publication leaves a readable file that must be admitted, not
/// recreated. Other profile databases and cache files are untouched.
pub fn apply_initialize_emoticon(
    location: &crate::ProfileLocation,
    keys: &crate::KeyFile,
    password: &[u8],
    options: &crate::WriteOptions,
) -> Result<crate::DatabaseKey> {
    use std::io::Read;
    crate::projection::ownership(location, keys, options)?;
    if password.len() != 32 {
        return Err(ProfileError::InvalidKeysFile);
    }
    crate::verify_profile_password(location, keys, password)?;
    let relative = "emoticon/emoticon.db";
    let directory = location.storage.join("emoticon");
    if !directory.exists() {
        std::fs::create_dir(&directory)?;
    }
    if std::fs::canonicalize(&directory)? != directory || !std::fs::metadata(&directory)?.is_dir() {
        return Err(ProfileError::UnsupportedFile);
    }
    let target = directory.join("emoticon.db");
    if target.exists() || target.symlink_metadata().is_ok() {
        return Err(ProfileError::UnsupportedFile);
    }
    let staged = wechat_platform::private_tempfile_in(&directory, ".emoticon-")?;
    let db = rusqlite::Connection::open(staged.path())?;
    let rc = unsafe {
        rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), password.len() as i32)
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        return Err(ProfileError::Database);
    }
    db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;PRAGMA synchronous=FULL;PRAGMA journal_mode=DELETE;")?;
    db.execute_batch(include_str!("layout/emoticon.db.sql"))?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(ProfileError::Database);
    }
    drop(db);
    staged.as_file().sync_all()?;
    let mut page = [0u8; 4096];
    std::fs::File::open(staged.path())?.read_exact(&mut page)?;
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&page[..16]);
    let raw = crate::keys::derive_key(password, &salt);
    if !crate::keys::verify_first_page(&page, raw.expose(), &salt) {
        return Err(ProfileError::KeyMismatch(relative.into()));
    }
    let key = crate::DatabaseKey::new(
        relative.into(),
        salt,
        raw.expose()
            .try_into()
            .map_err(|_| ProfileError::InvalidKeysFile)?,
    )?;
    staged
        .persist_noclobber(&target)
        .map_err(|_| ProfileError::UnsupportedFile)?;
    wechat_platform::sync_dir(&directory)?;
    Ok(key)
}
