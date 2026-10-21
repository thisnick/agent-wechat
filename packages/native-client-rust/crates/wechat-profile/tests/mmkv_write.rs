use wechat_profile::*;
#[test]
fn append_identity_cursor_unknown_fields_and_tombstones_roundtrip() {
    let k = [9; 16];
    let seed = create_mmkv_v4(&k, [7; 16], 4096).unwrap();
    assert_eq!(seed.data().len(), 4096);
    assert_eq!(seed.metadata().len(), 4096);
    let updates = vec![
        (
            "mmkv_key_user_name".into(),
            Some(mmkv_string_value(b"synthetic_self").unwrap().to_vec()),
        ),
        ("mmkv_key_latest_login_uin".into(), Some(vec![9])),
        (
            "key_wechat_device_id".into(),
            Some(mmkv_string_value(b"synthetic-device-16").unwrap().to_vec()),
        ),
        ("unknown-extension".into(), Some(vec![7, 8, 9])),
    ];
    let next = stage_mmkv_values(seed.data(), seed.metadata(), &k, &updates).unwrap();
    let identity = read_mmkv_identity(next.data(), next.metadata(), &k).unwrap();
    assert_eq!(identity.username, "synthetic_self");
    let sync =
        stage_mmkv_sync_key(next.data(), next.metadata(), &k, b"opaque-synthetic-cursor").unwrap();
    let records = decode_mmkv_records(sync.data(), sync.metadata(), &k).unwrap();
    assert_eq!(records.raw_value("unknown-extension"), Some(&[7, 8, 9][..]));
    assert_eq!(
        records
            .string_value("mmkv_key_sync_key")
            .unwrap()
            .unwrap()
            .as_slice(),
        b"opaque-synthetic-cursor"
    );
    let replay =
        stage_mmkv_sync_key(sync.data(), sync.metadata(), &k, b"opaque-synthetic-cursor").unwrap();
    assert!(replay.changed_keys.is_empty());
    assert_eq!(replay.data(), sync.data());
    assert_eq!(replay.metadata(), sync.metadata());
    let deleted = stage_mmkv_values(
        sync.data(),
        sync.metadata(),
        &k,
        &[("unknown-extension".into(), None)],
    )
    .unwrap();
    assert!(!decode_mmkv_records(deleted.data(), deleted.metadata(), &k)
        .unwrap()
        .contains("unknown-extension"));
    assert_eq!(&sync.metadata()[4..28], &seed.metadata()[4..28]);
    assert_eq!(&sync.metadata()[32..], &seed.metadata()[32..]);
}
#[test]
fn wrong_version_corruption_expiration_bounds_and_duplicate_updates_fail() {
    let k = [8; 16];
    let seed = create_mmkv_v4(&k, [2; 16], 4096).unwrap();
    let mut m = seed.metadata().to_vec();
    m[104] = 1;
    assert!(decode_mmkv_records(seed.data(), &m, &k).is_err());
    let mut d = seed.data().to_vec();
    d[4] ^= 1;
    assert!(decode_mmkv_records(&d, seed.metadata(), &k).is_err());
    assert!(create_mmkv_v4(&k, [2; 16], 4097).is_err());
    assert!(stage_mmkv_values(
        seed.data(),
        seed.metadata(),
        &k,
        &[("a".into(), Some(vec![1; 1024 * 1024 + 1]))]
    )
    .is_err());
    assert!(matches!(
        stage_mmkv_values(
            seed.data(),
            seed.metadata(),
            &k,
            &(0..4)
                .map(|i| (format!("large-{i}"), Some(vec![1; 1024 * 1024])))
                .collect::<Vec<_>>()
        ),
        Err(ProfileError::ResourceLimit)
    ));
    assert!(stage_mmkv_values(
        seed.data(),
        seed.metadata(),
        &k,
        &[("a".into(), Some(vec![1])), ("a".into(), None)]
    )
    .is_err());
    assert!(stage_mmkv_sync_key(seed.data(), seed.metadata(), &k, b"").is_err());
}
#[test]
fn full_store_grows_without_losing_unknown_records_and_recovers_data_first() {
    let key = [9; 16];
    let seed = create_mmkv_v4(&key, [7; 16], 4096).unwrap();
    let old = stage_mmkv_values(
        seed.data(),
        seed.metadata(),
        &key,
        &[("unknown-native-record".into(), Some(vec![6; 3800]))],
    )
    .unwrap();
    let value = vec![8; 600];
    let next = stage_mmkv_values(
        old.data(),
        old.metadata(),
        &key,
        &[("saved-login-refresh".into(), Some(value.clone()))],
    )
    .unwrap();
    assert_eq!(next.data().len(), 8192);
    let old_length = u32::from_le_bytes(old.metadata()[28..32].try_into().unwrap()) as usize;
    assert_eq!(
        &next.data()[..4 + old_length],
        &old.data()[..4 + old_length]
    );
    assert_eq!(&next.metadata()[4..28], &old.metadata()[4..28]);
    assert_eq!(&next.metadata()[32..], &old.metadata()[32..]);
    let old_view = decode_mmkv_records(next.data(), old.metadata(), &key).unwrap();
    assert_eq!(
        old_view.raw_value("unknown-native-record"),
        Some(&[6; 3800][..])
    );
    assert!(!old_view.contains("saved-login-refresh"));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("global_config");
    std::fs::write(&path, old.data()).unwrap();
    std::fs::write(path.with_extension("crc"), old.metadata()).unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "journaled-growth".into(),
    };
    let mut checks = 0;
    assert!(matches!(
        publish_mmkv_append_guarded(&path, &next, &options, || {
            checks += 1;
            checks == 1
        }),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(std::fs::read(&path).unwrap().len(), 8192);
    assert_eq!(
        std::fs::read(path.with_extension("crc")).unwrap(),
        old.metadata()
    );
    let replay = publish_mmkv_append(&path, &next, &options).unwrap();
    assert!(!replay.data_replaced && replay.metadata_replaced);
    let current = decode_mmkv_records(next.data(), next.metadata(), &key).unwrap();
    assert_eq!(
        current.raw_value("unknown-native-record"),
        Some(&[6; 3800][..])
    );
    assert_eq!(
        current.raw_value("saved-login-refresh"),
        Some(value.as_slice())
    );
    let repeated = stage_mmkv_values(
        next.data(),
        next.metadata(),
        &key,
        &[("saved-login-refresh".into(), Some(value))],
    )
    .unwrap();
    assert!(repeated.changed_keys.is_empty());
    assert_eq!(repeated.data(), next.data());
    assert_eq!(repeated.metadata(), next.metadata());
}
#[test]
fn publish_recovers_data_first_interruption_and_rejects_unexpected_generation() {
    let k = [9; 16];
    let seed = create_mmkv_v4(&k, [7; 16], 4096).unwrap();
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("store");
    let m = d.path().join("store.crc");
    std::fs::write(&p, seed.data()).unwrap();
    std::fs::write(&m, seed.metadata()).unwrap();
    let next = stage_mmkv_sync_key(seed.data(), seed.metadata(), &k, b"opaque-cursor").unwrap();
    // Power-loss state after data rename: the old metadata still reads a valid old
    // record view; replay then completes only metadata without duplicate append.
    std::fs::write(&p, next.data()).unwrap();
    assert!(!decode_mmkv_records(next.data(), seed.metadata(), &k)
        .unwrap()
        .contains("mmkv_key_sync_key"));
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "persisted-intent".into(),
    };
    let result = publish_mmkv_append(&p, &next, &options).unwrap();
    assert!(!result.data_replaced && result.metadata_replaced);
    assert!(
        publish_mmkv_append(&p, &next, &options)
            .unwrap()
            .already_applied
    );
    let changed = stage_mmkv_sync_key(next.data(), next.metadata(), &k, b"next-cursor").unwrap();
    let mut unknown = std::fs::read(&m).unwrap();
    unknown[8] ^= 1;
    std::fs::write(&m, unknown).unwrap();
    assert!(matches!(
        publish_mmkv_append(&p, &changed, &options),
        Err(ProfileError::SourceChanged)
    ));
}
#[test]
fn create_pair_is_noclobber_and_recovers_only_its_exact_partial_generation() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("login_configv2");
    let seed = create_mmkv_v4(&[7; 16], [2; 16], 4096).unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        durable_intent_id: "bootstrap-generation".into(),
        allow_plaintext: false,
    };
    std::fs::write(&path, seed.data()).unwrap();
    let got = publish_mmkv_create(&path, &seed, &options).unwrap();
    assert!(!got.data_replaced && got.metadata_replaced);
    assert!(
        publish_mmkv_create(&path, &seed, &options)
            .unwrap()
            .already_applied
    );
    let other = create_mmkv_v4(&[7; 16], [3; 16], 4096).unwrap();
    assert!(matches!(
        publish_mmkv_create(&path, &other, &options),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), seed.data());
    let second = d.path().join("new");
    assert!(
        publish_mmkv_create(&second, &seed, &options)
            .unwrap()
            .data_replaced
    );
    let broken = d.path().join("broken");
    std::fs::write(broken.with_extension("crc"), b"unknown").unwrap();
    assert!(matches!(
        publish_mmkv_create(&broken, &seed, &options),
        Err(ProfileError::SourceChanged)
    ));
    assert!(!broken.exists());
}
#[test]
fn append_authorization_fence_is_checked_before_each_file_and_partial_replays() {
    let key = [9; 16];
    let seed = create_mmkv_v4(&key, [7; 16], 4096).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store");
    let meta = path.with_extension("crc");
    std::fs::write(&path, seed.data()).unwrap();
    std::fs::write(&meta, seed.metadata()).unwrap();
    let next = stage_mmkv_sync_key(seed.data(), seed.metadata(), &key, b"synthetic-next").unwrap();
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-sealed-intent".into(),
    };
    assert!(matches!(
        publish_mmkv_append_guarded(&path, &next, &options, || false),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), seed.data());
    let mut checks = 0;
    assert!(matches!(
        publish_mmkv_append_guarded(&path, &next, &options, || {
            checks += 1;
            checks == 1
        }),
        Err(ProfileError::SourceChanged)
    ));
    assert_eq!(checks, 2);
    assert_eq!(std::fs::read(&path).unwrap(), next.data());
    assert_eq!(std::fs::read(&meta).unwrap(), seed.metadata());
    assert!(!decode_mmkv_records(next.data(), seed.metadata(), &key)
        .unwrap()
        .contains("mmkv_key_sync_key"));
    let replay = publish_mmkv_append_guarded(&path, &next, &options, || true).unwrap();
    assert!(!replay.data_replaced && replay.metadata_replaced);
    assert_eq!(
        decode_mmkv_records(
            &std::fs::read(&path).unwrap(),
            &std::fs::read(&meta).unwrap(),
            &key
        )
        .unwrap()
        .string_value("mmkv_key_sync_key")
        .unwrap()
        .unwrap()
        .as_slice(),
        b"synthetic-next"
    );
}
