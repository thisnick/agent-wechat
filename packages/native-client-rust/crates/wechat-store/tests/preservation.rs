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

#[test]
fn empty_wal_with_sqlite_rebuilt_index_preserves_committed_main_state() {
    for incomplete_tail in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "preserved", b"base").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; INSERT INTO runtime VALUES('committed',x'01');")
            .unwrap();
        let header = fs::read(path.with_file_name("control.sqlite-wal")).unwrap()[..32].to_vec();
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .unwrap();
        drop(db);
        // A valid WAL header without frames contains no committed page data.
        // Let SQLite itself rebuild its empty shared index, rather than
        // fabricating that index or its checksum in this fixture.
        companion(&path, "-wal", &header);
        let reader = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert_eq!(
            reader
                .query_row("SELECT value FROM runtime WHERE key='preserved'", [], |r| r
                    .get::<_, Vec<u8>>(0))
                .unwrap(),
            b"base"
        );
        drop(reader);
        if incomplete_tail {
            use std::io::Write;
            fs::OpenOptions::new()
                .append(true)
                .open(path.with_file_name("control.sqlite-wal"))
                .unwrap()
                .write_all(b"Incomplete frame")
                .unwrap();
        }
        let before = family(&path);
        assert_eq!(&before["-shm"][16..20], &[0; 4]);
        if incomplete_tail {
            assert!(store.runtime("one", "preserved").is_err());
            assert!(family(&path) == before);
        } else {
            assert_eq!(store.runtime("one", "preserved").unwrap().unwrap(), b"base");
            assert_eq!(store.runtime("one", "committed").unwrap().unwrap(), [1]);
            let backups = recovery_directories(&path);
            assert_eq!(backups.len(), 1);
            assert!(family(&backups[0].join("database.sqlite")) == before);
        }
    }
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

/// Log and WAL-index states that SQLite writes across one WAL restart. The
/// main file is the same in both: the restart follows a complete checkpoint,
/// and the second generation is only in the log.
struct Generations {
    database: Vec<u8>,
    first_wal: Vec<u8>,
    first_shm: Vec<u8>,
    second_wal: Vec<u8>,
    second_shm: Vec<u8>,
}
fn wal_salt(wal: &[u8]) -> &[u8] {
    &wal[16..24]
}
/// Salts of each physical frame, in log order.
fn frame_salts(wal: &[u8]) -> Vec<&[u8]> {
    let size = u32::from_be_bytes(wal[8..12].try_into().unwrap()) as usize + 24;
    wal[32..]
        .chunks_exact(size)
        .map(|frame| &frame[8..16])
        .collect()
}
fn generations(control: &Path) -> Generations {
    let scratch = tempfile::tempdir().unwrap();
    let path = scratch.path().join("control.sqlite");
    fs::copy(control, &path).unwrap();
    let member = |suffix: &str| fs::read(format!("{}{suffix}", path.display())).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    db.execute(
        "INSERT INTO runtime VALUES('generation',?1)",
        [vec![b'1'; 200_000]],
    )
    .unwrap();
    let (first_wal, first_shm) = (member("-wal"), member("-shm"));
    let (busy, log, done): (i64, i64, i64) = db
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(
        (busy, log),
        (0, done),
        "first generation fully checkpointed"
    );
    // The next writer restarts the log at frame 1 without truncating it.
    db.execute(
        "UPDATE runtime SET value=?1 WHERE key='generation'",
        [b"second".as_slice()],
    )
    .unwrap();
    let generations = Generations {
        database: member(""),
        first_wal,
        first_shm,
        second_wal: member("-wal"),
        second_shm: member("-shm"),
    };
    drop(db);
    let second = &generations.second_wal;
    assert_ne!(wal_salt(&generations.first_wal), wal_salt(second));
    let current = frame_salts(second)
        .iter()
        .take_while(|salt| **salt == wal_salt(second))
        .count();
    assert!(current >= 2, "second generation spans several frames");
    assert!(
        current < frame_salts(second).len(),
        "first-generation frames remain after the second generation"
    );
    generations
}
/// The index header SQLite publishes when it restarts the log: the next
/// generation's salts with no frames, before that generation's log header is
/// written. Both header copies carry a native-order checksum.
fn restarted_index(next: &[u8]) -> Vec<u8> {
    let mut index = next.to_vec();
    index[16..20].fill(0);
    let (mut s0, mut s1) = (0u32, 0u32);
    for pair in index[..40].chunks_exact(8) {
        let word = |bytes: &[u8]| u32::from_ne_bytes(bytes[..4].try_into().unwrap());
        s0 = s0.wrapping_add(word(pair)).wrapping_add(s1);
        s1 = s1.wrapping_add(word(&pair[4..])).wrapping_add(s0);
    }
    index[40..44].copy_from_slice(&s0.to_ne_bytes());
    index[44..48].copy_from_slice(&s1.to_ne_bytes());
    index.copy_within(..48, 48);
    index
}
fn install_family(path: &Path, database: &[u8], wal: &[u8], shm: &[u8]) {
    fs::write(path, database).unwrap();
    companion(path, "-wal", wal);
    companion(path, "-shm", shm);
}

#[test]
fn wal_index_from_another_log_generation_is_rebuilt_from_a_verified_log() {
    type Case = fn(&Generations) -> (&[u8], Vec<u8>, &'static [u8]);
    let cases: [(&str, Case); 3] = [
        // A crash left an index saved before the log restarted.
        ("earlier index", |g| {
            (&g.second_wal, g.first_shm.clone(), b"second")
        }),
        // A crash between the index restart and the new log header.
        ("restarted index", |g| {
            (
                &g.first_wal,
                restarted_index(&g.second_shm),
                &[b'1'; 200_000],
            )
        }),
        // A crash before the first commit of the restarted log was published.
        ("unpublished generation", |g| {
            (&g.second_wal, restarted_index(&g.second_shm), b"second")
        }),
    ];
    for (name, case) in cases {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "initial", b"base").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        let generations = generations(&path);
        let (wal, shm, expected) = case(&generations);
        install_family(&path, &generations.database, wal, &shm);
        let before = family(&path);
        assert_eq!(
            store.runtime("one", "generation").unwrap().as_deref(),
            Some(expected),
            "{name}"
        );
        let backups = recovery_directories(&path);
        assert_eq!(backups.len(), 1, "{name}");
        assert!(
            family(&backups[0].join("database.sqlite")) == before,
            "{name}: recovery family differs from pre-open source"
        );
    }
}

#[test]
fn wal_log_missing_indexed_commits_or_damaged_is_refused_unchanged() {
    type Case = fn(&Generations) -> (Vec<u8>, &[u8], &'static str);
    let cases: [(&str, Case); 2] = [
        // The index published second-generation commits; the log never
        // received that generation's header.
        ("newer index", |g| {
            (g.first_wal.clone(), &g.second_shm, "newer log generation")
        }),
        // A second-generation frame follows a frame that no longer matches
        // it, so part of that generation was damaged.
        ("damaged generation", |g| {
            let mut wal = g.second_wal.clone();
            wal[32 + 8] ^= 1;
            (wal, &g.first_shm, "frame identity")
        }),
    ];
    for (name, case) in cases {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("data");
        let store = Store::open(&root).unwrap();
        store.create_account("one").unwrap();
        store.put_runtime("one", "initial", b"base").unwrap();
        let path = root.join("accounts/one/agent/control.sqlite");
        let generations = generations(&path);
        let (wal, shm, message) = case(&generations);
        install_family(&path, &generations.database, &wal, shm);
        let before = family(&path);
        for _ in 0..2 {
            let error = store.runtime("one", "generation").unwrap_err().to_string();
            assert!(error.contains(message), "{name}: {error}");
            // The message names the database so an operator knows what to back up.
            assert!(
                error.starts_with("cannot open ") && error.contains("control.sqlite"),
                "{name}: {error}"
            );
            assert!(family(&path) == before, "{name}: family changed");
        }
        assert!(recovery_directories(&path).is_empty(), "{name}");
    }
}
