use rusqlite::Connection;
use wechat_profile::{KeyFile, ProfileLocation, ProfileReader, ReadOptions};

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let storage = root.path().join("account/db_storage/emoticon");
    std::fs::create_dir_all(&storage).unwrap();
    let db = Connection::open(storage.join("emoticon.db")).unwrap();
    db.execute_batch("CREATE TABLE kFavEmoticonOrderTable(md5 TEXT UNIQUE);
      CREATE TABLE kNonStoreEmoticonTable(type INTEGER,md5 TEXT,caption TEXT,cdn_url TEXT);
      CREATE TABLE kStoreEmoticonPackageTable(package_id_ TEXT,package_name_ TEXT,download_status_ INTEGER,sort_order_ INTEGER);
      CREATE TABLE kStoreEmoticonFilesTable(package_id_ TEXT,md5_ TEXT,sort_order_ INTEGER);
      CREATE TABLE kStoreEmoticonCaptionsTable(package_id_ TEXT,md5_ TEXT,language_ TEXT,caption_ TEXT);
      INSERT INTO kFavEmoticonOrderTable VALUES('11111111111111111111111111111111'),('22222222222222222222222222222222');
      INSERT INTO kNonStoreEmoticonTable VALUES(3,'11111111111111111111111111111111','','https://example.invalid/private'),(3,'22222222222222222222222222222222','Hello',NULL);
      INSERT INTO kStoreEmoticonPackageTable VALUES('not-in-feed','Installed',NULL,0);
      INSERT INTO kStoreEmoticonFilesTable VALUES('not-in-feed','11111111111111111111111111111111',2),('not-in-feed','22222222222222222222222222222222',1);
      INSERT INTO kStoreEmoticonCaptionsTable VALUES('not-in-feed','22222222222222222222222222222222','en','Hello'),('not-in-feed','22222222222222222222222222222222','zh','你好');").unwrap();
    root
}
fn reader(root: &tempfile::TempDir) -> ProfileReader {
    ProfileReader::open(
        ProfileLocation::new(root.path().join("account")).unwrap(),
        KeyFile::empty("account"),
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap()
}
#[test]
fn reads_official_order_installed_membership_and_localized_captions_without_transport_secrets() {
    let root = fixture();
    let file = root.path().join("account/db_storage/emoticon/emoticon.db");
    let before = std::fs::read(&file).unwrap();
    let read = reader(&root);
    let saved = read.saved_stickers().unwrap();
    assert_eq!(saved.items[0].id, "22222222222222222222222222222222");
    assert_eq!(saved.items[1].caption, None);
    assert_eq!(saved.revision, read.saved_stickers().unwrap().revision);
    let json = serde_json::to_string(&saved).unwrap();
    assert!(!json.contains("private"));
    assert_eq!(read.installed_sticker_packs().unwrap()[0].id, "not-in-feed");
    let items = read.installed_pack_stickers("not-in-feed").unwrap();
    assert_eq!(items[0].id, saved.items[0].id);
    assert_eq!(items[0].captions.len(), 2);
    assert!(read.installed_pack_stickers("missing").unwrap().is_empty());
    assert_eq!(std::fs::read(file).unwrap(), before);
}
#[test]
fn malformed_saved_identity_is_not_returned_as_an_opaque_usable_sticker() {
    let root = fixture();
    Connection::open(root.path().join("account/db_storage/emoticon/emoticon.db"))
        .unwrap()
        .execute(
            "INSERT INTO kFavEmoticonOrderTable VALUES('not-an-md5')",
            [],
        )
        .unwrap();
    assert!(reader(&root).saved_stickers().is_err());
}
#[test]
fn revision_changes_with_order_and_membership() {
    let root = fixture();
    let first = reader(&root).saved_stickers().unwrap().revision;
    let db = Connection::open(root.path().join("account/db_storage/emoticon/emoticon.db")).unwrap();
    db.execute(
        "DELETE FROM kFavEmoticonOrderTable WHERE md5=?1",
        ["11111111111111111111111111111111"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO kFavEmoticonOrderTable VALUES(?1)",
        ["11111111111111111111111111111111"],
    )
    .unwrap();
    let second = reader(&root).saved_stickers().unwrap().revision;
    assert_ne!(first, second);
    db.execute("DELETE FROM kFavEmoticonOrderTable", [])
        .unwrap();
    assert_ne!(second, reader(&root).saved_stickers().unwrap().revision);
}

fn writer_fixture() -> tempfile::TempDir {
    let root = fixture();
    let db = Connection::open(root.path().join("account/db_storage/emoticon/emoticon.db")).unwrap();
    db.execute_batch("DROP TABLE kNonStoreEmoticonTable;
      CREATE TABLE kNonStoreEmoticonTable(type INTEGER,md5 TEXT,caption TEXT,product_id TEXT,aes_key TEXT,thumb_url TEXT,tp_url TEXT,auth_key TEXT,cdn_url TEXT,extern_url TEXT,extern_md5 TEXT,encrypt_url TEXT,designer_id TEXT,activity_id TEXT);
      CREATE UNIQUE INDEX kNonStoreEmoticonTable_TYPE_MD5 ON kNonStoreEmoticonTable(type,md5);
      INSERT INTO kNonStoreEmoticonTable(type,md5,caption) VALUES(3,'11111111111111111111111111111111','Old'),(7,'11111111111111111111111111111111','Other type'),(3,'33333333333333333333333333333333','Orphan');").unwrap();
    root
}
#[test]
fn reconciliation_replays_newest_first_and_only_prunes_supplied_type_three_keys() {
    use wechat_profile::{
        apply_saved_stickers, SavedStickerProjection, SavedStickerRecord, WriteOptions,
    };
    let root = writer_fixture();
    let location = ProfileLocation::new(root.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: true,
        durable_intent_id: "synthetic-receipt".into(),
    };
    let projection = SavedStickerProjection {
        order: vec![
            "44444444444444444444444444444444".into(),
            "22222222222222222222222222222222".into(),
        ],
        removed_metadata: vec!["11111111111111111111111111111111".into()],
        records: vec![SavedStickerRecord {
            id: "44444444444444444444444444444444".into(),
            cdn_url: "https://example.invalid/original".into(),
            ..Default::default()
        }],
    };
    apply_saved_stickers(&location, &keys, &projection, &options).unwrap();
    let first = reader(&root).saved_stickers().unwrap();
    assert_eq!(
        first.items.iter().map(|item| &item.id).collect::<Vec<_>>(),
        projection.order.iter().collect::<Vec<_>>()
    );
    apply_saved_stickers(&location, &keys, &projection, &options).unwrap();
    assert_eq!(
        reader(&root).saved_stickers().unwrap().revision,
        first.revision
    );
    let db = Connection::open(root.path().join("account/db_storage/emoticon/emoticon.db")).unwrap();
    let types: Vec<i64> = db
        .prepare(
            "SELECT type FROM kNonStoreEmoticonTable WHERE md5='11111111111111111111111111111111'",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(types, vec![7]);
    assert_eq!(db.query_row("SELECT caption FROM kNonStoreEmoticonTable WHERE md5='33333333333333333333333333333333'",[],|row|row.get::<_,String>(0)).unwrap(),"Orphan");
}
#[test]
fn unknown_sticker_schema_or_missing_ownership_never_changes_order() {
    use wechat_profile::{apply_saved_stickers, SavedStickerProjection, WriteOptions};
    let root = writer_fixture();
    let location = ProfileLocation::new(root.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let mut options = WriteOptions {
        exclusive_ownership_confirmed: false,
        allow_plaintext: true,
        durable_intent_id: "receipt".into(),
    };
    let projection = SavedStickerProjection {
        order: vec![],
        removed_metadata: vec![],
        records: vec![],
    };
    let before = reader(&root).saved_stickers().unwrap().revision;
    assert!(apply_saved_stickers(&location, &keys, &projection, &options).is_err());
    options.exclusive_ownership_confirmed = true;
    let db = Connection::open(root.path().join("account/db_storage/emoticon/emoticon.db")).unwrap();
    db.execute_batch("ALTER TABLE kNonStoreEmoticonTable ADD COLUMN future TEXT")
        .unwrap();
    assert!(apply_saved_stickers(&location, &keys, &projection, &options).is_err());
    assert_eq!(reader(&root).saved_stickers().unwrap().revision, before);
}

#[test]
fn missing_subsystem_initialization_preserves_password_and_refuses_replacement() {
    use wechat_profile::{
        apply_initialize_emoticon, initialize_profile, verify_profile_password, WriteOptions,
    };
    let root = tempfile::tempdir().unwrap();
    let (location, mut keys, _) =
        initialize_profile(root.path().join("account"), &[42; 32]).unwrap();
    let path = location.root().join("db_storage/emoticon/emoticon.db");
    std::fs::remove_file(&path).unwrap();
    keys.databases.remove("emoticon/emoticon.db");
    let options = WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: false,
        durable_intent_id: "synthetic-init".into(),
    };
    let key = apply_initialize_emoticon(&location, &keys, &[42; 32], &options).unwrap();
    keys.databases.insert("emoticon/emoticon.db".into(), key);
    assert_eq!(
        verify_profile_password(&location, &keys, &[42; 32]).unwrap(),
        8
    );
    let before = std::fs::read(&path).unwrap();
    assert!(apply_initialize_emoticon(&location, &keys, &[42; 32], &options).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let reader = ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(reader.saved_stickers().unwrap().items.is_empty());
    assert!(reader.installed_sticker_packs().unwrap().is_empty());
}
