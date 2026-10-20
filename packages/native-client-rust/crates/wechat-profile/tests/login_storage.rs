use md5::{Digest, Md5};
use rusqlite::{params, Connection};
use std::fs;
use wechat_profile::*;
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    let global = root.join("xwechat_files/all_users/config");
    fs::create_dir_all(&global).unwrap();
    let seed = create_mmkv_v4(&[7; 16], [3; 16], 4096).unwrap();
    let token = vec![10, 5, 8, 1, 18, 1, 42, 18, 5, 8, 1, 18, 1, 43];
    let values = vec![
        (
            "mmkv_key_user_name".into(),
            Some(mmkv_string_value(b"synthetic_self").unwrap().to_vec()),
        ),
        (
            "key_wechat_device_id".into(),
            Some(mmkv_string_value(b"synthetic-device-16").unwrap().to_vec()),
        ),
        ("mmkv_key_latest_login_uin".into(), Some(vec![7])),
        (
            "mmkv_key_auto_auth_key".into(),
            Some(mmkv_string_value(&token).unwrap().to_vec()),
        ),
        ("mmkv_key_auto_login_flag".into(), Some(vec![3])),
        ("mmkv_key_logout_locked_flag".into(), Some(vec![1])),
    ];
    let pair = stage_mmkv_values(seed.data(), seed.metadata(), &[7; 16], &values).unwrap();
    fs::write(global.join("global_config"), pair.data()).unwrap();
    fs::write(global.join("global_config.crc"), pair.metadata()).unwrap();
    let legacy = root.join(".xwechat/login/synthetic_self");
    fs::create_dir_all(&legacy).unwrap();
    fs::write(legacy.join("key_info.dat"), b"synthetic legacy key info").unwrap();
    d
}
#[test]
fn stopped_saved_login_retains_locked_state_and_uses_native_legacy_fallback() {
    let d = fixture();
    assert!(matches!(
        read_profile_saved_login(d.path(), &[7; 16], false),
        Err(ProfileError::SnapshotRequired)
    ));
    let got = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(got.identity.username, "synthetic_self");
    assert_eq!(got.identity.uin, 7);
    assert_eq!(got.auto_login_flag, 3);
    assert_eq!(got.logout_locked, Some(true));
    assert_eq!(got.key_info_source, KeyInfoSource::LegacyFallback);
    assert_eq!(got.key_info.as_slice(), b"synthetic legacy key info");
    assert!(got.cookie.is_empty());
    assert!(got.pc_account_name.is_none());
    assert!(read_profile_saved_login(d.path(), &[8; 16], true).is_err());
}
#[test]
fn newest_database_row_is_used_or_falls_back_without_selecting_an_older_row() {
    let d = fixture();
    let folder = d
        .path()
        .join("xwechat_files/all_users/login/synthetic_self");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("key_info.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);PRAGMA journal_mode=WAL;").unwrap();
    let username = format!("{:x}", Md5::digest(b"synthetic_self"));
    let bytes = b"synthetic newest material";
    let hash = format!("{:x}", Md5::digest(bytes));
    c.execute(
        "INSERT INTO LoginKeyInfoTable VALUES(?1,'opaque',?2,?3)",
        params![username, hash, bytes.as_slice()],
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let got = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(got.key_info_source, KeyInfoSource::LatestDatabaseRow);
    assert_eq!(got.key_info.as_slice(), bytes);
    assert_eq!(before, fs::read(&path).unwrap());
    c.execute(
        "INSERT INTO LoginKeyInfoTable VALUES(?1,'opaque','wrong',x'0102')",
        [username],
    )
    .unwrap();
    let got = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(got.key_info_source, KeyInfoSource::LegacyFallback);
    assert_eq!(got.key_info.as_slice(), b"synthetic legacy key info");
}
#[test]
fn saved_login_generation_and_legacy_publication_are_bound_and_retry_safe() {
    use sha2::Sha256;
    let d = fixture();
    let global = d
        .path()
        .join("xwechat_files/all_users/config/global_config");
    let data = fs::read(&global).unwrap();
    let metadata = fs::read(global.with_extension("crc")).unwrap();
    let token = [10, 5, 8, 1, 18, 1, 44, 18, 5, 8, 1, 18, 1, 45];
    let login = SavedLoginProjection {
        auto_auth_key: &token,
        cookie: b"synthetic cookie",
        key_info: b"new key info",
        auto_login_flag: 3,
        pc_account_name: None,
        local_device_id64: None,
    };
    assert!(stage_saved_login_projection(&data, &metadata, &[7; 16], "other", &login).is_err());
    let staged =
        stage_saved_login_projection(&data, &metadata, &[7; 16], "synthetic_self", &login).unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        durable_intent_id: "saved-generation".into(),
        allow_plaintext: false,
    };
    publish_mmkv_append(&global, &staged, &options).unwrap();
    let path = d.path().join(".xwechat/login/synthetic_self/key_info.dat");
    assert!(matches!(
        publish_legacy_key_info(&path, login.key_info, None, &options),
        Err(ProfileError::SourceChanged)
    ));
    let hash = hex::encode(Sha256::digest(fs::read(&path).unwrap()));
    assert!(publish_legacy_key_info(&path, login.key_info, Some(&hash), &options).unwrap());
    assert!(!publish_legacy_key_info(&path, login.key_info, Some(&hash), &options).unwrap());
    let got = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(got.auto_auth_key.as_slice(), token);
    assert_eq!(got.key_info.as_slice(), login.key_info);
    assert_eq!(got.logout_locked, Some(true));
}
#[test]
fn known_keyinfo_database_and_legacy_publish_native_rows_with_cas_and_replay() {
    let d = fixture();
    let folder = d
        .path()
        .join("xwechat_files/all_users/login/synthetic_self");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("key_info.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);CREATE UNIQUE INDEX LoginKeyInfoTable_USER_KEYINFO ON LoginKeyInfoTable(user_name_md5,key_info_md5);PRAGMA journal_mode=WAL;").unwrap();
    let user = format!("{:x}", Md5::digest(b"synthetic_self"));
    for bytes in [b"synthetic older".as_slice(), b"synthetic latest"] {
        c.execute(
            "INSERT INTO LoginKeyInfoTable VALUES(?1,'',?2,?3)",
            params![user, format!("{:x}", Md5::digest(bytes)), bytes],
        )
        .unwrap();
    }
    c.execute(
        "INSERT INTO LoginKeyInfoTable VALUES('unrelated','opaque','unrelated',x'0708')",
        [],
    )
    .unwrap();
    let old = c
        .prepare("SELECT rowid,key_md5,key_info_data FROM LoginKeyInfoTable ORDER BY rowid")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    let inputs = [
        path.clone(),
        folder.join("key_info.db-wal"),
        folder.join("key_info.db-shm"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    .map(|p| {
        let b = fs::read(&p).unwrap();
        (p, b)
    })
    .collect::<Vec<_>>();
    let plan = plan_saved_key_info(d.path(), "synthetic_self", true).unwrap();
    for (p, b) in &inputs {
        assert_eq!(*b, fs::read(p).unwrap());
    }
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-accepted-keyinfo".into(),
    };
    let bytes = b"synthetic renewed key info";
    let got = publish_saved_key_info(d.path(), "synthetic_self", &plan, bytes, &options).unwrap();
    assert!(got.database_changed && got.legacy_changed);
    let got = publish_saved_key_info(d.path(), "synthetic_self", &plan, bytes, &options).unwrap();
    assert!(!got.database_changed && !got.legacy_changed);
    let selected = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(selected.key_info_source, KeyInfoSource::LatestDatabaseRow);
    assert_eq!(selected.key_info.as_slice(), bytes);
    for (row, key, data) in old {
        assert_eq!(
            c.query_row(
                "SELECT key_md5,key_info_data FROM LoginKeyInfoTable WHERE rowid=?1",
                [row],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            )
            .unwrap(),
            (key, data)
        );
    }
    assert_eq!(c.query_row("SELECT key_md5 FROM LoginKeyInfoTable WHERE user_name_md5=?1 ORDER BY rowid DESC LIMIT 1",[user.clone()],|r|r.get::<_,String>(0)).unwrap(),"");
    let next = plan_saved_key_info(d.path(), "synthetic_self", true).unwrap();
    c.execute(
        "INSERT INTO LoginKeyInfoTable VALUES(?1,'','external',x'090a')",
        [user],
    )
    .unwrap();
    assert!(matches!(
        publish_saved_key_info(
            d.path(),
            "synthetic_self",
            &next,
            b"later own credential",
            &options
        ),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(
        fs::read(d.path().join(".xwechat/login/synthetic_self/key_info.dat")).unwrap(),
        bytes
    );
}
#[test]
fn keyinfo_publication_refuses_unknown_schemas_and_unrelated_legacy_generation() {
    let d = fixture();
    let folder = d
        .path()
        .join("xwechat_files/all_users/login/synthetic_self");
    fs::create_dir_all(&folder).unwrap();
    let path = folder.join("key_info.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);CREATE UNIQUE INDEX LoginKeyInfoTable_USER_KEYINFO ON LoginKeyInfoTable(user_name_md5,key_info_md5);").unwrap();
    let plan = plan_saved_key_info(d.path(), "synthetic_self", true).unwrap();
    fs::write(
        d.path().join(".xwechat/login/synthetic_self/key_info.dat"),
        b"unexpected legacy generation",
    )
    .unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-rejected".into(),
    };
    assert!(matches!(
        publish_saved_key_info(d.path(), "synthetic_self", &plan, b"new key info", &options),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(
        c.query_row("SELECT count(*) FROM LoginKeyInfoTable", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    c.execute_batch(
        "CREATE TRIGGER unknown_login_effect AFTER INSERT ON LoginKeyInfoTable BEGIN SELECT 1;END;",
    )
    .unwrap();
    assert!(matches!(
        plan_saved_key_info(d.path(), "synthetic_self", true),
        Err(ProfileError::UnsupportedSchema(_))
    ));
    assert!(plan_saved_key_info(d.path(), "..", true).is_err());
}
#[test]
fn accepted_login_unlock_is_explicit_and_keeps_auto_login_policy_and_unknowns() {
    let d = fixture();
    let p = d
        .path()
        .join("xwechat_files/all_users/config/global_config");
    let data = fs::read(&p).unwrap();
    let meta = fs::read(p.with_extension("crc")).unwrap();
    let base = stage_mmkv_values(
        &data,
        &meta,
        &[7; 16],
        &[
            ("mmkv_key_auto_login_flag".into(), Some(vec![0])),
            ("unknown".into(), Some(vec![7, 8])),
        ],
    )
    .unwrap();
    let token = [10, 5, 8, 1, 18, 1, 44, 18, 5, 8, 1, 18, 1, 45];
    let input = SavedLoginProjection {
        auto_auth_key: &token,
        cookie: b"cookie",
        key_info: b"key info",
        auto_login_flag: saved_login_policy(base.data(), base.metadata(), &[7; 16]).unwrap(),
        pc_account_name: None,
        local_device_id64: None,
    };
    let staged = stage_accepted_saved_login_projection(
        base.data(),
        base.metadata(),
        &[7; 16],
        "synthetic_self",
        &input,
    )
    .unwrap();
    let records = decode_mmkv_records(staged.data(), staged.metadata(), &[7; 16]).unwrap();
    assert_eq!(
        records.raw_value("mmkv_key_logout_locked_flag"),
        Some(&[0][..])
    );
    assert_eq!(
        records.raw_value("mmkv_key_auto_login_flag"),
        Some(&[0][..])
    );
    assert_eq!(records.raw_value("unknown"), Some(&[7, 8][..]));
}
#[test]
fn keyinfo_guard_stops_between_database_and_legacy_and_exact_intent_replays() {
    let d = fixture();
    let folder = d
        .path()
        .join("xwechat_files/all_users/login/synthetic_self");
    fs::create_dir_all(&folder).unwrap();
    let db = Connection::open(folder.join("key_info.db")).unwrap();
    db.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);CREATE UNIQUE INDEX LoginKeyInfoTable_USER_KEYINFO ON LoginKeyInfoTable(user_name_md5,key_info_md5);").unwrap();
    let plan = plan_saved_key_info(d.path(), "synthetic_self", true).unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-fenced-journal".into(),
    };
    let bytes = b"synthetic accepted target";
    let legacy = d.path().join(".xwechat/login/synthetic_self/key_info.dat");
    let before = fs::read(&legacy).unwrap();
    let mut checks = 0;
    assert!(matches!(
        publish_saved_key_info_guarded(d.path(), "synthetic_self", &plan, bytes, &options, || {
            checks += 1;
            checks == 1
        }),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(checks, 2);
    assert_eq!(fs::read(&legacy).unwrap(), before);
    let got = read_profile_saved_login(d.path(), &[7; 16], true).unwrap();
    assert_eq!(got.key_info.as_slice(), bytes);
    assert_eq!(got.key_info_source, KeyInfoSource::LatestDatabaseRow);
    let replay =
        publish_saved_key_info(d.path(), "synthetic_self", &plan, bytes, &options).unwrap();
    assert!(!replay.database_changed && replay.legacy_changed);
    assert_eq!(fs::read(&legacy).unwrap(), bytes);
}

#[test]
fn keyinfo_non_native_unique_collation_is_not_write_authority() {
    let d = fixture();
    let folder = d
        .path()
        .join("xwechat_files/all_users/login/synthetic_self");
    fs::create_dir_all(&folder).unwrap();
    let c = Connection::open(folder.join("key_info.db")).unwrap();
    c.execute_batch("CREATE TABLE LoginKeyInfoTable(user_name_md5 TEXT,key_md5 TEXT,key_info_md5 TEXT,key_info_data BLOB);CREATE UNIQUE INDEX LoginKeyInfoTable_USER_KEYINFO ON LoginKeyInfoTable(user_name_md5 COLLATE NOCASE,key_info_md5);").unwrap();
    assert!(matches!(
        plan_saved_key_info(d.path(), "synthetic_self", true),
        Err(ProfileError::UnsupportedSchema(_))
    ));
    assert_eq!(
        c.query_row("SELECT count(*) FROM LoginKeyInfoTable", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
