use wechat_profile::*;
#[test]
fn authenticated_encryption_does_not_mask_an_unopenable_schema_as_a_wrong_password() {
    let temp = tempfile::tempdir().unwrap();
    let storage = temp.path().join("synthetic_account/db_storage");
    std::fs::create_dir_all(&storage).unwrap();
    let file = storage.join("contact.db");
    let password = [42u8; 32];
    let db = rusqlite::Connection::open(&file).unwrap();
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
        0
    );
    db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;CREATE TABLE synthetic(value TEXT);PRAGMA writable_schema=ON;UPDATE sqlite_master SET sql='CREATE TABLE synthetic(' WHERE name='synthetic';").unwrap();
    drop(db);
    let before = std::fs::read(&file).unwrap();
    let location = ProfileLocation::new(storage.parent().unwrap()).unwrap();
    let options = ReadOptions {
        stopped_or_consistent_snapshot: true,
        ..Default::default()
    };
    assert!(matches!(
        keys_from_password(location.clone(), &password, options.clone()),
        Err(ProfileError::DatabaseOpen(relative)) if relative == "contact.db"
    ));
    assert!(matches!(
        keys_from_password(location, &[43; 32], options),
        Err(ProfileError::KeyMismatch(relative)) if relative == "contact.db"
    ));
    assert_eq!(std::fs::read(&file).unwrap(), before);
}
#[test]
fn account_directory_rejects_path_components_before_creating_files() {
    for username in [
        "",
        ".",
        "..",
        "../account",
        "account/child",
        "account\\child",
    ] {
        assert!(official_account_directory(username, 1).is_err());
    }
    assert!(official_account_directory("synthetic.account", 1).is_ok());
    assert!(official_account_directory("synthetic_account", 0).is_err());
}
#[test]
fn fresh_encrypted_layout_reopens_with_password_and_private_keys() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("synthetic_account");
    let (location, keys, report) = initialize_profile(&target, &[42; 32]).unwrap();
    assert_eq!(keys.databases.len(), 9);
    assert!(report.encrypted);
    assert!(!report.official_startup_verified);
    let options = ReadOptions {
        stopped_or_consistent_snapshot: true,
        ..Default::default()
    };
    let reader = ProfileReader::open(location.clone(), keys.clone(), options.clone()).unwrap();
    assert_eq!(
        reader.contacts(&ListQuery::default()).unwrap().items.len(),
        0
    );
    assert_eq!(
        reader.threads(&ListQuery::default()).unwrap().items.len(),
        0
    );
    assert_eq!(reader.groups(&ListQuery::default()).unwrap().items.len(), 0);
    assert_eq!(
        verify_profile_password(&location, &keys, &[42; 32]).unwrap(),
        9
    );
    assert!(matches!(
        verify_profile_password(&location, &keys, &[43; 32]),
        Err(ProfileError::KeyMismatch(_))
    ));
    let derived = keys_from_password(location, &[42; 32], options).unwrap();
    assert_eq!(derived.databases.len(), 9);
    assert!(initialize_profile(&target, &[42; 32]).is_err());
}
#[test]
fn creation_refuses_invalid_password_and_never_overwrites_existing_tree() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("account");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"unchanged").unwrap();
    assert!(initialize_profile(&target, &[8; 32]).is_err());
    assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"unchanged");
    assert!(initialize_profile(d.path().join("new"), &[8; 31]).is_err());
    assert!(!d.path().join("new").exists());
}
#[test]
fn new_full_home_contains_selected_account_and_readable_identity() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("profile");
    std::fs::create_dir(&home).unwrap();
    let input = OfficialHomeBootstrap {
        username: "synthetic_self".into(),
        uin: 7,
        device_id: b"synthetic-device-16".to_vec(),
        global_values: vec![],
    };
    let token = [10, 5, 8, 1, 18, 1, 42, 18, 5, 8, 1, 18, 1, 43];
    let login = SavedLoginProjection {
        auto_auth_key: &token,
        cookie: b"synthetic-cookie",
        key_info: b"synthetic-keyinfo",
        auto_login_flag: 3,
        pc_account_name: Some("synthetic-desktop"),
        local_device_id64: Some(123),
    };
    let (location, keys, report) =
        initialize_official_home_with_login(&home, &input, &[8; 32], &[7; 16], &login).unwrap();
    assert_eq!(location.account_directory(), keys.account_binding);
    assert!(!report.database_password_hydration_verified);
    let data = std::fs::read(home.join(&report.identity_store_relative_path)).unwrap();
    let meta =
        std::fs::read(home.join(format!("{}.crc", report.identity_store_relative_path))).unwrap();
    let got = read_mmkv_identity(&data, &meta, &[7; 16]).unwrap();
    assert_eq!(got.username, input.username);
    assert_eq!(got.uin, 7);
    let saved = read_profile_saved_login(&home, &[7; 16], true).unwrap();
    assert_eq!(saved.auto_auth_key.as_slice(), token);
    assert_eq!(saved.key_info.as_slice(), b"synthetic-keyinfo");
    assert_eq!(saved.cookie.as_slice(), b"synthetic-cookie");
    let options = ReadOptions {
        stopped_or_consistent_snapshot: true,
        ..Default::default()
    };
    assert!(read_account_sync_key(&location, &got, &options)
        .unwrap()
        .is_empty());
    let path = account_sync_store_path(&location, &got).unwrap();
    let data = std::fs::read(&path).unwrap();
    let metadata = std::fs::read(path.with_extension("crc")).unwrap();
    let staged =
        stage_account_sync_key(&data, &metadata, &input.username, b"synthetic-cursor").unwrap();
    let write = WriteOptions {
        exclusive_ownership_confirmed: true,
        durable_intent_id: "synthetic-cursor-commit".into(),
        allow_plaintext: false,
    };
    publish_mmkv_append(&path, &staged, &write).unwrap();
    assert_eq!(
        read_account_sync_key(&location, &got, &options)
            .unwrap()
            .as_slice(),
        b"synthetic-cursor"
    );
    assert!(initialize_official_home(&home, &input, &[8; 32], &[7; 16]).is_err());
}

fn open_keyed(path: &std::path::Path, password: &[u8; 32]) -> rusqlite::Connection {
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
        0
    );
    db.execute_batch(
        "PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;",
    )
    .unwrap();
    db
}
fn journal_mode(path: &std::path::Path, password: &[u8; 32]) -> String {
    open_keyed(path, password)
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap()
}
/// Recreate the layout older daemon builds wrote (rollback journal).
fn downgrade_to_rollback(location: &ProfileLocation, keys: &KeyFile, password: &[u8; 32]) {
    for relative in keys.databases.keys() {
        let db = open_keyed(&location.root().join("db_storage").join(relative), password);
        let mode: String = db
            .query_row("PRAGMA journal_mode=DELETE", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
    }
}
fn write_options() -> WriteOptions {
    WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-wal".into(),
    }
}

#[test]
fn new_profiles_use_wal_with_every_page_in_the_main_file() {
    let d = tempfile::tempdir().unwrap();
    let (location, keys, _) = initialize_profile(d.path().join("account"), &[42; 32]).unwrap();
    for relative in keys.databases.keys() {
        let file = location.root().join("db_storage").join(relative);
        assert_eq!(journal_mode(&file, &[42; 32]), "wal", "{relative}");
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = file.as_os_str().to_owned();
            sidecar.push(suffix);
            // The profile is published as a directory move, so nothing may be
            // left behind in a sidecar.
            assert!(
                !std::path::Path::new(&sidecar).exists()
                    || std::fs::metadata(&sidecar).unwrap().len() == 0,
                "{relative}{suffix}"
            );
        }
    }
    assert_eq!(
        verify_profile_password(&location, &keys, &[42; 32]).unwrap(),
        9
    );
}

#[test]
fn rollback_profiles_convert_once_and_keep_their_data() {
    let d = tempfile::tempdir().unwrap();
    let (location, keys, _) = initialize_profile(d.path().join("account"), &[42; 32]).unwrap();
    downgrade_to_rollback(&location, &keys, &[42; 32]);
    let contact = location
        .root()
        .join("db_storage")
        .join("contact/contact.db");
    open_keyed(&contact, &[42; 32])
        .execute_batch("CREATE TABLE synthetic(value TEXT);INSERT INTO synthetic VALUES('kept')")
        .unwrap();
    let first = convert_profile_to_wal(&location, &keys, &write_options()).unwrap();
    assert_eq!(first.converted.len(), keys.databases.len());
    assert!(first.already_wal.is_empty() && first.skipped.is_empty());
    for relative in keys.databases.keys() {
        let file = location.root().join("db_storage").join(relative);
        assert_eq!(journal_mode(&file, &[42; 32]), "wal");
    }
    let kept: String = open_keyed(&contact, &[42; 32])
        .query_row("SELECT value FROM synthetic", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, "kept");
    let second = convert_profile_to_wal(&location, &keys, &write_options()).unwrap();
    assert!(second.converted.is_empty() && second.skipped.is_empty());
    assert_eq!(second.already_wal.len(), keys.databases.len());
    let mut denied = write_options();
    denied.exclusive_ownership_confirmed = false;
    assert!(convert_profile_to_wal(&location, &keys, &denied).is_err());
}

#[test]
fn conversion_skips_missing_busy_and_wrongly_keyed_databases() {
    let d = tempfile::tempdir().unwrap();
    let (location, keys, _) = initialize_profile(d.path().join("account"), &[42; 32]).unwrap();
    downgrade_to_rollback(&location, &keys, &[42; 32]);
    let storage = location.root().join("db_storage");
    std::fs::remove_file(storage.join("session/session.db")).unwrap();
    let mut wrong = keys.clone();
    wrong.databases.insert(
        "contact/contact.db".into(),
        DatabaseKey::new(
            "contact/contact.db".into(),
            keys.databases["contact/contact.db"].salt,
            [9; 32],
        )
        .unwrap(),
    );
    // Another connection mid-read keeps a shared lock on the rollback database.
    let held_path = storage.join("hardlink/hardlink.db");
    let held = open_keyed(&held_path, &[42; 32]);
    held.execute_batch("BEGIN").unwrap();
    held.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap();
    let report = convert_profile_to_wal(&location, &wrong, &write_options()).unwrap();
    let skipped: std::collections::BTreeMap<_, _> = report.skipped.iter().cloned().collect();
    assert_eq!(skipped["session/session.db"], "missing");
    assert_eq!(skipped["contact/contact.db"], "key_mismatch");
    assert_eq!(skipped["hardlink/hardlink.db"], "busy");
    assert_eq!(report.converted.len(), keys.databases.len() - 3);
    held.execute_batch("COMMIT").unwrap();
    assert_eq!(journal_mode(&held_path, &[42; 32]), "delete");
    let integrity: String = held
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    assert!(!storage.join("session/session.db").exists());
    drop(held);
    // The next pass finishes whatever was skipped and is still readable.
    let mut repaired = keys.clone();
    repaired.databases.remove("session/session.db");
    let next = convert_profile_to_wal(&location, &repaired, &write_options()).unwrap();
    assert_eq!(next.converted.len(), 2);
    assert_eq!(journal_mode(&held_path, &[42; 32]), "wal");
}
