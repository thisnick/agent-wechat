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
#[path = "thread_previews.rs"]
mod thread_previews;

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
fn group_member(contact: Contact, display: Option<String>) -> GroupMember {
    let label = contact
        .remark
        .clone()
        .filter(|s| !s.is_empty())
        .or_else(|| display.clone().filter(|s| !s.is_empty()))
        .or_else(|| contact.nickname.clone().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| contact.username.clone());
    GroupMember {
        username: contact.username.clone(),
        local_id: contact.local_id.clone(),
        label,
        room_display_name: display,
        contact,
    }
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
    pub fn labels(&self) -> Result<BTreeMap<String, String>> {
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
    /// Capture complete history membership in one pass per shard. This reads
    /// only IDs/timestamps; callers resolve delivered content with
    /// `messages_by_ids` against this same snapshot. No state is cached across
    /// snapshots, and incompatible message tables remain errors.
    pub fn message_references(
        &self,
        chat: Option<&str>,
        max_items: usize,
    ) -> Result<Vec<MessageReference>> {
        if max_items == 0 || max_items > 100_000 {
            return Err(ProfileError::InvalidQuery);
        }
        if let Some(chat) = chat {
            validate_name(chat)?;
        }
        let target = chat.map(|s| format!("Msg_{:x}", Md5::digest(s.as_bytes())));
        let mut names: BTreeSet<String> = self.labels()?.into_keys().collect();
        if let Some(chat) = chat {
            names.insert(chat.to_owned());
        }
        let mut records = Vec::new();
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
            names.extend(name_map(&c)?.into_values());
            for table in schema.tables.iter().filter(|t| {
                message_table(&t.name)
                    && target
                        .as_ref()
                        .is_none_or(|wanted| wanted.eq_ignore_ascii_case(&t.name))
            }) {
                require_message_columns(table, &db.relative_path)?;
                found = true;
                let sql = format!(
                    "SELECT local_id,create_time FROM {} LIMIT ?1",
                    quote_identifier(&table.name)
                );
                let mut statement = c.prepare(&sql)?;
                let mut rows = statement.query([max_items + 1 - records.len()])?;
                while let Some(row) = rows.next()? {
                    let local: i64 = row.get(0)?;
                    let time: i64 = row.get(1)?;
                    records.push((time, db.relative_path.clone(), table.name.clone(), local));
                    if records.len() > max_items {
                        return Err(ProfileError::ResourceLimit);
                    }
                }
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
        let mut hashes: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for name in names {
            hashes
                .entry(format!("{:x}", Md5::digest(name.as_bytes())))
                .or_default()
                .push(name);
        }
        records.sort_unstable_by(|a, b| b.cmp(a));
        Ok(records
            .into_iter()
            .map(|(_, database, table, local)| {
                let peer = hashes
                    .get(&table[4..].to_ascii_lowercase())
                    .filter(|v| v.len() == 1)
                    .map(|v| v[0].clone());
                MessageReference {
                    id: SourceMessageId {
                        database,
                        table,
                        local_id: local.to_string(),
                    },
                    peer,
                }
            })
            .collect())
    }

    /// Read the latest saved message for each requested conversation in one
    /// snapshot. One compound query per shard selects one indexed row per
    /// conversation; no point lookups or older-body decoding are needed.
    pub fn latest_messages(
        &self,
        chats: &[String],
        own_username: Option<&str>,
    ) -> Result<BTreeMap<String, Message>> {
        if chats.len() > 200 {
            return Err(ProfileError::InvalidQuery);
        }
        for chat in chats {
            validate_name(chat)?;
        }
        if let Some(own) = own_username {
            validate_name(own)?;
        }
        if chats.is_empty() {
            return Ok(BTreeMap::new());
        }
        let shards: Vec<_> = self
            .inventory()
            .iter()
            .filter(|db| shard(&db.relative_path))
            .collect();
        if shards.is_empty() || shards.iter().any(|db| db.state != DatabaseState::Readable) {
            return Err(ProfileError::KeyUnavailable("message shards".into()));
        }
        // The selected conversation already supplies the peer identity, so do
        // not rescan the contact namespace for every preview's table hash.
        let labels = BTreeMap::new();
        let mut result: BTreeMap<String, Message> = BTreeMap::new();
        for db in shards {
            let schema = self
                .schemas
                .iter()
                .find(|s| s.database == db.relative_path)
                .ok_or(ProfileError::InvalidMetadata)?;
            let c = self.snapshot.open(&db.relative_path)?;
            let names = name_map(&c)?;
            let mut selected = Vec::new();
            let mut columns = BTreeSet::new();
            for chat in chats {
                let table = format!("Msg_{:x}", Md5::digest(chat.as_bytes()));
                let Some(table) = schema
                    .tables
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(&table))
                else {
                    continue;
                };
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
                .all(|name| table.columns.iter().any(|c| c.name == *name))
                {
                    return Err(ProfileError::UnsupportedSchema(db.relative_path.clone()));
                }
                columns.extend(table.columns.iter().map(|column| column.name.clone()));
                selected.push((chat, table));
            }
            if selected.is_empty() {
                continue;
            }
            // Preserve unknown columns even when conversation tables differ.
            // A private discriminator must not collide with a profile column.
            let mut discriminator = "__agent_wechat_preview".to_string();
            while columns.contains(&discriminator) {
                discriminator.push('_');
            }
            let sql = selected.iter().enumerate().map(|(index, (_, table))| {
                let projection = columns.iter().map(|column| {
                    if table.columns.iter().any(|c| c.name == *column) {
                        quote_identifier(column)
                    } else {
                        format!("NULL AS {}", quote_identifier(column))
                    }
                }).collect::<Vec<_>>().join(",");
                format!("SELECT {index} AS {},{projection} FROM (SELECT * FROM {} ORDER BY create_time DESC,local_id DESC LIMIT 1)", quote_identifier(&discriminator), quote_identifier(&table.name))
            }).collect::<Vec<_>>().join(" UNION ALL ");
            #[cfg(test)]
            batch_lookup_tests::record_batch_query();
            let mut statement = c.prepare(&sql)?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let index: usize = row.get(0)?;
                let (chat, table) = selected.get(index).ok_or(ProfileError::InvalidMetadata)?;
                let mut fields = raw(row)?;
                fields.retain(|name, _| table.columns.iter().any(|column| column.name == *name));
                let local = num(&fields, "local_id")
                    .filter(|id| *id > 0)
                    .ok_or(ProfileError::InvalidMetadata)?;
                let message = self.message_from_fields_with_peer(
                    fields,
                    &SourceMessageId {
                        database: db.relative_path.clone(),
                        table: table.name.clone(),
                        local_id: local.to_string(),
                    },
                    own_username,
                    &labels,
                    &names,
                    Some(chat),
                )?;
                if result
                    .get(*chat)
                    .is_none_or(|old| message_order(&message) > message_order(old))
                {
                    result.insert((*chat).clone(), message);
                }
            }
        }
        Ok(result)
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
                require_message_columns(table, &db.relative_path)?;
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
        if ids.is_empty() {
            return Ok(Vec::new());
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

    /// Resolve an unsigned server ID in one conversation only. Return up to two
    /// observations so callers can reject ambiguous IDs across shards.
    pub fn messages_by_server_id(
        &self,
        chat: &str,
        server_id: &str,
        own_username: Option<&str>,
    ) -> Result<Vec<Message>> {
        validate_name(chat)?;
        let server = server_id
            .parse::<u64>()
            .map_err(|_| ProfileError::InvalidQuery)?;
        if server == 0 || server.to_string() != server_id {
            return Err(ProfileError::InvalidQuery);
        }
        let table_name = format!("Msg_{:x}", Md5::digest(chat.as_bytes()));
        let labels = self.labels()?;
        let mut result = Vec::new();
        let shards: Vec<_> = self.schemas.iter().filter(|s| shard(&s.database)).collect();
        if shards.len() > 128 {
            return Err(ProfileError::InvalidQuery);
        }
        if self
            .coverage()
            .databases
            .iter()
            .any(|d| shard(&d.relative_path) && d.state != DatabaseState::Readable)
        {
            return Err(ProfileError::KeyUnavailable("message shards".into()));
        }
        for schema in shards {
            let Some(table) = schema.tables.iter().find(|t| t.name == table_name) else {
                continue;
            };
            if !["local_id", "server_id"]
                .iter()
                .all(|name| table.columns.iter().any(|c| c.name == *name))
            {
                return Err(ProfileError::UnsupportedSchema(schema.database.clone()));
            }
            let c = self.snapshot.open(&schema.database)?;
            let names = name_map(&c)?;
            let mut statement = c.prepare(&format!(
                "SELECT local_id FROM {} WHERE server_id=?1 LIMIT 2",
                quote_identifier(&table_name)
            ))?;
            let ids = statement
                .query_map([server as i64], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for local in ids {
                let mut message = self.message_from_connection(
                    &c,
                    &SourceMessageId {
                        database: schema.database.clone(),
                        table: table_name.clone(),
                        local_id: local.to_string(),
                    },
                    own_username,
                    &labels,
                    &names,
                )?;
                // The exact table hash proves the peer even when Name2Id no
                // longer contains this conversation's label.
                message.peer = Some(chat.into());
                result.push(message);
                if result.len() == 2 {
                    return Ok(result);
                }
            }
        }
        Ok(result)
    }

    /// Read quoted originals for one page from this captured snapshot. Group by
    /// conversation/shard, decode only matching rows, and retain at most two
    /// observations per identity just like the single-reference lookup. The
    /// visitor consumes each row before the next, avoiding a page of large bodies.
    pub fn visit_messages_by_server_ids<E>(
        &self,
        targets: &[(String, String)],
        own_username: Option<&str>,
        mut visit: impl FnMut(Message) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>
    where
        E: From<ProfileError> + From<rusqlite::Error>,
    {
        if targets.len() > 200 {
            return Err(ProfileError::InvalidQuery.into());
        }
        if targets.is_empty() {
            return Ok(());
        }
        if let Some(own) = own_username {
            validate_name(own)?;
        }
        let mut groups: BTreeMap<&str, BTreeSet<i64>> = BTreeMap::new();
        for (chat, server) in targets {
            validate_name(chat)?;
            let number = server
                .parse::<u64>()
                .map_err(|_| ProfileError::InvalidQuery)?;
            if number == 0 || number.to_string() != *server {
                return Err(ProfileError::InvalidQuery.into());
            }
            groups.entry(chat).or_default().insert(number as i64);
        }
        let shards: Vec<_> = self.schemas.iter().filter(|s| shard(&s.database)).collect();
        if shards.len() > 128 {
            return Err(ProfileError::InvalidQuery.into());
        }
        if self
            .coverage()
            .databases
            .iter()
            .any(|d| shard(&d.relative_path) && d.state != DatabaseState::Readable)
        {
            return Err(ProfileError::KeyUnavailable("message shards".into()).into());
        }
        let labels = self.labels()?;
        let mut counts: BTreeMap<(&str, i64), usize> = BTreeMap::new();
        for schema in shards {
            let tables: Vec<_> = groups
                .iter()
                .filter_map(|(&chat, servers)| {
                    let table_name = format!("Msg_{:x}", Md5::digest(chat.as_bytes()));
                    let table = schema.tables.iter().find(|t| t.name == table_name)?;
                    let pending: Vec<_> = servers
                        .iter()
                        .copied()
                        .filter(|server| counts.get(&(chat, *server)).copied().unwrap_or(0) < 2)
                        .collect();
                    (!pending.is_empty()).then_some((chat, table, pending))
                })
                .collect();
            if tables.is_empty() {
                continue;
            }
            let c = self.snapshot.open(&schema.database)?;
            let names = name_map(&c)?;
            for (chat, table, servers) in tables {
                require_message_columns(table, &schema.database)?;
                let table_name = quote_identifier(&table.name);
                let placeholders = vec!["?"; servers.len()].join(",");
                // The window bounds duplicate observations without fetching or
                // decoding unrelated bodies, even if a profile has duplicate IDs.
                let sql = format!("SELECT * FROM {table_name} WHERE local_id IN (SELECT local_id FROM (SELECT local_id,ROW_NUMBER() OVER (PARTITION BY server_id ORDER BY local_id) AS quote_rank FROM {table_name} WHERE server_id IN ({placeholders})) WHERE quote_rank<=2) ORDER BY server_id,local_id");
                let mut statement = c.prepare(&sql)?;
                #[cfg(test)]
                batch_lookup_tests::record_batch_query();
                let mut rows = statement.query(rusqlite::params_from_iter(servers))?;
                while let Some(row) = rows.next()? {
                    let server: i64 = row.get("server_id")?;
                    let count = counts.entry((chat, server)).or_default();
                    if *count == 2 {
                        continue;
                    }
                    let local: i64 = row.get("local_id")?;
                    if local <= 0 {
                        return Err(ProfileError::InvalidQuery.into());
                    }
                    let id = SourceMessageId {
                        database: schema.database.clone(),
                        table: table.name.clone(),
                        local_id: local.to_string(),
                    };
                    let mut message =
                        self.message_from_fields(raw(row)?, &id, own_username, &labels, &names)?;
                    message.peer = Some(chat.into());
                    visit(message)?;
                    *count += 1;
                }
            }
        }
        Ok(())
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
        #[cfg(test)]
        batch_lookup_tests::record_point_query();
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
        self.message_from_fields(r, id, own_username, labels, names)
    }

    fn message_from_fields(
        &self,
        r: RawFields,
        id: &SourceMessageId,
        own_username: Option<&str>,
        labels: &BTreeMap<String, String>,
        names: &BTreeMap<i64, String>,
    ) -> Result<Message> {
        self.message_from_fields_with_peer(r, id, own_username, labels, names, None)
    }

    fn message_from_fields_with_peer(
        &self,
        r: RawFields,
        id: &SourceMessageId,
        own_username: Option<&str>,
        labels: &BTreeMap<String, String>,
        names: &BTreeMap<i64, String>,
        known_peer: Option<&str>,
    ) -> Result<Message> {
        let peer = if let Some(peer) = known_peer {
            Some(peer.to_owned())
        } else {
            let mut candidates = labels
                .keys()
                .cloned()
                .chain(names.values().cloned())
                .filter(|name| {
                    format!("{:x}", Md5::digest(name.as_bytes()))
                        .eq_ignore_ascii_case(&id.table[4..])
                })
                .collect::<BTreeSet<_>>();
            if candidates.len() == 1 {
                candidates.pop_first()
            } else {
                None
            }
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
            items.push(group_member(contact, display));
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
fn require_message_columns(table: &crate::TableSchema, database: &str) -> Result<()> {
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
    if required
        .iter()
        .all(|name| table.columns.iter().any(|c| c.name == *name))
    {
        Ok(())
    } else {
        Err(ProfileError::UnsupportedSchema(database.to_owned()))
    }
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

#[cfg(test)]
mod batch_lookup_tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static BATCH_QUERIES: Cell<usize> = const { Cell::new(0) };
        static POINT_QUERIES: Cell<usize> = const { Cell::new(0) };
    }
    pub(super) fn record_batch_query() {
        BATCH_QUERIES.with(|n| n.set(n.get() + 1));
    }
    pub(super) fn record_point_query() {
        POINT_QUERIES.with(|n| n.set(n.get() + 1));
    }

    fn fixture() -> (tempfile::TempDir, ProfileReader) {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("account/db_storage/message");
        std::fs::create_dir_all(&directory).unwrap();
        let table = format!("Msg_{:x}", Md5::digest(b"peer"));
        for shard in 0..2 {
            let db = Connection::open(directory.join(format!("message_{shard}.db"))).unwrap();
            db.execute_batch("CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY,is_session INTEGER)")
                .unwrap();
            for user in if shard == 0 {
                ["self", "peer"]
            } else {
                ["peer", "self"]
            } {
                db.execute("INSERT INTO Name2Id VALUES(?1,1)", [user])
                    .unwrap();
            }
            db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER)")).unwrap();
            for local in 1..=25 {
                db.execute(
                    &format!("INSERT INTO {table} VALUES(?1,?2,1,100,?3,?4,'',0,0)"),
                    params![
                        local,
                        shard * 25 + local,
                        if shard == 0 { 2 } else { 1 },
                        format!("Shard {shard} row {local}")
                    ],
                )
                .unwrap();
            }
            db.execute(
                &format!("INSERT INTO {table} VALUES(1000,-1,1,100,1,'Unsigned ID','',0,0)"),
                [],
            )
            .unwrap();
            for local in 2000..2003 {
                db.execute(
                    &format!("INSERT INTO {table} VALUES(?1,99,1,100,1,'Duplicate','',0,0)"),
                    [local],
                )
                .unwrap();
            }
        }
        let reader = ProfileReader::open(
            ProfileLocation::new(temp.path().join("account")).unwrap(),
            KeyFile::empty("account"),
            ReadOptions {
                allow_plaintext: true,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        (temp, reader)
    }

    fn collect(
        reader: &ProfileReader,
        targets: &[(String, String)],
        own: Option<&str>,
    ) -> Result<Vec<Message>> {
        let mut messages = Vec::new();
        reader.visit_messages_by_server_ids::<ProfileError>(targets, own, |message| {
            messages.push(message);
            Ok(())
        })?;
        Ok(messages)
    }

    #[test]
    fn sidebar_previews_use_one_compound_query_per_shard_for_a_full_page() {
        let (temp, original) = fixture();
        drop(original);
        let source = format!("Msg_{:x}", Md5::digest(b"peer"));
        let chats: Vec<_> = (0..50).map(|n| format!("peer_{n}")).collect();
        for shard in 0..2 {
            let db = Connection::open(
                temp.path()
                    .join(format!("account/db_storage/message/message_{shard}.db")),
            )
            .unwrap();
            for chat in &chats {
                let table = format!("Msg_{:x}", Md5::digest(chat.as_bytes()));
                db.execute_batch(&format!("CREATE TABLE {table} AS SELECT * FROM {source}; CREATE INDEX {table}_time ON {table}(create_time DESC,local_id DESC)")).unwrap();
            }
            let table = format!("Msg_{:x}", Md5::digest(chats[0].as_bytes()));
            db.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN __agent_wechat_preview BLOB; UPDATE {table} SET __agent_wechat_preview=X'0102'")).unwrap();
        }
        let reader = ProfileReader::open(
            ProfileLocation::new(temp.path().join("account")).unwrap(),
            KeyFile::empty("account"),
            ReadOptions {
                allow_plaintext: true,
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )
        .unwrap();
        BATCH_QUERIES.with(|n| n.set(0));
        POINT_QUERIES.with(|n| n.set(0));
        let latest = reader.latest_messages(&chats, Some("self")).unwrap();
        assert_eq!(latest.len(), chats.len());
        assert_eq!(BATCH_QUERIES.with(Cell::get), 2);
        assert_eq!(POINT_QUERIES.with(Cell::get), 0);
        for chat in &chats {
            let expected = reader.message(&latest[chat].id, Some("self")).unwrap();
            assert_eq!(latest[chat].raw_fields, expected.raw_fields);
            assert_eq!(latest[chat].sender, expected.sender);
        }
        assert!(!latest[&chats[1]]
            .raw_fields
            .contains_key("__agent_wechat_preview"));
    }

    #[test]
    fn a_page_uses_one_bulk_lookup_per_shard_and_no_original_point_reads() {
        let (_temp, reader) = fixture();
        let targets: Vec<_> = (1..=50).map(|n| ("peer".into(), n.to_string())).collect();
        BATCH_QUERIES.with(|n| n.set(0));
        POINT_QUERIES.with(|n| n.set(0));
        let batch = collect(&reader, &targets, Some("self")).unwrap();
        assert_eq!(BATCH_QUERIES.with(Cell::get), 2);
        assert_eq!(POINT_QUERIES.with(Cell::get), 0);
        assert_eq!(batch.len(), 50);
        assert!(batch
            .iter()
            .all(|m| m.sender.as_deref() == Some("peer") && m.outgoing == Some(false)));
        let individual: Vec<_> = targets
            .iter()
            .flat_map(|(thread, server)| {
                reader
                    .messages_by_server_id(thread, server, Some("self"))
                    .unwrap()
            })
            .collect();
        assert_eq!(POINT_QUERIES.with(Cell::get), 50);
        let by_id = |messages: Vec<Message>| {
            messages
                .into_iter()
                .map(|m| (format!("{:?}", m.id), serde_json::to_value(m).unwrap()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(by_id(batch), by_id(individual));
    }

    #[test]
    fn batch_lookup_keeps_unsigned_ids_duplicates_and_conversations_separate() {
        let (_temp, reader) = fixture();
        let targets = vec![
            ("peer".into(), u64::MAX.to_string()),
            ("peer".into(), "99".into()),
            ("peer".into(), "99".into()),
            ("other".into(), "1".into()),
            ("peer".into(), "99999".into()),
        ];
        let found = collect(&reader, &targets, None).unwrap();
        assert_eq!(found.len(), 4);
        assert_eq!(
            found
                .iter()
                .filter(|m| m.server_message_id.as_deref() == Some("99"))
                .count(),
            2
        );
        assert_eq!(
            found
                .iter()
                .filter(|m| m.server_message_id.as_deref() == Some(u64::MAX.to_string().as_str()))
                .count(),
            2
        );
        assert!(found.iter().all(|m| m.peer.as_deref() == Some("peer")));
        for invalid in ["0", "01", "-1", "18446744073709551616"] {
            assert!(matches!(
                collect(&reader, &[("peer".into(), invalid.into())], None),
                Err(ProfileError::InvalidQuery)
            ));
        }
        assert!(matches!(
            collect(&reader, &vec![("peer".into(), "1".into()); 201], None),
            Err(ProfileError::InvalidQuery)
        ));
        assert!(collect(&reader, &[], None).unwrap().is_empty());
    }

    #[test]
    fn batch_visitor_stops_on_a_consumer_error_without_buffering_more_originals() {
        let (_temp, reader) = fixture();
        let targets: Vec<_> = (1..=50).map(|n| ("peer".into(), n.to_string())).collect();
        let mut visited = 0;
        let error = reader.visit_messages_by_server_ids::<ProfileError>(&targets, None, |_| {
            visited += 1;
            Err(ProfileError::ResourceLimit)
        });
        assert!(matches!(error, Err(ProfileError::ResourceLimit)));
        assert_eq!(visited, 1);
    }
}
