//! Checked cache storage for exact local messages. Filesystem caches and ordinary
//! resource tables have independent transactions; callers retain a durable intent
//! and retry the same payload rather than treating them as one atomic commit.
use crate::{
    projection::{check_known_table, identity, map_id, ownership},
    writer::{find_database, open_write},
    *,
};
use md5::{Digest, Md5};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    fs,
    io::Read,
    path::{Component, Path},
};

pub const MAX_CACHED_MEDIA_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_CACHED_FILE_BYTES: usize = wechat_media::MAX_FILE_BYTES;
fn hardlink_limit(kind: HardlinkKind) -> usize {
    if kind == HardlinkKind::File {
        MAX_CACHED_FILE_BYTES
    } else {
        MAX_CACHED_MEDIA_BYTES
    }
}
fn cache_path_limit(relative: &str) -> usize {
    if relative.starts_with("msg/file/") {
        MAX_CACHED_FILE_BYTES
    } else {
        MAX_CACHED_MEDIA_BYTES
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedVoiceCache {
    pub database: String,
    pub changed: bool,
}
struct MessageBinding {
    local: i64,
    server: i64,
    time: i64,
    kind: i64,
    origin: i64,
    sender: String,
}
fn message_binding(
    location: &ProfileLocation,
    keys: &KeyFile,
    id: &SourceMessageId,
    chat: &str,
    options: &WriteOptions,
) -> Result<MessageBinding> {
    identity(chat)?;
    if !crate::schema::message_table(&id.table)
        || id.table != format!("Msg_{:x}", Md5::digest(chat.as_bytes()))
    {
        return Err(ProfileError::InvalidQuery);
    }
    let local = id
        .local_id
        .parse::<i64>()
        .map_err(|_| ProfileError::InvalidQuery)?;
    if local <= 0 {
        return Err(ProfileError::InvalidQuery);
    }
    let c = open_write(location, &id.database, keys, options.allow_plaintext)?;
    crate::writer::exact_columns(&c, &id.database, &id.table, crate::writer::MESSAGE_COLUMNS)?;
    c.query_row(&format!("SELECT m.server_id,m.create_time,m.local_type,m.origin_source,n.user_name FROM {} m JOIN Name2Id n ON n.rowid=m.real_sender_id WHERE m.local_id=?1",crate::schema::quote_identifier(&id.table)),[local],|r|Ok(MessageBinding{local,server:r.get(0)?,time:r.get(1)?,kind:r.get(2)?,origin:r.get(3)?,sender:r.get(4)?})).optional()?.ok_or(ProfileError::NotCached)
}
/// Preserve the original encoded voice, independently of any WAV derivative.
/// The supported media shard is selected only when no other media ordinal exists.
/// The message row must already exist and be type 34 in its own table namespace.
pub fn cache_voice_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    chat: &str,
    encoded: &[u8],
    options: &WriteOptions,
) -> Result<AppliedVoiceCache> {
    ownership(location, keys, options)?;
    if encoded.is_empty() || encoded.len() > 16 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let binding = message_binding(location, keys, source, chat, options)?;
    if binding.kind & 0x7fffffff != 34 {
        return Err(ProfileError::InvalidQuery);
    }
    let db = find_database(location, "media_0.db")?;
    // The inspected adapter has no media-shard rollover algorithm.
    let parent = location
        .storage
        .join(&db)
        .parent()
        .ok_or(ProfileError::InvalidProfile)?
        .to_path_buf();
    let count = fs::read_dir(parent)?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.strip_prefix("media_")
                    .and_then(|n| n.strip_suffix(".db"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .count();
    if count != 1 {
        return Err(ProfileError::WritesUnsupported);
    }
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "VoiceInfo")?;
    let tx = c.transaction()?;
    let chat_id = map_id(&tx, "Name2Id", "user_name", chat)?;
    let existing=tx.query_row("SELECT svr_id,voice_data FROM VoiceInfo WHERE chat_name_id=?1 AND create_time=?2 AND local_id=?3 AND data_index='0'",params![chat_id,binding.time,binding.local],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?))).optional()?;
    let changed = if let Some((sid, bytes)) = existing {
        if sid != binding.server || bytes != encoded {
            return Err(ProfileError::InvalidMetadata);
        }
        false
    } else {
        tx.execute("INSERT INTO VoiceInfo(chat_name_id,create_time,local_id,svr_id,voice_data,data_index)VALUES(?1,?2,?3,?4,?5,'0')",params![chat_id,binding.time,binding.local,binding.server,encoded])?;
        true
    };
    tx.commit()?;
    Ok(AppliedVoiceCache {
        database: db,
        changed,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HardlinkKind {
    Image,
    File,
    Video,
}
impl HardlinkKind {
    pub(crate) fn table(self) -> &'static str {
        match self {
            Self::Image => "image_hardlink_info_v4",
            Self::File => "file_hardlink_info_v4",
            Self::Video => "video_hardlink_info_v4",
        }
    }
    fn directory(self) -> Result<&'static str> {
        match self {
            Self::File => Ok("msg/file"),
            Self::Video => Ok("msg/video"),
            Self::Image => Err(ProfileError::InvalidQuery),
        }
    }
}
pub(crate) fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.contains(['\\', '\0'])
        && Path::new(value).components().count() == 1
        && Path::new(value)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}
pub(crate) fn digest_hex(value: &str, n: usize) -> bool {
    value.len() == n && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn open_cache(
    root: &Path,
    relative: &Path,
    max: usize,
) -> Result<(fs::File, fs::Metadata, std::path::PathBuf)> {
    if !crate::keys::valid_relative_path(relative.to_str().ok_or(ProfileError::InvalidMetadata)?) {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut here = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (i, part) in components.iter().enumerate() {
        here.push(part.as_os_str());
        let m = fs::symlink_metadata(&here).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ProfileError::NotCached
            } else {
                e.into()
            }
        })?;
        if wechat_platform::is_link(&m)
            || if i + 1 == components.len() {
                !m.is_file()
            } else {
                !m.is_dir()
            }
        {
            return Err(ProfileError::UnsupportedFile);
        }
    }
    let mut o = wechat_platform::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        o.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let f = o.open(&here)?;
    let before = f.metadata()?;
    if before.len() > max as u64 {
        return Err(ProfileError::ResourceLimit);
    }
    Ok((f, before, here))
}
fn cache_stable(file: &fs::File, before: &fs::Metadata, path: &Path) -> Result<()> {
    let after = file.metadata()?;
    if before.len() != after.len()
        || before.modified()? != after.modified()?
        || !wechat_platform::same_path_file(path, file)?
    {
        return Err(ProfileError::SourceChanged);
    }
    Ok(())
}
pub(crate) fn read_cache(root: &Path, relative: &Path, max: usize) -> Result<Vec<u8>> {
    let (mut file, before, path) = open_cache(root, relative, max)?;
    let mut data = Vec::with_capacity(before.len() as usize);
    (&mut file).take(max as u64 + 1).read_to_end(&mut data)?;
    cache_stable(&file, &before, &path)?;
    if data.len() > max {
        return Err(ProfileError::ResourceLimit);
    }
    Ok(data)
}
fn cache_matches(root: &Path, relative: &Path, bytes: &[u8], max: usize) -> Result<bool> {
    let (mut file, before, path) = open_cache(root, relative, max)?;
    if before.len() != bytes.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0; 64 * 1024];
    for expected in bytes.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..expected.len()])?;
        if &buffer[..expected.len()] != expected {
            return Ok(false);
        }
    }
    cache_stable(&file, &before, &path)?;
    Ok(true)
}

impl ProfileReader {
    /// File/video hardlinks use one month directory and the observed zero second
    /// directory sentinel. Every candidate is content-MD5 verified; ambiguous bytes
    /// or unknown path layouts are refused rather than guessed.
    pub fn cached_hardlink_file(&self, kind: HardlinkKind, md5: &str) -> Result<CachedBytes> {
        let directory = kind.directory()?;
        if !digest_hex(md5, 32) {
            return Err(ProfileError::InvalidMetadata);
        }
        let db = self
            .snapshot
            .inventory
            .iter()
            .filter(|d| d.relative_path.rsplit('/').next() == Some("hardlink.db"))
            .collect::<Vec<_>>();
        if db.len() != 1 {
            return Err(ProfileError::NotCached);
        }
        let db = &db[0].relative_path;
        let c = self.snapshot.open(db)?;
        let mut q=c.prepare(&format!("SELECT d.username,h.file_name,h.file_size,h.dir2 FROM {} h JOIN dir2id d ON d.rowid=h.dir1 WHERE lower(h.md5)=?1 LIMIT 201",kind.table()))?;
        let rows = q
            .query_map([md5.to_ascii_lowercase()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > 200 {
            return Err(ProfileError::ResourceLimit);
        }
        let mut found = None;
        for (month, name, size, dir2) in rows {
            if dir2 != 0
                || !valid_component(&month)
                || !valid_component(&name)
                || month.len() != 7
                || month.as_bytes()[4] != b'-'
            {
                return Err(ProfileError::InvalidMetadata);
            }
            let bytes = match read_cache(
                &self.snapshot.location.root,
                &Path::new(directory).join(month).join(name),
                hardlink_limit(kind),
            ) {
                Ok(b) => b,
                Err(ProfileError::NotCached) => continue,
                Err(e) => return Err(e),
            };
            if size < 0
                || bytes.len() as u64 != size as u64
                || format!("{:x}", Md5::digest(&bytes)) != md5.to_ascii_lowercase()
            {
                return Err(ProfileError::InvalidMetadata);
            }
            if found.as_ref().is_some_and(|old: &Vec<u8>| old != &bytes) {
                return Err(ProfileError::InvalidMetadata);
            }
            found = Some(bytes);
        }
        let bytes = found.ok_or(ProfileError::NotCached)?;
        Ok(CachedBytes {
            media_type: crate::images::validate_image(&bytes)
                .map(|v| v.0)
                .unwrap_or_else(|_| {
                    if kind == HardlinkKind::Video && bytes.get(4..8) == Some(b"ftyp") {
                        "video/mp4".into()
                    } else {
                        "application/octet-stream".into()
                    }
                }),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            source_database: db.clone(),
            source_table: kind.table().into(),
        })
    }
}

/// Avatar hashes identify the native source URL, not the image-content digest.
/// A caller supplies the observed URL hash explicitly; it is never inferred from
/// the bytes. No contact/group field or remote avatar is modified.
pub struct AvatarProjection<'a> {
    pub username: &'a str,
    pub source_url_md5: &'a str,
    pub bytes: &'a [u8],
    pub update_time: i64,
}
pub fn apply_avatar_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &AvatarProjection<'_>,
    options: &WriteOptions,
) -> Result<bool> {
    ownership(location, keys, options)?;
    identity(input.username)?;
    if !digest_hex(input.source_url_md5, 32)
        || input.bytes.is_empty()
        || input.bytes.len() > 16 * 1024 * 1024
    {
        return Err(ProfileError::InvalidMetadata);
    }
    crate::images::validate_image(input.bytes)?;
    let db = find_database(location, "head_image.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "head_image")?;
    let tx = c.transaction()?;
    let old = tx
        .query_row(
            "SELECT md5,image_buffer,update_time FROM head_image WHERE username=?1",
            [input.username],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    if old.as_ref().is_some_and(|(hash, bytes, time)| {
        hash == input.source_url_md5 && bytes == input.bytes && *time == input.update_time
    }) {
        return Ok(false);
    }
    tx.execute("INSERT INTO head_image(username,md5,image_buffer,update_time)VALUES(?1,?2,?3,?4)ON CONFLICT(username)DO UPDATE SET md5=excluded.md5,image_buffer=excluded.image_buffer,update_time=excluded.update_time",params![input.username,input.source_url_md5,input.bytes,input.update_time])?;
    tx.commit()?;
    Ok(true)
}

/// Exact native resource metadata. The destination allocates all local IDs and
/// translates names. Detail type/status/data_index and opaque protobuf bytes are
/// explicit build-adapter inputs rather than guessed media-wide defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaResourceProjection {
    pub source: SourceMessageId,
    pub chat: String,
    pub packed_info: Vec<u8>,
    pub details: Vec<RawFields>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedMediaResource {
    pub message_id: i64,
    pub detail_ids: Vec<i64>,
    pub changed: bool,
}
fn insert_values(c: &rusqlite::Connection, table: &str, fields: &RawFields) -> Result<i64> {
    let mut values = vec![];
    let mut expressions = vec![];
    for value in fields.values() {
        let (v, text) = crate::projection::bound(value)?;
        values.push(v);
        let p = format!("?{}", values.len());
        expressions.push(if text {
            format!("CAST({p} AS TEXT)")
        } else {
            p
        });
    }
    c.execute(
        &format!(
            "INSERT INTO {}({})VALUES({})",
            crate::schema::quote_identifier(table),
            fields
                .keys()
                .map(|k| crate::schema::quote_identifier(k))
                .collect::<Vec<_>>()
                .join(","),
            expressions.join(",")
        ),
        rusqlite::params_from_iter(values),
    )?;
    Ok(c.last_insert_rowid())
}
fn complete_fields(
    c: &rusqlite::Connection,
    table: &str,
    excluded: &[&str],
    fields: &RawFields,
) -> Result<()> {
    check_known_table(c, table)?;
    let report = crate::schema::inspect(c, "media")?;
    let t = report
        .tables
        .iter()
        .find(|t| t.name == table)
        .ok_or(ProfileError::InvalidMetadata)?;
    let expected = t
        .columns
        .iter()
        .map(|col| col.name.as_str())
        .filter(|col| !excluded.contains(col))
        .collect::<std::collections::BTreeSet<_>>();
    if fields
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>()
        != expected
    {
        return Err(ProfileError::InvalidMetadata);
    }
    for v in fields.values() {
        crate::projection::bound(v)?;
    }
    Ok(())
}
fn raw_row(
    c: &rusqlite::Connection,
    table: &str,
    key_column: &str,
    id: i64,
    excluded: &[&str],
) -> Result<RawFields> {
    let mut values = c.query_row(
        &format!(
            "SELECT * FROM {} WHERE {}=?1",
            crate::schema::quote_identifier(table),
            crate::schema::quote_identifier(key_column)
        ),
        [id],
        crate::reader::raw,
    )?;
    for col in excluded {
        values.remove(*col);
    }
    Ok(values)
}
fn resource_detail_rows(c: &rusqlite::Connection, message: i64) -> Result<Vec<(i64, RawFields)>> {
    let ids=c.prepare("SELECT resource_id FROM MessageResourceDetail WHERE message_id=?1 ORDER BY type,data_index,resource_id LIMIT 33")?.query_map([message],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if ids.len() > 32 {
        return Err(ProfileError::ResourceLimit);
    }
    ids.into_iter()
        .map(|id| {
            Ok((
                id,
                raw_row(
                    c,
                    "MessageResourceDetail",
                    "resource_id",
                    id,
                    &["resource_id", "message_id"],
                )?,
            ))
        })
        .collect()
}
pub fn apply_media_resource_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &MediaResourceProjection,
    options: &WriteOptions,
) -> Result<AppliedMediaResource> {
    apply_media_resource_set(location, keys, input, options, false)
}
/// Add one or more verified resource variants without erasing other qualities.
/// Only a matching pending (status0,size0) detail can transition to downloaded.
pub fn apply_media_resource_variants(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &MediaResourceProjection,
    options: &WriteOptions,
) -> Result<AppliedMediaResource> {
    apply_media_resource_set(location, keys, input, options, true)
}
#[allow(clippy::collapsible_else_if)]
fn apply_media_resource_set(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &MediaResourceProjection,
    options: &WriteOptions,
    partial: bool,
) -> Result<AppliedMediaResource> {
    ownership(location, keys, options)?;
    if input.packed_info.len() > 1024 * 1024 || input.details.len() > 32 {
        return Err(ProfileError::ResourceLimit);
    }
    crate::projection::proto_fields(&input.packed_info)?;
    let binding = message_binding(location, keys, &input.source, &input.chat, options)?;
    let db = find_database(location, "message_resource.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "MessageResourceInfo")?;
    check_known_table(&c, "MessageResourceDetail")?;
    check_known_table(&c, "ChatName2Id")?;
    check_known_table(&c, "SenderName2Id")?;
    for detail in &input.details {
        complete_fields(
            &c,
            "MessageResourceDetail",
            &["resource_id", "message_id"],
            detail,
        )?;
    }
    let tx = c.transaction()?;
    let chat = map_id(&tx, "ChatName2Id", "user_name", &input.chat)?;
    let sender = map_id(&tx, "SenderName2Id", "user_name", &binding.sender)?;
    let mut fields = RawFields::new();
    for (k, n) in [
        ("chat_id", chat),
        ("sender_id", sender),
        ("message_local_type", binding.kind),
        ("message_create_time", binding.time),
        ("message_local_id", binding.local),
        ("message_svr_id", binding.server),
        ("message_origin_source", binding.origin),
    ] {
        fields.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    fields.insert(
        "packed_info".into(),
        SqlValue::Blob(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &input.packed_info,
        )),
    );
    complete_fields(&tx, "MessageResourceInfo", &["message_id"], &fields)?;
    let ids=tx.prepare("SELECT message_id FROM MessageResourceInfo WHERE chat_id=?1 AND message_create_time=?2 AND message_local_id=?3 LIMIT 3")?.query_map(params![chat,binding.time,binding.local],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if ids.len() > 2 {
        return Err(ProfileError::InvalidMetadata);
    }
    if ids.len() == 2 {
        // Stock profiles can retain two exact resource aliases for one message.
        // Accept only a fully satisfied replay across every alias. Never merge,
        // delete, or partially update duplicate/pending/divergent resources.
        let mut first: Option<Vec<(i64, RawFields)>> = None;
        let mut first_ids = Vec::new();
        for id in &ids {
            if raw_row(
                &tx,
                "MessageResourceInfo",
                "message_id",
                *id,
                &["message_id"],
            )? != fields
            {
                return Err(ProfileError::InvalidMetadata);
            }
            let rows = resource_detail_rows(&tx, *id)?;
            if let Some(first) = &first {
                if first.iter().map(|(_, r)| r).collect::<Vec<_>>()
                    != rows.iter().map(|(_, r)| r).collect::<Vec<_>>()
                {
                    return Err(ProfileError::InvalidMetadata);
                }
            }
            if !partial && rows.len() != input.details.len() {
                return Err(ProfileError::InvalidMetadata);
            }
            let mut matched = Vec::new();
            for detail in &input.details {
                let matches = rows
                    .iter()
                    .filter(|(_, row)| row == detail)
                    .collect::<Vec<_>>();
                if matches.len() != 1 {
                    return Err(ProfileError::InvalidMetadata);
                }
                matched.push(matches[0].0);
            }
            if first.is_none() {
                first_ids = matched;
                first = Some(rows);
            }
        }
        tx.commit()?;
        return Ok(AppliedMediaResource {
            message_id: ids[0],
            detail_ids: first_ids,
            changed: false,
        });
    }
    let mut changed = false;
    let message_id = if let Some(id) = ids.first() {
        if raw_row(
            &tx,
            "MessageResourceInfo",
            "message_id",
            *id,
            &["message_id"],
        )? != fields
        {
            return Err(ProfileError::InvalidMetadata);
        }
        *id
    } else {
        changed = true;
        insert_values(&tx, "MessageResourceInfo", &fields)?
    };
    let existing=tx.prepare("SELECT resource_id FROM MessageResourceDetail WHERE message_id=?1 ORDER BY resource_id")?.query_map([message_id],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let mut detail_ids = vec![];
    // This projects a complete exact set. A later partial transfer must supply a
    // separate proved transition; it cannot silently replace existing cache rows.
    if partial {
        for supplied in &input.details {
            let kind = supplied.get("type").ok_or(ProfileError::InvalidMetadata)?;
            let index = supplied
                .get("data_index")
                .ok_or(ProfileError::InvalidMetadata)?;
            let mut matched = None;
            for id in &existing {
                let old = raw_row(
                    &tx,
                    "MessageResourceDetail",
                    "resource_id",
                    *id,
                    &["resource_id", "message_id"],
                )?;
                if old.get("type") == Some(kind) && old.get("data_index") == Some(index) {
                    if matched.is_some() {
                        return Err(ProfileError::InvalidMetadata);
                    }
                    matched = Some((*id, old));
                }
            }
            if let Some((id, old)) = matched {
                if old != *supplied {
                    let zero = SqlValue::Integer("0".into());
                    let packed_changed = old.get("packed_info") != supplied.get("packed_info");
                    let file_completion = binding.kind as u64 == (6u64 << 32) | 49
                        && crate::reader::num(&old, "type").is_some_and(|n| n & 0xffff == 3)
                        && (!packed_changed || file_name_completion(&old, supplied)?);
                    if old.get("status") != Some(&zero)
                        || (old.get("size") != Some(&zero)
                            && !(file_completion && old.get("size") == supplied.get("size")))
                        || supplied.get("status") != Some(&SqlValue::Integer("1".into()))
                        || (packed_changed && !file_completion)
                        || old.iter().any(|(k, v)| {
                            !["status", "size", "access_time", "packed_info"].contains(&k.as_str())
                                && supplied.get(k) != Some(v)
                        })
                    {
                        return Err(ProfileError::InvalidMetadata);
                    }
                    let (size, _) = crate::projection::bound(&supplied["size"])?;
                    let (access, _) = crate::projection::bound(&supplied["access_time"])?;
                    let (packed, _) = crate::projection::bound(&supplied["packed_info"])?;
                    tx.execute("UPDATE MessageResourceDetail SET status=1,size=?1,access_time=?2,packed_info=?3 WHERE resource_id=?4",params![size,access,packed,id])?;
                    changed = true;
                }
                detail_ids.push(id);
            } else {
                let mut fields = supplied.clone();
                fields.insert(
                    "message_id".into(),
                    SqlValue::Integer(message_id.to_string()),
                );
                detail_ids.push(insert_values(&tx, "MessageResourceDetail", &fields)?);
                changed = true;
            }
        }
    } else {
        if !existing.is_empty() {
            if existing.len() != input.details.len() {
                return Err(ProfileError::InvalidMetadata);
            }
            for (id, fields) in existing.into_iter().zip(&input.details) {
                if raw_row(
                    &tx,
                    "MessageResourceDetail",
                    "resource_id",
                    id,
                    &["resource_id", "message_id"],
                )? != *fields
                {
                    return Err(ProfileError::InvalidMetadata);
                }
                detail_ids.push(id);
            }
        } else {
            for detail in &input.details {
                let mut fields = detail.clone();
                fields.insert(
                    "message_id".into(),
                    SqlValue::Integer(message_id.to_string()),
                );
                detail_ids.push(insert_values(&tx, "MessageResourceDetail", &fields)?);
                changed = true;
            }
        }
    }
    tx.commit()?;
    Ok(AppliedMediaResource {
        message_id,
        detail_ids,
        changed,
    })
}

/// Native hardlink fields, excluding _rowid_/dir1/dir2. Directory numbers are
/// always allocated in the destination. `hardlink_md5_hash` computes the native
/// string hash; type/extra_buffer remain explicit in this low-level interface.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HardlinkProjection {
    pub kind: HardlinkKind,
    pub first_directory: String,
    pub second_directory: Option<String>,
    pub fields: RawFields,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedHardlink {
    pub row_id: i64,
    pub changed: bool,
}
pub fn apply_hardlink_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &HardlinkProjection,
    options: &WriteOptions,
) -> Result<AppliedHardlink> {
    ownership(location, keys, options)?;
    if !valid_component(&input.first_directory)
        || input
            .second_directory
            .as_ref()
            .is_some_and(|s| !valid_component(s))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let filename = match input.fields.get("file_name") {
        Some(SqlValue::Text(s)) if valid_component(s) => s,
        _ => return Err(ProfileError::InvalidMetadata),
    };
    let md5 = match input.fields.get("md5") {
        Some(SqlValue::Text(s)) if digest_hex(s, 32) => s,
        _ => return Err(ProfileError::InvalidMetadata),
    };
    let db = find_database(location, "hardlink.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "dir2id")?;
    complete_fields(
        &c,
        input.kind.table(),
        &["_rowid_", "dir1", "dir2"],
        &input.fields,
    )?;
    let tx = c.transaction()?;
    let dir1 = map_id(&tx, "dir2id", "username", &input.first_directory)?;
    let dir2 = if let Some(s) = &input.second_directory {
        map_id(&tx, "dir2id", "username", s)?
    } else {
        0
    };
    let mut fields = input.fields.clone();
    fields.insert("dir1".into(), SqlValue::Integer(dir1.to_string()));
    fields.insert("dir2".into(), SqlValue::Integer(dir2.to_string()));
    let ids = tx
        .prepare(&format!(
            "SELECT _rowid_ FROM {} WHERE md5=?1 AND file_name=?2 AND dir1=?3 AND dir2=?4",
            input.kind.table()
        ))?
        .query_map(params![md5, filename, dir1, dir2], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.len() > 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    let (row_id, changed) = if let Some(id) = ids.first() {
        if raw_row(&tx, input.kind.table(), "_rowid_", *id, &["_rowid_"])? != fields {
            return Err(ProfileError::InvalidMetadata);
        }
        (*id, false)
    } else {
        (insert_values(&tx, input.kind.table(), &fields)?, true)
    };
    tx.commit()?;
    Ok(AppliedHardlink { row_id, changed })
}

/// Create one content-verified cache file without replacing any existing path.
/// The adapter supplies a native relative path, encoded bytes and expected SHA.
/// Atomic publication is idempotent; SQL resource registration is a later step.
pub fn publish_cache_file(
    location: &ProfileLocation,
    relative: &str,
    bytes: &[u8],
    expected_sha256: &str,
    options: &WriteOptions,
) -> Result<bool> {
    if !options.exclusive_ownership_confirmed || options.durable_intent_id.is_empty() {
        return Err(ProfileError::SnapshotRequired);
    }
    if !crate::keys::valid_relative_path(relative)
        || !relative.starts_with("msg/")
        || !digest_hex(expected_sha256, 64)
        || bytes.is_empty()
        || bytes.len() > cache_path_limit(relative)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    if hex::encode(Sha256::digest(bytes)) != expected_sha256.to_ascii_lowercase() {
        return Err(ProfileError::InvalidMetadata);
    }
    let target = location.root.join(relative);
    let parent = target.parent().ok_or(ProfileError::InvalidMetadata)?;
    let mut here = location.root.clone();
    for p in parent
        .strip_prefix(&location.root)
        .map_err(|_| ProfileError::InvalidMetadata)?
        .components()
    {
        here.push(p.as_os_str());
        match fs::symlink_metadata(&here) {
            Ok(m) => {
                if !m.is_dir() || wechat_platform::is_link(&m) {
                    return Err(ProfileError::UnsupportedFile);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&here)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&here, fs::Permissions::from_mode(0o700))?;
                }
                wechat_platform::sync_dir(here.parent().ok_or(ProfileError::InvalidMetadata)?)?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    if fs::symlink_metadata(&target).is_ok() {
        if cache_matches(
            &location.root,
            Path::new(relative),
            bytes,
            cache_path_limit(relative),
        )? {
            return Ok(false);
        }
        return Err(ProfileError::SourceChanged);
    }
    use std::io::Write;
    let mut f = wechat_platform::private_tempfile_in(parent, ".wx-media-")?;
    f.write_all(bytes)?;
    f.as_file().sync_all()?;
    f.persist_noclobber(&target)
        .map_err(|e| ProfileError::Io(e.error))?;
    wechat_platform::sync_dir(parent)?;
    Ok(true)
}

/// Native 32-bit SuperFastHash of the 32 ASCII hex characters, not decoded MD5.
/// SQLite stores its unsigned result as a nonnegative 64-bit INTEGER.
pub fn hardlink_md5_hash(md5: &str) -> Result<u32> {
    if !digest_hex(md5, 32) {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut hash = md5.len() as u32;
    for block in md5.as_bytes().chunks_exact(4) {
        let a = u16::from_le_bytes([block[0], block[1]]) as u32;
        let b = u16::from_le_bytes([block[2], block[3]]) as u32;
        hash = hash.wrapping_add(a);
        let tmp = (b << 11) ^ hash;
        hash = (hash << 16) ^ tmp;
        hash = hash.wrapping_add(hash >> 11);
    }
    hash ^= hash << 3;
    hash = hash.wrapping_add(hash >> 5);
    hash ^= hash << 4;
    hash = hash.wrapping_add(hash >> 17);
    hash ^= hash << 25;
    hash = hash.wrapping_add(hash >> 6);
    Ok(hash)
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedImageCache {
    pub relative_path: String,
    pub changed: bool,
    pub plaintext_bytes: usize,
    pub encoded_bytes: usize,
}
/// Cache one actual JPEG/PNG quality under a deterministic local basename. The
/// observed DAT/resource/hardlink representation is retained; basename allocation
/// and UTC month are local policies, not a claim about the native generator.
/// No other variant is removed. Original bytes must match the message XML MD5.
pub fn cache_image_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    chat: &str,
    quality: ImageQuality,
    plaintext: &[u8],
    options: &WriteOptions,
) -> Result<AppliedImageCache> {
    ownership(location, keys, options)?;
    let binding = message_binding(location, keys, source, chat, options)?;
    if binding.kind & 0x7fffffff != 3 {
        return Err(ProfileError::InvalidQuery);
    }
    crate::images::validate_image(plaintext)?;
    let (suffix, kind) = match quality {
        ImageQuality::Original => ("_h", 65537),
        ImageQuality::Standard => ("", 131073),
        ImageQuality::Thumbnail => ("_t", 262145),
        _ => return Err(ProfileError::InvalidQuery),
    };
    let key = keys
        .image_dat
        .as_ref()
        .ok_or(ProfileError::ImageKeyUnavailable)?;
    let encoded = crate::encode_dat_v2(plaintext, key, None)?;
    let hash = format!("{:x}", Md5::digest(plaintext));
    if quality == ImageQuality::Original {
        let c = open_write(location, &source.database, keys, options.allow_plaintext)?;
        let row = c.query_row(
            &format!(
                "SELECT * FROM {} WHERE local_id=?1",
                crate::schema::quote_identifier(&source.table)
            ),
            [binding.local],
            crate::reader::raw,
        )?;
        let decoded = crate::reader::decode(
            &row,
            "message_content",
            "WCDB_CT_message_content",
            4 * 1024 * 1024,
        )?;
        if crate::images::image_md5(
            decoded
                .text
                .as_deref()
                .ok_or(ProfileError::InvalidMetadata)?,
        )?
        .as_deref()
            != Some(hash.as_str())
        {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let db = find_database(location, "message_resource.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    let mut rows=c.prepare("SELECT i.packed_info FROM MessageResourceInfo i JOIN ChatName2Id n ON n.rowid=i.chat_id WHERE n.user_name=?1 AND i.message_local_id=?2 AND i.message_create_time=?3 AND i.message_svr_id=?4 LIMIT 3")?.query_map(params![chat,binding.local,binding.time,binding.server],|r|r.get::<_,Vec<u8>>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if rows.len() > 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    let (base, packed) = if let Some(packed) = rows.pop() {
        let mut bases = vec![];
        for (f, w, b) in crate::projection::proto_fields(&packed)? {
            if f == 2 && w == 2 {
                for (f, w, b) in crate::projection::proto_fields(b)? {
                    if f == 1 && w == 2 {
                        let text = String::from_utf8(b.to_vec())
                            .map_err(|_| ProfileError::InvalidMetadata)?;
                        if !digest_hex(&text, 32) {
                            return Err(ProfileError::InvalidMetadata);
                        }
                        bases.push(text);
                    }
                }
            }
        }
        if bases.len() != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        (bases.remove(0), packed)
    } else {
        let base = format!(
            "{:x}",
            Md5::digest(
                format!(
                    "profile-image-v1\0{}\0{}\0{}",
                    source.database, source.table, source.local_id
                )
                .as_bytes()
            )
        );
        let mut packed = vec![18, 34, 10, 32];
        packed.extend(base.as_bytes());
        (base, packed)
    };
    drop(c);
    let month = chrono::DateTime::from_timestamp(binding.time, 0)
        .ok_or(ProfileError::InvalidMetadata)?
        .format("%Y-%m")
        .to_string();
    let directory = format!("{:x}", Md5::digest(chat.as_bytes()));
    let filename = format!("{base}{suffix}.dat");
    let relative_path = format!("msg/attach/{directory}/{month}/Img/{filename}");
    let mut changed = publish_cache_file(
        location,
        &relative_path,
        &encoded,
        &hex::encode(Sha256::digest(&encoded)),
        options,
    )?;
    // Native image extra_buffer field2 records encoded DAT size; file_size is plain.
    let mut extra = vec![16];
    write_varint(encoded.len() as u64, &mut extra);
    let mut fields = RawFields::new();
    for (k, n) in [
        ("md5_hash", hardlink_md5_hash(&hash)? as i64),
        ("type", 2),
        ("file_size", plaintext.len() as i64),
        ("modify_time", binding.time),
    ] {
        fields.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    fields.insert("md5".into(), SqlValue::Text(hash));
    fields.insert("file_name".into(), SqlValue::Text(filename));
    fields.insert(
        "extra_buffer".into(),
        SqlValue::Blob(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            extra,
        )),
    );
    changed |= apply_hardlink_projection(
        location,
        keys,
        &HardlinkProjection {
            kind: HardlinkKind::Image,
            first_directory: directory,
            second_directory: Some(month),
            fields,
        },
        options,
    )?
    .changed;
    let mut detail = RawFields::new();
    for (k, n) in [
        ("type", kind),
        ("size", encoded.len() as i64),
        ("create_time", binding.time),
        ("access_time", 0),
        ("status", 1),
    ] {
        detail.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    detail.insert("data_index".into(), SqlValue::Text("0".into()));
    detail.insert("packed_info".into(), SqlValue::Blob(String::new()));
    changed |= apply_media_resource_variants(
        location,
        keys,
        &MediaResourceProjection {
            source: source.clone(),
            chat: chat.into(),
            packed_info: packed,
            details: vec![detail],
        },
        options,
    )?
    .changed;
    Ok(AppliedImageCache {
        relative_path,
        changed,
        plaintext_bytes: plaintext.len(),
        encoded_bytes: encoded.len(),
    })
}
fn write_varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}

/// Resource categories from the supported Linux build's extension registry. An
/// unknown extension uses its observed generic category; these are file resource
/// types, independently of the file hardlink table's constant type 1.
/// Supported Linux 4.1.13.26 AMD64: registry 0x7554e70, classifier 0x7558600,
/// resource constructors 0x755c1ca/0x755d66f (category << 16 | 3).
pub fn file_resource_type(extension: &str) -> u32 {
    let category = match extension.to_ascii_lowercase().as_str() {
        "zip" | "7z" | "rar" | "gz" | "tar" | "z" => 1,
        "mp3" | "flac" | "ape" | "wav" | "wma" | "aac" | "amr" | "mmf" | "ram" | "au" | "aif"
        | "ogg" | "m4a" => 11,
        "dmg" => 21,
        "pkg" => 22,
        "exe" | "msi" | "com" => 23,
        "bat" => 24,
        "xls" | "xlsx" | "xlsm" | "csv" => 31,
        "numbers" => 32,
        "ods" => 33,
        "ppt" | "pptx" | "pps" | "ppsm" | "ppsx" | "pptm" => 41,
        "key" => 42,
        "odp" => 43,
        "pdf" => 51,
        "doc" | "docx" | "docm" => 52,
        "pages" => 53,
        "wps" => 54,
        "odt" => 55,
        "rtf" => 56,
        "txt" => 57,
        "log" | "md" | "m" | "mm" | "c" | "cc" | "cpp" | "h" | "hpp" => 58,
        _ => 0,
    };
    category << 16 | 3
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoQuality {
    Normal,
    Original,
    Thumbnail,
}
impl VideoQuality {
    fn resource_type(self) -> i64 {
        match self {
            Self::Normal => 131074,
            Self::Original => 65538,
            Self::Thumbnail => 196610,
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Self::Normal => ".mp4",
            Self::Original => "_raw.mp4",
            Self::Thumbnail => ".jpg",
        }
    }
}

// Only recognized leaf paths/attributes are interpreted. All signed URLs and
// unrelated message metadata stay in the original protected message row.
#[derive(Default)]
struct AttachmentXml {
    leaves: std::collections::BTreeMap<String, String>,
    video: Option<std::collections::BTreeMap<String, String>>,
}
fn attachment_xml(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    binding: &MessageBinding,
    options: &WriteOptions,
) -> Result<AttachmentXml> {
    use quick_xml::{events::Event, Reader};
    let c = open_write(location, &source.database, keys, options.allow_plaintext)?;
    let row = c.query_row(
        &format!(
            "SELECT * FROM {} WHERE local_id=?1",
            crate::schema::quote_identifier(&source.table)
        ),
        [binding.local],
        crate::reader::raw,
    )?;
    let decoded = crate::reader::decode(
        &row,
        "message_content",
        "WCDB_CT_message_content",
        1024 * 1024,
    )?;
    let xml = decoded
        .text
        .as_deref()
        .ok_or(ProfileError::InvalidMetadata)?;
    let xml = xml.trim_start();
    let xml = if xml.starts_with('<') {
        xml
    } else {
        let (sender, rest) = xml.split_once(":\n").ok_or(ProfileError::InvalidMetadata)?;
        identity(sender)?;
        rest
    };
    let mut r = Reader::from_str(xml);
    let mut stack = Vec::new();
    let mut out = AttachmentXml::default();
    let mut count = 0;
    let mut roots = 0;
    loop {
        match r.read_event() {
            Ok(Event::DocType(_)) => return Err(ProfileError::InvalidMetadata),
            Ok(event @ (Event::Start(_) | Event::Empty(_))) => {
                let empty = matches!(event, Event::Empty(_));
                let e = match event {
                    Event::Start(e) | Event::Empty(e) => e,
                    _ => unreachable!(),
                };
                count += 1;
                if count > 4096 || stack.len() >= 64 {
                    return Err(ProfileError::ResourceLimit);
                }
                let name = std::str::from_utf8(e.name().as_ref())
                    .map_err(|_| ProfileError::InvalidMetadata)?
                    .to_string();
                if stack.is_empty() {
                    roots += 1;
                    if roots != 1 || name != "msg" {
                        return Err(ProfileError::InvalidMetadata);
                    }
                }
                let path = stack
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once(name.as_str()))
                    .collect::<Vec<_>>()
                    .join("/");
                if path == "msg/videomsg" {
                    if out.video.is_some() {
                        return Err(ProfileError::InvalidMetadata);
                    }
                    let mut attrs = std::collections::BTreeMap::new();
                    for a in e.attributes() {
                        let a = a.map_err(|_| ProfileError::InvalidMetadata)?;
                        let name = std::str::from_utf8(a.key.as_ref())
                            .map_err(|_| ProfileError::InvalidMetadata)?;
                        if [
                            "length",
                            "md5",
                            "rawlength",
                            "rawmd5",
                            "cdnthumblength",
                            "cdnthumbmd5",
                        ]
                        .contains(&name)
                        {
                            let value = a
                                .decode_and_unescape_value(r.decoder())
                                .map_err(|_| ProfileError::InvalidMetadata)?
                                .into_owned();
                            if attrs.insert(name.to_string(), value).is_some() {
                                return Err(ProfileError::InvalidMetadata);
                            }
                        }
                    }
                    out.video = Some(attrs);
                }
                if matches!(
                    path.as_str(),
                    "msg/appmsg/title"
                        | "msg/appmsg/md5"
                        | "msg/appmsg/type"
                        | "msg/appmsg/appattach/totallen"
                        | "msg/appmsg/appattach/fileext"
                ) && out.leaves.insert(path, String::new()).is_some()
                {
                    return Err(ProfileError::InvalidMetadata);
                }
                // Empty elements do not have a following End event.
                if !empty {
                    stack.push(name);
                }
            }
            Ok(Event::Text(e)) => {
                let path = stack.join("/");
                if let Some(value) = out.leaves.get_mut(&path) {
                    let text = e.xml_content().map_err(|_| ProfileError::InvalidMetadata)?;
                    value.push_str(
                        &quick_xml::escape::unescape(&text)
                            .map_err(|_| ProfileError::InvalidMetadata)?,
                    );
                }
            }
            Ok(Event::CData(e)) => {
                if let Some(value) = out.leaves.get_mut(&stack.join("/")) {
                    value.push_str(&e.decode().map_err(|_| ProfileError::InvalidMetadata)?);
                }
            }
            Ok(Event::GeneralRef(e)) => {
                let name = e.decode().map_err(|_| ProfileError::InvalidMetadata)?;
                let escaped = format!("&{name};");
                let text = quick_xml::escape::unescape(&escaped)
                    .map_err(|_| ProfileError::InvalidMetadata)?;
                if let Some(value) = out.leaves.get_mut(&stack.join("/")) {
                    value.push_str(&text);
                }
            }
            Ok(Event::End(_)) => {
                stack.pop().ok_or(ProfileError::InvalidMetadata)?;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(ProfileError::InvalidMetadata),
        }
    }
    if roots != 1 || !stack.is_empty() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(out)
}
fn checked_decimal(s: &str) -> Result<u64> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ProfileError::InvalidMetadata);
    }
    s.parse().map_err(|_| ProfileError::InvalidMetadata)
}
fn bytes_match_metadata(
    bytes: &[u8],
    size: &str,
    md5: Option<&str>,
    require_md5: bool,
    max: usize,
) -> Result<String> {
    if bytes.is_empty() || bytes.len() > max || checked_decimal(size)? != bytes.len() as u64 {
        return Err(ProfileError::InvalidMetadata);
    }
    let digest = format!("{:x}", Md5::digest(bytes));
    if let Some(expected) = md5.filter(|s| !s.is_empty()) {
        if !digest_hex(expected, 32) || !expected.eq_ignore_ascii_case(&digest) {
            return Err(ProfileError::InvalidMetadata);
        }
    } else if require_md5 {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(digest)
}
fn protobuf_bytes(field: u32, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_varint((field as u64) << 3 | 2, &mut out);
    write_varint(bytes.len() as u64, &mut out);
    out.extend(bytes);
    out
}
fn file_detail_names(packed: &[u8]) -> Result<(String, Option<String>)> {
    let root = crate::projection::proto_fields(packed)?;
    if root.len() != 1 || root[0].0 != 1 || root[0].1 != 2 {
        return Err(ProfileError::InvalidMetadata);
    }
    let fields = crate::projection::proto_fields(root[0].2)?;
    if !(1..=2).contains(&fields.len())
        || fields[0].0 != 1
        || fields[0].1 != 2
        || fields.get(1).is_some_and(|f| f.0 != 2 || f.1 != 2)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let name = std::str::from_utf8(fields[0].2)
        .map_err(|_| ProfileError::InvalidMetadata)?
        .to_string();
    let local = fields
        .get(1)
        .map(|f| {
            std::str::from_utf8(f.2)
                .map(str::to_string)
                .map_err(|_| ProfileError::InvalidMetadata)
        })
        .transpose()?;
    if !valid_component(&name) || local.as_ref().is_some_and(|s| !valid_component(s)) {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok((name, local))
}
fn resource_snapshot(
    location: &ProfileLocation,
    keys: &KeyFile,
    binding: &MessageBinding,
    chat: &str,
    options: &WriteOptions,
) -> Result<Option<(Vec<u8>, Vec<RawFields>)>> {
    let db = find_database(location, "message_resource.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "MessageResourceInfo")?;
    check_known_table(&c, "MessageResourceDetail")?;
    let rows = c.prepare("SELECT i.message_id,i.packed_info FROM MessageResourceInfo i JOIN ChatName2Id n ON n.rowid=i.chat_id WHERE n.user_name=?1 AND i.message_local_id=?2 AND i.message_create_time=?3 AND i.message_svr_id=?4 LIMIT 3")?.query_map(params![chat,binding.local,binding.time,binding.server], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if rows.len() > 2 {
        return Err(ProfileError::InvalidMetadata);
    }
    if let Some((id, packed)) = rows.first() {
        let info = raw_row(
            &c,
            "MessageResourceInfo",
            "message_id",
            *id,
            &["message_id"],
        )?;
        let details = resource_detail_rows(&c, *id)?
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>();
        if rows.len() == 2
            && details.iter().any(|row| {
                crate::reader::num(row, "status") != Some(1)
                    || crate::reader::num(row, "size").is_none_or(|n| n <= 0)
            })
        {
            return Err(ProfileError::InvalidMetadata);
        }
        for (alias, _) in rows.iter().skip(1) {
            if raw_row(
                &c,
                "MessageResourceInfo",
                "message_id",
                *alias,
                &["message_id"],
            )? != info
                || resource_detail_rows(&c, *alias)?
                    .into_iter()
                    .map(|(_, r)| r)
                    .collect::<Vec<_>>()
                    != details
            {
                return Err(ProfileError::InvalidMetadata);
            }
        }
        Ok(Some((packed.clone(), details)))
    } else {
        Ok(None)
    }
}

fn resource_blob(fields: &RawFields, key: &str) -> Result<Option<Vec<u8>>> {
    match fields.get(key) {
        Some(SqlValue::Blob(value)) => {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value)
                .map(Some)
                .map_err(|_| ProfileError::InvalidMetadata)
        }
        Some(SqlValue::Null) | None => Ok(None),
        _ => Err(ProfileError::InvalidMetadata),
    }
}
fn file_name_completion(old: &RawFields, new: &RawFields) -> Result<bool> {
    let before = resource_blob(old, "packed_info")?.ok_or(ProfileError::InvalidMetadata)?;
    let after = resource_blob(new, "packed_info")?.ok_or(ProfileError::InvalidMetadata)?;
    let (original, local) = file_detail_names(&before)?;
    let (next_original, next_local) = file_detail_names(&after)?;
    Ok(original == next_original && local.is_none() && next_local.is_some())
}
fn retain_access(
    detail: &mut RawFields,
    existing: Option<&(Vec<u8>, Vec<RawFields>)>,
) -> Result<()> {
    if let Some((_, rows)) = existing {
        let mut matched = rows.iter().filter(|row| {
            row.get("type") == detail.get("type")
                && row.get("data_index") == detail.get("data_index")
        });
        if let Some(row) = matched.next() {
            if matched.next().is_some() {
                return Err(ProfileError::InvalidMetadata);
            }
            detail.insert(
                "access_time".into(),
                row.get("access_time")
                    .ok_or(ProfileError::InvalidMetadata)?
                    .clone(),
            );
        }
    }
    Ok(())
}
fn downloaded_detail(
    kind: i64,
    binding: &MessageBinding,
    length: usize,
    packed: &[u8],
) -> RawFields {
    let mut detail = RawFields::new();
    for (key, n) in [
        ("type", kind),
        ("size", length as i64),
        ("create_time", binding.time),
        ("access_time", 0),
        ("status", 1),
    ] {
        detail.insert(key.into(), SqlValue::Integer(n.to_string()));
    }
    detail.insert("data_index".into(), SqlValue::Text("0".into()));
    detail.insert(
        "packed_info".into(),
        SqlValue::Blob(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            packed,
        )),
    );
    detail
}
#[allow(clippy::too_many_arguments)]
fn publish_plain_hardlink(
    location: &ProfileLocation,
    keys: &KeyFile,
    binding: &MessageBinding,
    kind: HardlinkKind,
    filename: &str,
    plaintext: &[u8],
    hash: &str,
    options: &WriteOptions,
) -> Result<AppliedHardlinkCache> {
    let month = chrono::DateTime::from_timestamp(binding.time, 0)
        .ok_or(ProfileError::InvalidMetadata)?
        .format("%Y-%m")
        .to_string();
    let relative_path = format!("{}/{month}/{filename}", kind.directory()?);
    let mut changed = publish_cache_file(
        location,
        &relative_path,
        plaintext,
        &hex::encode(Sha256::digest(plaintext)),
        options,
    )?;
    let mut fields = RawFields::new();
    for (k, n) in [
        ("md5_hash", hardlink_md5_hash(hash)? as i64),
        ("type", if kind == HardlinkKind::File { 1 } else { 3 }),
        ("file_size", plaintext.len() as i64),
        ("modify_time", binding.time),
    ] {
        fields.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    fields.insert("md5".into(), SqlValue::Text(hash.into()));
    fields.insert("file_name".into(), SqlValue::Text(filename.into()));
    fields.insert("extra_buffer".into(), SqlValue::Blob(String::new()));
    // Existing native downloads record an independent filesystem modification
    // time. Preserve that value after validating this exact path and digest.
    let db = find_database(location, "hardlink.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, kind.table())?;
    let existing=c.prepare(&format!("SELECT h.modify_time FROM {} h JOIN dir2id d ON d.rowid=h.dir1 WHERE h.md5=?1 AND h.file_name=?2 AND d.username=?3 AND h.dir2=0 LIMIT 2",kind.table()))?.query_map(params![hash,filename,month],|r|r.get::<_,i64>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
    if existing.len() > 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    if let Some(time) = existing.first() {
        fields.insert("modify_time".into(), SqlValue::Integer(time.to_string()));
    }
    drop(c);
    changed |= apply_hardlink_projection(
        location,
        keys,
        &HardlinkProjection {
            kind,
            first_directory: month,
            second_directory: None,
            fields,
        },
        options,
    )?
    .changed;
    Ok(AppliedHardlinkCache {
        relative_path,
        changed,
        plaintext_bytes: plaintext.len(),
    })
}

/// Register an ordinary subtype6 attachment's verified bytes, resource row,
/// filename pair and native hardlink. A new collision-bounded local name is an
/// explicit allocation policy; an existing resource filename is retained.
pub fn cache_file_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    chat: &str,
    plaintext: &[u8],
    options: &WriteOptions,
) -> Result<AppliedHardlinkCache> {
    ownership(location, keys, options)?;
    let binding = message_binding(location, keys, source, chat, options)?;
    if binding.kind as u64 != (6u64 << 32) | 49 {
        return Err(ProfileError::InvalidQuery);
    }
    let xml = attachment_xml(location, keys, source, &binding, options)?;
    let leaf = |path: &str| {
        xml.leaves
            .get(path)
            .map(String::as_str)
            .ok_or(ProfileError::InvalidMetadata)
    };
    if leaf("msg/appmsg/type")? != "6" {
        return Err(ProfileError::InvalidMetadata);
    }
    let title = leaf("msg/appmsg/title")?;
    if !valid_component(title) || title.len() > 255 || title.chars().any(char::is_control) {
        return Err(ProfileError::InvalidMetadata);
    }
    let hash = bytes_match_metadata(
        plaintext,
        leaf("msg/appmsg/appattach/totallen")?,
        Some(leaf("msg/appmsg/md5")?),
        true,
        MAX_CACHED_FILE_BYTES,
    )?;
    let extension = leaf("msg/appmsg/appattach/fileext")?;
    let kind = file_resource_type(extension) as i64;
    let mut local = format!("{hash}_{title}");
    if local.len() > 255 {
        let extension = title
            .rsplit_once('.')
            .map(|(_, extension)| extension)
            .filter(|extension| {
                !extension.is_empty()
                    && extension.len() <= 32
                    && extension.bytes().all(|b| b.is_ascii_alphanumeric())
            })
            .unwrap_or("bin");
        local = format!("{hash}.{extension}");
    }
    let existing = resource_snapshot(location, keys, &binding, chat, options)?;
    if let Some((packed, details)) = &existing {
        if !packed.is_empty() || details.len() > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        for detail in details {
            if crate::reader::num(detail, "type") != Some(kind)
                || detail.get("data_index") != Some(&SqlValue::Text("0".into()))
            {
                return Err(ProfileError::InvalidMetadata);
            }
            {
                let packed =
                    resource_blob(detail, "packed_info")?.ok_or(ProfileError::InvalidMetadata)?;
                let (original, name) = file_detail_names(&packed)?;
                if original != title {
                    return Err(ProfileError::InvalidMetadata);
                }
                if let Some(name) = name {
                    local = name;
                }
            }
        }
    }
    if !valid_component(&local) || local.len() > 255 {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut nested = protobuf_bytes(1, title.as_bytes());
    nested.extend(protobuf_bytes(2, local.as_bytes()));
    let packed = protobuf_bytes(1, &nested);
    let mut detail = downloaded_detail(kind, &binding, plaintext.len(), &packed);
    retain_access(&mut detail, existing.as_ref())?;
    let mut result = publish_plain_hardlink(
        location,
        keys,
        &binding,
        HardlinkKind::File,
        &local,
        plaintext,
        &hash,
        options,
    )?;
    result.changed |= apply_media_resource_variants(
        location,
        keys,
        &MediaResourceProjection {
            source: source.clone(),
            chat: chat.into(),
            packed_info: vec![],
            details: vec![detail],
        },
        options,
    )?
    .changed;
    Ok(result)
}

/// Cache one video representation using the resource basename plus its exact
/// native suffix. Plain MP4 and JPEG thumbnail bytes are never interchanged, and
/// adding one quality preserves every other resource detail and basename.
#[allow(clippy::too_many_arguments)]
pub fn cache_video_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    chat: &str,
    quality: VideoQuality,
    plaintext: &[u8],
    options: &WriteOptions,
) -> Result<AppliedHardlinkCache> {
    ownership(location, keys, options)?;
    let binding = message_binding(location, keys, source, chat, options)?;
    if binding.kind & 0x7fffffff != 43 {
        return Err(ProfileError::InvalidQuery);
    }
    let xml = attachment_xml(location, keys, source, &binding, options)?;
    let attrs = xml.video.as_ref().ok_or(ProfileError::InvalidMetadata)?;
    let (size, digest) = match quality {
        VideoQuality::Normal => ("length", "md5"),
        VideoQuality::Original => ("rawlength", "rawmd5"),
        VideoQuality::Thumbnail => ("cdnthumblength", "cdnthumbmd5"),
    };
    let hash = bytes_match_metadata(
        plaintext,
        attrs.get(size).ok_or(ProfileError::InvalidMetadata)?,
        attrs.get(digest).map(String::as_str),
        quality != VideoQuality::Thumbnail,
        MAX_CACHED_MEDIA_BYTES,
    )?;
    if quality == VideoQuality::Thumbnail {
        if crate::images::validate_image(plaintext)?.0 != "image/jpeg" {
            return Err(ProfileError::InvalidMetadata);
        }
    } else if plaintext.len() < 12 || plaintext.get(4..8) != Some(b"ftyp") {
        return Err(ProfileError::InvalidMetadata);
    }
    let existing = resource_snapshot(location, keys, &binding, chat, options)?;
    let (base, packed) = if let Some((packed, _)) = existing.as_ref() {
        let fields = crate::projection::proto_fields(packed)?;
        if fields.len() != 1 || fields[0].0 != 2 || fields[0].1 != 2 {
            return Err(ProfileError::InvalidMetadata);
        }
        let nested = crate::projection::proto_fields(fields[0].2)?;
        if nested.len() != 1 || nested[0].0 != 1 || nested[0].1 != 2 {
            return Err(ProfileError::InvalidMetadata);
        }
        let base = std::str::from_utf8(nested[0].2)
            .map_err(|_| ProfileError::InvalidMetadata)?
            .to_string();
        if !digest_hex(&base, 32) {
            return Err(ProfileError::InvalidMetadata);
        }
        (base, packed.clone())
    } else {
        // Basename generation is our allocation policy; its representation and
        // joins are the native format, independently checked against stock rows.
        let base = format!(
            "{:x}",
            Md5::digest(
                format!(
                    "profile-video-v1\0{}\0{}\0{}",
                    source.database, source.table, source.local_id
                )
                .as_bytes()
            )
        );
        let packed = protobuf_bytes(2, &protobuf_bytes(1, base.as_bytes()));
        (base, packed)
    };
    let filename = format!("{base}{}", quality.suffix());
    let mut detail = downloaded_detail(quality.resource_type(), &binding, plaintext.len(), &[]);
    retain_access(&mut detail, existing.as_ref())?;
    let mut result = publish_plain_hardlink(
        location,
        keys,
        &binding,
        HardlinkKind::Video,
        &filename,
        plaintext,
        &hash,
        options,
    )?;
    result.changed |= apply_media_resource_variants(
        location,
        keys,
        &MediaResourceProjection {
            source: source.clone(),
            chat: chat.into(),
            packed_info: packed,
            details: vec![detail],
        },
        options,
    )?
    .changed;
    Ok(result)
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedHardlinkCache {
    pub relative_path: String,
    pub changed: bool,
    pub plaintext_bytes: usize,
}
/// Register verified ordinary file/video bytes in the observed month/hardlink
/// namespace. This does not invent a MessageResourceDetail subtype or packed
/// attachment record; adapters with that metadata can register it separately.
/// A digest prefix makes the local filename deterministic and collision bounded.
#[allow(clippy::too_many_arguments)]
pub fn cache_hardlink_media_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    source: &SourceMessageId,
    chat: &str,
    kind: HardlinkKind,
    filename: &str,
    plaintext: &[u8],
    options: &WriteOptions,
) -> Result<AppliedHardlinkCache> {
    ownership(location, keys, options)?;
    if plaintext.is_empty()
        || plaintext.len() > hardlink_limit(kind)
        || !valid_component(filename)
        || filename.len() > 180
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let binding = message_binding(location, keys, source, chat, options)?;
    let expected = match kind {
        HardlinkKind::File => 49,
        HardlinkKind::Video => 43,
        _ => return Err(ProfileError::InvalidQuery),
    };
    if binding.kind & 0x7fffffff != expected
        || (kind == HardlinkKind::File && (binding.kind as u64 >> 32) != 6)
    {
        return Err(ProfileError::InvalidQuery);
    }
    if kind == HardlinkKind::Video && plaintext.get(4..8) != Some(b"ftyp") {
        return Err(ProfileError::InvalidMetadata);
    }
    let hash = format!("{:x}", Md5::digest(plaintext));
    let month = chrono::DateTime::from_timestamp(binding.time, 0)
        .ok_or(ProfileError::InvalidMetadata)?
        .format("%Y-%m")
        .to_string();
    let local_name = format!("{hash}_{filename}");
    let relative_path = format!("{}/{month}/{local_name}", kind.directory()?);
    let mut changed = publish_cache_file(
        location,
        &relative_path,
        plaintext,
        &hex::encode(Sha256::digest(plaintext)),
        options,
    )?;
    let mut fields = RawFields::new();
    for (k, n) in [
        ("md5_hash", hardlink_md5_hash(&hash)? as i64),
        ("type", if kind == HardlinkKind::File { 1 } else { 3 }),
        ("file_size", plaintext.len() as i64),
        ("modify_time", binding.time),
    ] {
        fields.insert(k.into(), SqlValue::Integer(n.to_string()));
    }
    fields.insert("md5".into(), SqlValue::Text(hash));
    fields.insert("file_name".into(), SqlValue::Text(local_name));
    fields.insert("extra_buffer".into(), SqlValue::Blob(String::new()));
    changed |= apply_hardlink_projection(
        location,
        keys,
        &HardlinkProjection {
            kind,
            first_directory: month,
            second_directory: None,
            fields,
        },
        options,
    )?
    .changed;
    Ok(AppliedHardlinkCache {
        relative_path,
        changed,
        plaintext_bytes: plaintext.len(),
    })
}

#[cfg(test)]
mod cache_limit_tests {
    use super::*;
    #[test]
    fn only_ordinary_file_namespaces_receive_the_larger_limit() {
        assert_eq!(hardlink_limit(HardlinkKind::File), 536_870_912);
        assert_eq!(cache_path_limit("msg/file/2026-10/a.bin"), 536_870_912);
        for kind in [HardlinkKind::Video, HardlinkKind::Image] {
            assert_eq!(hardlink_limit(kind), MAX_CACHED_MEDIA_BYTES);
        }
        for path in ["msg/video/a", "msg/attach/a", "msg/file2/a"] {
            assert_eq!(cache_path_limit(path), MAX_CACHED_MEDIA_BYTES);
        }
    }
    #[test]
    fn cache_comparison_checks_all_chunks_without_a_second_file_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture");
        let mut bytes = vec![3; 128 * 1024 + 7];
        fs::write(&path, &bytes).unwrap();
        assert!(cache_matches(dir.path(), Path::new("fixture"), &bytes, bytes.len()).unwrap());
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert!(!cache_matches(dir.path(), Path::new("fixture"), &bytes, bytes.len()).unwrap());
        assert!(matches!(
            cache_matches(dir.path(), Path::new("fixture"), &bytes, bytes.len() - 1),
            Err(ProfileError::ResourceLimit)
        ));
    }
}
