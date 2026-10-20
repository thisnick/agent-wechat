//! Narrow incoming plain-text projection into an initialized official profile.
//! Callers must persist an application intent before entry and recover it before
//! advancing a native sync checkpoint. Message/session transactions are separate;
//! retries reconcile the same server identity rather than resend anything.
use crate::{
    keys::verify_first_page,
    schema::{inspect, quote_identifier},
    snapshot::configure_cipher,
    KeyFile, ProfileError, ProfileLocation, ReadOptions, Result, SourceMessageId,
};
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IncomingText {
    pub peer: String,
    pub own_username: String,
    pub server_message_id: String,
    pub server_sequence: i64,
    pub create_time: i64,
    pub content: String,
    pub source_xml: String,
    pub unread: bool,
}
#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub exclusive_ownership_confirmed: bool,
    pub allow_plaintext: bool,
    /// Reference to the caller's already durable apply intent, never a SyncKey.
    pub durable_intent_id: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedText {
    pub source_id: SourceMessageId,
    pub message_inserted: bool,
    pub session_projected: bool,
    pub cross_database_atomic: bool,
}

pub(crate) fn open_write(
    location: &ProfileLocation,
    rel: &str,
    keys: &KeyFile,
    allow_plaintext: bool,
) -> Result<Connection> {
    if !crate::keys::valid_relative_path(rel) {
        return Err(ProfileError::InvalidQuery);
    }
    let path = location.storage.join(rel);
    let canonical = fs::canonicalize(&path)?;
    if !canonical.starts_with(&location.storage) || canonical != path {
        return Err(ProfileError::UnsupportedFile);
    }
    let m = fs::symlink_metadata(&path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return Err(ProfileError::UnsupportedFile);
    }
    let mut page = [0u8; 4096];
    fs::File::open(&path)?.read_exact(&mut page)?;
    let c = Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(Duration::from_secs(2))?;
    if &page[..16] == b"SQLite format 3\0" {
        if !allow_plaintext {
            return Err(ProfileError::KeyUnavailable(rel.into()));
        }
    } else {
        let k = keys
            .databases
            .get(rel)
            .ok_or_else(|| ProfileError::KeyUnavailable(rel.into()))?;
        if !verify_first_page(&page, k.raw_page_key.expose(), &k.salt) {
            return Err(ProfileError::KeyMismatch(rel.into()));
        }
        configure_cipher(&c, &k.raw_page_key, rel)?;
    }
    c.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|_| ProfileError::KeyMismatch(rel.into()))?;
    c.execute_batch(
        "PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY;",
    )?;
    Ok(c)
}
pub(crate) const MESSAGE_COLUMNS: &[&str] = &[
    "local_id",
    "server_id",
    "local_type",
    "sort_seq",
    "real_sender_id",
    "create_time",
    "status",
    "upload_status",
    "download_status",
    "server_seq",
    "origin_source",
    "source",
    "message_content",
    "compress_content",
    "packed_info_data",
    "WCDB_CT_message_content",
    "WCDB_CT_source",
];
const SESSION_COLUMNS: &[&str] = &[
    "username",
    "type",
    "unread_count",
    "unread_first_msg_srv_id",
    "unread_first_pat_msg_local_id",
    "unread_first_pat_msg_sort_seq",
    "is_hidden",
    "summary",
    "draft",
    "status",
    "last_timestamp",
    "sort_timestamp",
    "last_clear_unread_timestamp",
    "last_msg_locald_id",
    "last_msg_type",
    "last_msg_sub_type",
    "last_msg_sender",
    "last_sender_display_name",
    "last_msg_ext_type",
];
pub(crate) fn exact_columns(
    c: &Connection,
    db: &str,
    table: &str,
    expected: &[&str],
) -> Result<crate::TableSchema> {
    let report = inspect(c, db)?;
    let t = report
        .tables
        .into_iter()
        .find(|t| t.name == table)
        .ok_or_else(|| ProfileError::UnsupportedSchema(db.into()))?;
    if t.columns
        .iter()
        .map(|v| v.name.as_str())
        .collect::<Vec<_>>()
        != expected
        || !t.triggers.is_empty()
    {
        return Err(ProfileError::UnsupportedSchema(db.into()));
    }
    for col in &t.columns {
        let ty = if [
            "username",
            "user_name",
            "source",
            "message_content",
            "compress_content",
            "summary",
            "draft",
            "last_msg_sender",
            "last_sender_display_name",
        ]
        .contains(&col.name.as_str())
        {
            "TEXT"
        } else if col.name == "packed_info_data" {
            "BLOB"
        } else {
            "INTEGER"
        };
        let pk = if col.name == "local_id" && crate::schema::message_table(table)
            || col.name == "user_name" && table == "Name2Id"
            || col.name == "username" && table == "SessionTable"
            || col.name == "username_id" && table == "SessionUnreadListTable_1"
        {
            1
        } else if col.name == "server_id" && table == "SessionUnreadListTable_1" {
            2
        } else {
            0
        };
        let default = if col.name.starts_with("WCDB_CT_") {
            Some("NULL")
        } else {
            None
        };
        if !col.declared_type.eq_ignore_ascii_case(ty)
            || col.primary_key_position != pk
            || col.not_null
            || col.hidden != 0
            || col
                .default_sql
                .as_deref()
                .map(str::to_ascii_uppercase)
                .as_deref()
                != default
        {
            return Err(ProfileError::UnsupportedSchema(db.into()));
        }
    }
    Ok(t)
}
fn var(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    out.push(n as u8);
}
fn packed(from: i64, to: i64) -> Result<Vec<u8>> {
    if from <= 0 || to <= 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut v = vec![8];
    var(from as u64, &mut v);
    v.push(16);
    var(to as u64, &mut v);
    v.extend([88, 0]);
    Ok(v)
}
pub(crate) fn find_database(location: &ProfileLocation, basename: &str) -> Result<String> {
    fn walk(
        root: &Path,
        here: &Path,
        name: &str,
        out: &mut Vec<String>,
        depth: usize,
    ) -> Result<()> {
        if depth > 16 {
            return Err(ProfileError::ResourceLimit);
        }
        for e in fs::read_dir(here)? {
            let e = e?;
            let m = fs::symlink_metadata(e.path())?;
            if m.file_type().is_symlink() {
                return Err(ProfileError::UnsupportedFile);
            }
            if m.is_dir() {
                walk(root, &e.path(), name, out, depth + 1)?;
            } else if e.file_name() == name {
                out.push(
                    e.path()
                        .strip_prefix(root)
                        .map_err(|_| ProfileError::InvalidProfile)?
                        .to_str()
                        .ok_or(ProfileError::InvalidProfile)?
                        .into(),
                );
            }
        }
        Ok(())
    }
    let mut found = Vec::new();
    walk(
        &location.storage,
        &location.storage,
        basename,
        &mut found,
        0,
    )?;
    if found.len() != 1 {
        return Err(ProfileError::UnsupportedSchema(basename.into()));
    }
    Ok(found.remove(0))
}
/// Refuse implicit rotation/routing when another ordinary shard is present.
pub(crate) fn require_single_message_shard(location: &ProfileLocation) -> Result<()> {
    fn visit(path: &Path, depth: usize, count: &mut usize, entries: &mut usize) -> Result<()> {
        if depth > 16 {
            return Err(ProfileError::ResourceLimit);
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            *entries += 1;
            if *entries > 16384 {
                return Err(ProfileError::ResourceLimit);
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                return Err(ProfileError::UnsupportedFile);
            }
            if metadata.is_dir() {
                visit(&entry.path(), depth + 1, count, entries)?;
            } else if let Some(name) = entry.file_name().to_str() {
                if name
                    .strip_prefix("message_")
                    .and_then(|s| s.strip_suffix(".db"))
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    *count += 1;
                }
            }
        }
        Ok(())
    }
    let (mut count, mut entries) = (0, 0);
    visit(&location.storage, 0, &mut count, &mut entries)?;
    if count == 1 {
        Ok(())
    } else {
        Err(ProfileError::WritesUnsupported)
    }
}
/// Supports only the verified existing direct-chat incoming text family. It does
/// not allocate new chats/maps, infer sort collisions, update FTS or project
/// unknown media/system commands. A partial error may follow a committed message
/// transaction; recover by retrying the same durable input.
pub fn apply_incoming_text(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &IncomingText,
    options: &WriteOptions,
) -> Result<AppliedText> {
    if !options.exclusive_ownership_confirmed {
        return Err(ProfileError::SnapshotRequired);
    }
    if options.durable_intent_id.is_empty() || options.durable_intent_id.len() > 256 {
        return Err(ProfileError::InvalidQuery);
    }
    if keys.account_binding != location.account {
        return Err(ProfileError::AccountMismatch);
    }
    if input.peer.is_empty()
        || input.peer == input.own_username
        || input.peer.ends_with("@chatroom")
        || input.peer.len() > 512
        || input.own_username.is_empty()
        || input.own_username.len() > 512
        || input.content.len() > 1024 * 1024
        || input.source_xml.len() > 1024 * 1024
        || input.content.contains('\0')
        || input.source_xml.contains('\0')
        || input.create_time <= 0
        || input.server_sequence < 0
    {
        return Err(ProfileError::WritesUnsupported);
    }
    let sid = input
        .server_message_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidQuery)?;
    if sid == 0 {
        return Err(ProfileError::InvalidQuery);
    }
    let signed = sid as i64;
    let sort = input
        .create_time
        .checked_mul(1000)
        .ok_or(ProfileError::InvalidQuery)?;
    let message_db = find_database(location, "message_0.db")?;
    let session_db = find_database(location, "session.db")?;
    // Other shards make write routing unknown; do not silently always use zero.
    let view = crate::ProfileReader::open(
        location.clone(),
        keys.clone(),
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: options.allow_plaintext,
            ..Default::default()
        },
    )?;
    if view
        .inventory()
        .iter()
        .filter(|d| {
            let n = d.relative_path.rsplit('/').next().unwrap_or("");
            n.starts_with("message_")
                && n.trim_start_matches("message_")
                    .strip_suffix(".db")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count()
        != 1
    {
        return Err(ProfileError::WritesUnsupported);
    }
    drop(view);
    let mut messages = open_write(location, &message_db, keys, options.allow_plaintext)?;
    let mut sessions = open_write(location, &session_db, keys, options.allow_plaintext)?;
    let table = format!("Msg_{:x}", Md5::digest(input.peer.as_bytes()));
    let table_schema = exact_columns(&messages, &message_db, &table, MESSAGE_COLUMNS)?;
    let expected_indexes = [
        ("_SERVERID", vec!["server_id"]),
        ("_SORTSEQ", vec!["sort_seq"]),
        ("_TYPE_SEQ", vec!["local_type", "sort_seq"]),
        ("_SENDERID", vec!["real_sender_id"]),
    ];
    if table_schema.indexes.len() != 4
        || !expected_indexes.iter().all(|(suffix, columns)| {
            table_schema.indexes.iter().any(|i| {
                i.name == format!("{table}{suffix}")
                    && !i.unique
                    && !i.partial
                    && i.columns.iter().map(|s| s.as_deref()).collect::<Vec<_>>()
                        == columns.iter().map(|s| Some(*s)).collect::<Vec<_>>()
            })
        })
    {
        return Err(ProfileError::UnsupportedSchema(message_db));
    }
    if table_schema.columns[0].declared_type.to_uppercase() != "INTEGER"
        || table_schema.columns[0].primary_key_position != 1
        || !table_schema
            .definition
            .as_ref()
            .is_some_and(|s| s.to_uppercase().contains("AUTOINCREMENT"))
    {
        return Err(ProfileError::UnsupportedSchema(message_db));
    }
    exact_columns(&sessions, &session_db, "SessionTable", SESSION_COLUMNS)?;
    exact_columns(
        &sessions,
        &session_db,
        "SessionUnreadListTable_1",
        &["username_id", "server_id", "local_id", "create_time"],
    )?;
    exact_columns(
        &messages,
        &message_db,
        "Name2Id",
        &["user_name", "is_session"],
    )?;
    exact_columns(&sessions, &session_db, "Name2Id", &["user_name"])?;
    let sender = messages
        .query_row(
            "SELECT rowid FROM Name2Id WHERE user_name=?1",
            [&input.peer],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(ProfileError::WritesUnsupported)?;
    let recipient = messages
        .query_row(
            "SELECT rowid FROM Name2Id WHERE user_name=?1",
            [&input.own_username],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(ProfileError::WritesUnsupported)?;
    let session_name = sessions
        .query_row(
            "SELECT rowid FROM Name2Id WHERE user_name=?1",
            [&input.peer],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(ProfileError::WritesUnsupported)?;
    let current=sessions.query_row("SELECT unread_count,last_msg_sender,last_sender_display_name,last_timestamp,sort_timestamp FROM SessionTable WHERE username=?1",[&input.peer],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?))).optional()?.ok_or(ProfileError::WritesUnsupported)?;
    if current.0 < 0 || !current.1.is_empty() || !current.2.is_empty() {
        return Err(ProfileError::WritesUnsupported);
    }
    let packed = packed(sender, recipient)?;
    let source = zstd::stream::encode_all(input.source_xml.as_bytes(), 3)
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let q = quote_identifier(&table);
    let tx = messages.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let existing=tx.query_row(&format!("SELECT local_id,local_type,sort_seq,real_sender_id,create_time,status,upload_status,download_status,server_seq,origin_source,source,message_content,compress_content,packed_info_data,WCDB_CT_message_content,WCDB_CT_source FROM {q} WHERE server_id=?1"),[signed],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?,r.get::<_,i64>(7)?,r.get::<_,i64>(8)?,r.get::<_,i64>(9)?,r.get::<_,Vec<u8>>(10)?,r.get::<_,String>(11)?,r.get::<_,String>(12)?,r.get::<_,Vec<u8>>(13)?,r.get::<_,i64>(14)?,r.get::<_,i64>(15)?))).optional()?;
    let (local, inserted) = if let Some(r) = existing {
        let duplicates: i64 = tx.query_row(
            &format!("SELECT count(*) FROM {q} WHERE server_id=?1"),
            [signed],
            |r| r.get(0),
        )?;
        if duplicates != 1
            || (r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9)
                != (
                    1,
                    sort,
                    sender,
                    input.create_time,
                    3,
                    0,
                    0,
                    input.server_sequence,
                    2,
                )
            || r.10 != source
            || r.11 != input.content
            || !r.12.is_empty()
            || r.13 != packed
            || r.14 != 0
            || r.15 != 4
        {
            return Err(ProfileError::InvalidMetadata);
        }
        (r.0, false)
    } else {
        let collisions: i64 = tx.query_row(
            &format!("SELECT count(*) FROM {q} WHERE sort_seq=?1"),
            [sort],
            |r| r.get(0),
        )?;
        if collisions != 0 {
            return Err(ProfileError::WritesUnsupported);
        }
        tx.execute(&format!("INSERT INTO {q}(server_id,local_type,sort_seq,real_sender_id,create_time,status,upload_status,download_status,server_seq,origin_source,source,message_content,compress_content,packed_info_data,WCDB_CT_message_content,WCDB_CT_source)VALUES(?1,1,?2,?3,?4,3,0,0,?5,2,?6,?7,'',?8,0,4)"),params![signed,sort,sender,input.create_time,input.server_sequence,source,input.content,packed])?;
        (tx.last_insert_rowid(), true)
    };
    tx.commit()?;
    let tx = sessions.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let unread_existing=tx.query_row("SELECT local_id,create_time FROM SessionUnreadListTable_1 WHERE username_id=?1 AND server_id=?2",params![session_name,signed],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?;
    if unread_existing.is_some_and(|(l, t)| l != local || t != input.create_time) {
        return Err(ProfileError::InvalidMetadata);
    }
    // Never resurrect an unread entry already consumed by a later official read.
    // The summary ID is the durable evidence that this exact projection completed.
    let already_projected=tx.query_row("SELECT last_msg_locald_id,last_timestamp,last_clear_unread_timestamp FROM SessionTable WHERE username=?1",[&input.peer],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?)))?;
    if input.unread
        && unread_existing.is_none()
        && !(already_projected.0 == local && already_projected.1 == input.create_time)
    {
        if already_projected.1 > input.create_time || already_projected.2 >= input.create_time {
            return Err(ProfileError::WritesUnsupported);
        }
        tx.execute("INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(?1,?2,?3,?4)",params![session_name,signed,local,input.create_time])?;
        tx.execute("UPDATE SessionTable SET unread_first_msg_srv_id=CASE WHEN unread_count=0 THEN ?1 ELSE unread_first_msg_srv_id END,unread_count=unread_count+1 WHERE username=?2",params![signed,input.peer])?;
    }
    if current.3 <= input.create_time && current.4 <= input.create_time {
        tx.execute("UPDATE SessionTable SET summary=?1,last_timestamp=?2,sort_timestamp=?2,last_msg_locald_id=?3,last_msg_type=1,last_msg_sub_type=0,last_msg_ext_type=0 WHERE username=?4",params![input.content,input.create_time,local,input.peer])?;
    }
    tx.commit()?;
    Ok(AppliedText {
        source_id: SourceMessageId {
            database: message_db,
            table,
            local_id: local.to_string(),
        },
        message_inserted: inserted,
        session_projected: true,
        cross_database_atomic: false,
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadProjection {
    pub removed_unread_rows: usize,
    pub unread_count_before: i64,
    pub unread_count_after: i64,
    pub exact_ids_only: bool,
}
/// Project only the selected source-qualified incoming observations. This is
/// desktop-local state: it neither acknowledges consumer events nor sends a read
/// receipt to the phone. Counter/list discrepancies already present are retained
/// by subtracting only removed rows rather than rebuilding from list cardinality.
pub fn mark_read_messages(
    location: &ProfileLocation,
    keys: &KeyFile,
    thread: &str,
    ids: &[SourceMessageId],
    options: &WriteOptions,
) -> Result<ReadProjection> {
    if !options.exclusive_ownership_confirmed {
        return Err(ProfileError::SnapshotRequired);
    }
    if options.durable_intent_id.is_empty()
        || ids.is_empty()
        || ids.len() > 200
        || thread.is_empty()
        || thread.len() > 512
    {
        return Err(ProfileError::InvalidQuery);
    }
    if keys.account_binding != location.account {
        return Err(ProfileError::AccountMismatch);
    }
    let target = format!("Msg_{:x}", Md5::digest(thread.as_bytes()));
    let mut identities = std::collections::BTreeSet::new();
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id.clone()) {
            continue;
        }
        if id.table != target {
            return Err(ProfileError::InvalidQuery);
        }
        let local = id
            .local_id
            .parse::<i64>()
            .map_err(|_| ProfileError::InvalidQuery)?;
        let c = open_write(location, &id.database, keys, options.allow_plaintext)?;
        exact_columns(&c, &id.database, &target, MESSAGE_COLUMNS)?;
        let row = c
            .query_row(
                &format!(
                    "SELECT server_id,create_time FROM {} WHERE local_id=?1",
                    quote_identifier(&target)
                ),
                [local],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?
            .ok_or(ProfileError::InvalidQuery)?;
        if row.0 == 0 {
            return Err(ProfileError::WritesUnsupported);
        }
        identities.insert((row.0, local, row.1));
    }
    let rel = find_database(location, "session.db")?;
    let mut c = open_write(location, &rel, keys, options.allow_plaintext)?;
    exact_columns(&c, &rel, "SessionTable", SESSION_COLUMNS)?;
    exact_columns(
        &c,
        &rel,
        "SessionUnreadListTable_1",
        &["username_id", "server_id", "local_id", "create_time"],
    )?;
    exact_columns(&c, &rel, "Name2Id", &["user_name"])?;
    let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let nameid = tx
        .query_row(
            "SELECT rowid FROM Name2Id WHERE user_name=?1",
            [thread],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .ok_or(ProfileError::WritesUnsupported)?;
    let (before, first) = tx
        .query_row(
            "SELECT unread_count,unread_first_msg_srv_id FROM SessionTable WHERE username=?1",
            [thread],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?
        .ok_or(ProfileError::WritesUnsupported)?;
    if before < 0 {
        return Err(ProfileError::WritesUnsupported);
    }
    let mut removed = 0;
    let mut removed_first = false;
    for (server, local, time) in identities {
        let existing=tx.query_row("SELECT local_id,create_time FROM SessionUnreadListTable_1 WHERE username_id=?1 AND server_id=?2",params![nameid,server],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?;
        if let Some((l, t)) = existing {
            if (l, t) != (local, time) {
                return Err(ProfileError::WritesUnsupported);
            }
            removed+=tx.execute("DELETE FROM SessionUnreadListTable_1 WHERE username_id=?1 AND server_id=?2 AND local_id=?3 AND create_time=?4",params![nameid,server,local,time])?;
            removed_first |= server == first;
        }
    }
    let after = before.saturating_sub(removed as i64).max(0);
    if removed > 0 {
        let next = if removed_first {
            if after == 0 {
                0
            } else {
                tx.query_row("SELECT server_id FROM SessionUnreadListTable_1 WHERE username_id=?1 ORDER BY create_time,local_id,server_id LIMIT 1",[nameid],|r|r.get::<_,i64>(0)).optional()?.unwrap_or(0)
            }
        } else {
            first
        };
        tx.execute(
            "UPDATE SessionTable SET unread_count=?1,unread_first_msg_srv_id=?2 WHERE username=?3",
            params![after, next, thread],
        )?;
    }
    tx.commit()?;
    Ok(ReadProjection {
        removed_unread_rows: removed,
        unread_count_before: before,
        unread_count_after: after,
        exact_ids_only: true,
    })
}
