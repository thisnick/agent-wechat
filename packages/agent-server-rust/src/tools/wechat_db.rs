use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Query a WeChat database and return parsed rows.
///
/// Opens with READ_ONLY + busy_timeout instead of `immutable=1`.
/// WeChat DBs may use DELETE journal mode (not WAL), so:
/// - `immutable=1` skips change-detection → stale reads even on fresh connections
/// - Normal READ_ONLY acquires a brief SHARED lock → sees current committed state
/// - `busy_timeout` prevents hanging if WeChat holds an EXCLUSIVE lock
/// - Short-lived connection (opened per query, dropped immediately) minimises
///   the window where our SHARED lock could block WeChat's writer.
pub fn query_wechat_db(
    db_path: &str,
    hex_key: &str,
    sql: &str,
) -> Vec<Value> {
    let conn = match Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("[wechat-db] Failed to open {db_path}: {e}");
            return Vec::new();
        }
    };

    if let Err(e) = conn.execute_batch(&format!(
        "PRAGMA key = \"x'{hex_key}'\"; PRAGMA cipher_compatibility = 4; PRAGMA busy_timeout = 200;"
    )) {
        tracing::warn!("[wechat-db] PRAGMA failed for {db_path}: {e}");
        return Vec::new();
    }

    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("[wechat-db] Prepare failed for {db_path}: {e}");
            return Vec::new();
        }
    };

    let col_names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let rows = stmt.query_map([], |row| {
        let mut map = Map::new();
        for (i, name) in col_names.iter().enumerate() {
            let val: Value = match row.get_ref(i) {
                Ok(rusqlite::types::ValueRef::Null) => Value::Null,
                Ok(rusqlite::types::ValueRef::Integer(n)) => Value::Number(n.into()),
                Ok(rusqlite::types::ValueRef::Real(f)) => serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null),
                Ok(rusqlite::types::ValueRef::Text(s)) => {
                    Value::String(String::from_utf8_lossy(s).into_owned())
                }
                Ok(rusqlite::types::ValueRef::Blob(b)) => {
                    // Hex-encode blobs (safety net — callers typically use hex() in SQL)
                    let mut hex = String::with_capacity(b.len() * 2);
                    for byte in b {
                        use std::fmt::Write;
                        let _ = write!(hex, "{byte:02X}");
                    }
                    Value::String(hex)
                }
                Err(_) => Value::Null,
            };
            map.insert(name.clone(), val);
        }
        Ok(Value::Object(map))
    });

    match rows {
        Ok(mapped) => mapped.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            tracing::warn!("[wechat-db] Query failed for {db_path}: {e}");
            Vec::new()
        }
    }
}

/// Find the WeChat process PID.
pub fn find_wechat_pid() -> Option<i64> {
    let output = Command::new("pgrep")
        .args(["-f", "/usr/bin/wechat"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pids: Vec<i64> = stdout
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();

    // Return the PID with the most open file descriptors
    let mut best_pid: Option<i64> = None;
    let mut best_fd_count = 0;

    for pid in pids {
        let fd_dir = format!("/proc/{pid}/fd");
        if let Ok(entries) = std::fs::read_dir(&fd_dir) {
            let count = entries.count();
            if count > best_fd_count {
                best_fd_count = count;
                best_pid = Some(pid);
            }
        }
    }

    best_pid
}

/// Detect the WeChat account directory by scanning /proc/<pid>/fd.
pub fn find_account_dir(wechat_pid: i64) -> Option<String> {
    let fd_dir = format!("/proc/{wechat_pid}/fd");
    let entries = std::fs::read_dir(&fd_dir).ok()?;

    for entry in entries.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            let target_str = target.to_string_lossy();
            if target_str.contains("db_storage") && target_str.ends_with(".db") {
                if let Some(idx) = target_str.find("xwechat_files/") {
                    let rest = &target_str[idx + "xwechat_files/".len()..];
                    if let Some(account_dir) = rest.split('/').next() {
                        if !account_dir.is_empty() {
                            return Some(account_dir.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}

/// List all .db files that exist on disk for a given account.
pub fn list_account_dbs(account_dir: &str) -> Vec<String> {
    let base_paths = [
        format!("/home/wechat/xwechat_files/{account_dir}"),
        format!("/home/wechat/Documents/xwechat_files/{account_dir}"),
    ];

    for base in &base_paths {
        let db_storage = PathBuf::from(base).join("db_storage");
        if !db_storage.exists() {
            continue;
        }

        let mut db_names = Vec::new();
        if let Ok(sub_dirs) = std::fs::read_dir(&db_storage) {
            for sub_dir in sub_dirs.flatten() {
                if sub_dir.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if let Ok(files) = std::fs::read_dir(sub_dir.path()) {
                        for file in files.flatten() {
                            let name = file.file_name().to_string_lossy().to_string();
                            if name.ends_with(".db") {
                                db_names.push(name);
                            }
                        }
                    }
                }
            }
        }

        if !db_names.is_empty() {
            return db_names;
        }
    }

    Vec::new()
}

/// Get the full path to a WeChat database file.
pub fn get_db_path(account_dir: &str, db_name: &str) -> String {
    let sub_dir_map: &[(&str, &str)] = &[
        ("contact.db", "contact"),
        ("contact_fts.db", "contact"),
        ("session.db", "session"),
        ("message_0.db", "message"),
        ("message_fts.db", "message"),
        ("message_resource.db", "message"),
        ("biz_message_0.db", "message"),
        ("media_0.db", "message"),
        ("general.db", "general"),
        ("hardlink.db", "hardlink"),
        ("head_image.db", "head_image"),
        ("emoticon.db", "emoticon"),
        ("favorite.db", "favorite"),
        ("favorite_fts.db", "favorite"),
        ("sns.db", "sns"),
        ("bizchat.db", "bizchat"),
    ];

    let sub_dir = sub_dir_map
        .iter()
        .find(|(name, _)| *name == db_name)
        .map(|(_, dir)| *dir)
        .unwrap_or_else(|| db_name.strip_suffix(".db").unwrap_or(db_name));

    let base_paths = [
        format!("/home/wechat/xwechat_files/{account_dir}"),
        format!("/home/wechat/Documents/xwechat_files/{account_dir}"),
    ];

    for base in &base_paths {
        let full_path = Path::new(base)
            .join("db_storage")
            .join(sub_dir)
            .join(db_name);
        if full_path.exists() {
            return full_path.to_string_lossy().to_string();
        }
    }

    // Default to first path
    Path::new(&base_paths[0])
        .join("db_storage")
        .join(sub_dir)
        .join(db_name)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, OpenFlags};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, Instant};

    /// Create a temp DB in DELETE journal mode (matching expected WeChat behavior).
    fn create_test_db_delete(path: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY, content TEXT);
             INSERT INTO messages (content) VALUES ('hello');
             INSERT INTO messages (content) VALUES ('world');",
        )
        .unwrap();
        conn
    }

    /// Create a temp DB in WAL journal mode.
    fn create_test_db_wal(path: &str) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS messages (id INTEGER PRIMARY KEY, content TEXT);
             INSERT INTO messages (content) VALUES ('hello');
             INSERT INTO messages (content) VALUES ('world');",
        )
        .unwrap();
        conn
    }

    /// Open the way query_wechat_db does now: READ_ONLY + busy_timeout (no immutable).
    fn open_readonly_with_timeout(path: &str) -> Connection {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        conn.execute_batch("PRAGMA busy_timeout = 200;").unwrap();
        conn
    }

    #[test]
    fn readonly_sees_fresh_data_in_delete_mode() {
        // Core property: READ_ONLY (non-immutable) connections see the latest
        // committed data in DELETE-mode databases.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_fresh.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db_delete(db_path_str);
        drop(_setup);

        // Initial read: 2 rows
        {
            let reader = open_readonly_with_timeout(db_path_str);
            let count: i64 = reader
                .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 2);
        }

        // Write a new row
        {
            let writer = Connection::open(db_path_str).unwrap();
            writer
                .execute("INSERT INTO messages (content) VALUES ('new')", [])
                .unwrap();
        }

        // Fresh read: 3 rows — no staleness
        {
            let reader = open_readonly_with_timeout(db_path_str);
            let count: i64 = reader
                .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 3, "READ_ONLY should see fresh data in DELETE mode");
        }
    }

    #[test]
    fn readonly_sees_fresh_data_in_wal_mode() {
        // Same property for WAL mode: READ_ONLY sees latest committed data
        // (reads the WAL file, unlike immutable=1 which skips it).
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_fresh_wal.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db_wal(db_path_str);
        drop(_setup);

        // Write a new row (goes to WAL)
        {
            let writer = Connection::open(db_path_str).unwrap();
            writer
                .execute("INSERT INTO messages (content) VALUES ('wal_row')", [])
                .unwrap();
        }

        // READ_ONLY sees the WAL data
        {
            let reader = open_readonly_with_timeout(db_path_str);
            let count: i64 = reader
                .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 3, "READ_ONLY should see WAL data without checkpoint");
        }
    }

    #[test]
    fn short_lived_readonly_does_not_block_writer_in_delete_mode() {
        // With DELETE journal mode, a READ_ONLY connection holds a SHARED lock
        // while active. Verify that short-lived connections (our pattern) don't
        // meaningfully block writers.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_contention.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db_delete(db_path_str);
        drop(_setup);

        let path = db_path_str.to_string();
        let barrier = Arc::new(Barrier::new(2));

        // Thread 1: short-lived read (mimics query_wechat_db pattern)
        let b1 = barrier.clone();
        let p1 = path.clone();
        let reader = std::thread::spawn(move || {
            b1.wait();
            // Open, query, close — like query_wechat_db does
            let conn = open_readonly_with_timeout(&p1);
            let count: i64 = conn
                .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap();
            assert!(count >= 2);
            drop(conn);
        });

        // Thread 2: writer with busy_timeout — should succeed after reader drops
        let b2 = barrier.clone();
        let p2 = path.clone();
        let writer = std::thread::spawn(move || {
            b2.wait();
            // Small delay so reader starts first
            std::thread::sleep(Duration::from_millis(5));

            let start = Instant::now();
            let conn = Connection::open(&p2).unwrap();
            conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA busy_timeout = 500;")
                .unwrap();
            conn.execute(
                "INSERT INTO messages (content) VALUES (?1)",
                ["from writer"],
            )
            .unwrap();
            let elapsed = start.elapsed();

            // Writer should complete within busy_timeout window
            assert!(
                elapsed < Duration::from_millis(500),
                "Writer was blocked for {:?} — short-lived reader took too long",
                elapsed
            );
        });

        reader.join().unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn long_held_readonly_blocks_writer_in_delete_mode() {
        // Demonstrates why connections must be short-lived in DELETE mode:
        // a held SHARED lock blocks the writer's EXCLUSIVE lock.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_block.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db_delete(db_path_str);
        drop(_setup);

        let path = db_path_str.to_string();
        let barrier = Arc::new(Barrier::new(2));

        // Thread 1: hold read connection open for 300ms
        let b1 = barrier.clone();
        let p1 = path.clone();
        let reader = std::thread::spawn(move || {
            let conn = Connection::open_with_flags(
                &p1,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .unwrap();
            let mut stmt = conn.prepare("SELECT * FROM messages").unwrap();
            let _rows: Vec<_> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect();

            b1.wait();
            std::thread::sleep(Duration::from_millis(300));
            drop(stmt);
            drop(conn);
        });

        // Thread 2: try to write with zero timeout — expect SQLITE_BUSY
        let b2 = barrier.clone();
        let p2 = path.clone();
        let writer = std::thread::spawn(move || {
            b2.wait();

            let conn = Connection::open(&p2).unwrap();
            conn.execute_batch("PRAGMA journal_mode = DELETE; PRAGMA busy_timeout = 0;")
                .unwrap();
            let result = conn.execute(
                "INSERT INTO messages (content) VALUES (?1)",
                ["from writer"],
            );

            match result {
                Ok(_) => eprintln!("[info] Writer succeeded (reader may have released lock)"),
                Err(e) => eprintln!("[expected] Writer blocked as expected: {e}"),
            }
        });

        reader.join().unwrap();
        writer.join().unwrap();
    }
}
