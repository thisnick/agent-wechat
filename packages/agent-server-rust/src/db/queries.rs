use rusqlite::{params, Connection};

pub struct FinderMessageSource<'a> {
    pub session_id: &'a str,
    pub account_dir: &'a str,
    pub chat_id: &'a str,
    pub local_id: i64,
    pub server_id: i64,
    pub object_id: &'a str,
    pub object_nonce_id: &'a str,
    pub raw_xml: &'a str,
}

pub fn upsert_finder_message_source(
    conn: &Connection,
    source: &FinderMessageSource<'_>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO finder_message_sources (
             session_id, account_dir, chat_id, local_id, server_id,
             object_id, object_nonce_id, raw_xml
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session_id, account_dir, chat_id, local_id) DO UPDATE SET
             server_id = excluded.server_id,
             object_id = excluded.object_id,
             object_nonce_id = excluded.object_nonce_id,
             raw_xml = excluded.raw_xml,
             updated_at = datetime('now')",
        params![
            source.session_id,
            source.account_dir,
            source.chat_id,
            source.local_id,
            source.server_id,
            source.object_id,
            source.object_nonce_id,
            source.raw_xml,
        ],
    )?;
    Ok(())
}

// ============================================
// SYNC STATE QUERIES
// ============================================

pub fn get_sync_state(conn: &Connection, key: &str, session_id: Option<&str>) -> Option<String> {
    let result = match session_id {
        Some(sid) => conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1 AND session_id = ?2",
                params![key, sid],
                |row| row.get::<_, String>(0),
            )
            .ok(),
        None => conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1 AND session_id IS NULL",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok(),
    };
    result
}

pub fn set_sync_state(conn: &Connection, key: &str, value: &str, session_id: Option<&str>) {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sync_state (session_id, key, value, updated_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session_id, key) DO UPDATE SET
           value = excluded.value,
           updated_at = excluded.updated_at",
        params![session_id, key, value, now],
    )
    .ok();
}

// ============================================
// SESSION QUERIES
// ============================================

pub fn get_session_logged_in_user(conn: &Connection, session_id: &str) -> Option<String> {
    conn.query_row(
        "SELECT logged_in_user FROM sessions WHERE id = ?1",
        params![session_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

pub fn update_session_logged_in_user(
    conn: &Connection,
    session_id: &str,
    logged_in_user: Option<&str>,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let login_state = if logged_in_user.is_some() { "logged_in" } else { "logged_out" };
    conn.execute(
        "UPDATE sessions SET logged_in_user = ?1, login_state = ?2, updated_at = ?3 WHERE id = ?4",
        params![logged_in_user, login_state, now, session_id],
    )
    .ok();
}

pub fn clear_session_data(conn: &Connection, session_id: &str) {
    conn.execute(
        "DELETE FROM wechat_keys WHERE session_id = ?1",
        params![session_id],
    )
    .ok();
    conn.execute(
        "DELETE FROM sync_state WHERE session_id = ?1",
        params![session_id],
    )
    .ok();
    conn.execute(
        "DELETE FROM finder_message_sources WHERE session_id = ?1",
        params![session_id],
    )
    .ok();
}

#[cfg(test)]
mod finder_source_tests {
    use super::{upsert_finder_message_source, FinderMessageSource};
    use rusqlite::Connection;

    fn source<'a>(raw_xml: &'a str, object_nonce_id: &'a str) -> FinderMessageSource<'a> {
        FinderMessageSource {
            session_id: "default",
            account_dir: "wxid_test",
            chat_id: "group@chatroom",
            local_id: 5023,
            server_id: 5912467981442574253,
            object_id: "15000325687253408694",
            object_nonce_id,
            raw_xml,
        }
    }

    #[test]
    fn finder_message_source_is_stored_idempotently() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE finder_message_sources (
                session_id TEXT NOT NULL,
                account_dir TEXT NOT NULL,
                chat_id TEXT NOT NULL,
                local_id INTEGER NOT NULL,
                server_id INTEGER NOT NULL,
                object_id TEXT NOT NULL,
                object_nonce_id TEXT NOT NULL,
                raw_xml TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (session_id, account_dir, chat_id, local_id)
            );",
        )
        .unwrap();

        upsert_finder_message_source(&conn, &source("<msg>first</msg>", "nonce-one"))
            .unwrap();
        upsert_finder_message_source(&conn, &source("<msg>second</msg>", "nonce-two"))
            .unwrap();

        let stored: (i64, String, String) = conn
            .query_row(
                "SELECT COUNT(*), object_nonce_id, raw_xml FROM finder_message_sources",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored.0, 1);
        assert_eq!(stored.1, "nonce-two");
        assert_eq!(stored.2, "<msg>second</msg>");
    }
}
