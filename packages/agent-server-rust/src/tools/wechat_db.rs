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

/// Pure helper: extract the account dir name (e.g. `wxid_xxx`) from a path that
/// points at a WeChat DB, e.g. `/home/wechat/xwechat_files/wxid_xxx/db_storage/..`.
/// Returns None for unrelated paths. No I/O, fully unit-testable.
pub fn account_dir_from_db_path(target: &str) -> Option<String> {
    if !target.contains("db_storage") || !target.ends_with(".db") {
        return None;
    }
    let idx = target.find("xwechat_files/")?;
    let rest = &target[idx + "xwechat_files/".len()..];
    let account_dir = rest.split('/').next()?;
    if account_dir.is_empty() {
        None
    } else {
        Some(account_dir.to_string())
    }
}

/// Scan a single process's /proc/<pid>/fd for an open WeChat DB and derive the
/// account dir. Tolerant of permission errors (returns None, never panics).
fn scan_pid_fd_for_account(pid: i64) -> Option<String> {
    let fd_dir = format!("/proc/{pid}/fd");
    let entries = std::fs::read_dir(&fd_dir).ok()?;
    for entry in entries.flatten() {
        if let Ok(target) = std::fs::read_link(entry.path()) {
            if let Some(acct) = account_dir_from_db_path(&target.to_string_lossy()) {
                return Some(acct);
            }
        }
    }
    None
}

/// Enumerate PIDs of WeChat-related processes (main client + helper/renderer
/// processes that may hold the DB file descriptors).
fn related_wechat_pids() -> Vec<i64> {
    let mut pids = Vec::new();
    // -f matches the full command line; covers main + WeChatAppEx/RadiumWMPF helpers.
    for pat in ["/usr/bin/wechat", "wechat", "WeChatAppEx", "RadiumWMPF"] {
        if let Ok(output) = Command::new("pgrep").args(["-f", pat]).output() {
            for s in String::from_utf8_lossy(&output.stdout).split_whitespace() {
                if let Ok(pid) = s.parse::<i64>() {
                    if !pids.contains(&pid) {
                        pids.push(pid);
                    }
                }
            }
        }
    }
    pids
}

/// Whether an account dir on disk has the core DBs we need (session + contact).
fn account_dir_has_core_dbs(account_dir: &str) -> bool {
    let dbs = list_account_dbs(account_dir);
    dbs.iter().any(|n| n == "session.db") && dbs.iter().any(|n| n == "contact.db")
}

/// Pure helper: pick the most-recently-modified candidate. No I/O.
fn select_newest_candidate(
    mut candidates: Vec<(String, std::time::SystemTime)>,
) -> Option<String> {
    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    candidates.into_iter().next().map(|(name, _)| name)
}

/// Filesystem fallback: scan xwechat_files/* for account dirs that contain the
/// core DBs, returning (account_dir_name, mtime) candidates.
fn filesystem_account_candidates() -> Vec<(String, std::time::SystemTime)> {
    let bases = [
        "/home/wechat/xwechat_files",
        "/home/wechat/Documents/xwechat_files",
    ];
    let mut out: Vec<(String, std::time::SystemTime)> = Vec::new();
    for base in bases {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Account dirs look like wxid_*; skip shared dirs like all_users.
            if !name.starts_with("wxid_") {
                continue;
            }
            if !account_dir_has_core_dbs(&name) {
                continue;
            }
            let db_storage = entry.path().join("db_storage");
            let mtime = std::fs::metadata(&db_storage)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if !out.iter().any(|(n, _)| n == &name) {
                out.push((name, mtime));
            }
        }
    }
    out
}

/// Detect the WeChat account directory (returns the account dir NAME, e.g.
/// `wxid_xxx`). Robust against the case where the main WeChat PID does not hold
/// the DB file descriptors (the root cause of upstream issue #153: the DB fds
/// are held by a helper/renderer process, so the original single-PID scan
/// returned None -> `logged_in_user` was never persisted -> /api/chats empty).
///
/// Strategy (first hit wins):
///   1. pid_fd            — scan the given PID's /proc/<pid>/fd (original behavior).
///   2. related_pid_fd    — scan all WeChat-related PIDs' fds.
///   3. filesystem        — scan xwechat_files/* for an account dir with core DBs;
///                          if multiple, pick the most-recently-modified.
/// Logs are REDACTED: method + candidate_count + selected only (never the wxid).
pub fn find_account_dir(wechat_pid: i64) -> Option<String> {
    // 1. Original: the given PID.
    if let Some(acct) = scan_pid_fd_for_account(wechat_pid) {
        tracing::info!(
            "[account-detect] method=pid_fd candidate_count=1 selected=true account=<redacted>"
        );
        return Some(acct);
    }

    // 2. Fallback: any WeChat-related process may hold the DB fds.
    let related = related_wechat_pids();
    for pid in &related {
        if *pid == wechat_pid {
            continue;
        }
        if let Some(acct) = scan_pid_fd_for_account(*pid) {
            tracing::info!(
                "[account-detect] method=related_pid_fd scanned_pids={} selected=true account=<redacted>",
                related.len()
            );
            return Some(acct);
        }
    }

    // 3. Fallback: filesystem scan.
    let candidates = filesystem_account_candidates();
    let count = candidates.len();
    let selected = select_newest_candidate(candidates);
    tracing::info!(
        "[account-detect] method=filesystem_fallback candidate_count={} selected={} account=<redacted>",
        count,
        selected.is_some()
    );
    if selected.is_none() {
        tracing::warn!(
            "[account-detect] all methods failed (pid_fd + related_pid_fd + filesystem); logged_in_user will not be set"
        );
    }
    selected
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

    // ---- account-dir detection (issue #153 fix) ----
    use super::{account_dir_from_db_path, select_newest_candidate};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn account_dir_from_db_path_extracts_wxid() {
        let p = "/home/wechat/xwechat_files/wxid_abc123/db_storage/session/session.db";
        assert_eq!(account_dir_from_db_path(p), Some("wxid_abc123".to_string()));
    }

    #[test]
    fn account_dir_from_db_path_handles_documents_variant() {
        let p = "/home/wechat/Documents/xwechat_files/wxid_xyz/db_storage/contact/contact.db";
        assert_eq!(account_dir_from_db_path(p), Some("wxid_xyz".to_string()));
    }

    #[test]
    fn account_dir_from_db_path_rejects_unrelated_paths() {
        assert_eq!(account_dir_from_db_path("/proc/61/maps"), None);
        assert_eq!(account_dir_from_db_path("/home/wechat/.pki/nssdb/key4.db"), None);
        // db_storage but not a .db file
        assert_eq!(
            account_dir_from_db_path("/home/wechat/xwechat_files/wxid_a/db_storage/"),
            None
        );
    }

    #[test]
    fn select_newest_candidate_picks_latest_mtime() {
        let older = UNIX_EPOCH + Duration::from_secs(1000);
        let newer = UNIX_EPOCH + Duration::from_secs(2000);
        let candidates = vec![
            ("wxid_old".to_string(), older),
            ("wxid_new".to_string(), newer),
        ];
        assert_eq!(select_newest_candidate(candidates), Some("wxid_new".to_string()));
    }

    #[test]
    fn select_newest_candidate_single_and_empty() {
        assert_eq!(
            select_newest_candidate(vec![("wxid_only".to_string(), UNIX_EPOCH)]),
            Some("wxid_only".to_string())
        );
        assert_eq!(select_newest_candidate(Vec::new()), None);
    }
}
