use wechat_profile::*;
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
    assert_eq!(keys.databases.len(), 7);
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
        7
    );
    assert!(matches!(
        verify_profile_password(&location, &keys, &[43; 32]),
        Err(ProfileError::KeyMismatch(_))
    ));
    let derived = keys_from_password(location, &[42; 32], options).unwrap();
    assert_eq!(derived.databases.len(), 7);
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
