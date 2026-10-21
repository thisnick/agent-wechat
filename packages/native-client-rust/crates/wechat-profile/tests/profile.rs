use base64::{engine::general_purpose::STANDARD, Engine};
use md5::{Digest, Md5};
use rusqlite::{params, Connection};
use std::{fs, path::Path};
use wechat_profile::*;

fn options() -> ReadOptions {
    ReadOptions {
        stopped_or_consistent_snapshot: true,
        allow_plaintext: true,
        ..Default::default()
    }
}
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("account/db_storage/message")).unwrap();
    fs::create_dir_all(d.path().join("account/db_storage/contact")).unwrap();
    fs::create_dir_all(d.path().join("account/db_storage/session")).unwrap();
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch("CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,nick_name TEXT,remark TEXT,alias TEXT,local_type INTEGER); CREATE TABLE chat_room(id INTEGER PRIMARY KEY,username TEXT,owner TEXT,ext_buffer BLOB); CREATE TABLE chatroom_member(room_id INTEGER,member_id INTEGER); CREATE TABLE chat_room_info_detail(room_id_ INTEGER PRIMARY KEY,username_ TEXT,announcement_ TEXT);
 INSERT INTO contact VALUES(1,'self','Me','','',1),(2,'peer','Peer','Work','',1),(3,'room@chatroom','Room','','',2); INSERT INTO chat_room VALUES(3,'room@chatroom','peer',X'0A0C0A04706565721204526F6F6D'); INSERT INTO chatroom_member VALUES(3,2); INSERT INTO chat_room_info_detail VALUES(3,'room@chatroom','Notice');").unwrap();
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    c.execute_batch("CREATE TABLE SessionTable(username TEXT PRIMARY KEY,unread_count INTEGER,is_hidden INTEGER,sort_timestamp INTEGER,last_timestamp INTEGER); INSERT INTO SessionTable VALUES('peer',2,0,100,100),('room@chatroom',0,0,100,90);").unwrap();
    for index in 0..2 {
        let p = d
            .path()
            .join(format!("account/db_storage/message/message_{index}.db"));
        let c = Connection::open(p).unwrap();
        c.execute_batch("CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY,is_session INTEGER);")
            .unwrap();
        if index == 0 {
            c.execute_batch("INSERT INTO Name2Id VALUES('self',1),('peer',1)")
                .unwrap();
        } else {
            c.execute_batch("INSERT INTO Name2Id VALUES('peer',1),('self',1)")
                .unwrap();
        }
        let t = format!("Msg_{:x}", Md5::digest(b"peer"));
        c.execute_batch(&format!("CREATE TABLE {t}(local_id INTEGER PRIMARY KEY AUTOINCREMENT,server_id INTEGER,local_type INTEGER,sort_seq INTEGER,real_sender_id INTEGER,create_time INTEGER,status INTEGER,upload_status INTEGER,download_status INTEGER,server_seq INTEGER,origin_source INTEGER,source TEXT,message_content TEXT,compress_content TEXT,packed_info_data BLOB,WCDB_CT_message_content INTEGER DEFAULT NULL,WCDB_CT_source INTEGER DEFAULT NULL);")).unwrap();
        for local in 1..=3 {
            c.execute(&format!("INSERT INTO {t}(server_id,local_type,sort_seq,real_sender_id,create_time,source,message_content,WCDB_CT_message_content,WCDB_CT_source)VALUES(?1,1,?2,?3,100,'metadata',?4,0,0)"),params![if local==1 {-1i64}else{index*100+local},1000+local,if index==0 {2}else{1},format!("source {index} row {local}")]).unwrap();
        }
    }
    d
}
fn reader(d: &Path) -> ProfileReader {
    ProfileReader::open(
        ProfileLocation::new(d.join("account")).unwrap(),
        KeyFile::empty("account"),
        options(),
    )
    .unwrap()
}
#[test]
fn empty_next_ordinal_does_not_hide_history_but_missing_message_namespace_is_explicit() {
    let d = fixture();
    let path = d.path().join("account/db_storage/message/message_2.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE db_info(Key TEXT PRIMARY KEY,ValueInt64 INTEGER,ValueDouble REAL,ValueStdStr TEXT,ValueBlob BLOB)").unwrap();
    drop(c);
    let r = reader(d.path());
    let p = r
        .messages(&MessageQuery {
            chat: Some("peer".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(p.items.len(), 6);
    assert!(r
        .schemas()
        .iter()
        .any(|s| s.database == "message/message_2.db"));
    drop(r);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("CREATE TABLE Msg_0123456789abcdef0123456789abcdef(local_id INTEGER)")
        .unwrap();
    drop(c);
    let r = reader(d.path());
    assert!(matches!(
        r.messages(&MessageQuery::default()),
        Err(ProfileError::UnsupportedSchema(_))
    ));
}
#[test]
fn discovers_all_shards_and_uses_each_name_map() {
    let d = fixture();
    let r = reader(d.path());
    let p = r
        .messages(&MessageQuery {
            chat: Some("peer".into()),
            own_username: Some("self".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(p.items.len(), 6);
    assert!(p
        .items
        .iter()
        .all(|v| v.sender.as_deref() == Some("peer") && v.outgoing == Some(false)));
    assert_eq!(
        p.items
            .iter()
            .filter(|v| v.server_message_id.as_deref() == Some("18446744073709551615"))
            .count(),
        2
    );
    assert_ne!(p.items[0].id, p.items[3].id);
    assert_eq!(p.items[0].server_id_signed.as_deref(), Some("103"));
}
#[test]
fn cursor_pages_equal_timestamps_without_skips_across_shards() {
    let d = fixture();
    let r = reader(d.path());
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let p = r
            .messages(&MessageQuery {
                chat: Some("peer".into()),
                limit: Some(2),
                cursor,
                ..Default::default()
            })
            .unwrap();
        ids.extend(p.items.into_iter().map(|m| m.id));
        cursor = p.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 6);
    let unique: std::collections::BTreeSet<_> = ids.into_iter().collect();
    assert_eq!(unique.len(), 6);
}
#[test]
fn cursor_rejects_query_mix_and_changed_generation() {
    let d = fixture();
    let r = reader(d.path());
    let p = r
        .contacts(&ListQuery {
            limit: Some(1),
            ..Default::default()
        })
        .unwrap();
    let cursor = p.next_cursor.unwrap();
    assert!(matches!(
        r.threads(&ListQuery {
            cursor: Some(cursor.clone()),
            ..Default::default()
        }),
        Err(ProfileError::InvalidCursor)
    ));
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute("UPDATE contact SET remark='Changed' WHERE id=2", [])
        .unwrap();
    let next = reader(d.path());
    assert!(matches!(
        next.contacts(&ListQuery {
            cursor: Some(cursor),
            ..Default::default()
        }),
        Err(ProfileError::CursorExpired)
    ));
}
#[test]
fn contacts_threads_groups_and_room_aliases() {
    let d = fixture();
    let r = reader(d.path());
    let p = r.contacts(&ListQuery::default()).unwrap();
    assert_eq!(p.items.len(), 3);
    assert_eq!(p.items[0].label, "Work");
    let t = r.threads(&ListQuery::default()).unwrap();
    assert_eq!(t.items.len(), 2);
    assert_eq!(t.items[0].label, "Work");
    assert_eq!(t.items[0].unread_count, Some(2));
    let g = r.groups(&ListQuery::default()).unwrap();
    assert_eq!(g.items.len(), 1);
    assert_eq!(g.items[0].announcement.as_deref(), Some("Notice"));
    let m = r
        .group_members("room@chatroom", &ListQuery::default())
        .unwrap();
    assert_eq!(m.items.len(), 1);
    assert_eq!(m.items[0].label, "Work");
    assert_eq!(m.items[0].room_display_name.as_deref(), Some("Room"));
}
#[test]
fn read_requires_quiescence_and_plaintext_opt_in() {
    let d = fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    assert!(matches!(
        ProfileReader::open(
            loc.clone(),
            KeyFile::empty("account"),
            ReadOptions::default()
        ),
        Err(ProfileError::SnapshotRequired)
    ));
    let r = ProfileReader::open(
        loc,
        KeyFile::empty("account"),
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(r.coverage().partial);
    assert!(matches!(
        r.contacts(&ListQuery::default()),
        Err(ProfileError::KeyUnavailable(_))
    ));
}
#[test]
fn read_leaves_main_wal_shm_bytes_unchanged() {
    let d = fixture();
    let p = d.path().join("account/db_storage/contact/contact.db");
    let c = Connection::open(&p).unwrap();
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; UPDATE contact SET remark='Committed WAL' WHERE id=2;").unwrap();
    let files = [
        p.clone(),
        p.with_file_name("contact.db-wal"),
        p.with_file_name("contact.db-shm"),
    ];
    let before: Vec<_> = files
        .iter()
        .map(fs::read)
        .collect::<std::io::Result<_>>()
        .unwrap();
    let r = reader(d.path());
    let contacts = r.contacts(&ListQuery::default()).unwrap();
    assert_eq!(contacts.items[0].label, "Committed WAL");
    let after: Vec<_> = files
        .iter()
        .map(fs::read)
        .collect::<std::io::Result<_>>()
        .unwrap();
    assert_eq!(before, after);
}
#[test]
fn lossless_raw_fields_and_bounded_compression() {
    let d = fixture();
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let t = format!("Msg_{:x}", Md5::digest(b"peer"));
    let encoded = zstd::stream::encode_all("Hello 🙂".as_bytes(), 3).unwrap();
    c.execute(
        &format!("UPDATE {t} SET message_content=?1,WCDB_CT_message_content=4 WHERE local_id=3"),
        [encoded],
    )
    .unwrap();
    c.execute_batch(&format!(
        "UPDATE {t} SET source=CAST(X'FFFE' AS TEXT) WHERE local_id=2;"
    ))
    .unwrap();
    let r = reader(d.path());
    let p = r
        .messages(&MessageQuery {
            chat: Some("peer".into()),
            ..Default::default()
        })
        .unwrap();
    let compressed = p
        .items
        .iter()
        .find(|m| m.id.database.ends_with("message_0.db") && m.id.local_id == "3")
        .unwrap();
    assert_eq!(compressed.content.text.as_deref(), Some("Hello 🙂"));
    assert_eq!(compressed.content.state, DecodeState::Zstd);
    let invalid = p
        .items
        .iter()
        .find(|m| m.id.database.ends_with("message_0.db") && m.id.local_id == "2")
        .unwrap();
    assert_eq!(invalid.source.state, DecodeState::InvalidUtf8);
    assert!(matches!(
        invalid.raw_fields.get("source"),
        Some(SqlValue::TextBytes(_))
    ));
}
#[test]
fn expansion_bomb_is_reported_without_losing_original() {
    let d = fixture();
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let t = format!("Msg_{:x}", Md5::digest(b"peer"));
    let encoded = zstd::stream::encode_all(vec![b'x'; 128 * 1024].as_slice(), 3).unwrap();
    c.execute(
        &format!("UPDATE {t} SET message_content=?1,WCDB_CT_message_content=4 WHERE local_id=3"),
        [encoded],
    )
    .unwrap();
    let r = ProfileReader::open(
        ProfileLocation::new(d.path().join("account")).unwrap(),
        KeyFile::empty("account"),
        ReadOptions {
            max_blob_bytes: 8192,
            ..options()
        },
    )
    .unwrap();
    let p = r.messages(&MessageQuery::default()).unwrap();
    let m = p
        .items
        .iter()
        .find(|m| m.id.database.ends_with("message_0.db") && m.id.local_id == "3")
        .unwrap();
    assert_eq!(m.content.state, DecodeState::InvalidCompressedData);
    assert!(m.content.text.is_none());
    assert!(matches!(
        m.raw_fields.get("message_content"),
        Some(SqlValue::Blob(_))
    ));
}
#[test]
fn schema_fingerprints_detect_drift_and_never_authorize_generic_writes() {
    let d = fixture();
    let a = reader(d.path());
    let before = a
        .schemas()
        .iter()
        .find(|s| s.database.ends_with("contact.db"))
        .unwrap();
    assert_eq!(before.user_version, 0);
    assert!(matches!(
        before.require_writable(),
        Err(ProfileError::WritesUnsupported)
    ));
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch("ALTER TABLE contact ADD COLUMN future_field TEXT;")
        .unwrap();
    let b = reader(d.path());
    let after = b
        .schemas()
        .iter()
        .find(|s| s.database.ends_with("contact.db"))
        .unwrap();
    assert_ne!(before.fingerprint, after.fingerprint);
    assert!(after.supports("contacts"));
    assert!(!after.writes_supported);
}
#[cfg(unix)]
#[test]
fn refuses_source_symlink_and_world_readable_keys() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let d = fixture();
    symlink(
        "contact.db",
        d.path().join("account/db_storage/contact/alias.db"),
    )
    .unwrap();
    assert!(matches!(
        ProfileReader::open(
            ProfileLocation::new(d.path().join("account")).unwrap(),
            KeyFile::empty("account"),
            options()
        ),
        Err(ProfileError::UnsupportedFile)
    ));
    let p = d.path().join("keys.json");
    fs::write(&p, b"{}").unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        KeyFile::load(p),
        Err(ProfileError::InsecureKeysFile)
    ));
}
#[test]
fn key_file_is_versioned_account_bound_private_and_redacted() {
    let d = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut key = KeyFile::empty("account");
    key.databases.insert(
        "message/message_0.db".into(),
        DatabaseKey::new("message/message_0.db".into(), [3u8; 16], [7u8; 32]).unwrap(),
    );
    key.image_dat = Some(ImageDatKey::new([9u8; 16], 0xab));
    let p = d.path().join("keys.json");
    key.write_new(&p).unwrap();
    assert!(key.write_new(&p).is_err());
    let parsed = KeyFile::load(&p).unwrap();
    assert_eq!(parsed.account_binding, "account");
    let debug = format!("{parsed:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains(&STANDARD.encode([7u8; 32])));
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    json["schemaVersion"] = 2.into();
    assert!(matches!(
        KeyFile::from_json(&serde_json::to_vec(&json).unwrap()),
        Err(ProfileError::InvalidKeysFile)
    ));
}
#[test]
fn sqlcipher_real_key_open_and_wrong_salt_fail_closed() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("account/db_storage/contact");
    fs::create_dir_all(&root).unwrap();
    let p = root.join("contact.db");
    let raw = [0x42u8; 32];
    let c = Connection::open(&p).unwrap();
    c.execute_batch(&format!("PRAGMA key=\"x'{}'\"; PRAGMA cipher_compatibility=4; CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,nick_name TEXT,remark TEXT,alias TEXT,local_type INTEGER); INSERT INTO contact VALUES(1,'peer','Name','','',1);",hex::encode(raw))).unwrap();
    drop(c);
    let file = fs::read(&p).unwrap();
    assert_ne!(&file[..16], b"SQLite format 3\0");
    let salt: [u8; 16] = file[..16].try_into().unwrap();
    let mut keys = KeyFile::empty("account");
    keys.databases.insert(
        "contact/contact.db".into(),
        DatabaseKey::new("contact/contact.db".into(), salt, raw).unwrap(),
    );
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let r = ProfileReader::open(
        loc.clone(),
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.contacts(&ListQuery::default()).unwrap().items.len(), 1);
    let mut keys = KeyFile::empty("account");
    keys.databases.insert(
        "contact/contact.db".into(),
        DatabaseKey::new("contact/contact.db".into(), [0u8; 16], raw).unwrap(),
    );
    let r = ProfileReader::open(
        loc,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.inventory()[0].state, DatabaseState::KeyMismatch);
    assert!(matches!(
        r.contacts(&ListQuery::default()),
        Err(ProfileError::KeyMismatch(_))
    ));
}
#[test]
fn dat_round_trips_boundaries_and_preserves_layout() {
    let key = ImageDatKey::new([0x31; 16], 0x6d);
    for size in [1024, 1025, 1040, 65536, 1024 * 1024 + 2000] {
        let plain: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let encoded = encode_dat_v2(&plain, &key, None).unwrap();
        assert_eq!(encoded.len(), size + 31);
        let (decoded, layout) = decode_dat_v2(&encoded, &key).unwrap();
        assert_eq!(decoded, plain);
        assert_eq!(
            encode_dat_v2(&decoded, &key, Some(layout)).unwrap(),
            encoded
        );
    }
}
#[test]
fn dat_rejects_unsupported_and_malformed_partitions() {
    let key = ImageDatKey::new([0x31; 16], 0x6d);
    assert!(encode_dat_v2(&[0; 32], &key, None).is_err());
    let encoded = encode_dat_v2(&[0; 1024], &key, None).unwrap();
    for i in [0, 14] {
        let mut bad = encoded.clone();
        bad[i] ^= 1;
        assert!(decode_dat_v2(&bad, &key).is_err());
    }
    let mut bad = encoded.clone();
    bad[10..14].copy_from_slice(&1024u32.to_le_bytes());
    assert!(decode_dat_v2(&bad, &key).is_err());
    let mut bad = encoded;
    *bad.last_mut().unwrap() ^= 1;
    assert!(decode_dat_v2(&bad, &key).is_err());
}

fn writable_fixture() -> tempfile::TempDir {
    let d = fixture();
    fs::remove_file(d.path().join("account/db_storage/message/message_1.db")).unwrap();
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let t = format!("Msg_{:x}", Md5::digest(b"peer"));
    for (suffix, cols) in [
        ("SERVERID", "server_id"),
        ("SORTSEQ", "sort_seq"),
        ("TYPE_SEQ", "local_type,sort_seq"),
        ("SENDERID", "real_sender_id"),
    ] {
        c.execute_batch(&format!("CREATE INDEX {t}_{suffix} ON {t}({cols});"))
            .unwrap();
    }
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    c.execute_batch("DROP TABLE SessionTable; CREATE TABLE SessionTable(username TEXT PRIMARY KEY,type INTEGER,unread_count INTEGER,unread_first_msg_srv_id INTEGER,unread_first_pat_msg_local_id INTEGER,unread_first_pat_msg_sort_seq INTEGER,is_hidden INTEGER,summary TEXT,draft TEXT,status INTEGER,last_timestamp INTEGER,sort_timestamp INTEGER,last_clear_unread_timestamp INTEGER,last_msg_locald_id INTEGER,last_msg_type INTEGER,last_msg_sub_type INTEGER,last_msg_sender TEXT,last_sender_display_name TEXT,last_msg_ext_type INTEGER);
 CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY); INSERT INTO Name2Id VALUES('peer');
 CREATE TABLE SessionUnreadListTable_1(username_id INTEGER,server_id INTEGER,local_id INTEGER,create_time INTEGER, CONSTRAINT _NameId_ServerId PRIMARY KEY(username_id,server_id));
 INSERT INTO SessionTable VALUES('peer',1,0,0,0,0,0,'Before','Keep draft',7,100,100,99,3,1,0,'','',0);").unwrap();
    d
}
fn incoming() -> IncomingText {
    IncomingText {
        peer: "peer".into(),
        own_username: "self".into(),
        server_message_id: "18446744073709551500".into(),
        server_sequence: 12,
        create_time: 101,
        content: "New incoming 🙂".into(),
        source_xml: "<msgsource/>".into(),
        unread: true,
    }
}
fn write_options() -> WriteOptions {
    WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: true,
        durable_intent_id: "synthetic-durable-intent".into(),
    }
}
#[test]
fn narrow_text_writer_allocates_preserves_and_retries_without_duplicates() {
    let d = writable_fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let a = apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    assert!(a.message_inserted && a.session_projected);
    assert_eq!(a.source_id.local_id, "4");
    assert!(!a.cross_database_atomic);
    let b = apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    assert!(!b.message_inserted);
    assert_eq!(a.source_id, b.source_id);
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    let row=c.query_row("SELECT unread_count,draft,status,last_clear_unread_timestamp,unread_first_msg_srv_id FROM SessionTable WHERE username='peer'",[],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?))).unwrap();
    assert_eq!(row, (1, "Keep draft".into(), 7, 99, -116));
    let count: i64 = c
        .query_row("SELECT count(*) FROM SessionUnreadListTable_1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let r = reader(d.path());
    let messages = r.messages(&MessageQuery::default()).unwrap();
    let new = messages
        .items
        .iter()
        .find(|m| m.id.local_id == "4")
        .unwrap();
    assert_eq!(new.content.text.as_deref(), Some("New incoming 🙂"));
    assert_eq!(new.source.text.as_deref(), Some("<msgsource/>"));
    assert_eq!(new.source.state, DecodeState::Zstd);
}
#[test]
fn narrow_writer_recovers_message_commit_before_session_projection() {
    let d = writable_fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    // Recreate the exact state at the separate message/session transaction boundary.
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    c.execute_batch("DELETE FROM SessionUnreadListTable_1; UPDATE SessionTable SET unread_count=0,unread_first_msg_srv_id=0,summary='Before',last_timestamp=100,sort_timestamp=100,last_msg_locald_id=3;").unwrap();
    let a = apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    assert!(!a.message_inserted && a.session_projected);
    let count: i64 = c
        .query_row("SELECT unread_count FROM SessionTable", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn narrow_writer_refuses_collision_schema_drift_groups_and_uncertain_ownership() {
    let d = writable_fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let mut input = incoming();
    input.peer = "room@chatroom".into();
    assert!(matches!(
        apply_incoming_text(&loc, &keys, &input, &write_options()),
        Err(ProfileError::WritesUnsupported)
    ));
    let mut opts = write_options();
    opts.exclusive_ownership_confirmed = false;
    assert!(matches!(
        apply_incoming_text(&loc, &keys, &incoming(), &opts),
        Err(ProfileError::SnapshotRequired)
    ));
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let t = format!("Msg_{:x}", Md5::digest(b"peer"));
    c.execute(
        &format!("UPDATE {t} SET sort_seq=101000 WHERE local_id=3"),
        [],
    )
    .unwrap();
    assert!(matches!(
        apply_incoming_text(&loc, &keys, &incoming(), &write_options()),
        Err(ProfileError::WritesUnsupported)
    ));
    c.execute(
        &format!("UPDATE {t} SET sort_seq=1003 WHERE local_id=3"),
        [],
    )
    .unwrap();
    c.execute_batch(&format!("ALTER TABLE {t} ADD COLUMN future_column TEXT;"))
        .unwrap();
    assert!(matches!(
        apply_incoming_text(&loc, &keys, &incoming(), &write_options()),
        Err(ProfileError::UnsupportedSchema(_))
    ));
}
#[test]
fn projection_retry_never_resurrects_an_officially_read_message() {
    let d = writable_fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    c.execute_batch("DELETE FROM SessionUnreadListTable_1; UPDATE SessionTable SET unread_count=0,unread_first_msg_srv_id=0,last_clear_unread_timestamp=102;").unwrap();
    let a = apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    assert!(!a.message_inserted);
    let count: i64 = c
        .query_row("SELECT unread_count FROM SessionTable", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[test]
fn exact_read_projection_preserves_other_arrivals_and_counter_discrepancies() {
    let d = writable_fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let first = apply_incoming_text(&loc, &keys, &incoming(), &write_options()).unwrap();
    let mut second = incoming();
    second.server_message_id = "123456".into();
    second.create_time = 102;
    let second = apply_incoming_text(&loc, &keys, &second, &write_options()).unwrap();
    let result = mark_read_messages(
        &loc,
        &keys,
        "peer",
        std::slice::from_ref(&first.source_id),
        &write_options(),
    )
    .unwrap();
    assert_eq!(
        (
            result.removed_unread_rows,
            result.unread_count_before,
            result.unread_count_after
        ),
        (1, 2, 1)
    );
    let retry =
        mark_read_messages(&loc, &keys, "peer", &[first.source_id], &write_options()).unwrap();
    assert_eq!(retry.removed_unread_rows, 0);
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    let remaining: i64 = c
        .query_row("SELECT server_id FROM SessionUnreadListTable_1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 123456);
    // Preserve a pre-existing count/list exception instead of converting list size
    // into unread_count or treating every row as unprocessed model input.
    c.execute_batch("UPDATE SessionTable SET unread_count=0,unread_first_msg_srv_id=0;")
        .unwrap();
    let result =
        mark_read_messages(&loc, &keys, "peer", &[second.source_id], &write_options()).unwrap();
    assert_eq!(
        (result.unread_count_before, result.unread_count_after),
        (0, 0)
    );
}
#[test]
fn cached_image_requires_unique_binding_and_original_content_proof() {
    let d = fixture();
    let key = ImageDatKey::new([0x51; 16], 0x3b);
    let mut png = std::io::Cursor::new(Vec::new());
    let im = image::DynamicImage::new_rgb8(32, 32);
    im.write_to(&mut png, image::ImageFormat::Png).unwrap();
    let mut bytes = png.into_inner();
    while bytes.len() < 1024 {
        bytes.push(0);
    }
    let hash = format!("{:x}", Md5::digest(&bytes));
    let base = "0123456789abcdef0123456789abcdef";
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let table = format!("Msg_{:x}", Md5::digest(b"peer"));
    c.execute(
        &format!("UPDATE {table} SET local_type=3,message_content=?1 WHERE local_id=3"),
        [format!("<msg><img md5=\"{hash}\"/></msg>")],
    )
    .unwrap();
    fs::create_dir_all(d.path().join("account/db_storage/hardlink")).unwrap();
    let h = Connection::open(d.path().join("account/db_storage/hardlink/hardlink.db")).unwrap();
    h.execute_batch("CREATE TABLE dir2id(username TEXT PRIMARY KEY);CREATE TABLE image_hardlink_info_v4(md5 TEXT,file_name TEXT,dir1 INTEGER,dir2 INTEGER);INSERT INTO dir2id VALUES('dirone'),('dirtwo');").unwrap();
    h.execute(
        "INSERT INTO image_hardlink_info_v4 VALUES(?1,?2,1,2)",
        params![hash, format!("{base}_h.dat")],
    )
    .unwrap();
    let images = d.path().join("account/msg/attach/dirone/dirtwo/Img");
    fs::create_dir_all(&images).unwrap();
    fs::write(
        images.join(format!("{base}_h.dat")),
        encode_dat_v2(&bytes, &key, None).unwrap(),
    )
    .unwrap();
    let mut keys = KeyFile::empty("account");
    keys.image_dat = Some(key);
    let r = ProfileReader::open(
        ProfileLocation::new(d.path().join("account")).unwrap(),
        keys,
        options(),
    )
    .unwrap();
    let id = SourceMessageId {
        database: "message/message_0.db".into(),
        table,
        local_id: "3".into(),
    };
    let got = r.cached_image(&id, ImageQuality::Original).unwrap();
    assert_eq!(got.media.bytes, bytes);
    assert_eq!(got.quality, VerifiedImageQuality::Original);
    assert_eq!((got.width, got.height), (32, 32));
    assert!(matches!(
        r.cached_image(&id, ImageQuality::Standard),
        Err(ProfileError::ImageQualityUnavailable)
    ));
}
#[test]
fn cache_key_export_is_offline_account_scoped_and_never_initializes_agent_database() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("account/db_storage/contact");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("contact.db");
    let raw = [0x45u8; 32];
    let c = Connection::open(&path).unwrap();
    c.execute_batch(&format!("PRAGMA key=\"x'{}'\"; PRAGMA cipher_compatibility=4; CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,nick_name TEXT,remark TEXT,alias TEXT,local_type INTEGER); INSERT INTO contact VALUES(1,'peer','Name','','',1);",hex::encode(raw))).unwrap();
    drop(c);
    let agent = d.path().join("agent.db");
    let c = Connection::open(&agent).unwrap();
    c.execute_batch("PRAGMA key='synthetic-agent-passphrase'; CREATE TABLE wechat_keys(session_id TEXT,account_dir TEXT,db_name TEXT,hex_key TEXT,verified_at TEXT);").unwrap();
    c.execute(
        "INSERT INTO wechat_keys VALUES('session','account','contact.db',?1,'synthetic')",
        [hex::encode(raw)],
    )
    .unwrap();
    drop(c);
    let before = fs::read(&agent).unwrap();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = keys_from_agent_cache(
        loc.clone(),
        &agent,
        b"synthetic-agent-passphrase",
        None,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(keys.databases.len(), 1);
    assert_eq!(before, fs::read(&agent).unwrap());
    assert!(keys_from_agent_cache(
        loc,
        &agent,
        b"wrong-passphrase",
        None,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        }
    )
    .is_err());
    assert_eq!(before, fs::read(&agent).unwrap());
}
#[test]
fn malformed_room_protobuf_fails_without_false_membership() {
    let d = fixture();
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch("UPDATE chat_room SET ext_buffer=X'0AFF' WHERE id=3;")
        .unwrap();
    let r = reader(d.path());
    assert!(matches!(
        r.group_members("room@chatroom", &ListQuery::default()),
        Err(ProfileError::InvalidMetadata)
    ));
}
#[test]
fn without_rowid_schema_primary_key_index_is_supported_read_only() {
    let d = fixture();
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch(
        "CREATE TABLE unknown_without_rowid(key TEXT PRIMARY KEY,value BLOB) WITHOUT ROWID;",
    )
    .unwrap();
    let r = reader(d.path());
    assert!(r
        .schemas()
        .iter()
        .any(|s| s.tables.iter().any(|t| t.name == "unknown_without_rowid")));
    assert_eq!(r.contacts(&ListQuery::default()).unwrap().items.len(), 3);
}
#[test]
fn direct_message_lookup_retains_database_namespace_and_checks_ids() {
    let d = fixture();
    let r = reader(d.path());
    let id = SourceMessageId {
        database: "message/message_1.db".into(),
        table: format!("Msg_{:x}", Md5::digest(b"peer")),
        local_id: "1".into(),
    };
    let m = r.message(&id, Some("self")).unwrap();
    assert_eq!(m.id, id);
    assert_eq!(m.sender.as_deref(), Some("peer"));
    assert_eq!(m.peer.as_deref(), Some("peer"));
    assert_eq!(m.server_message_id.as_deref(), Some("18446744073709551615"));
    let bad = SourceMessageId {
        local_id: "01".into(),
        ..id
    };
    assert!(matches!(
        r.message(&bad, None),
        Err(ProfileError::InvalidQuery)
    ));
}
#[test]
fn supplied_account_password_derives_each_file_salt_and_exports_no_password() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("account/db_storage/contact");
    fs::create_dir_all(&root).unwrap();
    let password = [0x23u8; 32];
    let salt = [0x71u8; 16];
    let mut raw = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha512>(&password, &salt, 256000, &mut raw);
    let c = Connection::open(root.join("contact.db")).unwrap();
    c.execute_batch(&format!("PRAGMA key=\"x'{}'\"; PRAGMA cipher_compatibility=4; PRAGMA cipher_salt=\"x'{}'\"; CREATE TABLE contact(id INTEGER PRIMARY KEY,username TEXT,nick_name TEXT,remark TEXT,alias TEXT,local_type INTEGER); INSERT INTO contact VALUES(1,'peer','Name','','',1);",hex::encode(raw),hex::encode(salt))).unwrap();
    drop(c);
    assert_eq!(&fs::read(root.join("contact.db")).unwrap()[..16], salt);
    let location = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = keys_from_password(
        location.clone(),
        &password,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(keys.databases.len(), 1);
    let r = ProfileReader::open(
        location,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.contacts(&ListQuery::default()).unwrap().items.len(), 1);
}
#[test]
fn non_finite_sqlite_real_is_retained_as_a_typed_string() {
    let d = fixture();
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch("ALTER TABLE contact ADD COLUMN extra_real REAL;UPDATE contact SET extra_real=1e999 WHERE id=2;").unwrap();
    let r = reader(d.path());
    let p = r.contacts(&ListQuery::default()).unwrap();
    let contact = p.items.iter().find(|v| v.username == "peer").unwrap();
    assert_eq!(
        contact.raw_fields.get("extra_real"),
        Some(&SqlValue::Real("inf".into()))
    );
    let encoded = serde_json::to_value(contact).unwrap();
    assert_eq!(encoded["rawFields"]["extra_real"]["value"], "inf");
}
#[test]
fn voice_payload_uses_media_shard_names_and_avatar_requires_no_dat_key() {
    let d = fixture();
    let m = Connection::open(d.path().join("account/db_storage/message/media_2.db")).unwrap();
    m.execute_batch("CREATE TABLE Name2Id(user_name TEXT PRIMARY KEY);INSERT INTO Name2Id VALUES('other'),('peer');CREATE TABLE VoiceInfo(chat_name_id INTEGER,create_time INTEGER,local_id INTEGER,svr_id INTEGER,voice_data BLOB,data_index TEXT DEFAULT '0');INSERT INTO VoiceInfo VALUES(2,100,3,-1,X'02232153494C4B', '0');").unwrap();
    fs::create_dir_all(d.path().join("account/db_storage/head_image")).unwrap();
    let h = Connection::open(d.path().join("account/db_storage/head_image/head_image.db")).unwrap();
    h.execute_batch("CREATE TABLE head_image(username TEXT PRIMARY KEY,md5 TEXT,image_buffer BLOB,update_time INTEGER);").unwrap();
    let mut image = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut image, image::ImageFormat::Png)
        .unwrap();
    let png = image.into_inner();
    h.execute(
        "INSERT INTO head_image VALUES('peer','url-cache-identity',?1,100)",
        [&png],
    )
    .unwrap();
    let r = reader(d.path());
    let voice = r
        .cached_voice(
            "message/media_2.db",
            "peer",
            "3",
            Some("18446744073709551615"),
        )
        .unwrap();
    assert_eq!(voice.bytes, b"\x02#!SILK");
    assert_eq!(voice.media_type, "application/octet-stream");
    assert!(matches!(
        r.cached_voice("message/media_2.db", "other", "3", None),
        Err(ProfileError::NotCached)
    ));
    let avatar = r.avatar("peer").unwrap();
    assert_eq!(avatar.bytes, png);
    assert_eq!(avatar.media_type, "image/png");
}

#[test]
fn selected_message_ids_keep_shard_namespaces_and_skip_only_missing_rows() {
    let d = fixture();
    let r = reader(d.path());
    let messages = r.messages(&MessageQuery::default()).unwrap().items;
    let selected: Vec<_> = messages
        .iter()
        .filter(|m| m.id.local_id == "2")
        .map(|m| m.id.clone())
        .collect();
    assert_eq!(selected.len(), 2);
    let mut missing = selected[0].clone();
    missing.local_id = "999".into();
    let mut ids = selected.clone();
    ids.push(missing);
    let got = r.messages_by_ids(&ids, Some("self")).unwrap();
    assert_eq!(got.len(), 2);
    for message in got {
        let expected = r.message(&message.id, Some("self")).unwrap();
        assert_eq!(
            serde_json::to_value(message).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    assert!(matches!(
        r.messages_by_ids(&vec![selected[0].clone(); 201], None),
        Err(ProfileError::InvalidQuery)
    ));
}
