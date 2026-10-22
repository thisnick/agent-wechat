//! Store admission errors must never replace existing account state.
use std::{collections::BTreeMap, fs, path::Path};
use wechat_store::Store;

fn family(path: &Path) -> BTreeMap<String, Vec<u8>> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .filter_map(|suffix| {
            let member = path.with_file_name(format!("{}{suffix}", path.file_name()?.to_str()?));
            member
                .exists()
                .then(|| (suffix.into(), fs::read(member).unwrap()))
        })
        .collect()
}
fn corrupt_family(path: &Path, plausible_header: bool) {
    use std::io::Write;
    if plausible_header {
        let mut bytes = fs::read(path).unwrap();
        assert_eq!(&bytes[..16], b"SQLite format 3\0");
        bytes[100..].fill(0);
        fs::write(path, bytes).unwrap();
    } else {
        fs::write(path, b"Synthetic malformed database; preserve for recovery").unwrap();
    }
    for suffix in ["-wal", "-shm", "-journal"] {
        let member = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap().to_str().unwrap()
        ));
        let mut file = wechat_platform::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(member)
            .unwrap();
        file.write_all(b"Synthetic preserved sidecar").unwrap();
    }
}

#[test]
fn corrupt_registry_is_refused_repeatedly_without_replacing_database_or_sidecars() {
    for plausible_header in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        drop(Store::open(&root).unwrap());
        let path = root.join("registry.sqlite");
        corrupt_family(&path, plausible_header);
        let before = family(&path);
        for _ in 0..2 {
            assert!(Store::open(&root).is_err());
            assert!(family(&path) == before, "original database family changed");
            assert!(!fs::read_dir(&root).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".sqlite-admission-")));
        }
    }
}

#[test]
fn corrupt_account_control_is_refused_without_replacing_database_or_sidecars() {
    for plausible_header in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "preserved", b"state").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        corrupt_family(&path, plausible_header);
        let before = family(&path);
        assert!(store.runtime("one", "preserved").is_err());
        assert!(family(&path) == before, "original database family changed");
        drop(store);
        let store = Store::open_existing(&root).unwrap();
        assert!(store.runtime("one", "preserved").is_err());
        assert!(family(&path) == before, "original database family changed");
    }
}

#[test]
fn failed_account_migration_rolls_back_without_replacing_prior_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let store = Store::open(&root).unwrap();
    store.create_account("one").unwrap();
    store.put_runtime("one", "preserved", b"state").unwrap();
    let path = root.join("accounts/one/agent/control.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    // V004 creates a table before its index. A conflicting retained index
    // makes that migration fail after its first statement has executed.
    db.execute_batch("DROP INDEX pagination_expiry; DROP TABLE pagination_views; CREATE INDEX pagination_expiry ON copy_receipts(manifest_json); PRAGMA user_version=3;").unwrap();
    drop(db);
    let before = family(&path);
    for _ in 0..2 {
        assert!(store.runtime("one", "preserved").is_err());
        assert!(family(&path) == before, "original database family changed");
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(
            db.query_row("SELECT value FROM runtime WHERE key='preserved'", [], |r| r
                .get::<_, Vec<u8>>(0))
                .unwrap(),
            b"state"
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='pagination_views'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}

#[test]
fn protected_validation_keeps_committed_wal_visible_and_refreshes_admission_after_writes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let store = Store::open(&root).unwrap();
    store.create_account("one").unwrap();
    store.put_runtime("one", "initial", b"base").unwrap();
    let path = root.join("accounts/one/agent/control.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    for value in ["first committed value", "second committed value"] {
        db.execute("INSERT INTO runtime VALUES('committed',?1) ON CONFLICT(key)DO UPDATE SET value=excluded.value",[value.as_bytes()]).unwrap();
        let before = family(&path);
        assert_eq!(
            store.runtime("one", "committed").unwrap().unwrap(),
            value.as_bytes()
        );
        let after = family(&path);
        assert_eq!(before[""], after[""]);
        assert_eq!(before["-wal"], after["-wal"]);
    }
}

fn companion(path: &Path, suffix: &str, bytes: &[u8]) {
    use std::io::Write;
    let name = path.with_file_name(format!(
        "{}{suffix}",
        path.file_name().unwrap().to_str().unwrap()
    ));
    let mut file = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(name)
        .unwrap();
    file.write_all(bytes).unwrap();
}
fn recovery_directories(path: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".sqlite-recovery-")
        })
        .collect()
}
#[test]
fn valid_main_with_malformed_wal_or_index_is_refused_without_companion_cleanup() {
    for suffix in ["-wal", "-shm"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "preserved", b"before").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        companion(&path, suffix, b"Malformed nonempty companion");
        let before = family(&path);
        for _ in 0..2 {
            assert!(store.runtime("one", "preserved").is_err());
            assert!(
                family(&path) == before,
                "valid main or unrecognized companion changed"
            );
        }
        assert!(recovery_directories(&path).is_empty());
    }
}
#[test]
fn published_wal_checksum_corruption_is_refused_but_unpublished_tail_is_preserved() {
    for corrupt_published in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "initial", b"base").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        db.execute(
            "INSERT INTO runtime VALUES('committed',?1)",
            [b"committed".as_slice()],
        )
        .unwrap();
        let wal = path.with_file_name("control.sqlite-wal");
        let mut bytes = fs::read(&wal).unwrap();
        if corrupt_published {
            bytes[32 + 24 + 7] ^= 1;
        } else {
            bytes.extend_from_slice(b"Incomplete uncommitted physical tail");
        }
        fs::write(&wal, bytes).unwrap();
        let before = family(&path);
        if corrupt_published {
            assert!(store
                .runtime("one", "committed")
                .unwrap_err()
                .to_string()
                .contains("checksum"));
        } else {
            assert_eq!(
                store.runtime("one", "committed").unwrap().unwrap(),
                b"committed"
            );
        }
        let after = family(&path);
        assert_eq!(after[""], before[""]);
        assert_eq!(after["-wal"], before["-wal"]);
        if corrupt_published {
            assert_eq!(after["-shm"], before["-shm"]);
        }
    }
}
#[test]
fn ignored_rollback_journal_has_a_durable_private_deduplicated_recovery_family() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let store = Store::open(&root).unwrap();
    store.create_account("one").unwrap();
    store.put_runtime("one", "preserved", b"before").unwrap();
    let path = root.join("accounts/one/agent/control.sqlite");
    companion(
        &path,
        "-journal",
        b"Unrecognized rollback journal; preserve original bytes",
    );
    let before = family(&path);
    assert_eq!(
        store.runtime("one", "preserved").unwrap().unwrap(),
        b"before"
    );
    let backups = recovery_directories(&path);
    assert_eq!(backups.len(), 1);
    assert!(wechat_platform::private_path(&backups[0]).unwrap());
    let saved = backups[0].join("database.sqlite");
    assert!(
        family(&saved) == before,
        "recovery family differs from pre-open source"
    );
    for name in fs::read_dir(&backups[0]).unwrap() {
        assert!(wechat_platform::private_path(&name.unwrap().path()).unwrap());
    }
    // Restore the exact ambiguous family to simulate repeated startup recovery.
    // The previous operator backup must be reused without mutation or copies.
    for (suffix, bytes) in &before {
        let name = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap().to_str().unwrap()
        ));
        if name.exists() {
            fs::write(name, bytes).unwrap();
        } else {
            companion(&path, suffix, bytes);
        }
    }
    assert_eq!(
        store.runtime("one", "preserved").unwrap().unwrap(),
        b"before"
    );
    assert_eq!(recovery_directories(&path), backups);
    assert!(family(&saved) == before);
    drop(store);
    let store = Store::open_existing(&root).unwrap();
    assert_eq!(
        store.runtime("one", "preserved").unwrap().unwrap(),
        b"before"
    );
    assert_eq!(recovery_directories(&path), backups);
}

#[test]
fn hot_rollback_fixture_child() {
    let Some(path) = std::env::var_os("AGENT_WECHAT_TEST_HOT_JOURNAL") else {
        return;
    };
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA cache_size=1; BEGIN IMMEDIATE; UPDATE runtime SET value=zeroblob(200000) WHERE key='preserved';").unwrap();
    // Deliberately bypass SQLite destructors in this synthetic child process.
    std::process::exit(0);
}
#[test]
fn hot_rollback_recovery_keeps_committed_state_and_pre_recovery_family() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("data");
    let store = Store::open(&root).unwrap();
    store.create_account("one").unwrap();
    store.put_runtime("one", "preserved", b"before").unwrap();
    let path = root.join("accounts/one/agent/control.sqlite");
    assert!(std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "hot_rollback_fixture_child", "--nocapture"])
        .env("AGENT_WECHAT_TEST_HOT_JOURNAL", &path)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success());
    let before = family(&path);
    assert_eq!(
        &before["-journal"][..8],
        b"\xd9\xd5\x05\xf9\x20\xa1\x63\xd7"
    );
    assert_eq!(
        store.runtime("one", "preserved").unwrap().unwrap(),
        b"before"
    );
    let backups = recovery_directories(&path);
    assert_eq!(backups.len(), 1);
    assert!(family(&backups[0].join("database.sqlite")) == before);
}
