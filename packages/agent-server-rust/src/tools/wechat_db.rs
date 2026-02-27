use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Query a WeChat database and return parsed rows.
/// Opens the database with `immutable=1` to avoid acquiring any shared locks
/// that could interfere with WeChat's own writes. Since we open a fresh
/// connection per query and drop it immediately, immutable mode is safe —
/// we always see the latest committed state at open time.
pub fn query_wechat_db(
    db_path: &str,
    hex_key: &str,
    sql: &str,
) -> Vec<Value> {
    let uri = format!("file:{}?immutable=1", db_path);
    let conn = match Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("[wechat-db] Failed to open {db_path}: {e}");
            return Vec::new();
        }
    };

    if let Err(e) = conn.execute_batch(&format!(
        "PRAGMA key = \"x'{hex_key}'\"; PRAGMA cipher_compatibility = 4;"
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

    /// Create a temp DB that simulates WeChat's encrypted DB pattern.
    /// Uses plaintext SQLite (no encryption) since we're testing lock behavior,
    /// not crypto. Lock semantics are identical.
    fn create_test_db(path: &str) -> Connection {
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

    /// Open a read-only connection using the OLD approach (plain SQLITE_OPEN_READ_ONLY).
    /// This acquires shared locks that can block writer checkpointing/commits.
    fn open_readonly(path: &str) -> Connection {
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap()
    }

    /// Open a read-only connection using the NEW approach (immutable=1 URI).
    /// This acquires NO locks at all.
    fn open_immutable(path: &str) -> Connection {
        let uri = format!("file:{}?immutable=1", path);
        Connection::open_with_flags(
            &uri,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap()
    }

    #[test]
    fn immutable_read_does_not_block_writer() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_path_str = db_path.to_str().unwrap();

        // Create DB with DELETE journal mode (not WAL) — worst case for lock contention
        let _setup = create_test_db(db_path_str);
        drop(_setup);

        let path = db_path_str.to_string();
        let barrier = Arc::new(Barrier::new(2));

        // Thread 1: open immutable reader, hold it open, signal writer to proceed
        let b1 = barrier.clone();
        let p1 = path.clone();
        let reader = std::thread::spawn(move || {
            let conn = open_immutable(&p1);
            let count: i64 = conn
                .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
                .unwrap();
            assert!(count >= 2);

            // Signal: reader is holding connection open
            b1.wait();

            // Keep connection alive while writer tries to write
            std::thread::sleep(Duration::from_millis(200));
            drop(conn);
        });

        // Thread 2: wait for reader, then try to write — should NOT be blocked
        let b2 = barrier.clone();
        let p2 = path.clone();
        let writer = std::thread::spawn(move || {
            // Wait for reader to be holding its connection
            b2.wait();

            let start = Instant::now();
            let conn = Connection::open(&p2).unwrap();
            conn.execute_batch("PRAGMA journal_mode = DELETE;").unwrap();
            conn.execute(
                "INSERT INTO messages (content) VALUES (?1)",
                ["from writer"],
            )
            .unwrap();
            let elapsed = start.elapsed();

            // Writer should complete quickly (< 100ms), not blocked by reader
            assert!(
                elapsed < Duration::from_millis(100),
                "Writer was blocked for {:?} — immutable reader is holding locks!",
                elapsed
            );
        });

        reader.join().unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn readonly_reader_can_block_writer_in_delete_mode() {
        // This test demonstrates the problem that immutable=1 solves.
        // With DELETE journal mode, a read-only reader holds a SHARED lock
        // that prevents the writer from acquiring an EXCLUSIVE lock.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_readonly.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db(db_path_str);
        drop(_setup);

        let path = db_path_str.to_string();
        let barrier = Arc::new(Barrier::new(2));

        // Thread 1: plain read-only reader with active statement (holds SHARED lock)
        let b1 = barrier.clone();
        let p1 = path.clone();
        let reader = std::thread::spawn(move || {
            let conn = open_readonly(&p1);
            // Start a query to acquire SHARED lock
            let mut stmt = conn.prepare("SELECT * FROM messages").unwrap();
            let _rows: Vec<_> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect();

            // Signal writer while we still hold the connection
            b1.wait();
            // Hold the connection open
            std::thread::sleep(Duration::from_millis(300));
            drop(stmt);
            drop(conn);
        });

        // Thread 2: try to write while reader holds SHARED lock
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

            // With busy_timeout=0 and DELETE mode, write may fail with SQLITE_BUSY
            // if the reader's shared lock is still held.
            // Note: this depends on OS-level locking behavior, so we just log the result
            // rather than hard-assert — the important thing is the immutable test above ALWAYS passes.
            match result {
                Ok(_) => eprintln!("[info] Writer succeeded (reader may have released lock)"),
                Err(e) => eprintln!("[expected] Writer blocked/failed as expected: {e}"),
            }
        });

        reader.join().unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn immutable_reads_are_consistent_per_connection() {
        // Verify that immutable=1 sees a consistent snapshot at open time
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_consistent.db");
        let db_path_str = db_path.to_str().unwrap();

        let _setup = create_test_db(db_path_str);
        drop(_setup);

        // Open immutable reader — should see 2 rows
        let reader = open_immutable(db_path_str);
        let count_before: i64 = reader
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_before, 2);

        // Write more data via a separate connection
        {
            let writer = Connection::open(db_path_str).unwrap();
            writer
                .execute("INSERT INTO messages (content) VALUES ('new')", [])
                .unwrap();
        }

        // Immutable reader may or may not see the new row (implementation-defined).
        // The point is: it doesn't crash, corrupt, or lock.
        let count_after: i64 = reader
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert!(count_after >= 2); // At least the original data

        drop(reader);

        // Fresh immutable connection MUST see the new row
        let reader2 = open_immutable(db_path_str);
        let count_fresh: i64 = reader2
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_fresh, 3, "Fresh immutable connection should see committed writes");
    }
}
