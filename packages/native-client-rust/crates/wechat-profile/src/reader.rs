use crate::{
    model::*,
    schema::{inspect, message_table, quote_identifier},
    snapshot::Snapshot,
    KeyFile, ProfileError, ProfileLocation, ReadOptions, Result, SchemaReport,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use md5::Md5;
use rusqlite::{params, types::ValueRef, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
};

pub struct ProfileReader {
    pub(crate) snapshot: Snapshot,
    schemas: Vec<SchemaReport>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    version: u32,
    generation: String,
    query: String,
    key: CursorKey,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum CursorKey {
    Name {
        name: String,
    },
    Thread {
        time: i64,
        name: String,
    },
    Message {
        time: i64,
        database: String,
        table: String,
        local: i64,
    },
}

fn limit(n: Option<usize>) -> Result<usize> {
    let n = n.unwrap_or(50);
    if !(1..=200).contains(&n) {
        Err(ProfileError::InvalidQuery)
    } else {
        Ok(n)
    }
}
fn query_hash<T: Serialize>(kind: &str, q: &T) -> Result<String> {
    let b = serde_json::to_vec(&(kind, q)).map_err(|_| ProfileError::InvalidQuery)?;
    Ok(hex::encode(Sha256::digest(b)))
}
fn encode_cursor(generation: &str, query: &str, key: CursorKey) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&Cursor {
            version: 1,
            generation: generation.into(),
            query: query.into(),
            key,
        })
        .map_err(|_| ProfileError::InvalidCursor)?,
    ))
}
fn decode_cursor(s: Option<&str>, generation: &str, query: &str) -> Result<Option<CursorKey>> {
    let Some(s) = s else {
        return Ok(None);
    };
    if s.len() > 8192 {
        return Err(ProfileError::InvalidCursor);
    }
    let b = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| ProfileError::InvalidCursor)?;
    let c: Cursor = serde_json::from_slice(&b).map_err(|_| ProfileError::InvalidCursor)?;
    if c.version != 1 || c.query != query {
        return Err(ProfileError::InvalidCursor);
    }
    if c.generation != generation {
        return Err(ProfileError::CursorExpired);
    }
    Ok(Some(c.key))
}
pub(crate) fn raw(row: &Row<'_>) -> rusqlite::Result<RawFields> {
    let mut out = BTreeMap::new();
    for (i, name) in row.as_ref().column_names().iter().enumerate() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => SqlValue::Null,
            ValueRef::Integer(n) => SqlValue::Integer(n.to_string()),
            ValueRef::Real(n) => SqlValue::Real(n.to_string()),
            ValueRef::Blob(b) => SqlValue::Blob(STANDARD.encode(b)),
            ValueRef::Text(b) => match std::str::from_utf8(b) {
                Ok(s) => SqlValue::Text(s.into()),
                Err(_) => SqlValue::TextBytes(STANDARD.encode(b)),
            },
        };
        out.insert((*name).into(), value);
    }
    Ok(out)
}
fn text(r: &RawFields, k: &str) -> Option<String> {
    match r.get(k) {
        Some(SqlValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}
pub(crate) fn num(r: &RawFields, k: &str) -> Option<i64> {
    match r.get(k) {
        Some(SqlValue::Integer(s)) => s.parse().ok(),
        _ => None,
    }
}
fn blob(r: &RawFields, k: &str) -> Result<Option<Vec<u8>>> {
    match r.get(k) {
        Some(SqlValue::Text(s)) => Ok(Some(s.as_bytes().to_vec())),
        Some(SqlValue::Blob(s) | SqlValue::TextBytes(s)) => Ok(Some(
            STANDARD
                .decode(s)
                .map_err(|_| ProfileError::InvalidMetadata)?,
        )),
        _ => Ok(None),
    }
}
fn label(r: &RawFields, username: &str) -> String {
    text(r, "remark")
        .filter(|s| !s.is_empty())
        .or_else(|| text(r, "nick_name").filter(|s| !s.is_empty()))
        .unwrap_or_else(|| username.into())
}
fn contact(r: RawFields) -> Result<Contact> {
    let username = text(&r, "username").ok_or(ProfileError::InvalidMetadata)?;
    let id = num(&r, "id").ok_or(ProfileError::InvalidMetadata)?;
    Ok(Contact {
        label: label(&r, &username),
        is_group: username.ends_with("@chatroom"),
        username,
        local_id: id.to_string(),
        nickname: text(&r, "nick_name"),
        remark: text(&r, "remark"),
        alias: text(&r, "alias"),
        local_type: num(&r, "local_type"),
        raw_fields: r,
    })
}
pub(crate) fn decode(r: &RawFields, column: &str, flag: &str, max: usize) -> Result<DecodedField> {
    let flag = num(r, flag);
    let Some(mut bytes) = blob(r, column)? else {
        return Ok(DecodedField {
            text: None,
            state: DecodeState::Null,
            compression_flag: flag,
        });
    };
    if bytes.len() > max {
        return Err(ProfileError::ResourceLimit);
    }
    let mut state = DecodeState::Utf8;
    if flag.is_some_and(|f| f != 0 && f != 4) {
        return Ok(DecodedField {
            text: None,
            state: DecodeState::UnsupportedCompression,
            compression_flag: flag,
        });
    }
    if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let result = (|| {
            let mut decoder = zstd::stream::read::Decoder::new(bytes.as_slice())
                .ok()?
                .take((max + 1) as u64);
            let mut output = Vec::new();
            decoder.read_to_end(&mut output).ok()?;
            if output.len() > max {
                return None;
            }
            Some(output)
        })();
        let Some(decoded) = result else {
            return Ok(DecodedField {
                text: None,
                state: DecodeState::InvalidCompressedData,
                compression_flag: flag,
            });
        };
        bytes = decoded;
        state = DecodeState::Zstd;
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok(DecodedField {
            text: Some(s),
            state,
            compression_flag: flag,
        }),
        Err(_) => Ok(DecodedField {
            text: None,
            state: DecodeState::InvalidUtf8,
            compression_flag: flag,
        }),
    }
}
fn name_map(c: &Connection) -> Result<BTreeMap<i64, String>> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='Name2Id')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        // The stock client can create an empty next-ordinal database before
        // initializing its message tables. It has no sender namespace yet.
        let messages: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name GLOB 'Msg_*')",
            [],
            |r| r.get(0),
        )?;
        if messages {
            return Err(ProfileError::UnsupportedSchema(
                "message sender namespace".into(),
            ));
        }
        return Ok(BTreeMap::new());
    }
    let mut s = c.prepare("SELECT rowid,user_name FROM Name2Id")?;
    let map = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    if map.len() > 1_000_000 {
        return Err(ProfileError::ResourceLimit);
    }
    Ok(map)
}
fn shard(name: &str) -> bool {
    let b = name.rsplit('/').next().unwrap_or("");
    let stem = b.strip_suffix(".db").unwrap_or("");
    ["message_", "biz_message_"].iter().any(|p| {
        stem.strip_prefix(p)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}
fn validate_name(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 4096 || s.contains('\0') {
        Err(ProfileError::InvalidQuery)
    } else {
        Ok(())
    }
}

impl ProfileReader {
    pub fn open(location: ProfileLocation, keys: KeyFile, options: ReadOptions) -> Result<Self> {
        let snapshot = Snapshot::create(location, keys, options)?;
        let mut schemas = Vec::new();
        for db in &snapshot.inventory {
            if db.state == DatabaseState::Readable {
                schemas.push(
                    inspect(&snapshot.open(&db.relative_path)?, &db.relative_path)
                        .map_err(|_| ProfileError::UnsupportedSchema(db.relative_path.clone()))?,
                );
            }
        }
        Ok(Self { snapshot, schemas })
    }
    pub fn generation(&self) -> &str {
        &self.snapshot.generation
    }
    pub fn schemas(&self) -> &[SchemaReport] {
        &self.schemas
    }
    pub fn coverage(&self) -> Coverage {
        Coverage {
            partial: self
                .snapshot
                .inventory
                .iter()
                .any(|d| d.state != DatabaseState::Readable),
            databases: self.snapshot.inventory.clone(),
        }
    }
    pub fn inventory(&self) -> &[DatabaseInventory] {
        &self.snapshot.inventory
    }
    pub fn require_writable(&self) -> Result<()> {
        Err(ProfileError::WritesUnsupported)
    }
    fn role(&self, basename: &str, capability: &str) -> Result<&str> {
        let candidates: Vec<_> = self
            .snapshot
            .inventory
            .iter()
            .filter(|v| v.relative_path.rsplit('/').next() == Some(basename))
            .collect();
        if candidates.len() != 1 {
            return Err(ProfileError::UnsupportedSchema(basename.into()));
        }
        let db = candidates[0];
        match db.state {
            DatabaseState::KeyUnavailable => {
                return Err(ProfileError::KeyUnavailable(db.relative_path.clone()))
            }
            DatabaseState::KeyMismatch => {
                return Err(ProfileError::KeyMismatch(db.relative_path.clone()))
            }
            _ => {}
        }
        if !self
            .schemas
            .iter()
            .any(|s| s.database == db.relative_path && s.supports(capability))
        {
            return Err(ProfileError::UnsupportedSchema(db.relative_path.clone()));
        }
        Ok(&db.relative_path)
    }
    fn page<T>(
        &self,
        mut items: Vec<T>,
        n: usize,
        q: &str,
        key: impl Fn(&T) -> CursorKey,
    ) -> Result<Page<T>> {
        let more = items.len() > n;
        items.truncate(n);
        let next_cursor = if more {
            items
                .last()
                .map(|v| encode_cursor(self.generation(), q, key(v)))
                .transpose()?
        } else {
            None
        };
        Ok(Page {
            items,
            next_cursor,
            source_generation: self.generation().into(),
            coverage: self.coverage(),
        })
    }
    pub fn contacts(&self, q: &ListQuery) -> Result<Page<Contact>> {
        let n = limit(q.limit)?;
        let hash = query_hash("contacts", &q.search)?;
        let after = match decode_cursor(q.cursor.as_deref(), self.generation(), &hash)? {
            None => String::new(),
            Some(CursorKey::Name { name }) => name,
            _ => return Err(ProfileError::InvalidCursor),
        };
        let c = self.snapshot.open(self.role("contact.db", "contacts")?)?;
        let mut s=c.prepare("SELECT * FROM contact WHERE username>?1 AND (?2 IS NULL OR instr(username,?2)>0 OR instr(coalesce(nick_name,''),?2)>0 OR instr(coalesce(remark,''),?2)>0 OR instr(coalesce(alias,''),?2)>0) ORDER BY username LIMIT ?3")?;
        let records = s
            .query_map(params![after, q.search, n + 1], raw)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let items = records
            .into_iter()
            .map(contact)
            .collect::<Result<Vec<_>>>()?;
        self.page(items, n, &hash, |v| CursorKey::Name {
            name: v.username.clone(),
        })
    }
    pub fn threads(&self, q: &ListQuery) -> Result<Page<Thread>> {
        let n = limit(q.limit)?;
        let hash = query_hash("threads", &q.search)?;
        let after = match decode_cursor(q.cursor.as_deref(), self.generation(), &hash)? {
            None => None,
            Some(CursorKey::Thread { time, name }) => Some((time, name)),
            _ => return Err(ProfileError::InvalidCursor),
        };
        let c = self.snapshot.open(self.role("session.db", "threads")?)?;
        let mut s=c.prepare("SELECT * FROM SessionTable WHERE (?1 IS NULL OR coalesce(sort_timestamp,0)<?1 OR (coalesce(sort_timestamp,0)=?1 AND username>?2)) AND (?3 IS NULL OR instr(username,?3)>0) ORDER BY coalesce(sort_timestamp,0) DESC,username ASC LIMIT ?4")?;
        let records = s
            .query_map(
                params![
                    after.as_ref().map(|a| a.0),
                    after.as_ref().map(|a| &a.1),
                    q.search,
                    n + 1
                ],
                raw,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let labels = self.labels()?;
        let items = records
            .into_iter()
            .map(|r| {
                let username = text(&r, "username").ok_or(ProfileError::InvalidMetadata)?;
                Ok(Thread {
                    label: labels
                        .get(&username)
                        .cloned()
                        .unwrap_or_else(|| username.clone()),
                    username,
                    unread_count: num(&r, "unread_count"),
                    hidden: num(&r, "is_hidden").map(|n| n != 0),
                    sort_timestamp: num(&r, "sort_timestamp"),
                    last_timestamp: num(&r, "last_timestamp"),
                    raw_fields: r,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        self.page(items, n, &hash, |v| CursorKey::Thread {
            time: v.sort_timestamp.unwrap_or(0),
            name: v.username.clone(),
        })
    }
    fn labels(&self) -> Result<BTreeMap<String, String>> {
        // Contact coverage can be unavailable without preventing thread reads.
        let Ok(rel) = self.role("contact.db", "contacts") else {
            return Ok(BTreeMap::new());
        };
        let c = self.snapshot.open(rel)?;
        let mut s = c.prepare("SELECT username,remark,nick_name FROM contact LIMIT 1000001")?;
        let mut out = BTreeMap::new();
        let mut rows = s.query([])?;
        while let Some(r) = rows.next()? {
            let name: String = r.get(0)?;
            let remark: Option<String> = r.get(1)?;
            let nick: Option<String> = r.get(2)?;
            out.insert(
                name.clone(),
                remark
                    .filter(|s| !s.is_empty())
                    .or_else(|| nick.filter(|s| !s.is_empty()))
                    .unwrap_or(name),
            );
            if out.len() > 1_000_000 {
                return Err(ProfileError::ResourceLimit);
            }
        }
        Ok(out)
    }
    pub fn messages(&self, q: &MessageQuery) -> Result<Page<Message>> {
        let n = limit(q.limit)?;
        if let Some(chat) = &q.chat {
            validate_name(chat)?;
        }
        if let Some(own) = &q.own_username {
            validate_name(own)?;
        }
        let hash = query_hash("messages", &(&q.chat, &q.own_username, q.message_type))?;
        let after = match decode_cursor(q.cursor.as_deref(), self.generation(), &hash)? {
            None => None,
            Some(CursorKey::Message {
                time,
                database,
                table,
                local,
            }) if message_table(&table) => Some((time, database, table, local)),
            _ => return Err(ProfileError::InvalidCursor),
        };
        let target = q
            .chat
            .as_ref()
            .map(|s| format!("Msg_{:x}", Md5::digest(s.as_bytes())));
        let mut all_names: BTreeSet<String> = self.labels()?.into_keys().collect();
        for db in self
            .snapshot
            .inventory
            .iter()
            .filter(|v| shard(&v.relative_path) && v.state == DatabaseState::Readable)
        {
            let c = self.snapshot.open(&db.relative_path)?;
            all_names.extend(name_map(&c)?.into_values());
        }
        if let Some(chat) = &q.chat {
            all_names.insert(chat.clone());
        }
        let mut hashes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in all_names {
            hashes
                .entry(format!("{:x}", Md5::digest(name.as_bytes())))
                .or_default()
                .push(name);
        }
        let mut items = Vec::new();
        let mut found = false;
        for db in self
            .snapshot
            .inventory
            .iter()
            .filter(|v| shard(&v.relative_path))
        {
            if db.state != DatabaseState::Readable {
                continue;
            }
            let schema = self
                .schemas
                .iter()
                .find(|s| s.database == db.relative_path)
                .ok_or(ProfileError::InvalidMetadata)?;
            let c = self.snapshot.open(&db.relative_path)?;
            let names = name_map(&c)?;
            for table in schema.tables.iter().filter(|t| {
                message_table(&t.name)
                    && target
                        .as_ref()
                        .is_none_or(|wanted| wanted.eq_ignore_ascii_case(&t.name))
            }) {
                let table_name = &table.name;
                let required = [
                    "local_id",
                    "server_id",
                    "local_type",
                    "create_time",
                    "real_sender_id",
                    "message_content",
                    "source",
                    "WCDB_CT_message_content",
                    "WCDB_CT_source",
                ];
                if !required
                    .iter()
                    .all(|n| table.columns.iter().any(|c| c.name == *n))
                {
                    return Err(ProfileError::UnsupportedSchema(db.relative_path.clone()));
                }
                found = true;
                let (max_time, equal_mode, local_bound) =
                    if let Some((time, ref database, ref last_table, local)) = after {
                        let cmp = (&db.relative_path, table_name).cmp(&(database, last_table));
                        (
                            Some(time),
                            if cmp.is_lt() {
                                1
                            } else if cmp.is_gt() {
                                -1
                            } else {
                                0
                            },
                            Some(local),
                        )
                    } else {
                        (None, 0, None)
                    };
                let sql=format!("SELECT * FROM {} WHERE (?1 IS NULL OR create_time<?1 OR (create_time=?1 AND (?2=1 OR (?2=0 AND local_id<?3)))) AND (?4 IS NULL OR (local_type & 2147483647)=?4) ORDER BY create_time DESC,local_id DESC LIMIT ?5",quote_identifier(table_name));
                let mut s = c.prepare(&sql)?;
                let records = s
                    .query_map(
                        params![max_time, equal_mode, local_bound, q.message_type, n + 1],
                        raw,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let peer = hashes
                    .get(&table_name[4..].to_ascii_lowercase())
                    .filter(|v| v.len() == 1)
                    .map(|v| v[0].clone());
                for r in records {
                    let local = num(&r, "local_id").ok_or(ProfileError::InvalidMetadata)?;
                    let signed = num(&r, "server_id");
                    let sender = num(&r, "real_sender_id").and_then(|id| names.get(&id).cloned());
                    let outgoing = q
                        .own_username
                        .as_ref()
                        .and_then(|own| sender.as_ref().map(|s| s == own));
                    let local_type = num(&r, "local_type").ok_or(ProfileError::InvalidMetadata)?;
                    items.push(Message {
                        id: SourceMessageId {
                            database: db.relative_path.clone(),
                            table: table_name.clone(),
                            local_id: local.to_string(),
                        },
                        server_message_id: signed
                            .filter(|v| *v != 0)
                            .map(|v| (v as u64).to_string()),
                        server_id_signed: signed.map(|n| n.to_string()),
                        peer: peer.clone(),
                        sender,
                        outgoing,
                        message_type: local_type & 0x7fff_ffff,
                        local_type: local_type.to_string(),
                        create_time: num(&r, "create_time").ok_or(ProfileError::InvalidMetadata)?,
                        sort_seq: num(&r, "sort_seq").map(|n| n.to_string()),
                        content: decode(
                            &r,
                            "message_content",
                            "WCDB_CT_message_content",
                            self.snapshot.options.max_blob_bytes,
                        )?,
                        source: decode(
                            &r,
                            "source",
                            "WCDB_CT_source",
                            self.snapshot.options.max_blob_bytes,
                        )?,
                        raw_fields: r,
                    });
                }
                items.sort_by(|a, b| message_order(b).cmp(&message_order(a)));
                items.truncate(n + 1);
            }
        }
        if !found
            && self
                .snapshot
                .inventory
                .iter()
                .any(|d| shard(&d.relative_path) && d.state != DatabaseState::Readable)
        {
            return Err(ProfileError::KeyUnavailable("message shards".into()));
        }
        self.page(items, n, &hash, |v| CursorKey::Message {
            time: v.create_time,
            database: v.id.database.clone(),
            table: v.id.table.clone(),
            local: v.id.local_id.parse().unwrap_or(0),
        })
    }

    /// Read one source-qualified observation without paging through history.
    /// Resolve sender numbers only through this database's own Name2Id map.
    pub fn message(&self, id: &SourceMessageId, own_username: Option<&str>) -> Result<Message> {
        if !message_table(&id.table) || !shard(&id.database) {
            return Err(ProfileError::InvalidQuery);
        }
        let c = self.snapshot.open(&id.database)?;
        let names = name_map(&c)?;
        let labels = self.labels()?;
        self.message_from_connection(&c, id, own_username, &labels, &names)
    }

    /// Resolve at most one page without decoding unrelated history. Missing rows
    /// are omitted; unreadable databases and incompatible records remain errors.
    /// Source-qualified IDs preserve each shard's own sender namespace.
    pub fn messages_by_ids(
        &self,
        ids: &[SourceMessageId],
        own_username: Option<&str>,
    ) -> Result<Vec<Message>> {
        if ids.len() > 200 {
            return Err(ProfileError::InvalidQuery);
        }
        if let Some(own) = own_username {
            validate_name(own)?;
        }
        let mut groups: BTreeMap<&str, Vec<&SourceMessageId>> = BTreeMap::new();
        for id in ids {
            if !message_table(&id.table) || !shard(&id.database) {
                return Err(ProfileError::InvalidQuery);
            }
            groups.entry(&id.database).or_default().push(id);
        }
        let labels = self.labels()?;
        let mut result = Vec::with_capacity(ids.len());
        for (database, group) in groups {
            let c = self.snapshot.open(database)?;
            let names = name_map(&c)?;
            for id in group {
                match self.message_from_connection(&c, id, own_username, &labels, &names) {
                    Ok(message) => result.push(message),
                    Err(ProfileError::NotCached) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(result)
    }

    fn message_from_connection(
        &self,
        c: &Connection,
        id: &SourceMessageId,
        own_username: Option<&str>,
        labels: &BTreeMap<String, String>,
        names: &BTreeMap<i64, String>,
    ) -> Result<Message> {
        if !message_table(&id.table) || !shard(&id.database) {
            return Err(ProfileError::InvalidQuery);
        }
        if let Some(own) = own_username {
            validate_name(own)?;
        }
        let local = id
            .local_id
            .parse::<i64>()
            .map_err(|_| ProfileError::InvalidQuery)?;
        if local <= 0 || local.to_string() != id.local_id {
            return Err(ProfileError::InvalidQuery);
        }
        let table = self
            .schemas
            .iter()
            .find(|s| s.database == id.database)
            .and_then(|s| s.tables.iter().find(|t| t.name == id.table))
            .ok_or(ProfileError::InvalidQuery)?;
        if ![
            "local_id",
            "server_id",
            "local_type",
            "create_time",
            "real_sender_id",
            "message_content",
            "source",
            "WCDB_CT_message_content",
            "WCDB_CT_source",
        ]
        .iter()
        .all(|n| table.columns.iter().any(|c| c.name == *n))
        {
            return Err(ProfileError::UnsupportedSchema(id.database.clone()));
        }
        let r = c
            .query_row(
                &format!(
                    "SELECT * FROM {} WHERE local_id=?1",
                    quote_identifier(&id.table)
                ),
                [local],
                raw,
            )
            .optional()?
            .ok_or(ProfileError::NotCached)?;
        let mut candidates = labels
            .keys()
            .cloned()
            .chain(names.values().cloned())
            .filter(|name| {
                format!("{:x}", Md5::digest(name.as_bytes())).eq_ignore_ascii_case(&id.table[4..])
            })
            .collect::<BTreeSet<_>>();
        let peer = if candidates.len() == 1 {
            candidates.pop_first()
        } else {
            None
        };
        let signed = num(&r, "server_id");
        let sender = num(&r, "real_sender_id").and_then(|n| names.get(&n).cloned());
        let outgoing = own_username.and_then(|own| sender.as_ref().map(|s| s == own));
        let local_type = num(&r, "local_type").ok_or(ProfileError::InvalidMetadata)?;
        Ok(Message {
            id: id.clone(),
            server_message_id: signed.filter(|v| *v != 0).map(|v| (v as u64).to_string()),
            server_id_signed: signed.map(|n| n.to_string()),
            peer,
            sender,
            outgoing,
            message_type: local_type & 0x7fff_ffff,
            local_type: local_type.to_string(),
            create_time: num(&r, "create_time").ok_or(ProfileError::InvalidMetadata)?,
            sort_seq: num(&r, "sort_seq").map(|n| n.to_string()),
            content: decode(
                &r,
                "message_content",
                "WCDB_CT_message_content",
                self.snapshot.options.max_blob_bytes,
            )?,
            source: decode(
                &r,
                "source",
                "WCDB_CT_source",
                self.snapshot.options.max_blob_bytes,
            )?,
            raw_fields: r,
        })
    }
    pub fn groups(&self, q: &ListQuery) -> Result<Page<Group>> {
        let n = limit(q.limit)?;
        let hash = query_hash("groups", &q.search)?;
        let after = match decode_cursor(q.cursor.as_deref(), self.generation(), &hash)? {
            None => String::new(),
            Some(CursorKey::Name { name }) => name,
            _ => return Err(ProfileError::InvalidCursor),
        };
        let c = self.snapshot.open(self.role("contact.db", "groups")?)?;
        let mut s=c.prepare("SELECT * FROM chat_room WHERE username>?1 AND (?2 IS NULL OR instr(username,?2)>0) ORDER BY username LIMIT ?3")?;
        let records = s
            .query_map(params![after, q.search, n + 1], raw)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let labels = self.labels()?;
        let mut items = Vec::new();
        for r in records {
            let username = text(&r, "username").ok_or(ProfileError::InvalidMetadata)?;
            let id = num(&r, "id").ok_or(ProfileError::InvalidMetadata)?;
            let announcement = c
                .query_row(
                    "SELECT username_,announcement_ FROM chat_room_info_detail WHERE room_id_=?1",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()?;
            if announcement.as_ref().is_some_and(|a| a.0 != username) {
                return Err(ProfileError::InvalidMetadata);
            }
            items.push(Group {
                label: labels
                    .get(&username)
                    .cloned()
                    .unwrap_or_else(|| username.clone()),
                username,
                local_id: id.to_string(),
                owner: text(&r, "owner"),
                announcement: announcement.and_then(|v| v.1),
                raw_fields: r,
            });
        }
        self.page(items, n, &hash, |v| CursorKey::Name {
            name: v.username.clone(),
        })
    }
    pub fn group_members(&self, room: &str, q: &ListQuery) -> Result<Page<GroupMember>> {
        validate_name(room)?;
        let n = limit(q.limit)?;
        let hash = query_hash("members", &(room, &q.search))?;
        let after = match decode_cursor(q.cursor.as_deref(), self.generation(), &hash)? {
            None => String::new(),
            Some(CursorKey::Name { name }) => name,
            _ => return Err(ProfileError::InvalidCursor),
        };
        let c = self.snapshot.open(self.role("contact.db", "groups")?)?;
        let room_row = c
            .query_row(
                "SELECT id,ext_buffer FROM chat_room WHERE username=?1",
                [room],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<Vec<u8>>>(1)?)),
            )
            .optional()?
            .ok_or(ProfileError::NotCached)?;
        let displays = room_displays(room_row.1.as_deref().unwrap_or(&[]))?;
        let mut sql_members = BTreeSet::new();
        let mut stmt=c.prepare("SELECT contact.username FROM chatroom_member JOIN contact ON contact.id=chatroom_member.member_id WHERE chatroom_member.room_id=?1")?;
        for row in stmt.query_map([room_row.0], |r| r.get::<_, String>(0))? {
            sql_members.insert(row?);
            if sql_members.len() > 100000 {
                return Err(ProfileError::ResourceLimit);
            }
        }
        if !displays.is_empty() && displays.keys().cloned().collect::<BTreeSet<_>>() != sql_members
        {
            return Err(ProfileError::InvalidMetadata);
        }
        let mut stmt=c.prepare("SELECT c.* FROM contact c JOIN chatroom_member m ON m.member_id=c.id WHERE m.room_id=?1 AND c.username>?2 AND (?3 IS NULL OR instr(c.username,?3)>0 OR instr(coalesce(c.nick_name,''),?3)>0 OR instr(coalesce(c.remark,''),?3)>0) ORDER BY c.username LIMIT ?4")?;
        let records = stmt
            .query_map(params![room_row.0, after, q.search, n + 1], raw)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut items = Vec::new();
        for r in records {
            let contact = contact(r)?;
            let display = displays.get(&contact.username).cloned().flatten();
            let label = contact
                .remark
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| display.clone().filter(|s| !s.is_empty()))
                .or_else(|| contact.nickname.clone().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| contact.username.clone());
            items.push(GroupMember {
                username: contact.username.clone(),
                local_id: contact.local_id.clone(),
                label,
                room_display_name: display,
                contact,
            });
        }
        self.page(items, n, &hash, |v| CursorKey::Name {
            name: v.username.clone(),
        })
    }
    pub fn avatar(&self, username: &str) -> Result<CachedBytes> {
        validate_name(username)?;
        let rel = self.role("head_image.db", "avatars")?;
        let c = self.snapshot.open(rel)?;
        let bytes = c
            .query_row(
                "SELECT image_buffer FROM head_image WHERE username=?1",
                [username],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or(ProfileError::NotCached)?;
        let (media_type, _, _) = super::images::validate_image(&bytes)?;
        Ok(CachedBytes {
            media_type,
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            source_database: rel.into(),
            source_table: "head_image".into(),
        })
    }
    /// Read one exact cached voice record. The supplied media-shard namespace is
    /// explicit; its Name2Id is not interchangeable with the message shard.
    pub fn cached_voice(
        &self,
        database: &str,
        chat: &str,
        local_id: &str,
        server_id: Option<&str>,
    ) -> Result<CachedBytes> {
        validate_name(chat)?;
        let local: i64 = local_id.parse().map_err(|_| ProfileError::InvalidQuery)?;
        let sid = server_id
            .map(|s| s.parse::<u64>().map(|n| n as i64))
            .transpose()
            .map_err(|_| ProfileError::InvalidQuery)?;
        let c = self.snapshot.open(database)?;
        if !self
            .schemas
            .iter()
            .any(|s| s.database == database && s.supports("voice"))
        {
            return Err(ProfileError::UnsupportedSchema(database.into()));
        }
        let mut s=c.prepare("SELECT voice_data FROM VoiceInfo JOIN Name2Id n ON n.rowid=VoiceInfo.chat_name_id WHERE n.user_name=?1 AND VoiceInfo.local_id=?2 AND (?3 IS NULL OR VoiceInfo.svr_id=?3) AND length(voice_data)>0 LIMIT 2")?;
        let mut found = s
            .query_map(params![chat, local, sid], |r| r.get::<_, Vec<u8>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if found.len() != 1 {
            return Err(if found.is_empty() {
                ProfileError::NotCached
            } else {
                ProfileError::InvalidQuery
            });
        }
        let bytes = found.remove(0);
        Ok(CachedBytes {
            media_type: "application/octet-stream".into(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            source_database: database.into(),
            source_table: "VoiceInfo".into(),
        })
    }
    pub fn decode_dat(&self, bytes: &[u8]) -> Result<(Vec<u8>, crate::DatLayout)> {
        crate::decode_dat_v2(
            bytes,
            self.snapshot
                .keys
                .image_dat
                .as_ref()
                .ok_or(ProfileError::ImageKeyUnavailable)?,
        )
    }
}
fn message_order(m: &Message) -> (i64, &str, &str, i64) {
    (
        m.create_time,
        &m.id.database,
        &m.id.table,
        m.id.local_id.parse().unwrap_or(0),
    )
}

pub(crate) fn protobuf_fields(b: &[u8]) -> Result<Vec<(u32, u8, &[u8])>> {
    fn var(b: &[u8], i: &mut usize) -> Result<u64> {
        let mut n = 0u64;
        for k in 0..10 {
            let x = *b.get(*i).ok_or(ProfileError::InvalidMetadata)?;
            *i += 1;
            if k == 9 && x > 1 {
                return Err(ProfileError::InvalidMetadata);
            }
            n |= u64::from(x & 127) << (7 * k);
            if x < 128 {
                return Ok(n);
            }
        }
        Err(ProfileError::InvalidMetadata)
    }
    if b.len() > 16 * 1024 * 1024 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let tag = var(b, &mut i)?;
        let n = (tag >> 3) as u32;
        let w = (tag & 7) as u8;
        if tag >> 3 == 0 || tag >> 3 >= 1 << 29 {
            return Err(ProfileError::InvalidMetadata);
        }
        let (start, end) = match w {
            0 => {
                let start = i;
                var(b, &mut i)?;
                (start, i)
            }
            1 => (i, i.checked_add(8).ok_or(ProfileError::InvalidMetadata)?),
            5 => (i, i.checked_add(4).ok_or(ProfileError::InvalidMetadata)?),
            2 => {
                let z =
                    usize::try_from(var(b, &mut i)?).map_err(|_| ProfileError::InvalidMetadata)?;
                (i, i.checked_add(z).ok_or(ProfileError::InvalidMetadata)?)
            }
            _ => return Err(ProfileError::InvalidMetadata),
        };
        let value = b.get(start..end).ok_or(ProfileError::InvalidMetadata)?;
        i = end;
        out.push((n, w, value));
        if out.len() > 100000 {
            return Err(ProfileError::ResourceLimit);
        }
    }
    Ok(out)
}
fn room_displays(b: &[u8]) -> Result<BTreeMap<String, Option<String>>> {
    let mut out = BTreeMap::new();
    for (n, w, v) in protobuf_fields(b)? {
        if n != 1 {
            continue;
        }
        if w != 2 {
            return Err(ProfileError::InvalidMetadata);
        }
        let fields = protobuf_fields(v)?;
        let scalar = |number| -> Result<Option<String>> {
            let mut values = fields.iter().filter(|(n, _, _)| *n == number);
            let one = values.next();
            if values.next().is_some() {
                return Err(ProfileError::InvalidMetadata);
            }
            one.map(|(_, w, v)| {
                if *w == 2 {
                    String::from_utf8(v.to_vec()).map_err(|_| ProfileError::InvalidMetadata)
                } else {
                    Err(ProfileError::InvalidMetadata)
                }
            })
            .transpose()
        };
        let username = scalar(1)?.ok_or(ProfileError::InvalidMetadata)?;
        let display = scalar(2)?;
        if out.insert(username, display).is_some() {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    Ok(out)
}
