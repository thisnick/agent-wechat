use rusqlite::Connection;
use wechat_profile::*;
fn fixture() -> (tempfile::TempDir, ProfileLocation, KeyFile, WriteOptions) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("account");
    std::fs::create_dir_all(root.join("db_storage/emoticon")).unwrap();
    let db = Connection::open(root.join("db_storage/emoticon/emoticon.db")).unwrap();
    db.execute_batch(include_str!("../src/layout/emoticon.db.sql"))
        .unwrap();
    (
        dir,
        ProfileLocation::new(root).unwrap(),
        KeyFile::empty("account"),
        WriteOptions {
            exclusive_ownership_confirmed: true,
            allow_plaintext: true,
            durable_intent_id: "pack-fixture-receipt".into(),
        },
    )
}
fn read(location: &ProfileLocation, keys: &KeyFile) -> ProfileReader {
    ProfileReader::open(
        location.clone(),
        keys.clone(),
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn message_original_is_independent_of_saved_membership_and_pack_cache_removal() {
    let (_dir, location, keys, options) = fixture();
    let archive = wechat_media::decode_sticker_archive(
        include_bytes!("../../wechat-media/fixtures/sticker-pack.zip"),
        "pack.fixture",
        &[
            "2c5f182a5005cfdeec4bd4071fad0e39".into(),
            "3cba29f7d86c1245ca196c29923a28eb".into(),
        ],
    )
    .unwrap();
    let item = &archive.items[0];
    assert!(apply_message_sticker_cache(
        &location,
        7,
        "synthetic_self",
        "2026-11",
        &item.id,
        &item.original,
        &options
    )
    .unwrap());
    assert!(!apply_message_sticker_cache(
        &location,
        7,
        "synthetic_self",
        "2026-11",
        &item.id,
        &item.original,
        &options
    )
    .unwrap());
    apply_remove_sticker_pack_cache(&location, "pack.fixture", &options).unwrap();
    assert_eq!(
        read_message_sticker_cache(&location, 7, "synthetic_self", "2026-11", &item.id).unwrap(),
        item.original
    );
    assert!(read(&location, &keys)
        .saved_stickers()
        .unwrap()
        .items
        .is_empty());
    assert!(apply_message_sticker_cache(
        &location,
        7,
        "synthetic_self",
        "../bad!",
        &item.id,
        &item.original,
        &options
    )
    .is_err());
    assert!(
        read_message_sticker_cache(&location, 8, "synthetic_self", "2026-11", &item.id).is_err()
    );
}
#[test]
fn membership_does_not_reset_existing_content_state_and_removal_preserves_saved_references() {
    let (_dir, location, keys, options) = fixture();
    let old = StickerPackRecord {
        id: "keep".into(),
        name: "Before".into(),
        payment_status: 5,
        download_status: 1,
        install_time: 41,
        remove_time: 42,
        sort_order: 9,
        ..Default::default()
    };
    apply_sticker_pack_membership(&location, &keys, &[old], &[], &options).unwrap();
    let fresh = StickerPackRecord {
        id: "keep".into(),
        name: "Fresh".into(),
        sort_order: 20,
        ..Default::default()
    };
    let new = StickerPackRecord {
        id: "remove".into(),
        name: "New".into(),
        sort_order: 19,
        ..Default::default()
    };
    apply_sticker_pack_membership(&location, &keys, &[fresh, new], &[], &options).unwrap();
    let stored = read(&location, &keys).sticker_pack_record("keep").unwrap();
    assert_eq!(
        (
            stored.name.as_str(),
            stored.payment_status,
            stored.download_status,
            stored.install_time,
            stored.remove_time,
            stored.sort_order
        ),
        ("Before", 5, 1, 41, 42, 9)
    );
    let db = Connection::open(location.root().join("db_storage/emoticon/emoticon.db")).unwrap();
    let id = "00000000000000000000000000000000";
    // Any NonStore type preserves captions; it is not limited to Saved/type3.
    db.execute(
        "INSERT INTO kNonStoreEmoticonTable(type,md5,product_id) VALUES(7,?1,'keep')",
        [id],
    )
    .unwrap();
    db.execute("INSERT INTO kFavEmoticonOrderTable VALUES(?1)", [id])
        .unwrap();
    for pack in ["keep", "remove"] {
        apply_sticker_pack_captions(
            &location,
            &keys,
            pack,
            &[StickerPackCaptionRecord {
                id: id.into(),
                language: "en".into(),
                caption: "Hello".into(),
            }],
            &options,
        )
        .unwrap();
    }
    apply_sticker_pack_membership(
        &location,
        &keys,
        &[],
        &["keep".into(), "remove".into()],
        &options,
    )
    .unwrap();
    assert!(read(&location, &keys)
        .installed_sticker_packs()
        .unwrap()
        .is_empty());
    let count = |sql| db.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(
        count("SELECT count(*) FROM kStoreEmoticonCaptionsTable WHERE package_id_='keep'"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM kStoreEmoticonCaptionsTable WHERE package_id_='remove'"),
        0
    );
    assert_eq!(count("SELECT count(*) FROM kNonStoreEmoticonTable"), 1);
    assert_eq!(count("SELECT count(*) FROM kFavEmoticonOrderTable"), 1);
}
#[test]
fn content_requires_membership_and_updates_only_readiness_with_one_based_ordinals() {
    let (_dir, location, keys, options) = fixture();
    let file = StickerPackFileRecord {
        id: "00000000000000000000000000000000".into(),
        file_type: 2,
        sort_order: 1,
        original_size: 20,
        original_offset: 0,
        thumbnail_size: 10,
        thumbnail_offset: 0,
    };
    assert!(matches!(
        apply_sticker_pack_content(
            &location,
            &keys,
            "pack",
            std::slice::from_ref(&file),
            &options
        ),
        Err(ProfileError::NotCached)
    ));
    let record = StickerPackRecord {
        id: "pack".into(),
        name: "Preserved".into(),
        install_time: 123,
        sort_order: 8,
        ..Default::default()
    };
    apply_sticker_pack_membership(&location, &keys, &[record], &[], &options).unwrap();
    apply_sticker_pack_content(&location, &keys, "pack", &[file], &options).unwrap();
    let reader = read(&location, &keys);
    let pack = reader.sticker_pack_record("pack").unwrap();
    assert_eq!(
        (pack.download_status, pack.install_time, pack.sort_order),
        (1, 123, 8)
    );
    assert_eq!(pack.name, "Preserved");
    let files = reader.sticker_pack_files("pack").unwrap();
    assert_eq!(files[0].sort_order, 1);
    assert_eq!(files[0].original_size, 20);
    let mut denied = options.clone();
    denied.exclusive_ownership_confirmed = false;
    assert!(
        apply_sticker_pack_membership(&location, &keys, &[], &["pack".into()], &denied).is_err()
    );
    assert_eq!(
        read(&location, &keys)
            .sticker_pack_record("pack")
            .unwrap()
            .download_status,
        1
    );
}
#[test]
fn cache_publication_replay_and_cleanup_are_scoped_to_one_product() {
    let (_dir, location, _keys, options) = fixture();
    let bytes = b"synthetic aggregate bytes";
    for pack in ["chosen", "other"] {
        for kind in [
            StickerPackCacheKind::Original,
            StickerPackCacheKind::Thumbnail,
            StickerPackCacheKind::Icon,
        ] {
            assert!(apply_sticker_pack_cache(
                &location,
                42,
                "wxid_sticker_fixture",
                pack,
                kind,
                bytes,
                &options
            )
            .unwrap());
            assert!(!apply_sticker_pack_cache(
                &location,
                42,
                "wxid_sticker_fixture",
                pack,
                kind,
                bytes,
                &options
            )
            .unwrap());
        }
    }
    assert_eq!(
        read_sticker_pack_cache(
            &location,
            42,
            "wxid_sticker_fixture",
            "chosen",
            StickerPackCacheKind::Original
        )
        .unwrap(),
        bytes
    );
    assert!(apply_sticker_pack_cache(
        &location,
        42,
        "wxid_sticker_fixture",
        "chosen",
        StickerPackCacheKind::Original,
        b"different",
        &options
    )
    .is_err());
    apply_remove_sticker_pack_cache(&location, "chosen", &options).unwrap();
    apply_remove_sticker_pack_cache(&location, "chosen", &options).unwrap();
    assert!(matches!(
        read_sticker_pack_cache(
            &location,
            42,
            "wxid_sticker_fixture",
            "chosen",
            StickerPackCacheKind::Original
        ),
        Err(ProfileError::NotCached)
    ));
    assert_eq!(
        read_sticker_pack_cache(
            &location,
            42,
            "wxid_sticker_fixture",
            "other",
            StickerPackCacheKind::Original
        )
        .unwrap(),
        bytes
    );
}
