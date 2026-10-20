use crate::{ProfileError, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSchema {
    pub name: String,
    pub declared_type: String,
    pub not_null: bool,
    pub default_sql: Option<String>,
    pub primary_key_position: i64,
    pub hidden: i64,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSchema {
    pub name: String,
    pub unique: bool,
    pub origin: String,
    pub partial: bool,
    pub columns: Vec<Option<String>>,
    pub definition: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableSchema {
    pub name: String,
    pub definition: Option<String>,
    pub columns: Vec<ColumnSchema>,
    pub indexes: Vec<IndexSchema>,
    pub triggers: Vec<(String, Option<String>)>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaReport {
    pub database: String,
    pub fingerprint: String,
    pub user_version: i64,
    pub application_id: i64,
    /// SQLite's schema counter is diagnostic; it is not a WeChat release.
    pub sqlite_schema_counter: i64,
    pub tables: Vec<TableSchema>,
    pub readable_capabilities: Vec<String>,
    pub writes_supported: bool,
}
pub(crate) fn quote_identifier(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
pub(crate) fn message_table(s: &str) -> bool {
    s.len() == 36 && s.starts_with("Msg_") && s.as_bytes()[4..].iter().all(u8::is_ascii_hexdigit)
}

pub(crate) fn inspect(c: &Connection, database: &str) -> Result<SchemaReport> {
    let user_version = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let application_id = c.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let counter = c.query_row("PRAGMA schema_version", [], |r| r.get(0))?;
    let mut tables = Vec::new();
    let mut stm =
        c.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")?;
    let names = stm
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if names.len() > 16384 {
        return Err(ProfileError::ResourceLimit);
    }
    for (name, definition) in names {
        // Custom WCDB tokenizers/modules are not part of this reader. Retain
        // their SQL in the fingerprint without asking SQLite to instantiate it.
        if definition.as_ref().is_some_and(|s| {
            s.trim_start()
                .to_ascii_uppercase()
                .starts_with("CREATE VIRTUAL TABLE")
        }) {
            tables.push(TableSchema {
                name,
                definition,
                columns: Vec::new(),
                indexes: Vec::new(),
                triggers: Vec::new(),
            });
            continue;
        }
        let q = quote_identifier(&name);
        let mut s = c.prepare(&format!("PRAGMA table_xinfo({q})"))?;
        let columns = s
            .query_map([], |r| {
                Ok(ColumnSchema {
                    name: r.get(1)?,
                    declared_type: r.get(2)?,
                    not_null: r.get::<_, i64>(3)? != 0,
                    default_sql: r.get(4)?,
                    primary_key_position: r.get(5)?,
                    hidden: r.get(6)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut s = c.prepare(&format!("PRAGMA index_list({q})"))?;
        let raw = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)? != 0,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut indexes = Vec::new();
        for (name, unique, origin, partial) in raw {
            let mut s = c.prepare(&format!("PRAGMA index_info({})", quote_identifier(&name)))?;
            let columns = s
                .query_map([], |r| r.get::<_, Option<String>>(2))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let definition = c
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='index' AND name=?1",
                    [&name],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            indexes.push(IndexSchema {
                name,
                unique,
                origin,
                partial,
                columns,
                definition,
            });
        }
        indexes.sort_by(|a, b| a.name.cmp(&b.name));
        let mut s = c.prepare(
            "SELECT name,sql FROM sqlite_master WHERE type='trigger' AND tbl_name=?1 ORDER BY name",
        )?;
        let triggers = s
            .query_map([&name], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        tables.push(TableSchema {
            name,
            definition,
            columns,
            indexes,
            triggers,
        });
    }
    // Counter excluded: rebuilding an identical schema need not change the
    // compatibility fingerprint. The actual definitions/columns/indexes bind it.
    let canonical = serde_json::to_vec(&(user_version, application_id, &tables))
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let fingerprint = hex::encode(Sha256::digest(&canonical));
    let map: BTreeMap<_, BTreeSet<_>> = tables
        .iter()
        .map(|t| {
            (
                t.name.as_str(),
                t.columns.iter().map(|v| v.name.as_str()).collect(),
            )
        })
        .collect();
    let has = |table: &str, columns: &[&str]| {
        map.get(table)
            .is_some_and(|s| columns.iter().all(|c| s.contains(c)))
    };
    let mut caps = Vec::new();
    if has(
        "contact",
        &[
            "id",
            "username",
            "nick_name",
            "remark",
            "alias",
            "local_type",
        ],
    ) {
        caps.push("contacts".into());
    }
    if has("chat_room", &["id", "username", "owner", "ext_buffer"])
        && has("chatroom_member", &["room_id", "member_id"])
    {
        caps.push("groups".into());
    }
    if has(
        "SessionTable",
        &[
            "username",
            "unread_count",
            "sort_timestamp",
            "last_timestamp",
            "is_hidden",
        ],
    ) {
        caps.push("threads".into());
    }
    if has(
        "head_image",
        &["username", "md5", "image_buffer", "update_time"],
    ) {
        caps.push("avatars".into());
    }
    if has(
        "VoiceInfo",
        &["chat_name_id", "local_id", "svr_id", "voice_data"],
    ) && has("Name2Id", &["user_name"])
    {
        caps.push("voice".into());
    }
    if has("Name2Id", &["user_name"])
        && tables.iter().filter(|t| message_table(&t.name)).any(|t| {
            [
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
            .all(|n| t.columns.iter().any(|c| c.name == *n))
        })
    {
        caps.push("messages".into());
    }
    Ok(SchemaReport {
        database: database.into(),
        fingerprint,
        user_version,
        application_id,
        sqlite_schema_counter: counter,
        tables,
        readable_capabilities: caps,
        writes_supported: false,
    })
}

impl SchemaReport {
    pub fn supports(&self, capability: &str) -> bool {
        self.readable_capabilities.iter().any(|v| v == capability)
    }
    /// No generic writer is authorized merely by a readable or additive schema.
    pub fn require_writable(&self) -> Result<()> {
        Err(ProfileError::WritesUnsupported)
    }
}
