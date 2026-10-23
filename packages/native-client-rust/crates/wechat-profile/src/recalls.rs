//! Narrow, journal-owned recall projection for the supported official schemas.
use crate::{
    projection::{check_known_table, ownership},
    schema::quote_identifier,
    writer::{find_database, open_write},
    *,
};
use rusqlite::params;

/// Replace the live row while preserving its source/server identity and ordering.
/// Media cache files and unknown official files have independent lifetimes.
pub fn apply_recall_projection(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server_id: &str,
    recalled_at: Option<i64>,
    options: &WriteOptions,
) -> Result<Option<SourceMessageId>> {
    apply_recall_projection_with_notice(location, keys, peer, server_id, recalled_at, None, options)
}

/// Preserve the server's replacement text as XML text, never as markup.
pub fn apply_recall_projection_with_notice(
    location: &ProfileLocation,
    keys: &KeyFile,
    peer: &str,
    server_id: &str,
    recalled_at: Option<i64>,
    replacement_text: Option<&str>,
    options: &WriteOptions,
) -> Result<Option<SourceMessageId>> {
    ownership(location, keys, options)?;
    let notice = replacement_text.unwrap_or("[Message recalled]");
    if notice.is_empty()
        || notice.len() > 16 * 1024
        || notice.chars().any(|c| {
            (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                || matches!(c, '\u{fffe}' | '\u{ffff}')
        })
    {
        return Err(ProfileError::InvalidMetadata);
    }
    let Some(source) = find_projected_message(location, keys, peer, server_id, options)? else {
        return Ok(None);
    };
    let sid = server_id
        .parse::<u64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let local = source
        .local_id
        .parse::<i64>()
        .map_err(|_| ProfileError::InvalidMetadata)?;
    let mut messages = open_write(location, &source.database, keys, options.allow_plaintext)?;
    crate::writer::exact_columns(
        &messages,
        &source.database,
        &source.table,
        crate::writer::MESSAGE_COLUMNS,
    )?;
    let session_db = find_database(location, "session.db")?;
    let mut sessions = open_write(location, &session_db, keys, options.allow_plaintext)?;
    check_known_table(&sessions, "SessionTable")?;
    let resource_db = find_database(location, "message_resource.db")?;
    let mut resources = open_write(location, &resource_db, keys, options.allow_plaintext)?;
    for table in [
        "ChatName2Id",
        "MessageResourceInfo",
        "MessageResourceDetail",
    ] {
        check_known_table(&resources, table)?;
    }
    let table = quote_identifier(&source.table);
    let created: i64 = messages.query_row(
        &format!("SELECT create_time FROM {table} WHERE local_id=?1 AND server_id=?2"),
        params![local, sid as i64],
        |r| r.get(0),
    )?;
    // The caller retains the operation receipt for recovery. Clean search in
    // its own transaction before clearing this row; either phase can be replayed.
    remove_recalled_search(location, keys, peer, server_id, &source, created, options)?;
    let escaped = notice
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\r', "&#13;");
    let xml=format!("<sysmsg type=\"revokemsg\"><revokemsg><content>{escaped}</content><revoketime>{}</revoketime></revokemsg></sysmsg>",recalled_at.unwrap_or(0));
    let tx = messages.transaction()?;
    tx.execute(
        "INSERT INTO Name2Id(user_name,is_session) VALUES('',0) ON CONFLICT(user_name) DO NOTHING",
        [],
    )?;
    let sender: i64 = tx.query_row("SELECT rowid FROM Name2Id WHERE user_name=''", [], |r| {
        r.get(0)
    })?;
    tx.execute(&format!("UPDATE {table} SET local_type=10000,status=4,real_sender_id=?3,message_content=?4,compress_content=NULL,WCDB_CT_message_content=0 WHERE local_id=?1 AND server_id=?2"),params![local,sid as i64,sender,xml])?;
    tx.commit()?;
    // Each database has its own transaction. The caller's durable marker and
    // projection journal permit retry after any intermediate commit.
    let tx = resources.transaction()?;
    tx.execute("DELETE FROM MessageResourceDetail WHERE message_id IN (SELECT message_id FROM MessageResourceInfo WHERE chat_id=(SELECT rowid FROM ChatName2Id WHERE user_name=?1) AND message_svr_id=?2 AND message_local_id=?3 AND message_create_time=?4)",params![peer,sid as i64,local,created])?;
    tx.execute("DELETE FROM MessageResourceInfo WHERE chat_id=(SELECT rowid FROM ChatName2Id WHERE user_name=?1) AND message_svr_id=?2 AND message_local_id=?3 AND message_create_time=?4",params![peer,sid as i64,local,created])?;
    tx.commit()?;
    let tx = sessions.transaction()?;
    tx.execute("UPDATE SessionTable SET summary=?4,last_msg_type=10000,last_msg_sub_type=0,last_msg_sender='',last_sender_display_name='' WHERE username=?1 AND last_msg_locald_id=?2 AND last_timestamp=?3",params![peer,local,created,notice])?;
    tx.commit()?;
    Ok(Some(source))
}
