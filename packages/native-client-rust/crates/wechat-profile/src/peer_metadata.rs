//! Cached contact metadata from the same official database read transaction.
use crate::{DatabaseState, ProfileError, ProfileReader, Result, SchemaReport};
use rusqlite::{params_from_iter, types::ValueRef, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const MAX_PEERS: usize = 200;
const MAX_CATALOG_ROWS: usize = 4096;
const MAX_METADATA_TEXT_BYTES: usize = 4096;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerMetadata {
    pub local_type: Option<i64>,
    pub flag: Option<i64>,
    pub delete_flag: Option<i64>,
    pub verify_flag: Option<i64>,
    /// Present field 13 of the ordinary contact extra message; absent is not zero.
    pub contact_service_type: Option<i32>,
    pub business: Option<PeerBusinessMetadata>,
    pub openim: Option<PeerOpenImMetadata>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerBusinessMetadata {
    pub service_type: Option<i64>,
    pub accept_type: Option<i64>,
    pub child_type: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOpenImMetadata {
    pub app_id: Option<String>,
    pub wording_id: Option<String>,
    pub wordings: Vec<PeerOpenImWording>,
    pub applications: Vec<PeerOpenImApplication>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOpenImWording {
    pub language_id: i64,
    pub wording: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerOpenImApplication {
    pub language_id: i64,
    pub account_type_id: Option<String>,
    pub account_type_cached: bool,
}

fn integer(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Integer(value) => Some(value),
        _ => None,
    })
}

fn text(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(index)? {
        ValueRef::Text(value) if value.len() <= MAX_METADATA_TEXT_BYTES => {
            std::str::from_utf8(value).ok().map(str::to_owned)
        }
        _ => None,
    })
}

fn has_table(schema: &SchemaReport, table: &str, columns: &[&str]) -> Result<bool> {
    let Some(table) = schema.tables.iter().find(|item| item.name == table) else {
        return Ok(false);
    };
    if columns
        .iter()
        .all(|name| table.columns.iter().any(|column| column.name == *name))
    {
        Ok(true)
    } else {
        Err(ProfileError::UnsupportedSchema(schema.database.clone()))
    }
}

impl ProfileReader {
    /// Read one page's saved metadata without refreshing contacts or catalogs.
    /// Missing peers are omitted. A malformed extra buffer affects only its peer;
    /// an incompatible database schema remains an error.
    pub fn peer_metadata(&self, peers: &[String]) -> Result<BTreeMap<String, PeerMetadata>> {
        if peers.len() > MAX_PEERS
            || peers
                .iter()
                .any(|name| name.is_empty() || name.len() > 4096 || name.contains('\0'))
        {
            return Err(ProfileError::InvalidQuery);
        }
        if peers.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut candidates = self
            .snapshot
            .inventory
            .iter()
            .filter(|item| item.relative_path.rsplit('/').next() == Some("contact.db"));
        let database = candidates
            .next()
            .ok_or_else(|| ProfileError::UnsupportedSchema("contact.db".into()))?;
        if candidates.next().is_some() {
            return Err(ProfileError::UnsupportedSchema("contact.db".into()));
        }
        match database.state {
            DatabaseState::KeyUnavailable => {
                return Err(ProfileError::KeyUnavailable(database.relative_path.clone()))
            }
            DatabaseState::KeyMismatch => {
                return Err(ProfileError::KeyMismatch(database.relative_path.clone()))
            }
            _ => {}
        }
        let schema = self
            .schemas
            .iter()
            .find(|schema| schema.database == database.relative_path)
            .ok_or_else(|| ProfileError::UnsupportedSchema(database.relative_path.clone()))?;
        if !has_table(
            schema,
            "contact",
            &[
                "id",
                "username",
                "local_type",
                "flag",
                "delete_flag",
                "verify_flag",
                "extra_buffer",
            ],
        )? {
            return Err(ProfileError::UnsupportedSchema(
                database.relative_path.clone(),
            ));
        }
        let connection = self.snapshot.open(&database.relative_path)?;
        // Owned readers already begin a transaction. Copied snapshots are
        // immutable to callers; begin here as well for all catalog joins.
        let transaction = connection
            .is_autocommit()
            .then(|| connection.unchecked_transaction())
            .transpose()?;
        let result = read_metadata(
            &connection,
            schema,
            peers,
            self.snapshot.options.max_blob_bytes,
        );
        if let Some(transaction) = transaction {
            transaction.rollback()?;
        }
        result
    }
}

fn read_metadata(
    connection: &Connection,
    schema: &SchemaReport,
    peers: &[String],
    max_blob_bytes: usize,
) -> Result<BTreeMap<String, PeerMetadata>> {
    let placeholders = vec!["?"; peers.len()].join(",");
    record_query();
    let mut statement = connection.prepare(&format!(
        "SELECT username,local_type,flag,delete_flag,verify_flag,CASE WHEN length(extra_buffer)<={max_blob_bytes} THEN extra_buffer END FROM contact WHERE id IN (SELECT min(id) FROM contact WHERE username IN ({placeholders}) GROUP BY username HAVING count(*)=1) LIMIT {}",
        MAX_PEERS + 1
    ))?;
    let mut rows = statement.query(params_from_iter(peers))?;
    let mut metadata = BTreeMap::new();
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > MAX_PEERS {
            return Err(ProfileError::ResourceLimit);
        }
        let Some(name) = text(row, 0)? else {
            continue;
        };
        let local_type = integer(row, 1)?;
        let bytes = match row.get_ref(5)? {
            ValueRef::Blob(value) | ValueRef::Text(value) => Some(value),
            _ => None,
        };
        let (contact_service_type, openim) = match local_type {
            Some(5..=7) => (None, bytes.and_then(|value| decode_openim(value).ok())),
            Some(0..=4) => (
                bytes
                    .and_then(|value| decode_service_type(value).ok())
                    .flatten(),
                None,
            ),
            _ => (None, None),
        };
        let item = PeerMetadata {
            local_type,
            flag: integer(row, 2)?,
            delete_flag: integer(row, 3)?,
            verify_flag: integer(row, 4)?,
            contact_service_type,
            business: None,
            openim,
        };
        metadata.insert(name, item);
    }
    drop(rows);
    drop(statement);
    if has_table(
        schema,
        "biz_info",
        &["username", "type", "accept_type", "child_type"],
    )? {
        read_business(connection, &placeholders, peers, &mut metadata)?;
    }
    read_openim_catalogs(connection, schema, &mut metadata)?;
    Ok(metadata)
}

fn read_business(
    connection: &Connection,
    placeholders: &str,
    peers: &[String],
    metadata: &mut BTreeMap<String, PeerMetadata>,
) -> Result<()> {
    record_query();
    let mut statement = connection.prepare(&format!(
        "SELECT username,type,accept_type,child_type FROM biz_info WHERE username IN ({placeholders}) GROUP BY username HAVING count(*)=1 LIMIT {}",
        MAX_PEERS + 1
    ))?;
    let mut rows = statement.query(params_from_iter(peers))?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > MAX_PEERS {
            return Err(ProfileError::ResourceLimit);
        }
        let Some(name) = text(row, 0)? else {
            continue;
        };
        let Some(item) = metadata.get_mut(&name) else {
            continue;
        };
        item.business = Some(PeerBusinessMetadata {
            service_type: integer(row, 1)?,
            accept_type: integer(row, 2)?,
            child_type: integer(row, 3)?,
        });
    }
    Ok(())
}

fn read_openim_catalogs(
    connection: &Connection,
    schema: &SchemaReport,
    metadata: &mut BTreeMap<String, PeerMetadata>,
) -> Result<()> {
    let has_wordings = has_table(
        schema,
        "openim_wording",
        &["app_id", "wording_id", "lang_id", "wording"],
    )?;
    let has_applications = has_table(
        schema,
        "openim_appid",
        &["app_id", "lang_id", "acct_type_id"],
    )?;
    let has_account_types = has_table(schema, "openim_acct_type", &["acc_type_id", "lang_id"])?;
    let pairs: BTreeSet<_> = metadata
        .values()
        .filter_map(|item| {
            let item = item.openim.as_ref()?;
            Some((
                item.app_id.as_ref()?.clone(),
                item.wording_id.as_ref()?.clone(),
            ))
        })
        .collect();
    let applications: BTreeSet<_> = metadata
        .values()
        .filter_map(|item| item.openim.as_ref()?.app_id.clone())
        .collect();
    let mut wordings = BTreeMap::<(String, String), Vec<PeerOpenImWording>>::new();
    if has_wordings && !pairs.is_empty() {
        let values = vec!["(?,?)"; pairs.len()].join(",");
        let arguments: Vec<_> = pairs
            .iter()
            .flat_map(|(app, wording)| [app, wording])
            .collect();
        record_query();
        let mut statement = connection.prepare(&format!(
            "WITH requested(app,wording) AS (VALUES {values}) SELECT w.app_id,w.wording_id,w.lang_id,w.wording FROM requested r CROSS JOIN openim_wording w WHERE w.app_id=r.app AND w.wording_id=r.wording GROUP BY w.app_id,w.wording_id,w.lang_id HAVING count(*)=1 ORDER BY w.app_id,w.wording_id,w.lang_id LIMIT {}",
            MAX_CATALOG_ROWS + 1
        ))?;
        let mut rows = statement.query(params_from_iter(arguments))?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > MAX_CATALOG_ROWS {
                return Err(ProfileError::ResourceLimit);
            }
            if let (Some(app), Some(id), Some(language_id), Some(wording)) = (
                text(row, 0)?,
                text(row, 1)?,
                integer(row, 2)?,
                text(row, 3)?,
            ) {
                wordings
                    .entry((app, id))
                    .or_default()
                    .push(PeerOpenImWording {
                        language_id,
                        wording,
                    });
            }
        }
    }
    let mut app_records = BTreeMap::<String, Vec<PeerOpenImApplication>>::new();
    if has_applications && !applications.is_empty() {
        let values = vec!["(?)"; applications.len()].join(",");
        let account_join = if has_account_types {
            "LEFT JOIN openim_acct_type a ON a.acc_type_id=p.acct_type_id AND a.lang_id=p.lang_id"
        } else {
            ""
        };
        let cached = if has_account_types {
            "a.acc_type_id IS NOT NULL"
        } else {
            "0"
        };
        record_query();
        let mut statement = connection.prepare(&format!(
            "WITH requested(app) AS (VALUES {values}) SELECT p.app_id,p.lang_id,p.acct_type_id,{cached} FROM requested r CROSS JOIN openim_appid p {account_join} WHERE p.app_id=r.app GROUP BY p.app_id,p.lang_id HAVING count(*)=1 ORDER BY p.app_id,p.lang_id LIMIT {}",
            MAX_CATALOG_ROWS + 1
        ))?;
        let mut rows = statement.query(params_from_iter(&applications))?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            if count > MAX_CATALOG_ROWS {
                return Err(ProfileError::ResourceLimit);
            }
            if let (Some(app), Some(language_id)) = (text(row, 0)?, integer(row, 1)?) {
                app_records
                    .entry(app)
                    .or_default()
                    .push(PeerOpenImApplication {
                        language_id,
                        account_type_id: text(row, 2)?,
                        account_type_cached: integer(row, 3)? == Some(1),
                    });
            }
        }
    }
    for item in metadata
        .values_mut()
        .filter_map(|item| item.openim.as_mut())
    {
        if let (Some(app), Some(id)) = (&item.app_id, &item.wording_id) {
            item.wordings = wordings
                .get(&(app.clone(), id.clone()))
                .cloned()
                .unwrap_or_default();
        }
        if let Some(app) = &item.app_id {
            item.applications = app_records.get(app).cloned().unwrap_or_default();
        }
    }
    Ok(())
}

fn decode_service_type(bytes: &[u8]) -> Result<Option<i32>> {
    let mut service_type = None;
    walk_fields(bytes, |number, value| {
        if number == 13 {
            let WireValue::Varint(value) = value else {
                return Err(ProfileError::InvalidMetadata);
            };
            service_type = Some(value as u32 as i32);
        }
        Ok(())
    })?;
    Ok(service_type)
}

fn decode_openim(bytes: &[u8]) -> Result<PeerOpenImMetadata> {
    let mut item = PeerOpenImMetadata {
        app_id: None,
        wording_id: None,
        wordings: Vec::new(),
        applications: Vec::new(),
    };
    walk_fields(bytes, |number, value| {
        if matches!(number, 1 | 2) {
            let WireValue::Bytes(bytes) = value else {
                return Err(ProfileError::InvalidMetadata);
            };
            if bytes.len() > MAX_METADATA_TEXT_BYTES {
                return Err(ProfileError::ResourceLimit);
            }
            let value = std::str::from_utf8(bytes)
                .map_err(|_| ProfileError::InvalidMetadata)?
                .to_owned();
            if number == 1 {
                item.app_id = Some(value);
            } else {
                item.wording_id = Some(value);
            }
        }
        Ok(())
    })?;
    Ok(item)
}

enum WireValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Other,
}

fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = *bytes.get(*cursor).ok_or(ProfileError::InvalidMetadata)?;
        *cursor += 1;
        if shift == 9 && byte > 1 {
            return Err(ProfileError::InvalidMetadata);
        }
        value |= u64::from(byte & 0x7f) << (shift * 7);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(ProfileError::InvalidMetadata)
}

fn walk_fields<'a>(
    bytes: &'a [u8],
    mut field: impl FnMut(u32, WireValue<'a>) -> Result<()>,
) -> Result<()> {
    let mut cursor = 0;
    let mut groups = Vec::new();
    let mut count = 0;
    while cursor < bytes.len() {
        count += 1;
        if count > 100_000 {
            return Err(ProfileError::ResourceLimit);
        }
        let tag = varint(bytes, &mut cursor)?;
        let number = tag >> 3;
        if number == 0 || number >= 1 << 29 {
            return Err(ProfileError::InvalidMetadata);
        }
        let wire = tag & 7;
        if wire == 3 {
            if groups.len() >= 32 {
                return Err(ProfileError::ResourceLimit);
            }
            if groups.is_empty() {
                field(number as u32, WireValue::Other)?;
            }
            groups.push(number);
            continue;
        }
        if wire == 4 {
            if groups.pop() != Some(number) {
                return Err(ProfileError::InvalidMetadata);
            }
            continue;
        }
        let value = match wire {
            0 => WireValue::Varint(varint(bytes, &mut cursor)?),
            1 | 2 | 5 => {
                let length = match wire {
                    1 => 8,
                    5 => 4,
                    _ => usize::try_from(varint(bytes, &mut cursor)?)
                        .map_err(|_| ProfileError::InvalidMetadata)?,
                };
                let end = cursor
                    .checked_add(length)
                    .ok_or(ProfileError::InvalidMetadata)?;
                let value = bytes
                    .get(cursor..end)
                    .ok_or(ProfileError::InvalidMetadata)?;
                cursor = end;
                if wire == 2 {
                    WireValue::Bytes(value)
                } else {
                    WireValue::Other
                }
            }
            _ => return Err(ProfileError::InvalidMetadata),
        };
        if groups.is_empty() {
            field(number as u32, value)?;
        }
    }
    if groups.is_empty() {
        Ok(())
    } else {
        Err(ProfileError::InvalidMetadata)
    }
}

fn record_query() {
    #[cfg(test)]
    tests::QUERIES.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
mod tests;
