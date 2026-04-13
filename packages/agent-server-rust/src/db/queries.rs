use rusqlite::{params, Connection};
use std::collections::HashMap;

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

const PAYMENT_RECEIPTS_PREFIX: &str = "payment_receipts:";

fn payment_receipts_key(chat_id: &str) -> String {
    format!("{PAYMENT_RECEIPTS_PREFIX}{chat_id}")
}

fn read_payment_receipts(
    conn: &Connection,
    session_id: &str,
    chat_id: &str,
) -> HashMap<String, String> {
    get_sync_state(conn, &payment_receipts_key(chat_id), Some(session_id))
        .and_then(|json| serde_json::from_str::<HashMap<String, String>>(&json).ok())
        .unwrap_or_default()
}

fn write_payment_receipts(
    conn: &Connection,
    session_id: &str,
    chat_id: &str,
    receipts: &HashMap<String, String>,
) {
    if let Ok(json) = serde_json::to_string(receipts) {
        set_sync_state(
            conn,
            &payment_receipts_key(chat_id),
            &json,
            Some(session_id),
        );
    }
}

pub fn get_payment_receipts(
    conn: &Connection,
    session_id: &str,
    chat_id: &str,
) -> HashMap<i64, String> {
    read_payment_receipts(conn, session_id, chat_id)
        .into_iter()
        .filter_map(|(local_id, received_at)| {
            local_id.parse::<i64>().ok().map(|id| (id, received_at))
        })
        .collect()
}

pub fn mark_payment_received(
    conn: &Connection,
    session_id: &str,
    chat_id: &str,
    local_id: i64,
) -> String {
    let mut receipts = read_payment_receipts(conn, session_id, chat_id);
    let now = chrono::Utc::now().to_rfc3339();
    let received_at = receipts
        .entry(local_id.to_string())
        .or_insert_with(|| now.clone())
        .clone();

    write_payment_receipts(conn, session_id, chat_id, &receipts);

    received_at
}

pub fn remove_payment_receipts(
    conn: &Connection,
    session_id: &str,
    chat_id: &str,
    local_ids: &[i64],
) {
    if local_ids.is_empty() {
        return;
    }

    let mut receipts = read_payment_receipts(conn, session_id, chat_id);
    let mut changed = false;

    for local_id in local_ids {
        changed |= receipts.remove(&local_id.to_string()).is_some();
    }

    if changed {
        write_payment_receipts(conn, session_id, chat_id, &receipts);
    }
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
    let login_state = if logged_in_user.is_some() {
        "logged_in"
    } else {
        "logged_out"
    };
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
}
