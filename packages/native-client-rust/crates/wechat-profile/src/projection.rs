//! Explicit, lossless projections into the inspected ordinary schemas. Callers
//! retain the accepted server intent and opaque values; this module never guesses
//! status, compression, group roster completeness or shard rotation.
use crate::{
    schema::quote_identifier,
    writer::{find_database, open_write},
    *,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use rusqlite::{params, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

pub(crate) fn ownership(
    location: &ProfileLocation,
    keys: &KeyFile,
    options: &WriteOptions,
) -> Result<()> {
    if !options.exclusive_ownership_confirmed {
        return Err(ProfileError::SnapshotRequired);
    }
    if options.durable_intent_id.is_empty() || options.durable_intent_id.len() > 256 {
        return Err(ProfileError::InvalidQuery);
    }
    if location.account_directory() != keys.account_binding {
        return Err(ProfileError::AccountMismatch);
    }
    Ok(())
}
pub(crate) fn identity(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        Err(ProfileError::InvalidQuery)
    } else {
        Ok(())
    }
}
pub(crate) fn map_id(c: &Connection, table: &str, column: &str, name: &str) -> Result<i64> {
    identity(name)?;
    let table = quote_identifier(table);
    let column = quote_identifier(column);
    c.execute(
        &format!("INSERT INTO {table}({column})VALUES(?1)ON CONFLICT({column})DO NOTHING"),
        [name],
    )?;
    c.query_row(
        &format!("SELECT rowid FROM {table} WHERE {column}=?1"),
        [name],
        |r| r.get(0),
    )
    .map_err(Into::into)
}
pub(crate) fn bound(v: &SqlValue) -> Result<(Value, bool)> {
    Ok(match v {
        SqlValue::Null => (Value::Null, false),
        SqlValue::Integer(s) => (
            Value::Integer(s.parse().map_err(|_| ProfileError::InvalidMetadata)?),
            false,
        ),
        SqlValue::Real(s) => {
            let f: f64 = s.parse().map_err(|_| ProfileError::InvalidMetadata)?;
            if !f.is_finite() {
                return Err(ProfileError::InvalidMetadata);
            }
            (Value::Real(f), false)
        }
        SqlValue::Text(s) => {
            if s.len() > 4 * 1024 * 1024 {
                return Err(ProfileError::ResourceLimit);
            }
            (Value::Text(s.clone()), false)
        }
        SqlValue::TextBytes(s) | SqlValue::Blob(s) => {
            let b = STANDARD
                .decode(s)
                .map_err(|_| ProfileError::InvalidMetadata)?;
            if b.len() > 4 * 1024 * 1024 {
                return Err(ProfileError::ResourceLimit);
            }
            (Value::Blob(b), matches!(v, SqlValue::TextBytes(_)))
        }
    })
}
fn row_values(
    c: &Connection,
    table: &str,
    columns: &[String],
    key_column: &str,
    key: &Value,
) -> Result<Option<RawFields>> {
    if columns.is_empty() {
        return Err(ProfileError::InvalidMetadata);
    }
    let q = format!(
        "SELECT {} FROM {} WHERE {}=?1",
        columns
            .iter()
            .map(|s| quote_identifier(s))
            .collect::<Vec<_>>()
            .join(","),
        quote_identifier(table),
        quote_identifier(key_column)
    );
    c.query_row(&q, [key], crate::reader::raw)
        .optional()
        .map_err(Into::into)
}
pub(crate) fn check_known_table(c: &Connection, table: &str) -> Result<()> {
    let relative = match table {
        "contact" | "chat_room" | "chat_room_info_detail" | "chatroom_member" | "name2id" => {
            "contact/contact.db"
        }
        "SessionTable" | "SessionUnreadListTable_1" | "SessionUnreadStatTable_1" => {
            "session/session.db"
        }
        "MessageResourceInfo" | "MessageResourceDetail" | "ChatName2Id" | "SenderName2Id" => {
            "message/message_resource.db"
        }
        "VoiceInfo" => "message/media_0.db",
        "image_hardlink_info_v4"
        | "file_hardlink_info_v4"
        | "video_hardlink_info_v4"
        | "dir2id"
        | "db_info" => "hardlink/hardlink.db",
        "head_image" => "head_image/head_image.db",
        _ => return Err(ProfileError::WritesUnsupported),
    };
    let sql = crate::initialize::LAYOUTS
        .iter()
        .find(|x| x.0 == relative)
        .ok_or(ProfileError::WritesUnsupported)?
        .1;
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(sql)?;
    let expected = crate::schema::inspect(&expected, relative)?;
    let actual = crate::schema::inspect(c, relative)?;
    let a = actual
        .tables
        .iter()
        .find(|t| t.name == table)
        .ok_or_else(|| ProfileError::UnsupportedSchema(relative.into()))?;
    let e = expected
        .tables
        .iter()
        .find(|t| t.name == table)
        .ok_or(ProfileError::InvalidMetadata)?;
    let index = |t: &crate::TableSchema| {
        t.indexes
            .iter()
            .map(|i| (i.name.clone(), i.unique, i.partial, i.columns.clone()))
            .collect::<Vec<_>>()
    };
    if a.columns != e.columns || !a.triggers.is_empty() || index(a) != index(e) {
        return Err(ProfileError::UnsupportedSchema(relative.into()));
    }
    Ok(())
}
fn apply_row(
    c: &Connection,
    table: &str,
    key_column: &str,
    key: SqlValue,
    fields: &RawFields,
    require_complete_new: bool,
) -> Result<bool> {
    check_known_table(c, table)?;
    let report = crate::schema::inspect(c, "projection")?;
    let t = report
        .tables
        .iter()
        .find(|t| t.name == table)
        .ok_or_else(|| ProfileError::UnsupportedSchema(table.into()))?;
    if !t.triggers.is_empty() {
        return Err(ProfileError::UnsupportedSchema(table.into()));
    }
    let columns = t.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
    if fields.contains_key(key_column) || fields.keys().any(|f| !columns.contains(f)) {
        return Err(ProfileError::InvalidMetadata);
    }
    let (key_value, _) = bound(&key)?;
    let selected = fields.keys().cloned().collect::<Vec<_>>();
    let existing = row_values(c, table, &selected, key_column, &key_value)?;
    if existing.as_ref().is_some_and(|r| r == fields) {
        return Ok(false);
    }
    if existing.is_none()
        && require_complete_new
        && columns
            .iter()
            .any(|col| col != key_column && !fields.contains_key(col))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut all = fields.clone();
    all.insert(key_column.into(), key);
    let mut values = vec![];
    let mut expr = vec![];
    let names = all.keys().cloned().collect::<Vec<_>>();
    for v in all.values() {
        let (value, as_text) = bound(v)?;
        values.push(value);
        let p = format!("?{}", values.len());
        expr.push(if as_text {
            format!("CAST({p} AS TEXT)")
        } else {
            p
        });
    }
    let updates = names
        .iter()
        .filter(|n| n.as_str() != key_column)
        .map(|n| format!("{}=excluded.{}", quote_identifier(n), quote_identifier(n)))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "INSERT INTO {}({})VALUES({})ON CONFLICT({})DO UPDATE SET {}",
        quote_identifier(table),
        names
            .iter()
            .map(|s| quote_identifier(s))
            .collect::<Vec<_>>()
            .join(","),
        expr.join(","),
        quote_identifier(key_column),
        updates
    );
    c.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(true)
}
/// Full scalar/BLOB contact data with id/username allocated from the destination
/// name map. Existing rows accept patches; new rows require every remaining field.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactProjection {
    pub username: String,
    pub fields: RawFields,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedContact {
    pub local_id: i64,
    pub changed: bool,
}
pub fn apply_contact_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &ContactProjection,
    options: &WriteOptions,
) -> Result<AppliedContact> {
    ownership(location, keys, options)?;
    identity(&input.username)?;
    if input.fields.contains_key("id") || input.fields.contains_key("username") {
        return Err(ProfileError::InvalidMetadata);
    }
    let db = find_database(location, "contact.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    let tx = c.transaction()?;
    let id = map_id(&tx, "name2id", "username", &input.username)?;
    let mut fields = input.fields.clone();
    fields.insert("username".into(), SqlValue::Text(input.username.clone()));
    let changed = apply_row(
        &tx,
        "contact",
        "id",
        SqlValue::Integer(id.to_string()),
        &fields,
        true,
    )?;
    tx.commit()?;
    Ok(AppliedContact {
        local_id: id,
        changed,
    })
}
/// Caller supplies the actual retained group fields; complete_roster is explicit.
/// No absent member is deleted for an incomplete/delta roster.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupProjection {
    pub username: String,
    pub fields: RawFields,
    pub detail_fields: Option<RawFields>,
    pub members: Vec<String>,
    pub complete_roster: bool,
}
fn proto_var(data: &[u8], at: &mut usize) -> Result<u64> {
    let mut n = 0;
    for i in 0..10 {
        let b = *data.get(*at).ok_or(ProfileError::InvalidMetadata)?;
        *at += 1;
        if i == 9 && b > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        n |= u64::from(b & 127) << (i * 7);
        if b & 128 == 0 {
            return Ok(n);
        }
    }
    Err(ProfileError::InvalidMetadata)
}
pub(crate) fn proto_fields(data: &[u8]) -> Result<Vec<(u64, u8, &[u8])>> {
    let mut at = 0;
    let mut out = vec![];
    if data.len() > 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    while at < data.len() {
        let tag = proto_var(data, &mut at)?;
        let wire = (tag & 7) as u8;
        if tag >> 3 == 0 {
            return Err(ProfileError::InvalidMetadata);
        }
        let start = at;
        match wire {
            0 => {
                proto_var(data, &mut at)?;
            }
            1 => at = at.checked_add(8).ok_or(ProfileError::ResourceLimit)?,
            2 => {
                let n = usize::try_from(proto_var(data, &mut at)?)
                    .map_err(|_| ProfileError::ResourceLimit)?;
                let start = at;
                at = at.checked_add(n).ok_or(ProfileError::ResourceLimit)?;
                out.push((
                    tag >> 3,
                    wire,
                    data.get(start..at).ok_or(ProfileError::InvalidMetadata)?,
                ));
                continue;
            }
            5 => at = at.checked_add(4).ok_or(ProfileError::ResourceLimit)?,
            _ => return Err(ProfileError::InvalidMetadata),
        }
        out.push((
            tag >> 3,
            wire,
            data.get(start..at).ok_or(ProfileError::InvalidMetadata)?,
        ));
        if out.len() > 65536 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    Ok(out)
}
pub fn apply_group_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &GroupProjection,
    options: &WriteOptions,
) -> Result<()> {
    ownership(location, keys, options)?;
    identity(&input.username)?;
    if !input.username.ends_with("@chatroom")
        || input.members.len() > 10000
        || input
            .members
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != input.members.len()
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let db = find_database(location, "contact.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    let tx = c.transaction()?;
    let id = map_id(&tx, "name2id", "username", &input.username)?;
    let exists = tx
        .query_row(
            "SELECT id FROM contact WHERE id=?1 AND username=?2",
            params![id, input.username],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut fields = input.fields.clone();
    if fields.contains_key("id") || fields.contains_key("username") {
        return Err(ProfileError::InvalidMetadata);
    }
    fields.insert("username".into(), SqlValue::Text(input.username.clone()));
    if input.complete_roster {
        let raw = match fields.get("ext_buffer") {
            Some(SqlValue::Blob(v)) => STANDARD
                .decode(v)
                .map_err(|_| ProfileError::InvalidMetadata)?,
            _ => return Err(ProfileError::InvalidMetadata),
        };
        let mut members = std::collections::BTreeSet::new();
        for (_, wire, nested) in proto_fields(&raw)?.into_iter().filter(|f| f.0 == 1) {
            if wire != 2 {
                return Err(ProfileError::InvalidMetadata);
            }
            for (field, wire, v) in proto_fields(nested)? {
                if field == 1 {
                    if wire != 2 {
                        return Err(ProfileError::InvalidMetadata);
                    }
                    members.insert(
                        std::str::from_utf8(v)
                            .map_err(|_| ProfileError::InvalidMetadata)?
                            .to_owned(),
                    );
                }
            }
        }
        if members != input.members.iter().cloned().collect() {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    apply_row(
        &tx,
        "chat_room",
        "id",
        SqlValue::Integer(id.to_string()),
        &fields,
        true,
    )?;
    if let Some(fields) = &input.detail_fields {
        apply_row(
            &tx,
            "chat_room_info_detail",
            "room_id_",
            SqlValue::Integer(id.to_string()),
            fields,
            true,
        )?;
    }
    let mut member_ids = vec![];
    for name in &input.members {
        identity(name)?;
        let member = tx
            .query_row("SELECT id FROM contact WHERE username=?1", [name], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            .ok_or(ProfileError::InvalidMetadata)?;
        member_ids.push(member)
    }
    if input.complete_roster {
        let existing = tx
            .prepare("SELECT member_id FROM chatroom_member WHERE room_id=?1")?
            .query_map([id], |r| r.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for old in existing {
            if !member_ids.contains(&old) {
                tx.execute(
                    "DELETE FROM chatroom_member WHERE room_id=?1 AND member_id=?2",
                    params![id, old],
                )?;
            }
        }
    }
    for member in member_ids {
        tx.execute("INSERT INTO chatroom_member(room_id,member_id)VALUES(?1,?2)ON CONFLICT(room_id,member_id)DO NOTHING",params![id,member])?;
    }
    tx.commit()?;
    Ok(())
}
/// Exact stored columns supplied by a protocol/build adapter, excluding fields
/// allocated/transformed by this destination (local_id, sender id, packed endpoints).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageProjection {
    pub peer: String,
    pub sender: String,
    pub from: String,
    pub to: String,
    pub server_message_id: String,
    pub fields: RawFields,
    /// Complete encoded fields other than endpoints1/2, including opaque fields.
    pub packed_other_fields: Vec<u8>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppliedMessage {
    pub source_id: SourceMessageId,
    pub inserted: bool,
}
pub(crate) fn var_out(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push((n as u8 & 127) | 128);
        n >>= 7
    }
    out.push(n as u8)
}
pub fn apply_message_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &MessageProjection,
    options: &WriteOptions,
) -> Result<AppliedMessage> {
    ownership(location, keys, options)?;
    for s in [&input.peer, &input.sender, &input.from, &input.to] {
        identity(s)?
    }
    let sid = input
        .server_message_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    if sid == 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let table = format!("Msg_{:x}", Md5::digest(input.peer.as_bytes()));
    for (f, _, _) in proto_fields(&input.packed_other_fields)? {
        if f == 1 || f == 2 {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let db = find_database(location, "message_0.db")?;
    crate::writer::require_single_message_shard(location)?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    let tx = c.transaction()?;
    let sender = map_id(&tx, "Name2Id", "user_name", &input.sender)?;
    let from = map_id(&tx, "Name2Id", "user_name", &input.from)?;
    let to = map_id(&tx, "Name2Id", "user_name", &input.to)?;
    tx.execute_batch(
        &include_str!("layout/conversation.sql")
            .replace("__MESSAGE_TABLE__", &table)
            .replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
            .replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS "),
    )?;
    let schema = crate::writer::exact_columns(&tx, &db, &table, crate::writer::MESSAGE_COLUMNS)?;
    let indexes = [
        ("_SERVERID", vec!["server_id"]),
        ("_SORTSEQ", vec!["sort_seq"]),
        ("_TYPE_SEQ", vec!["local_type", "sort_seq"]),
        ("_SENDERID", vec!["real_sender_id"]),
    ];
    if schema.indexes.len() != 4
        || !indexes.iter().all(|(suffix, cols)| {
            schema.indexes.iter().any(|i| {
                i.name == format!("{table}{suffix}")
                    && !i.unique
                    && !i.partial
                    && i.columns.iter().map(|x| x.as_deref()).collect::<Vec<_>>()
                        == cols.iter().map(|x| Some(*x)).collect::<Vec<_>>()
            })
        })
    {
        return Err(ProfileError::UnsupportedSchema(db));
    }
    let mut fields = input.fields.clone();
    for name in [
        "local_id",
        "server_id",
        "real_sender_id",
        "packed_info_data",
    ] {
        if fields.contains_key(name) {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let mut packed = vec![8];
    var_out(from as u64, &mut packed);
    packed.push(16);
    var_out(to as u64, &mut packed);
    packed.extend(&input.packed_other_fields);
    fields.insert(
        "server_id".into(),
        SqlValue::Integer((sid as i64).to_string()),
    );
    fields.insert(
        "real_sender_id".into(),
        SqlValue::Integer(sender.to_string()),
    );
    fields.insert(
        "packed_info_data".into(),
        SqlValue::Blob(STANDARD.encode(packed)),
    );
    let ids = tx
        .prepare(&format!(
            "SELECT local_id FROM {} WHERE server_id=?1",
            quote_identifier(&table)
        ))?
        .query_map([sid as i64], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.len() > 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    let (local, inserted) = if let Some(id) = ids.first() {
        let cols = fields.keys().cloned().collect::<Vec<_>>();
        if row_values(&tx, &table, &cols, "local_id", &Value::Integer(*id))?.as_ref()
            != Some(&fields)
        {
            return Err(ProfileError::InvalidMetadata);
        }
        (*id, false)
    } else {
        let schema = crate::schema::inspect(&tx, &db)?;
        let expected = schema
            .tables
            .iter()
            .find(|t| t.name == table)
            .ok_or(ProfileError::InvalidMetadata)?;
        if fields.len() + 1 != expected.columns.len()
            || expected
                .columns
                .iter()
                .any(|col| col.name != "local_id" && !fields.contains_key(&col.name))
        {
            return Err(ProfileError::InvalidMetadata);
        }
        // No invented tie-breaker. The explicit adapter must provide a collision-free
        // sort sequence, or resolve the collision under a separately proved policy.
        let sort = match fields.get("sort_seq") {
            Some(SqlValue::Integer(n)) => n
                .parse::<i64>()
                .map_err(|_| ProfileError::InvalidMetadata)?,
            _ => return Err(ProfileError::InvalidMetadata),
        };
        let count: i64 = tx.query_row(
            &format!(
                "SELECT count(*) FROM {} WHERE sort_seq=?1",
                quote_identifier(&table)
            ),
            [sort],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err(ProfileError::WritesUnsupported);
        }
        // Temporarily use the next AUTOINCREMENT through an explicit NULL key; SQLite
        // itself performs allocation and preserves its sqlite_sequence namespace.
        let names = fields.keys().cloned().collect::<Vec<_>>();
        let mut vals = vec![];
        let mut expr = vec![];
        for v in fields.values() {
            let (value, text) = bound(v)?;
            vals.push(value);
            let p = format!("?{}", vals.len());
            expr.push(if text {
                format!("CAST({p} AS TEXT)")
            } else {
                p
            })
        }
        tx.execute(
            &format!(
                "INSERT INTO {}({})VALUES({})",
                quote_identifier(&table),
                names
                    .iter()
                    .map(|s| quote_identifier(s))
                    .collect::<Vec<_>>()
                    .join(","),
                expr.join(",")
            ),
            rusqlite::params_from_iter(vals),
        )?;
        (tx.last_insert_rowid(), true)
    };
    tx.commit()?;
    Ok(AppliedMessage {
        source_id: SourceMessageId {
            database: db,
            table,
            local_id: local.to_string(),
        },
        inserted,
    })
}
/// Explicit session fields. New sessions require a complete row; patches retain
/// unrelated drafts/read metadata. Caller determines actual native values.
pub fn apply_session_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    fields: &RawFields,
    options: &WriteOptions,
) -> Result<bool> {
    ownership(location, keys, options)?;
    identity(peer)?;
    let db = find_database(location, "session.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    let tx = c.transaction()?;
    map_id(&tx, "Name2Id", "user_name", peer)?;
    let changed = apply_row(
        &tx,
        "SessionTable",
        "username",
        SqlValue::Text(peer.into()),
        fields,
        true,
    )?;
    tx.commit()?;
    Ok(changed)
}

/// Sparse contact update. Existing rows preserve every omitted field. For a new
/// row only, omitted nullable/no-default schema columns become SQL NULL as an
/// explicit independent-client policy, not a claim about native enum defaults.
pub fn apply_contact_patch(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &ContactProjection,
    options: &WriteOptions,
) -> Result<AppliedContact> {
    ownership(location, keys, options)?;
    identity(&input.username)?;
    let db = find_database(location, "contact.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "contact")?;
    let exists = c
        .query_row(
            "SELECT id FROM contact WHERE username=?1",
            [&input.username],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    let mut input = input.clone();
    if !exists {
        let mut fields = nullable_fields(&c, &db, "contact", &["id", "username"])?;
        fields.extend(input.fields);
        input.fields = fields;
    }
    drop(c);
    apply_contact_projection(location, keys, &input, options)
}
fn nullable_fields(
    c: &Connection,
    database: &str,
    table: &str,
    exclude: &[&str],
) -> Result<RawFields> {
    let report = crate::schema::inspect(c, database)?;
    let t = report
        .tables
        .iter()
        .find(|t| t.name == table)
        .ok_or(ProfileError::InvalidMetadata)?;
    let mut fields = RawFields::new();
    for col in &t.columns {
        if exclude.contains(&col.name.as_str()) {
            continue;
        }
        if col.not_null || col.default_sql.is_some() {
            return Err(ProfileError::WritesUnsupported);
        }
        fields.insert(col.name.clone(), SqlValue::Null);
    }
    Ok(fields)
}
/// Sparse group updates use the same explicit SQL NULL policy for a new row.
/// Existing owner/opaque buffers are preserved unless specifically supplied.
pub fn apply_group_patch(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &GroupProjection,
    options: &WriteOptions,
) -> Result<()> {
    ownership(location, keys, options)?;
    let db = find_database(location, "contact.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "chat_room")?;
    let exists = c
        .query_row(
            "SELECT id FROM chat_room WHERE username=?1",
            [&input.username],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some();
    let mut input = input.clone();
    if !exists {
        let mut fields = nullable_fields(&c, &db, "chat_room", &["id", "username"])?;
        fields.extend(input.fields);
        input.fields = fields;
    }
    if let Some(detail) = &input.detail_fields {
        check_known_table(&c, "chat_room_info_detail")?;
        let exists = c
            .query_row(
                "SELECT room_id_ FROM chat_room_info_detail WHERE username_=?1",
                [&input.username],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        if !exists {
            let mut fields = nullable_fields(&c, &db, "chat_room_info_detail", &["room_id_"])?;
            fields.extend(detail.clone());
            input.detail_fields = Some(fields);
        }
    }
    drop(c);
    apply_group_projection(location, keys, &input, options)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeMemberPatch {
    pub username: String,
    pub display: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeRosterPatch {
    pub username: String,
    pub members: Vec<NativeMemberPatch>,
    pub complete_roster: bool,
}
struct ProtoChunk<'a> {
    field: u64,
    wire: u8,
    raw: &'a [u8],
    value: &'a [u8],
}
fn proto_chunks(data: &[u8]) -> Result<Vec<ProtoChunk<'_>>> {
    if data.len() > 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut at = 0;
    let mut chunks = vec![];
    while at < data.len() {
        let start = at;
        let tag = proto_var(data, &mut at)?;
        let field = tag >> 3;
        let wire = (tag & 7) as u8;
        if field == 0 || field >= 1 << 29 {
            return Err(ProfileError::InvalidMetadata);
        }
        let mut value_start = at;
        match wire {
            0 => {
                proto_var(data, &mut at)?;
            }
            1 => at = at.checked_add(8).ok_or(ProfileError::ResourceLimit)?,
            5 => at = at.checked_add(4).ok_or(ProfileError::ResourceLimit)?,
            2 => {
                let size = usize::try_from(proto_var(data, &mut at)?)
                    .map_err(|_| ProfileError::ResourceLimit)?;
                value_start = at;
                at = at.checked_add(size).ok_or(ProfileError::ResourceLimit)?
            }
            _ => return Err(ProfileError::InvalidMetadata),
        }
        let raw = data.get(start..at).ok_or(ProfileError::InvalidMetadata)?;
        let value = data
            .get(value_start..at)
            .ok_or(ProfileError::InvalidMetadata)?;
        chunks.push(ProtoChunk {
            field,
            wire,
            raw,
            value,
        });
        if chunks.len() > 65536 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    Ok(chunks)
}
fn proto_bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
    var_out((field << 3) | 2, out);
    var_out(value.len() as u64, out);
    out.extend_from_slice(value)
}
fn member_name(data: &[u8]) -> Result<String> {
    let mut name = None;
    let mut display_seen = false;
    for chunk in proto_chunks(data)? {
        match chunk.field {
            1 => {
                if chunk.wire != 2 || name.is_some() {
                    return Err(ProfileError::InvalidMetadata);
                }
                let value =
                    std::str::from_utf8(chunk.value).map_err(|_| ProfileError::InvalidMetadata)?;
                identity(value)?;
                name = Some(value.to_owned());
            }
            2 => {
                if chunk.wire != 2 || display_seen {
                    return Err(ProfileError::InvalidMetadata);
                }
                std::str::from_utf8(chunk.value).map_err(|_| ProfileError::InvalidMetadata)?;
                display_seen = true;
            }
            _ => {}
        }
    }
    name.ok_or(ProfileError::InvalidMetadata)
}
fn patch_member(old: Option<&[u8]>, patch: &NativeMemberPatch) -> Result<Vec<u8>> {
    identity(&patch.username)?;
    if patch
        .display
        .as_ref()
        .is_some_and(|s| s.len() > 65536 || s.contains('\0'))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut out = vec![];
    let mut wrote_display = false;
    if let Some(old) = old {
        if member_name(old)? != patch.username {
            return Err(ProfileError::InvalidMetadata);
        }
        for chunk in proto_chunks(old)? {
            if chunk.field == 2 {
                if let Some(display) = &patch.display {
                    proto_bytes(2, display.as_bytes(), &mut out);
                    wrote_display = true;
                } else {
                    out.extend_from_slice(chunk.raw);
                }
            } else {
                out.extend_from_slice(chunk.raw);
            }
        }
    } else {
        proto_bytes(1, patch.username.as_bytes(), &mut out);
    }
    if !wrote_display {
        if let Some(display) = &patch.display {
            proto_bytes(2, display.as_bytes(), &mut out)
        }
    }
    Ok(out)
}
/// Translate only the proved network member identity/display semantics into the
/// local repeated-field1 representation. Caller proves roster completeness (for
/// example, a present declared count on a version-zero full response). Unknown
/// root/member protobuf chunks, existing owner and unrelated contact fields are
/// retained byte-for-byte. No network version is guessed into opaque fields.
pub fn apply_native_roster_patch(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &NativeRosterPatch,
    options: &WriteOptions,
) -> Result<()> {
    ownership(location, keys, options)?;
    identity(&input.username)?;
    if !input.username.ends_with("@chatroom") || input.members.len() > 10000 {
        return Err(ProfileError::InvalidMetadata);
    }
    let mut patches = std::collections::BTreeMap::new();
    for member in &input.members {
        identity(&member.username)?;
        if patches.insert(member.username.clone(), member).is_some() {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    let db = find_database(location, "contact.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    check_known_table(&c, "chat_room")?;
    check_known_table(&c, "chatroom_member")?;
    let room = c
        .query_row(
            "SELECT id,ext_buffer FROM chat_room WHERE username=?1",
            [&input.username],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
        )
        .optional()?;
    let old = room
        .as_ref()
        .and_then(|(_, bytes)| bytes.as_deref())
        .unwrap_or(&[]);
    let chunks = proto_chunks(old)?;
    let mut old_members = std::collections::BTreeMap::new();
    for chunk in &chunks {
        if chunk.field == 1 {
            if chunk.wire != 2 {
                return Err(ProfileError::InvalidMetadata);
            }
            let name = member_name(chunk.value)?;
            if old_members.insert(name, chunk.value).is_some() {
                return Err(ProfileError::InvalidMetadata);
            }
        }
    }
    let mut result_members = if input.complete_roster {
        std::collections::BTreeSet::new()
    } else {
        old_members.keys().cloned().collect()
    };
    if !input.complete_roster {
        if let Some((id, _)) = &room {
            let names=c.prepare("SELECT c.username FROM chatroom_member m JOIN contact c ON c.id=m.member_id WHERE m.room_id=?1")?.query_map([id],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            result_members.extend(names);
        }
    }
    result_members.extend(patches.keys().cloned());
    let mut packed = vec![];
    let mut written = std::collections::BTreeSet::new();
    for chunk in chunks {
        if chunk.field != 1 {
            packed.extend_from_slice(chunk.raw);
            continue;
        }
        let name = member_name(chunk.value)?;
        if !result_members.contains(&name) {
            continue;
        }
        if let Some(patch) = patches.get(&name) {
            proto_bytes(1, &patch_member(Some(chunk.value), patch)?, &mut packed)
        } else {
            packed.extend_from_slice(chunk.raw)
        }
        written.insert(name);
    }
    // New members have no invented opaque metadata. Their contact rows must already
    // exist; the SQL projection checks that binding and allocates destination IDs.
    for name in result_members.difference(&written) {
        let empty = NativeMemberPatch {
            username: name.clone(),
            display: None,
        };
        let patch = patches.get(name).copied().unwrap_or(&empty);
        proto_bytes(1, &patch_member(None, patch)?, &mut packed);
    }
    let mut fields = RawFields::new();
    fields.insert("ext_buffer".into(), SqlValue::Blob(STANDARD.encode(packed)));
    drop(c);
    apply_group_patch(
        location,
        keys,
        &GroupProjection {
            username: input.username.clone(),
            fields,
            detail_fields: None,
            members: result_members.into_iter().collect(),
            complete_roster: input.complete_roster,
        },
        options,
    )
}

/// WCDB compression state 4 observed on the supported fields. Compression is
/// independent of media type; raw protocol bytes stay in the durable sidecar.
pub fn compressed_profile_field(bytes: &[u8]) -> Result<SqlValue> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let compressed =
        zstd::stream::encode_all(bytes, 3).map_err(|_| ProfileError::InvalidMetadata)?;
    Ok(SqlValue::Blob(STANDARD.encode(compressed)))
}
/// Find one exact server identity in the sole supported ordinary shard. This
/// never adopts a first match from an ambiguous or rotated shard family.
pub fn find_projected_message(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server_id: &str,
    options: &WriteOptions,
) -> Result<Option<SourceMessageId>> {
    ownership(location, keys, options)?;
    identity(peer)?;
    crate::writer::require_single_message_shard(location)?;
    let sid = server_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    if sid == 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let db = find_database(location, "message_0.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    let table = format!("Msg_{:x}", Md5::digest(peer.as_bytes()));
    let present = c.query_row(
        "SELECT count(*)FROM sqlite_master WHERE type='table' AND name=?1",
        [&table],
        |r| r.get::<_, i64>(0),
    )?;
    if present == 0 {
        return Ok(None);
    }
    crate::writer::exact_columns(&c, &db, &table, crate::writer::MESSAGE_COLUMNS)?;
    let ids = c
        .prepare(&format!(
            "SELECT local_id FROM {} WHERE server_id=?1 LIMIT 2",
            quote_identifier(&table)
        ))?
        .query_map([sid as i64], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if ids.len() > 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(ids.first().map(|id| SourceMessageId {
        database: db,
        table,
        local_id: id.to_string(),
    }))
}
/// Choose the first unused millisecond slot in this message's second, or retain
/// the already stored slot for a replay. This is an independent collision policy
/// within the observed native 1000-slots/second representation, not a recovered
/// native rollover algorithm. Caller holds the lease through insertion.
pub fn choose_message_sort_sequence(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server_id: &str,
    time: i64,
    options: &WriteOptions,
) -> Result<i64> {
    ownership(location, keys, options)?;
    identity(peer)?;
    if time < 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let base = time.checked_mul(1000).ok_or(ProfileError::ResourceLimit)?;
    let end = base.checked_add(999).ok_or(ProfileError::ResourceLimit)?;
    let db = find_database(location, "message_0.db")?;
    let c = open_write(location, &db, keys, options.allow_plaintext)?;
    let table = format!("Msg_{:x}", Md5::digest(peer.as_bytes()));
    if c.query_row(
        "SELECT count(*)FROM sqlite_master WHERE type='table' AND name=?1",
        [&table],
        |r| r.get::<_, i64>(0),
    )? == 0
    {
        return Ok(base);
    }
    if let Some(id) = find_projected_message(location, keys, peer, server_id, options)? {
        let value = c.query_row(
            &format!(
                "SELECT create_time,sort_seq FROM {} WHERE local_id=?1",
                quote_identifier(&table)
            ),
            [id.local_id
                .parse::<i64>()
                .map_err(|_| ProfileError::InvalidMetadata)?],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )?;
        if value.0 != time {
            return Err(ProfileError::InvalidMetadata);
        }
        return Ok(value.1);
    }
    let used = c
        .prepare(&format!(
            "SELECT sort_seq FROM {} WHERE sort_seq BETWEEN ?1 AND ?2",
            quote_identifier(&table)
        ))?
        .query_map(params![base, end], |r| r.get::<_, i64>(0))?
        .collect::<std::result::Result<std::collections::BTreeSet<_>, _>>()?;
    (base..=end)
        .find(|n| !used.contains(n))
        .ok_or(ProfileError::ResourceLimit)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MessageSessionProjection {
    pub source: SourceMessageId,
    pub peer: String,
    pub summary: String,
    pub outgoing: bool,
    pub unread: bool,
    pub sender: Option<String>,
    pub sender_display: Option<String>,
    /// Only a durable coordinator intent can establish that an existing row came
    /// from this unfinished projection. Imported/adopted rows never gain unread.
    pub created_by_this_intent: bool,
}
/// Reconcile a committed message into a desktop session, preserving drafts,
/// unrelated unread rows and preexisting counter/list discrepancies. It makes no
/// server/phone read request and is retryable after a message-only commit.
pub fn apply_message_session_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    input: &MessageSessionProjection,
    options: &WriteOptions,
) -> Result<()> {
    ownership(location, keys, options)?;
    identity(&input.peer)?;
    if input.summary.len() > 4 * 1024 * 1024
        || input.source.table != format!("Msg_{:x}", Md5::digest(input.peer.as_bytes()))
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let messages = open_write(
        location,
        &input.source.database,
        keys,
        options.allow_plaintext,
    )?;
    crate::writer::exact_columns(
        &messages,
        &input.source.database,
        &input.source.table,
        crate::writer::MESSAGE_COLUMNS,
    )?;
    let local = input
        .source
        .local_id
        .parse::<i64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let (server, time, kind) = messages.query_row(
        &format!(
            "SELECT server_id,create_time,local_type FROM {} WHERE local_id=?1",
            quote_identifier(&input.source.table)
        ),
        [local],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )?;
    drop(messages);
    let db = find_database(location, "session.db")?;
    let mut c = open_write(location, &db, keys, options.allow_plaintext)?;
    for t in [
        "SessionTable",
        "SessionUnreadListTable_1",
        "SessionUnreadStatTable_1",
    ] {
        check_known_table(&c, t)?;
    }
    let tx = c.transaction()?;
    let session = map_id(&tx, "Name2Id", "user_name", &input.peer)?;
    let old=tx.query_row("SELECT unread_count,last_msg_locald_id,last_timestamp,last_clear_unread_timestamp FROM SessionTable WHERE username=?1",[&input.peer],|r|Ok((r.get::<_,Option<i64>>(0)?.unwrap_or(0),r.get::<_,Option<i64>>(1)?.unwrap_or(0),r.get::<_,Option<i64>>(2)?.unwrap_or(0),r.get::<_,Option<i64>>(3)?.unwrap_or(0)))).optional()?;
    if old.is_none() {
        tx.execute("INSERT INTO SessionTable(username,type,unread_count,unread_first_msg_srv_id,unread_first_pat_msg_local_id,unread_first_pat_msg_sort_seq,is_hidden,summary,draft,status,last_timestamp,sort_timestamp,last_clear_unread_timestamp,last_msg_locald_id,last_msg_type,last_msg_sub_type,last_msg_sender,last_sender_display_name,last_msg_ext_type)VALUES(?1,0,0,0,0,0,0,'','',0,0,0,0,0,0,0,'','',0)",[&input.peer])?;
    }
    let old = old.unwrap_or((0, 0, 0, 0));
    if old.0 < 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    let unread=tx.query_row("SELECT local_id,create_time FROM SessionUnreadListTable_1 WHERE username_id=?1 AND server_id=?2",params![session,server],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).optional()?;
    if unread.is_some_and(|v| v != (local, time)) {
        return Err(ProfileError::InvalidMetadata);
    }
    let done = old.1 == local && old.2 == time;
    if !input.outgoing
        && input.unread
        && input.created_by_this_intent
        && unread.is_none()
        && !done
        && old.3 < time
    {
        // A newer sidebar summary is not a read marker: sync may arrive out of
        // order. Only an intent-owned new row and the explicit local clear cutoff
        // permit this addition; imported/adopted rows never regain unread.
        tx.execute("INSERT INTO SessionUnreadListTable_1(username_id,server_id,local_id,create_time)VALUES(?1,?2,?3,?4)",params![session,server,local,time])?;
        tx.execute("UPDATE SessionTable SET unread_first_msg_srv_id=CASE WHEN unread_count=0 THEN ?1 ELSE unread_first_msg_srv_id END,unread_count=unread_count+1 WHERE username=?2",params![server,input.peer])?;
    }
    if old.2 <= time && (old.2 < time || old.1 <= local) {
        let sender = if input.peer.ends_with("@chatroom") {
            input.sender.as_deref().unwrap_or("")
        } else {
            ""
        };
        let display = if input.peer.ends_with("@chatroom") {
            input.sender_display.as_deref().unwrap_or("")
        } else {
            ""
        };
        tx.execute("UPDATE SessionTable SET summary=?1,last_timestamp=?2,sort_timestamp=?2,last_msg_locald_id=?3,last_msg_type=?4,last_msg_sub_type=?5,last_msg_sender=?6,last_sender_display_name=?7,last_msg_ext_type=0 WHERE username=?8",params![input.summary,time,local,kind&0x7fffffff,kind>>32,sender,display,input.peer])?;
    }
    tx.commit()?;
    Ok(())
}
