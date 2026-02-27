use crate::db::get_db;
use crate::sessions;
use crate::tools::wechat_db::get_db_path;
use crate::tools::wechat_keys::get_stored_keys;
use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

/// Spawn a background task that periodically checkpoints WeChat's WAL-mode databases.
///
/// WeChat writes to SQLite databases using WAL (Write-Ahead Logging) mode, which
/// defers flushing data from the WAL file to the main DB file. Our reads use
/// `immutable=1` (which skips the WAL entirely) for stability, so we need to
/// trigger periodic checkpoints to keep the main DB file up to date.
///
/// Uses PASSIVE checkpoint mode, which never blocks WeChat's writer.
pub fn spawn_checkpoint_task() {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            run_checkpoints();
        }
    });
}

fn run_checkpoints() {
    let session = match sessions::manager::get_session("default") {
        Some(s) => s,
        None => return,
    };

    let account_dir = match &session.logged_in_user {
        Some(a) => a.clone(),
        None => return,
    };

    let keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &account_dir)
    };

    for (db_name, hex_key) in &keys {
        // Skip metadata keys (e.g. _image_aes)
        if db_name.starts_with('_') {
            continue;
        }

        let db_path = get_db_path(&account_dir, db_name);
        if let Err(e) = checkpoint_db(&db_path, hex_key) {
            tracing::debug!("[checkpoint] {db_name}: {e}");
        }
    }
}

fn checkpoint_db(db_path: &str, hex_key: &str) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    conn.execute_batch(&format!(
        "PRAGMA key = \"x'{hex_key}'\"; PRAGMA cipher_compatibility = 4;"
    ))?;

    conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;

    Ok(())
}
