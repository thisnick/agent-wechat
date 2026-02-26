use crate::db::get_db;
use crate::sessions;
use crate::tools::wechat_db::get_db_path;
use crate::tools::wechat_keys::get_stored_keys;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

/// Set of DB names whose journal mode has already been logged.
static LOGGED_MODES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Spawn a background task that periodically checkpoints WeChat's WAL-mode databases.
///
/// If a database uses WAL, the checkpoint flushes the WAL to the main DB file.
/// If a database uses DELETE (or any non-WAL) journal mode, the checkpoint is
/// skipped — there is no WAL file to flush, and reads already see the main DB
/// directly.
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
        if let Err(e) = checkpoint_db(&db_path, hex_key, db_name) {
            tracing::debug!("[checkpoint] {db_name}: {e}");
        }
    }
}

fn checkpoint_db(
    db_path: &str,
    hex_key: &str,
    db_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    conn.execute_batch(&format!(
        "PRAGMA key = \"x'{hex_key}'\"; PRAGMA cipher_compatibility = 4;"
    ))?;

    // Detect and log journal mode (once per DB name)
    let mode: String = conn.query_row("PRAGMA journal_mode;", [], |r| r.get(0))?;

    {
        let mut guard = LOGGED_MODES.lock().unwrap();
        let set = guard.get_or_insert_with(HashSet::new);
        if set.insert(db_name.to_string()) {
            tracing::info!("[checkpoint] {db_name}: journal_mode={mode}");
        }
    }

    if mode == "wal" {
        conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
    }
    // DELETE / PERSIST / TRUNCATE / OFF / MEMORY — no WAL to checkpoint

    Ok(())
}
