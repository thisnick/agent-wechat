//! Official per-peer friend-message rows. Tickets and raw history remain private.
use crate::*;
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const COLUMNS: &[&str] = &[
    "user_name_",
    "type_",
    "timestamp_",
    "encrypt_user_name_",
    "content_",
    "is_sender_",
    "ticket_",
    "scene_",
    "fmessage_detail_buf_",
    "remark_",
    "label_ids_",
];
/// A retained verification-history entry. A locally sent entry may have ID zero.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRequestMessage {
    /// Decimal server message ID; locally sent entries use zero.
    pub server_id: String,
    /// Verification greeting retained in this history entry.
    pub content: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Whether this account sent the verification message.
    pub sender: bool,
}
/// One canonical per-peer row, including protected verification material.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRequestRecord {
    /// Exact canonical per-peer identity.
    pub username: String,
    /// Verification message type from the supported sync envelope.
    pub message_type: i64,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Stored discovery alias, preserved independently of the row identity.
    pub encrypted_username: String,
    /// Verification greeting retained in this history entry.
    pub content: String,
    /// Whether this account sent the verification message.
    pub sender: bool,
    /// Private verification ticket; never expose in public responses.
    pub ticket: String,
    /// Verification source value accompanying the target.
    pub scene: u32,
    /// Original encoded history and flags, including unknown fields.
    pub detail: Vec<u8>,
    /// Stored per-peer remark.
    pub remark: String,
    /// Stored contact label identifiers.
    pub label_ids: String,
    /// Opaque hash of the exact persisted row values.
    pub version: String,
    /// Retained verification conversation in stored order.
    pub history: Vec<ContactRequestMessage>,
    /// Stored attention flag; absent means unknown.
    pub attention: Option<bool>,
    /// Stored waiting flag; absent means unknown.
    pub waiting: Option<bool>,
    /// Stored aging flag; absent means unknown.
    pub aged: Option<bool>,
}
#[derive(Clone)]
struct Field {
    number: u32,
    value: Vec<u8>,
    scalar: Option<u64>,
    raw: Vec<u8>,
}
fn varint(bytes: &[u8], pos: &mut usize) -> Result<u64> {
    let mut out = 0;
    for i in 0..10 {
        let b = *bytes.get(*pos).ok_or(ProfileError::InvalidMetadata)?;
        *pos += 1;
        if i == 9 && b > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        out |= u64::from(b & 127) << (7 * i);
        if b & 128 == 0 {
            return Ok(out);
        }
    }
    Err(ProfileError::InvalidMetadata)
}
fn uint(mut n: u64) -> Vec<u8> {
    let mut out = vec![];
    loop {
        let b = (n & 127) as u8;
        n >>= 7;
        out.push(b | if n > 0 { 128 } else { 0 });
        if n == 0 {
            return out;
        }
    }
}
fn bytes(n: u32, b: &[u8]) -> Vec<u8> {
    [uint(u64::from(n) * 8 + 2), uint(b.len() as u64), b.to_vec()].concat()
}
fn scalar(n: u32, v: u64) -> Vec<u8> {
    [uint(u64::from(n) * 8), uint(v)].concat()
}
fn decode(b: &[u8]) -> Result<Vec<Field>> {
    if b.len() > 4 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut pos = 0;
    let mut out = vec![];
    while pos < b.len() {
        if out.len() > 10000 {
            return Err(ProfileError::ResourceLimit);
        }
        let start = pos;
        let key = varint(b, &mut pos)?;
        let number = u32::try_from(key >> 3).map_err(|_| ProfileError::InvalidMetadata)?;
        if number == 0 || number >= 1 << 29 {
            return Err(ProfileError::InvalidMetadata);
        }
        let mut n = None;
        let value = match key & 7 {
            0 => {
                n = Some(varint(b, &mut pos)?);
                vec![]
            }
            w @ (1 | 2 | 5) => {
                let len = match w {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(varint(b, &mut pos)?)
                        .map_err(|_| ProfileError::InvalidMetadata)?,
                };
                let end = pos
                    .checked_add(len)
                    .filter(|end| *end <= b.len())
                    .ok_or(ProfileError::InvalidMetadata)?;
                let v = b[pos..end].to_vec();
                pos = end;
                v
            }
            _ => return Err(ProfileError::InvalidMetadata),
        };
        out.push(Field {
            number,
            value,
            scalar: n,
            raw: b[start..pos].to_vec(),
        });
    }
    Ok(out)
}
fn one(fields: &[Field], n: u32) -> Result<Option<&Field>> {
    let mut iter = fields.iter().filter(|f| f.number == n);
    let v = iter.next();
    if iter.next().is_some() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(v)
}
fn boolean(fields: &[Field], n: u32) -> Result<Option<bool>> {
    one(fields, n)?
        .map(|f| match f.scalar {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(ProfileError::InvalidMetadata),
        })
        .transpose()
}
fn text(fields: &[Field], n: u32) -> Result<String> {
    one(fields, n)?
        .map(|f| String::from_utf8(f.value.clone()).map_err(|_| ProfileError::InvalidMetadata))
        .transpose()
        .map(|v| v.unwrap_or_default())
}
fn history(detail: &[u8]) -> Result<Vec<ContactRequestMessage>> {
    let fields = decode(detail)?;
    let mut out = vec![];
    for f in fields.iter().filter(|f| f.number == 1) {
        let m = decode(&f.value)?;
        out.push(ContactRequestMessage {
            server_id: one(&m, 1)?.and_then(|f| f.scalar).unwrap_or(0).to_string(),
            content: text(&m, 2)?,
            timestamp: i64::try_from(one(&m, 3)?.and_then(|f| f.scalar).unwrap_or(0))
                .map_err(|_| ProfileError::InvalidMetadata)?,
            sender: boolean(&m, 4)?.unwrap_or(false),
        });
    }
    Ok(out)
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ContactRequestRecord> {
    let detail = match r.get_ref(8)? {
        ValueRef::Text(v) | ValueRef::Blob(v) => v.to_vec(),
        ValueRef::Null => vec![],
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    // Every column but the identity is nullable, and stored rows do hold NULL.
    // NULL reads as the empty value; `null_columns` lets a write-back keep it.
    let nullable_text = |i| r.get::<_, Option<String>>(i).map(Option::unwrap_or_default);
    let nullable_integer = |i| r.get::<_, Option<i64>>(i).map(Option::unwrap_or_default);
    Ok(ContactRequestRecord {
        username: r.get(0)?,
        message_type: nullable_integer(1)?,
        timestamp: nullable_integer(2)?,
        encrypted_username: nullable_text(3)?,
        content: nullable_text(4)?,
        sender: nullable_integer(5)? != 0,
        ticket: nullable_text(6)?,
        scene: r.get::<_, Option<u32>>(7)?.unwrap_or_default(),
        detail,
        remark: nullable_text(9)?,
        label_ids: nullable_text(10)?,
        version: String::new(),
        history: vec![],
        attention: None,
        waiting: None,
        aged: None,
    })
}
/// The NULL columns of the stored row, by name; `None` when there is no row.
fn null_columns(db: &Connection, username: &str) -> Result<Option<Vec<&'static str>>> {
    let checks = COLUMNS[1..]
        .iter()
        .map(|c| format!("{c} IS NULL"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT {checks} FROM FMessageTable WHERE user_name_=?1");
    Ok(db
        .query_row(&sql, [username], |r| {
            let mut nulls = vec![];
            for (i, column) in COLUMNS[1..].iter().enumerate() {
                if r.get::<_, bool>(i)? {
                    nulls.push(*column);
                }
            }
            Ok(nulls)
        })
        .optional()?)
}
/// The integer to write for a column, or NULL when the stored column was NULL
/// and the value is still zero.
fn kept_integer(nulls: &[&str], column: &str, value: i64) -> Option<i64> {
    (!(value == 0 && nulls.contains(&column))).then_some(value)
}
/// The text to write for a column, or NULL when the stored column was NULL and
/// the value is still empty.
fn kept_text<'a>(nulls: &[&str], column: &str, value: &'a str) -> Option<&'a str> {
    (!(value.is_empty() && nulls.contains(&column))).then_some(value)
}
fn finish(mut row: ContactRequestRecord) -> Result<ContactRequestRecord> {
    row.version = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            &row.username,
            row.message_type,
            row.timestamp,
            &row.encrypted_username,
            &row.content,
            row.sender,
            &row.ticket,
            row.scene,
            &row.detail,
            &row.remark,
            &row.label_ids,
        ))
        .map_err(|_| ProfileError::InvalidMetadata)?,
    ));
    let f = decode(&row.detail)?;
    row.attention = boolean(&f, 2)?;
    row.waiting = boolean(&f, 3)?;
    row.aged = boolean(&f, 4)?;
    row.history = history(&row.detail)?;
    Ok(row)
}
fn schema(db: &Connection, relative: &str) -> Result<()> {
    let table = crate::schema::inspect_named_table(db, "FMessageTable")?
        .ok_or_else(|| ProfileError::UnsupportedSchema(relative.into()))?;
    if table.columns.len() != COLUMNS.len() || !table.triggers.is_empty() {
        return Err(ProfileError::UnsupportedSchema(relative.into()));
    }
    for col in &table.columns {
        let expected =
            if ["type_", "timestamp_", "is_sender_", "scene_"].contains(&col.name.as_str()) {
                "INTEGER"
            } else {
                "TEXT"
            };
        if !COLUMNS.contains(&col.name.as_str())
            || !col.declared_type.eq_ignore_ascii_case(expected)
            || col.not_null
            || col.primary_key_position != 0
            || col.hidden != 0
            || col.default_sql.is_some()
        {
            return Err(ProfileError::UnsupportedSchema(relative.into()));
        }
    }
    let duplicate: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM FMessageTable GROUP BY user_name_ HAVING COUNT(*)>1)",
        [],
        |r| r.get(0),
    )?;
    if duplicate {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(())
}

impl ProfileReader {
    /// Read retained per-peer request rows. No network access or pending-state inference.
    pub fn contact_requests(&self) -> Result<Vec<ContactRequestRecord>> {
        let databases: Vec<_> = self
            .inventory()
            .iter()
            .filter(|d| d.relative_path.rsplit('/').next() == Some("general.db"))
            .collect();
        if databases.is_empty() {
            return Ok(vec![]);
        }
        if databases.len() != 1 {
            return Err(ProfileError::UnsupportedSchema("general.db".into()));
        }
        let relative = &databases[0].relative_path;
        let db = self.snapshot.open(relative)?;
        schema(&db, relative)?;
        let mut q=db.prepare("SELECT user_name_,type_,timestamp_,encrypt_user_name_,content_,is_sender_,ticket_,scene_,fmessage_detail_buf_,remark_,label_ids_ FROM FMessageTable ORDER BY timestamp_ DESC,user_name_ LIMIT 10001")?;
        let rows = q
            .query_map([], row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if rows.len() > 10000 {
            return Err(ProfileError::ResourceLimit);
        }
        rows.into_iter().map(finish).collect()
    }
    /// Read one exact retained request row, including its current version.
    pub fn contact_request(&self, username: &str) -> Result<ContactRequestRecord> {
        crate::projection::identity(username)?;
        let rows = self.contact_requests()?;
        rows.iter()
            .find(|r| r.username == username)
            .or_else(|| rows.iter().find(|r| r.encrypted_username == username))
            .cloned()
            .ok_or(ProfileError::NotCached)
    }
}
/// Exact row values prepared by the application from retained protocol evidence.
/// The adapter validates the schema and preserves unrelated profile databases.
#[derive(Clone, Serialize, Deserialize)]
pub struct ContactRequestProjection {
    /// Exact canonical per-peer identity.
    pub username: String,
    /// Stored discovery alias, preserved independently of the row identity.
    pub encrypted_username: String,
    /// Verification message type from the supported sync envelope.
    pub message_type: i64,
    /// Verification greeting or history entry for this operation.
    pub message: ContactRequestMessage,
    /// Private verification ticket; never expose in public responses.
    pub ticket: String,
    /// Verification source value accompanying the target.
    pub scene: u32,
    /// Attention flag to persist.
    pub attention: bool,
    /// Waiting flag to persist.
    pub waiting: bool,
    /// Aged flag to persist.
    pub aged: bool,
}
/// A complete prepared row with a before/after version fence. Contains private history.
#[derive(Clone, Serialize, Deserialize)]
pub struct StagedContactRequest {
    /// Expected previous row version, or None for a new row.
    pub before_version: Option<String>,
    /// Complete row to publish after checking its previous version.
    pub row: ContactRequestRecord,
}
/// Prepare one append while preserving unknown history fields. Incoming
/// IDs deduplicate. Sent ID-zero entries always append; distinct successful sends
/// may have the same text and timestamp. Persist this stage before applying it.
pub fn stage_contact_request_projection(
    previous: Option<&ContactRequestRecord>,
    input: &ContactRequestProjection,
) -> Result<StagedContactRequest> {
    crate::projection::identity(&input.username)?;
    if ![37, 65].contains(&input.message_type)
        || input.message.timestamp < 0
        || input.message.content.len() > 4096
        || input.ticket.len() > 65536
        || previous.is_some_and(|p| p.username != input.username)
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let before_version = previous.map(|r| r.version.clone());
    if (!input.message.sender || input.message.server_id != "0")
        && previous.is_some_and(|p| {
            p.history
                .iter()
                .any(|m| m.server_id == input.message.server_id)
        })
    {
        return Ok(StagedContactRequest {
            before_version,
            row: previous.unwrap().clone(),
        });
    }
    let mut row = previous.cloned().unwrap_or(ContactRequestRecord {
        username: input.username.clone(),
        message_type: input.message_type,
        timestamp: input.message.timestamp,
        encrypted_username: input.encrypted_username.clone(),
        content: String::new(),
        sender: input.message.sender,
        ticket: String::new(),
        scene: input.scene,
        detail: vec![],
        remark: String::new(),
        label_ids: String::new(),
        version: String::new(),
        history: vec![],
        attention: None,
        waiting: None,
        aged: None,
    });
    let m = &input.message;
    let id = m
        .server_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let msg = [
        scalar(1, id),
        bytes(2, m.content.as_bytes()),
        scalar(3, m.timestamp as u64),
        scalar(4, u64::from(m.sender)),
    ]
    .concat();
    let mut detail = decode(&row.detail)?
        .into_iter()
        .filter(|f| ![2, 3, 4].contains(&f.number))
        .flat_map(|f| f.raw)
        .collect::<Vec<_>>();
    detail.extend(bytes(1, &msg));
    detail.extend(scalar(2, u64::from(input.attention)));
    detail.extend(scalar(3, u64::from(input.waiting)));
    detail.extend(scalar(4, u64::from(input.aged)));
    if row.history.iter().all(|h| h.timestamp <= m.timestamp) {
        row.message_type = input.message_type;
        row.timestamp = m.timestamp;
        row.content = m.content.clone();
        row.sender = m.sender;
        row.scene = input.scene;
        if !m.sender {
            row.ticket = input.ticket.clone();
        }
    }
    row.detail = detail;
    row = finish(row)?;
    Ok(StagedContactRequest {
        before_version,
        row,
    })
}
/// Apply a previously journaled complete row in one general.db transaction.
/// A matching after-version is a replay no-op. Any other change since staging
/// fails closed. No contact/session database is changed; caller serializes writers
/// and retains the stage and intent until this write and its checkpoint complete.
pub fn apply_contact_request_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    stage: &StagedContactRequest,
    options: &WriteOptions,
) -> Result<()> {
    crate::projection::ownership(location, keys, options)?;
    let input = &stage.row;
    crate::projection::identity(&input.username)?;
    let relative = crate::writer::find_database(location, "general.db")?;
    let mut db = crate::writer::open_write(location, &relative, keys, options.allow_plaintext)?;
    schema(&db, &relative)?;
    let tx = db.transaction()?;
    let old=tx.query_row("SELECT user_name_,type_,timestamp_,encrypt_user_name_,content_,is_sender_,ticket_,scene_,fmessage_detail_buf_,remark_,label_ids_ FROM FMessageTable WHERE user_name_=?1",[&input.username],row).optional()?.map(finish).transpose()?;
    if old.as_ref().is_some_and(|r| r.version == input.version) {
        tx.commit()?;
        return Ok(());
    }
    if old.as_ref().map(|r| &r.version) != stage.before_version.as_ref() {
        return Err(ProfileError::SourceChanged);
    }
    if finish(input.clone())?.version != input.version {
        return Err(ProfileError::InvalidMetadata);
    }
    // A column the row stored as NULL stays NULL when the change leaves it empty,
    // so the stored row keeps its form; a value the change gives it is written.
    // New rows are written as before.
    let nulls = null_columns(&tx, &input.username)?.unwrap_or_default();
    tx.execute(
        "DELETE FROM FMessageTable WHERE user_name_=?1",
        [&input.username],
    )?;
    tx.execute(
        "INSERT INTO FMessageTable VALUES(?1,?2,?3,?4,?5,?6,?7,?8,CAST(?9 AS TEXT),?10,?11)",
        params![
            input.username,
            kept_integer(&nulls, "type_", input.message_type),
            kept_integer(&nulls, "timestamp_", input.timestamp),
            kept_text(&nulls, "encrypt_user_name_", &input.encrypted_username),
            kept_text(&nulls, "content_", &input.content),
            kept_integer(&nulls, "is_sender_", i64::from(input.sender)),
            kept_text(&nulls, "ticket_", &input.ticket),
            kept_integer(&nulls, "scene_", i64::from(input.scene)),
            (!(input.detail.is_empty() && nulls.contains(&"fmessage_detail_buf_")))
                .then_some(&input.detail),
            kept_text(&nulls, "remark_", &input.remark),
            kept_text(&nulls, "label_ids_", &input.label_ids)
        ],
    )?;
    tx.commit()?;
    Ok(())
}
/// Delete the exact local per-peer row/history with a version precondition. No
/// server request, rejection or cancellation occurs. One general.db transaction;
/// caller must retain a durable intent and serialize with all profile writers.
pub fn apply_contact_request_deletion(
    location: &ProfileLocation,
    keys: &KeyFile,
    username: &str,
    expected_version: &str,
    options: &WriteOptions,
) -> Result<()> {
    crate::projection::ownership(location, keys, options)?;
    crate::projection::identity(username)?;
    let relative = crate::writer::find_database(location, "general.db")?;
    let mut db = crate::writer::open_write(location, &relative, keys, options.allow_plaintext)?;
    schema(&db, &relative)?;
    let tx = db.transaction()?;
    let old=tx.query_row("SELECT user_name_,type_,timestamp_,encrypt_user_name_,content_,is_sender_,ticket_,scene_,fmessage_detail_buf_,remark_,label_ids_ FROM FMessageTable WHERE user_name_=?1",[username],row).optional()?;
    if let Some(old) = old {
        if finish(old)?.version != expected_version {
            return Err(ProfileError::SourceChanged);
        }
        tx.execute("DELETE FROM FMessageTable WHERE user_name_=?1", [username])?;
    }
    tx.commit()?;
    Ok(())
}

/// Create only a missing general.db with the supported friend-request table.
/// Existing files are never replaced. The caller publishes the returned key in
/// its protected manifest and recovers a crash after publication by admitting
/// the existing file with the same authenticated password. Requires ownership
/// and a durable intent; other official subsystem schemas are not initialized.
pub fn apply_initialize_contact_requests(
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
    let relative = "general/general.db";
    let directory = location.storage.join("general");
    if !directory.exists() {
        std::fs::create_dir(&directory)?;
    }
    if std::fs::canonicalize(&directory)? != directory || !std::fs::metadata(&directory)?.is_dir() {
        return Err(ProfileError::UnsupportedFile);
    }
    let target = directory.join("general.db");
    if target.exists() || target.symlink_metadata().is_ok() {
        return Err(ProfileError::UnsupportedFile);
    }
    let staged = wechat_platform::private_tempfile_in(&directory, ".general-")?;
    let db = rusqlite::Connection::open(staged.path())?;
    let rc = unsafe {
        rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), password.len() as i32)
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        return Err(ProfileError::Database);
    }
    db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;PRAGMA synchronous=FULL;")?;
    crate::initialize::enable_wal(&db)?;
    db.execute_batch(include_str!("layout/general.db.sql"))?;
    let integrity: String = db.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(ProfileError::Database);
    }
    // Same WAL layout as initialize_profile; the staged file is renamed alone.
    crate::initialize::finish_wal_database(db, staged.path())?;
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

/// Stage the official detail-only attention clear after membership is established.
/// Other flags, current columns and retained history remain byte-preserved.
pub fn stage_contact_request_attention(
    previous: &ContactRequestRecord,
) -> Result<StagedContactRequest> {
    let mut row = previous.clone();
    let mut detail = decode(&row.detail)?
        .into_iter()
        .filter(|f| f.number != 2)
        .flat_map(|f| f.raw)
        .collect::<Vec<_>>();
    detail.extend(scalar(2, 0));
    row.detail = detail;
    Ok(StagedContactRequest {
        before_version: Some(previous.version.clone()),
        row: finish(row)?,
    })
}
impl ProfileReader {
    /// Resolve the saved contact for an ordinary or encrypted request identity.
    /// An @stranger alias is tried first and must identify exactly one row;
    /// ordinary identities use the canonical username lookup.
    /// No relationship is inferred from the lookup alone; inspect membership flags.
    pub fn contact_for_request(&self, username: &str) -> Result<Option<Contact>> {
        crate::projection::identity(username)?;
        if username.ends_with("@stranger") {
            let databases: Vec<_> = self
                .inventory()
                .iter()
                .filter(|d| d.relative_path.rsplit('/').next() == Some("contact.db"))
                .collect();
            if databases.len() != 1 {
                return Err(ProfileError::UnsupportedSchema("contact.db".into()));
            }
            let db = self.snapshot.open(&databases[0].relative_path)?;
            let mut q =
                db.prepare("SELECT username FROM contact WHERE encrypt_username=?1 LIMIT 2")?;
            let ids = q
                .query_map([username], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if ids.len() > 1 {
                return Err(ProfileError::InvalidMetadata);
            }
            if let Some(id) = ids.first() {
                return self.contact(id).map(Some);
            }
        }
        match self.contact(username) {
            Ok(c) => Ok(Some(c)),
            Err(ProfileError::NotCached) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(id: &str, sender: bool, time: i64, text: &str) -> ContactRequestProjection {
        ContactRequestProjection {
            username: "example_peer".into(),
            encrypted_username: "encrypted@example".into(),
            message_type: 37,
            message: ContactRequestMessage {
                server_id: id.into(),
                content: text.into(),
                timestamp: time,
                sender,
            },
            ticket: "incoming-ticket".into(),
            scene: 3,
            attention: true,
            waiting: sender,
            aged: false,
        }
    }
    #[test]
    fn incoming_ids_deduplicate_but_identical_sent_entries_append() {
        let first =
            stage_contact_request_projection(None, &input("42", false, 100, "hello")).unwrap();
        let duplicate =
            stage_contact_request_projection(Some(&first.row), &input("42", false, 100, "hello"))
                .unwrap();
        assert_eq!(duplicate.row.version, first.row.version);
        let sent =
            stage_contact_request_projection(Some(&first.row), &input("0", true, 101, "same"))
                .unwrap();
        let sent_again =
            stage_contact_request_projection(Some(&sent.row), &input("0", true, 101, "same"))
                .unwrap();
        assert_eq!(sent_again.row.history.len(), 3);
        assert_eq!(sent_again.row.ticket, "incoming-ticket");
        let new_sent =
            stage_contact_request_projection(None, &input("0", true, 101, "same")).unwrap();
        assert_eq!(new_sent.row.ticket, "");
    }
    #[test]
    fn older_messages_append_and_change_detail_without_replacing_current_columns() {
        let first =
            stage_contact_request_projection(None, &input("99", false, 200, "latest")).unwrap();
        let mut prior = first.row;
        prior.detail.extend(bytes(90, b"opaque"));
        prior = finish(prior).unwrap();
        let older =
            stage_contact_request_projection(Some(&prior), &input("98", true, 100, "older"))
                .unwrap();
        assert_eq!(older.row.content, "latest");
        assert_eq!(older.row.timestamp, 200);
        assert!(!older.row.sender);
        assert_eq!(older.row.waiting, Some(true));
        assert_eq!(older.row.history.len(), 2);
        assert!(decode(&older.row.detail)
            .unwrap()
            .iter()
            .any(|f| f.number == 90 && f.value == b"opaque"));
        let cleared = stage_contact_request_attention(&older.row).unwrap();
        assert_eq!(cleared.row.attention, Some(false));
        assert_eq!(cleared.row.waiting, Some(true));
        assert_eq!(cleared.row.history.len(), 2);
        assert_eq!(cleared.row.content, "latest");
    }
    #[test]
    fn staged_write_replays_once_and_rejects_conflicting_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profile");
        let (location, keys, _) = initialize_profile(&path, &[7; 32]).unwrap();
        let options = WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: false,
            durable_intent_id: "fixture-intent".into(),
        };
        let first =
            stage_contact_request_projection(None, &input("42", false, 100, "hello")).unwrap();
        apply_contact_request_projection(&location, &keys, &first, &options).unwrap();
        apply_contact_request_projection(&location, &keys, &first, &options).unwrap();
        let reader = || {
            ProfileReader::open(
                location.clone(),
                keys.clone(),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let old = reader().contact_request("example_peer").unwrap();
        assert_eq!(old.history.len(), 1);
        let next =
            stage_contact_request_projection(Some(&old), &input("43", false, 101, "new")).unwrap();
        apply_contact_request_projection(&location, &keys, &next, &options).unwrap();
        assert!(apply_contact_request_projection(&location, &keys, &first, &options).is_err());
        assert!(apply_contact_request_deletion(
            &location,
            &keys,
            "example_peer",
            &old.version,
            &options
        )
        .is_err());
        apply_contact_request_deletion(
            &location,
            &keys,
            "example_peer",
            &next.row.version,
            &options,
        )
        .unwrap();
        assert!(matches!(
            reader().contact_request("example_peer"),
            Err(ProfileError::NotCached)
        ));
    }

    /// Every non-identity column is nullable and stored rows do hold NULL. Rows
    /// must read, and writing a row back must keep the columns it leaves empty NULL.
    #[test]
    fn rows_with_null_columns_read_and_keep_them_null_on_write_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profile");
        let (location, keys, _) = initialize_profile(&path, &[7; 32]).unwrap();
        let options = WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: false,
            durable_intent_id: "fixture-intent".into(),
        };
        let relative = crate::writer::find_database(&location, "general.db").unwrap();
        let db = || crate::writer::open_write(&location, &relative, &keys, false).unwrap();
        // A received request with NULL remark and labels.
        let received = stage_contact_request_projection(None, &input("42", false, 100, "hello"))
            .unwrap()
            .row;
        db().execute(
            "INSERT INTO FMessageTable VALUES(?1,?2,?3,?4,?5,?6,?7,?8,CAST(?9 AS TEXT),NULL,NULL)",
            params![
                received.username,
                received.message_type,
                received.timestamp,
                received.encrypted_username,
                received.content,
                i64::from(received.sender),
                received.ticket,
                received.scene,
                received.detail
            ],
        )
        .unwrap();
        // Every nullable column NULL.
        db().execute(
            "INSERT INTO FMessageTable VALUES('sparse_peer',NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)",
            [],
        )
        .unwrap();
        let nulls = |user: &str| null_columns(&db(), user).unwrap().unwrap();
        let reader = || {
            ProfileReader::open(
                location.clone(),
                keys.clone(),
                ReadOptions {
                    stopped_or_consistent_snapshot: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let rows = reader().contact_requests().unwrap();
        assert_eq!(rows.len(), 2);
        let peer = reader().contact_request("example_peer").unwrap();
        assert_eq!((peer.remark.as_str(), peer.label_ids.as_str()), ("", ""));
        assert_eq!(peer.history.len(), 1);
        let sparse = reader().contact_request("sparse_peer").unwrap();
        assert_eq!(
            (sparse.message_type, sparse.timestamp, sparse.scene),
            (0, 0, 0)
        );
        assert!(sparse.content.is_empty() && sparse.ticket.is_empty() && sparse.history.is_empty());

        // A later message rewrites the whole row; remark_ and label_ids_ stay NULL.
        let next =
            stage_contact_request_projection(Some(&peer), &input("43", false, 101, "new")).unwrap();
        apply_contact_request_projection(&location, &keys, &next, &options).unwrap();
        assert!(nulls("example_peer").ends_with(&["remark_", "label_ids_"]));
        assert_eq!(
            reader().contact_request("example_peer").unwrap().version,
            next.row.version
        );
        // Columns the message sets get values; the ones it leaves empty stay NULL.
        let mut message = input("44", false, 102, "first");
        message.username = "sparse_peer".into();
        message.encrypted_username = String::new();
        let filled = stage_contact_request_projection(Some(&sparse), &message).unwrap();
        apply_contact_request_projection(&location, &keys, &filled, &options).unwrap();
        // `is_sender_` stays NULL too: a received message leaves it 0, which is empty.
        assert_eq!(
            nulls("sparse_peer"),
            ["encrypt_user_name_", "is_sender_", "remark_", "label_ids_"]
        );
        assert_eq!(
            reader().contact_request("sparse_peer").unwrap().version,
            filled.row.version
        );

        apply_contact_request_deletion(
            &location,
            &keys,
            "example_peer",
            &next.row.version,
            &options,
        )
        .unwrap();
        // New rows are written as before, with empty text.
        let fresh =
            stage_contact_request_projection(None, &input("45", false, 103, "again")).unwrap();
        apply_contact_request_projection(&location, &keys, &fresh, &options).unwrap();
        assert!(nulls("example_peer").is_empty());
    }
}
