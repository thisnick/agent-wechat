use super::exec::{exec_command, ExecOptions};
use super::wechat_db::{get_db_path, list_account_dbs};
use rusqlite::{params, Connection, OpenFlags};
use std::collections::HashMap;

/// Extract all WeChat DB credentials (async, non-blocking).
/// Calls the Python extract-keys script.
pub async fn extract_keys_async(wechat_pid: i64) -> HashMap<String, String> {
    let out_path = format!("/tmp/wechat_keys_{wechat_pid}.json");

    let _ = exec_command(
        "env",
        &[
            "HOME=/home/wechat",
            "python3",
            "/opt/tools/extract-keys.py",
            "--pid",
            &wechat_pid.to_string(),
            "--output",
            &out_path,
        ],
        &ExecOptions {
            timeout_ms: 120_000,
            ..Default::default()
        },
    )
    .await;

    let result = (|| -> Option<HashMap<String, String>> {
        let content = std::fs::read_to_string(&out_path).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
        let keys = parsed.get("keys")?.as_object()?;
        let map: HashMap<String, String> = keys
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();

        let db_keys: Vec<_> = map.keys().filter(|k| !k.starts_with('_')).collect();
        let has_image_aes = map.contains_key("_image_aes");
        tracing::info!(
            "[wechat-keys] Extracted {} DB keys, image key: {}",
            db_keys.len(),
            if has_image_aes { "yes" } else { "no" }
        );
        Some(map)
    })();

    // Clean up temp file
    let _ = std::fs::remove_file(&out_path);

    result.unwrap_or_default()
}

/// Get stored keys for a session + account from the agent DB.
pub fn get_stored_keys(
    conn: &Connection,
    session_id: &str,
    account_dir: &str,
) -> HashMap<String, String> {
    let mut stmt = conn
        .prepare(
            "SELECT db_name, hex_key FROM wechat_keys
             WHERE session_id = ?1 AND account_dir = ?2",
        )
        .unwrap();

    let rows = stmt
        .query_map(params![session_id, account_dir], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
            ))
        })
        .unwrap();

    let mut map = HashMap::new();
    for row in rows.flatten() {
        map.insert(row.0, row.1);
    }
    map
}

/// Store extracted keys in the agent DB.
pub fn store_keys(
    conn: &Connection,
    session_id: &str,
    account_dir: &str,
    keys: &HashMap<String, String>,
) {
    let now = chrono::Utc::now().to_rfc3339();
    for (db_name, hex_key) in keys {
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO wechat_keys (id, session_id, account_dir, db_name, hex_key, verified_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(session_id, account_dir, db_name) DO UPDATE SET
               hex_key = excluded.hex_key,
               verified_at = excluded.verified_at",
            params![id, session_id, account_dir, db_name, hex_key, now],
        )
        .ok();
    }
}

/// Store a single key.
pub fn store_single_key(
    conn: &Connection,
    session_id: &str,
    account_dir: &str,
    db_name: &str,
    hex_key: &str,
) {
    let mut keys = HashMap::new();
    keys.insert(db_name.to_string(), hex_key.to_string());
    store_keys(conn, session_id, account_dir, &keys);
}

/// Verify a single key against a database file.
/// Opens with immutable=1 to avoid acquiring any locks that could interfere
/// with WeChat's own writes/checkpoints.
pub fn verify_key(db_path: &str, hex_key: &str) -> bool {
    let uri = format!("file:{}?immutable=1", db_path);
    let conn = match Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return false,
    };

    if conn
        .execute_batch(&format!(
            "PRAGMA key = \"x'{hex_key}'\"; PRAGMA cipher_compatibility = 4;"
        ))
        .is_err()
    {
        return false;
    }

    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })
    .is_ok()
}

/// Check if credential setup is needed.
///
/// 1. Check that all required DB keys exist in stored_keys
/// 2. Scan disk for additional required DBs (message_N, media_N) without keys
/// 3. Spot-check one key for validity
pub fn needs_key_extraction(
    conn: &Connection,
    session_id: &str,
    account_dir: &str,
) -> bool {
    let stored_keys = get_stored_keys(conn, session_id, account_dir);

    if stored_keys.is_empty() {
        tracing::info!("[wechat-keys] No stored keys, extraction needed");
        return true;
    }

    let required_exact: &[&str] = &[
        "session.db",
        "contact.db",
        "emoticon.db",
        "head_image.db",
        "hardlink.db",
    ];
    let required_prefixes: &[&str] = &["message_", "media_"];

    // Check required exact keys are present (regardless of disk scan)
    let missing_required: Vec<&str> = required_exact
        .iter()
        .filter(|name| !stored_keys.contains_key(**name))
        .copied()
        .collect();

    if !missing_required.is_empty() {
        tracing::info!(
            "[wechat-keys] Missing required keys: {}",
            missing_required.join(", ")
        );
        return true;
    }

    // Scan disk for sharded DBs (message_N.db, media_N.db) missing keys
    let existing_dbs = list_account_dbs(account_dir);
    tracing::debug!(
        "[wechat-keys] Disk scan: {} files for account {}",
        existing_dbs.len(),
        account_dir
    );

    let missing_on_disk: Vec<_> = existing_dbs
        .iter()
        .filter(|name| {
            required_prefixes.iter().any(|p| name.starts_with(p))
                && !stored_keys.contains_key(name.as_str())
        })
        .collect();

    if !missing_on_disk.is_empty() {
        tracing::info!(
            "[wechat-keys] Missing keys for on-disk DBs: {}",
            missing_on_disk.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        );
        return true;
    }

    // Spot-check one key
    let check_db = "session.db";
    if let Some(check_key) = stored_keys.get(check_db) {
        let check_path = get_db_path(account_dir, check_db);
        if !verify_key(&check_path, check_key) {
            tracing::info!("[wechat-keys] Spot-check failed for {check_db}, re-extraction needed");
            return true;
        }
    }

    let db_key_count = stored_keys.keys().filter(|k| !k.starts_with('_')).count();
    tracing::info!(
        "[wechat-keys] {db_key_count} DB keys stored, spot-check passed, image_aes={}",
        stored_keys.contains_key("_image_aes")
    );
    false
}

/// Get stored image keys.
pub fn get_image_keys(
    conn: &Connection,
    session_id: &str,
    account_dir: &str,
) -> Option<(String, Option<u8>)> {
    let keys = get_stored_keys(conn, session_id, account_dir);
    let aes_hex = keys.get("_image_aes")?;
    let xor_byte = keys
        .get("_image_xor")
        .and_then(|h| u8::from_str_radix(h, 16).ok());
    Some((aes_hex.clone(), xor_byte))
}
