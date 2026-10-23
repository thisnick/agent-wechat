//! Exact deletion of recalled ordinary-message search entries. Existing terms
//! come from FTS5 itself; no new text is tokenized and no shadow table is written.
//! The whitelist supports the older contentful FTS table with a three-column
//! auxiliary table. The newer external-content layout remains unsupported.
mod tokenizer;

use crate::{
    projection::{identity, ownership},
    schema::quote_identifier,
    writer::{exact_columns, open_write, MESSAGE_COLUMNS},
    *,
};
use md5::{Digest, Md5};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{collections::BTreeMap, path::Path, time::Instant};
use tokenizer::{Gate, Token};

const DATABASE: &str = "message_fts.db";
const MAX_CONTENT: usize = 4 * 1024 * 1024;
const MAX_TOKENS: usize = 262_144;
const MAX_TOKEN_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOTAL_CONTENT: usize = 16 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 64;

fn unsupported() -> ProfileError {
    ProfileError::UnsupportedSchema(DATABASE.into())
}

fn main_sql(partition: usize) -> String {
    format!("CREATE VIRTUAL TABLE message_fts_v4_{partition} USING fts5(tokenize = 'MMFtsTokenizer disable_pinyin', acontent, message_local_id UNINDEXED, sort_seq UNINDEXED, local_type UNINDEXED, session_id UNINDEXED, sender_id UNINDEXED, create_time UNINDEXED)")
}

fn aux_sql(partition: usize) -> String {
    format!("CREATE TABLE message_fts_v4_aux_{partition}(message_local_id INTEGER, sort_seq INTEGER, session_id INTEGER, CONSTRAINT sessionId_localId_sortseq PRIMARY KEY(session_id, message_local_id, sort_seq))")
}

fn normalized(sql: &str) -> String {
    sql.chars()
        .filter(|c| !c.is_ascii_whitespace() && !matches!(c, '\'' | '"'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn schema(connection: &Connection) -> Result<Vec<usize>> {
    let mut query = connection.prepare("SELECT name,sql FROM sqlite_master WHERE type='table'")?;
    let tables = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    if tables.len() > 4096 {
        return Err(ProfileError::ResourceLimit);
    }
    let mut allowed = BTreeMap::new();
    let mut partitions = Vec::new();
    for partition in 0..4 {
        let table = format!("message_fts_v4_{partition}");
        let aux = format!("message_fts_v4_aux_{partition}");
        let expected = [
            (table.clone(), main_sql(partition)),
            (aux, aux_sql(partition)),
            (format!("{table}_content"), format!("CREATE TABLE '{table}_content'(id INTEGER PRIMARY KEY, c0, c1, c2, c3, c4, c5, c6)")),
            (format!("{table}_docsize"), format!("CREATE TABLE '{table}_docsize'(id INTEGER PRIMARY KEY, sz BLOB)")),
            (format!("{table}_data"), format!("CREATE TABLE '{table}_data'(id INTEGER PRIMARY KEY, block BLOB)")),
            (format!("{table}_idx"), format!("CREATE TABLE '{table}_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID")),
            (format!("{table}_config"), format!("CREATE TABLE '{table}_config'(k PRIMARY KEY, v) WITHOUT ROWID")),
        ];
        let present = expected
            .iter()
            .filter(|(name, _)| tables.contains_key(name))
            .count();
        if present != 0 && present != expected.len() {
            return Err(unsupported());
        }
        if present != 0 {
            partitions.push(partition);
        }
        for (name, sql) in expected {
            if tables
                .get(&name)
                .is_some_and(|actual| normalized(actual) != normalized(&sql))
            {
                return Err(unsupported());
            }
            allowed.insert(name, ());
        }
    }
    for name in tables.keys().filter(|name| name.starts_with("message_fts")) {
        if !allowed.contains_key(name)
            && !matches!(
                name.as_str(),
                "message_fts_v4_range" | "message_fts_v4_session_delete_info"
            )
        {
            return Err(unsupported());
        }
    }
    if !partitions.is_empty()
        && !tables.get("name2id").is_some_and(|sql| {
            normalized(sql) == normalized("CREATE TABLE name2id(username TEXT PRIMARY KEY)")
        })
    {
        return Err(unsupported());
    }
    let custom: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE (type='trigger' OR (type='index' AND sql IS NOT NULL)) AND tbl_name LIKE 'message_fts_v4_%'", [], |row| row.get(0))?;
    if custom != 0 {
        return Err(unsupported());
    }
    Ok(partitions)
}

fn search_database(location: &ProfileLocation) -> Result<Option<String>> {
    let mut paths = Vec::new();
    crate::snapshot::walk(&location.storage, &mut paths, 0, 4096)?;
    for path in &paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ProfileError::UnsupportedFile)?;
        if name.starts_with("message_")
            && name.ends_with(".db")
            && !matches!(
                name,
                "message_0.db" | "message_fts.db" | "message_resource.db"
            )
        {
            // The index omits the server and source-database identity. Refuse
            // rotated or unknown shards instead of deleting a colliding tuple.
            return Err(ProfileError::WritesUnsupported);
        }
    }
    let paths = paths
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == DATABASE))
        .collect::<Vec<_>>();
    if paths.len() > 1 {
        return Err(unsupported());
    }
    paths
        .first()
        .map(|path| {
            path.strip_prefix(&location.storage)
                .ok()
                .and_then(Path::to_str)
                .map(|path| path.replace('\\', "/"))
                .ok_or(ProfileError::UnsupportedFile)
        })
        .transpose()
}

struct Document {
    table: String,
    rowid: i64,
    content: Vec<u8>,
    tokens: Vec<Token>,
}

fn docsize(bytes: &[u8], positions: i64) -> Result<()> {
    let mut at = 0;
    for column in 0..7 {
        let start = at;
        let mut value = 0u64;
        loop {
            let byte = *bytes.get(at).ok_or(ProfileError::InvalidMetadata)?;
            at += 1;
            if at - start > 5 || (at - start == 1 && byte == 0x80) {
                return Err(ProfileError::InvalidMetadata);
            }
            value = (value << 7) | u64::from(byte & 0x7f);
            if byte & 0x80 == 0 {
                break;
            }
        }
        if value != if column == 0 { positions as u64 } else { 0 } {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    if at != bytes.len() {
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(())
}

fn frozen_tokens(connection: &Connection, table: &str, rowid: i64) -> Result<Vec<Token>> {
    let mut query = connection.prepare("SELECT CAST(term AS BLOB),col,offset FROM temp.recall_terms WHERE doc=?1 ORDER BY offset,term")?;
    let mut rows = query.query([rowid])?;
    let mut tokens: Vec<Token> = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let token = Token {
            bytes: row.get(0)?,
            position: row.get(2)?,
        };
        let column: String = row.get(1)?;
        bytes = bytes
            .checked_add(token.bytes.len())
            .ok_or(ProfileError::ResourceLimit)?;
        if tokens.len() >= MAX_TOKENS || bytes > MAX_TOKEN_BYTES {
            return Err(ProfileError::ResourceLimit);
        }
        if column != "acontent"
            || token.bytes.is_empty()
            || token.bytes.len() > 32768
            || token.position < 0
            || tokens.last().map_or(token.position != 0, |previous| {
                token.position < previous.position
                    || token.position > previous.position + 1
                    || (token.position == previous.position && token.bytes <= previous.bytes)
            })
        {
            return Err(ProfileError::InvalidMetadata);
        }
        tokens.push(token);
    }
    let sizes: Vec<u8> = connection.query_row(
        &format!(
            "SELECT sz FROM {} WHERE id=?1",
            quote_identifier(&format!("{table}_docsize"))
        ),
        [rowid],
        |row| row.get(0),
    )?;
    docsize(&sizes, tokens.last().map_or(0, |token| token.position + 1))?;
    Ok(tokens)
}

/// Remove only the supported ordinary index entries for this exact owned source.
/// The caller retains its recall intent and retries local work after interruption.
pub fn remove_recalled_search(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server: &str,
    source: &SourceMessageId,
    created: i64,
    options: &WriteOptions,
) -> Result<()> {
    ownership(location, keys, options)?;
    identity(peer)?;
    let server_id = server
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let local = source
        .local_id
        .parse::<i64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let expected_table = format!("Msg_{:x}", Md5::digest(peer.as_bytes()));
    if server_id == 0
        || server_id.to_string() != server
        || local <= 0
        || local.to_string() != source.local_id
        || created <= 0
        || source.table != expected_table
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let Some(database) = search_database(location)? else {
        return Ok(());
    };
    if find_projected_message(location, keys, peer, server, options)?.as_ref() != Some(source) {
        return Err(ProfileError::InvalidMetadata);
    }
    let main = open_write(location, &source.database, keys, options.allow_plaintext)?;
    exact_columns(&main, &source.database, &source.table, MESSAGE_COLUMNS)?;
    let (actual_created, sort): (i64, i64) = main.query_row(
        &format!(
            "SELECT create_time,sort_seq FROM {} WHERE local_id=?1 AND server_id=?2",
            quote_identifier(&source.table)
        ),
        params![local, server_id as i64],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if actual_created != created || sort <= 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    drop(main);
    let mut connection = open_write(location, &database, keys, options.allow_plaintext)?;
    let start = Instant::now();
    connection.progress_handler(1000, Some(move || start.elapsed().as_secs() >= 10));
    let gate = tokenizer::register(&connection)?;
    let partitions = schema(&connection)?;
    if partitions.is_empty() {
        return Ok(());
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let session: Option<i64> = transaction
        .query_row(
            "SELECT rowid FROM name2id WHERE username=?1",
            [peer],
            |row| row.get(0),
        )
        .optional()?;
    let Some(session) = session else {
        return Ok(());
    };
    let mut documents = Vec::new();
    let mut total_content = 0usize;
    let mut total_token_bytes = 0usize;
    for partition in &partitions {
        let table = format!("message_fts_v4_{partition}");
        let aux = format!("message_fts_v4_aux_{partition}");
        let aux_ids = transaction.prepare(&format!("SELECT rowid FROM {aux} WHERE session_id=?1 AND message_local_id=?2 AND sort_seq=?3 LIMIT 2"))?
            .query_map(params![session,local,sort], |row| row.get::<_,i64>(0))?
            .collect::<std::result::Result<Vec<_>,_>>()?;
        let first = documents.len();
        transaction.execute_batch(&format!(
            "CREATE VIRTUAL TABLE temp.recall_terms USING fts5vocab(main,{table},instance)"
        ))?;
        let mut query = transaction.prepare(&format!("SELECT rowid,create_time,length(CAST(acontent AS BLOB)),CASE WHEN length(CAST(acontent AS BLOB))<={MAX_CONTENT} THEN CAST(acontent AS BLOB) END FROM {table} WHERE session_id=?1 AND message_local_id=?2 AND sort_seq=?3 LIMIT {}", MAX_DOCUMENTS+1))?;
        let mut rows = query.query(params![session, local, sort])?;
        while let Some(row) = rows.next()? {
            if documents.len() != first {
                return Err(ProfileError::InvalidMetadata);
            }
            let rowid: i64 = row.get(0)?;
            let time: i64 = row.get(1)?;
            let length: Option<i64> = row.get(2)?;
            if time != created || rowid <= 0 {
                return Err(ProfileError::InvalidMetadata);
            }
            if documents.len() >= MAX_DOCUMENTS
                || length.is_some_and(|n| n < 0 || n > MAX_CONTENT as i64)
            {
                return Err(ProfileError::ResourceLimit);
            }
            let content: Option<Vec<u8>> = row.get(3)?;
            let content = content.unwrap_or_default();
            let tokens = frozen_tokens(&transaction, &table, rowid)?;
            total_content = total_content
                .checked_add(content.len())
                .ok_or(ProfileError::ResourceLimit)?;
            for token in &tokens {
                total_token_bytes = total_token_bytes
                    .checked_add(token.bytes.len())
                    .ok_or(ProfileError::ResourceLimit)?;
            }
            if total_content > MAX_TOTAL_CONTENT || total_token_bytes > MAX_TOKEN_BYTES {
                return Err(ProfileError::ResourceLimit);
            }
            documents.push(Document {
                table: table.clone(),
                rowid,
                content,
                tokens,
            });
        }
        drop(rows);
        drop(query);
        let main_ids = documents[first..]
            .iter()
            .map(|document| document.rowid)
            .collect::<Vec<_>>();
        if main_ids != aux_ids {
            return Err(ProfileError::InvalidMetadata);
        }
        transaction.execute_batch("DROP TABLE temp.recall_terms")?;
    }
    for document in documents {
        let aux = document
            .table
            .replace("message_fts_v4_", "message_fts_v4_aux_");
        let rowid = document.rowid;
        delete_document(&transaction, &gate, document)?;
        if transaction.execute(&format!("DELETE FROM {aux} WHERE rowid=?1 AND session_id=?2 AND message_local_id=?3 AND sort_seq=?4"), params![rowid,session,local,sort])? != 1 {
            return Err(ProfileError::InvalidMetadata);
        }
    }
    transaction.commit()?;
    Ok(())
}

fn delete_document(connection: &Connection, gate: &Gate, document: Document) -> Result<()> {
    gate.arm(document.content, document.tokens)?;
    let deleted = connection.execute(
        &format!(
            "DELETE FROM {} WHERE rowid=?1",
            quote_identifier(&document.table)
        ),
        [document.rowid],
    );
    let disarmed = gate.disarm();
    if deleted? != 1 {
        return Err(ProfileError::InvalidMetadata);
    }
    disarmed?;
    let retained: i64 = connection.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE rowid=?1",
            quote_identifier(&document.table)
        ),
        [document.rowid],
        |row| row.get(0),
    )?;
    if retained != 0 {
        return Err(ProfileError::InvalidMetadata);
    }
    connection.execute_batch(&format!(
        "CREATE VIRTUAL TABLE temp.recall_terms USING fts5vocab(main,{},instance)",
        document.table
    ))?;
    let remaining: i64 = connection.query_row(
        "SELECT count(*) FROM temp.recall_terms WHERE doc=?1",
        [document.rowid],
        |row| row.get(0),
    )?;
    connection.execute_batch("DROP TABLE temp.recall_terms")?;
    if remaining != 0 {
        // Returning before commit rolls back both the FTS5 deletion and every
        // prior deletion in this recall's single search-database transaction.
        return Err(ProfileError::InvalidMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
