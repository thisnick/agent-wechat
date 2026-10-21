use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::Connection;
use std::{collections::BTreeMap, fs};
use wechat_profile::*;
fn fixture() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (name, sql) in [
        (
            "contact/contact.db",
            include_str!("../src/layout/contact.db.sql"),
        ),
        (
            "session/session.db",
            include_str!("../src/layout/session.db.sql"),
        ),
        (
            "message/message_0.db",
            include_str!("../src/layout/message_0.db.sql"),
        ),
    ] {
        let p = d.path().join("account/db_storage").join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        Connection::open(p).unwrap().execute_batch(sql).unwrap();
    }
    d
}
fn options() -> WriteOptions {
    WriteOptions {
        exclusive_ownership_confirmed: true,
        allow_plaintext: true,
        durable_intent_id: "synthetic-accepted-intent".into(),
    }
}
#[test]
fn named_writes_preserve_meaning_when_contact_session_and_message_columns_move() {
    use md5::{Digest, Md5};
    let d = fixture();
    for (database, table, before, after) in [
        (
            "contact/contact.db",
            "contact",
            "id INTEGER PRIMARY KEY, username TEXT",
            "username TEXT, id INTEGER PRIMARY KEY",
        ),
        (
            "session/session.db",
            "SessionTable",
            "username TEXT PRIMARY KEY, type INTEGER",
            "type INTEGER, username TEXT PRIMARY KEY",
        ),
    ] {
        let c = Connection::open(d.path().join("account/db_storage").join(database)).unwrap();
        let sql: String = c
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        let indexes: Vec<String> = c.prepare("SELECT sql FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND sql IS NOT NULL").unwrap().query_map([table], |row| row.get(0)).unwrap().map(|row| row.unwrap()).collect();
        assert!(sql.contains(before));
        c.execute_batch(&format!(
            "DROP TABLE {table};{};",
            sql.replace(before, after)
        ))
        .unwrap();
        for index in indexes {
            c.execute_batch(&index).unwrap();
        }
    }
    let c = Connection::open(d.path().join("account/db_storage/message/message_0.db")).unwrap();
    let table = format!("Msg_{:x}", Md5::digest(b"peer"));
    c.execute_batch(
        &include_str!("../src/layout/conversation.sql")
            .replace("__MESSAGE_TABLE__", &table)
            .replace("local_id INTEGER PRIMARY KEY AUTOINCREMENT, ", "")
            .replace(
                "WCDB_CT_source INTEGER DEFAULT NULL)",
                "WCDB_CT_source INTEGER DEFAULT NULL, local_id INTEGER PRIMARY KEY AUTOINCREMENT)",
            ),
    )
    .unwrap();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    apply_contact_patch(
        &loc,
        &keys,
        &ContactProjection {
            username: "peer".into(),
            fields: RawFields::new(),
        },
        &o,
    )
    .unwrap();
    let applied =
        apply_message_projection(&loc, &keys, &message("peer", "peer", "7", 100), &o).unwrap();
    apply_message_session_projection(
        &loc,
        &keys,
        &MessageSessionProjection {
            source: applied.source_id.clone(),
            peer: "peer".into(),
            summary: "Synthetic".into(),
            outgoing: false,
            unread: true,
            sender: Some("peer".into()),
            sender_display: None,
            created_by_this_intent: true,
        },
        &o,
    )
    .unwrap();
    apply_incoming_text(
        &loc,
        &keys,
        &IncomingText {
            peer: "peer".into(),
            own_username: "self".into(),
            server_message_id: "8".into(),
            server_sequence: 9,
            create_time: 101,
            content: "Synthetic narrow text".into(),
            source_xml: "<msgsource/>".into(),
            unread: true,
        },
        &o,
    )
    .unwrap();
    let read = mark_read_messages(&loc, &keys, "peer", &[applied.source_id], &o).unwrap();
    assert_eq!(read.removed_unread_rows, 1);
    assert_eq!(read.unread_count_after, 1);
    let reader = ProfileReader::open(
        loc,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap();
    let rows = reader
        .messages(&MessageQuery {
            own_username: Some("self".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.items.len(), 2);
    assert!(rows
        .items
        .iter()
        .all(|message| message.peer.as_deref() == Some("peer")));
}
#[test]
fn unread_column_reordering_does_not_admit_other_schema_changes() {
    for change in [
        "ALTER TABLE SessionUnreadListTable_1 ADD COLUMN unsupported INTEGER;",
        "CREATE TRIGGER unsupported AFTER INSERT ON SessionUnreadListTable_1 BEGIN SELECT 1; END;",
        "DROP INDEX SessionUnreadListTable_1_NameId_CreateTime;",
    ] {
        let d = fixture();
        let loc = ProfileLocation::new(d.path().join("account")).unwrap();
        let keys = KeyFile::empty("account");
        let o = options();
        let applied =
            apply_message_projection(&loc, &keys, &message("peer", "peer", "7", 100), &o).unwrap();
        let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
        c.execute_batch("DROP TABLE SessionUnreadListTable_1;
            CREATE TABLE SessionUnreadListTable_1(username_id INTEGER,server_id INTEGER,create_time INTEGER,local_id INTEGER,CONSTRAINT _NameId_ServerId PRIMARY KEY(username_id,server_id));
            CREATE INDEX SessionUnreadListTable_1_NameId_CreateTime ON SessionUnreadListTable_1(username_id ASC,create_time ASC);").unwrap();
        c.execute_batch(change).unwrap();
        assert!(matches!(
            apply_message_session_projection(
                &loc,
                &keys,
                &MessageSessionProjection {
                    source: applied.source_id,
                    peer: "peer".into(),
                    summary: "Synthetic".into(),
                    outgoing: false,
                    unread: true,
                    sender: Some("peer".into()),
                    sender_display: None,
                    created_by_this_intent: true,
                },
                &o
            ),
            Err(ProfileError::UnsupportedSchema(_))
        ));
        assert_eq!(
            c.query_row("SELECT count(*) FROM SessionTable", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
fn null_fields(c: &Connection, t: &str, omit: &[&str]) -> RawFields {
    c.prepare(&format!("PRAGMA table_info({t})"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(|n| n.unwrap())
        .filter(|n| !omit.contains(&n.as_str()))
        .map(|n| (n, SqlValue::Null))
        .collect()
}
fn contact(root: &std::path::Path, name: &str, label: &str) -> ContactProjection {
    let c = Connection::open(root.join("account/db_storage/contact/contact.db")).unwrap();
    let mut fields = null_fields(&c, "contact", &["id", "username"]);
    fields.insert("nick_name".into(), SqlValue::Text(label.into()));
    fields.insert(
        "local_type".into(),
        SqlValue::Integer(if name.ends_with("@chatroom") { 2 } else { 1 }.to_string()),
    );
    ContactProjection {
        username: name.into(),
        fields,
    }
}
fn message(peer: &str, sender: &str, sid: &str, time: i64) -> MessageProjection {
    let mut f = BTreeMap::new();
    for (k, v) in [
        ("local_type", 1),
        ("sort_seq", time * 1000),
        ("create_time", time),
        ("status", 3),
        ("upload_status", 0),
        ("download_status", 0),
        ("server_seq", 8),
        ("origin_source", 2),
        ("WCDB_CT_message_content", 0),
        ("WCDB_CT_source", 4),
    ] {
        f.insert(k.into(), SqlValue::Integer(v.to_string()));
    }
    f.insert(
        "source".into(),
        SqlValue::Blob(
            STANDARD.encode(zstd::stream::encode_all(b"<source/>".as_slice(), 3).unwrap()),
        ),
    );
    f.insert(
        "message_content".into(),
        SqlValue::Text("Synthetic only 🙂".into()),
    );
    f.insert("compress_content".into(), SqlValue::Text("".into()));
    MessageProjection {
        peer: peer.into(),
        sender: sender.into(),
        from: sender.into(),
        to: "self".into(),
        server_message_id: sid.into(),
        fields: f,
        packed_other_fields: vec![88, 0],
    }
}
#[test]
fn new_contacts_group_roster_messages_and_sessions_roundtrip_and_replay() {
    let d = fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    for (n, label) in [
        ("self", "Me"),
        ("peer", "Peer"),
        ("other", "Other"),
        ("room@chatroom", "Room"),
    ] {
        let input = contact(d.path(), n, label);
        let a = apply_contact_projection(&loc, &keys, &input, &o).unwrap();
        let b = apply_contact_projection(&loc, &keys, &input, &o).unwrap();
        assert_eq!(a.local_id, b.local_id);
        assert!(!b.changed);
    }
    let ext = vec![10, 6, 10, 4, b'p', b'e', b'e', b'r'];
    let mut group_fields = BTreeMap::new();
    group_fields.insert("owner".into(), SqlValue::Text("peer".into()));
    group_fields.insert("ext_buffer".into(), SqlValue::Blob(STANDARD.encode(&ext)));
    let group = GroupProjection {
        username: "room@chatroom".into(),
        fields: group_fields,
        detail_fields: None,
        members: vec!["peer".into()],
        complete_roster: true,
    };
    apply_group_projection(&loc, &keys, &group, &o).unwrap();
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    let old: i64 = c
        .query_row("SELECT rowid FROM chatroom_member", [], |r| r.get(0))
        .unwrap();
    apply_group_projection(&loc, &keys, &group, &o).unwrap();
    assert_eq!(
        old,
        c.query_row("SELECT rowid FROM chatroom_member", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
    );
    let incoming = message("peer", "peer", "18446744073709551615", 100);
    let a = apply_message_projection(&loc, &keys, &incoming, &o).unwrap();
    assert!(a.inserted);
    let b = apply_message_projection(&loc, &keys, &incoming, &o).unwrap();
    assert!(!b.inserted);
    assert_eq!(a.source_id, b.source_id);
    let mut outgoing = message("peer", "self", "8", 101);
    outgoing.to = "peer".into();
    outgoing
        .fields
        .insert("status".into(), SqlValue::Integer("2".into()));
    apply_message_projection(&loc, &keys, &outgoing, &o).unwrap();
    let mut grouped = message("room@chatroom", "peer", "9", 102);
    grouped.from = "room@chatroom".into();
    apply_message_projection(&loc, &keys, &grouped, &o).unwrap();
    let c = Connection::open(d.path().join("account/db_storage/session/session.db")).unwrap();
    let mut fields = null_fields(&c, "SessionTable", &["username"]);
    for (k, v) in [
        ("unread_count", 1),
        ("is_hidden", 0),
        ("sort_timestamp", 100),
        ("last_timestamp", 100),
    ] {
        fields.insert(k.into(), SqlValue::Integer(v.to_string()));
    }
    fields.insert(
        "last_msg_locald_id".into(),
        SqlValue::Integer(a.source_id.local_id),
    );
    fields.insert("summary".into(), SqlValue::Text("Synthetic only 🙂".into()));
    assert!(apply_session_projection(&loc, &keys, "peer", &fields, &o).unwrap());
    assert!(!apply_session_projection(&loc, &keys, "peer", &fields, &o).unwrap());
    let r = ProfileReader::open(
        loc,
        keys,
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.contacts(&ListQuery::default()).unwrap().items.len(), 4);
    assert_eq!(r.groups(&ListQuery::default()).unwrap().items.len(), 1);
    assert_eq!(
        r.group_members("room@chatroom", &ListQuery::default())
            .unwrap()
            .items
            .len(),
        1
    );
    let messages = r
        .messages(&MessageQuery {
            own_username: Some("self".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(messages.items.len(), 3);
    assert_eq!(
        messages
            .items
            .iter()
            .filter(|m| m.outgoing == Some(true))
            .count(),
        1
    );
    assert_eq!(r.threads(&ListQuery::default()).unwrap().items.len(), 1);
}
#[test]
fn conflicts_unknown_fields_schema_changes_and_unowned_writes_fail_without_new_rows() {
    let d = fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    let input = contact(d.path(), "peer", "Peer");
    apply_contact_projection(&loc, &keys, &input, &o).unwrap();
    let mut wrong = input.clone();
    wrong.fields.insert("unrecognized".into(), SqlValue::Null);
    assert!(apply_contact_projection(&loc, &keys, &wrong, &o).is_err());
    let mut unowned = options();
    unowned.exclusive_ownership_confirmed = false;
    assert!(apply_contact_projection(&loc, &keys, &input, &unowned).is_err());
    let m = message("peer", "peer", "7", 100);
    apply_message_projection(&loc, &keys, &m, &o).unwrap();
    let mut conflict = m.clone();
    conflict
        .fields
        .insert("message_content".into(), SqlValue::Text("different".into()));
    assert!(apply_message_projection(&loc, &keys, &conflict, &o).is_err());
    let mut invalid = m.clone();
    invalid.server_message_id = "8".into();
    assert!(apply_message_projection(&loc, &keys, &invalid, &o).is_err());
    let c = Connection::open(d.path().join("account/db_storage/contact/contact.db")).unwrap();
    c.execute_batch("ALTER TABLE contact ADD COLUMN unknown_native_field TEXT")
        .unwrap();
    assert!(matches!(
        apply_contact_projection(&loc, &keys, &input, &o),
        Err(ProfileError::UnsupportedSchema(_))
    ));
}

#[test]
fn sparse_contact_and_native_roster_updates_preserve_opaque_fields_and_only_remove_complete_members(
) {
    let d = fixture();
    let loc = ProfileLocation::new(d.path().join("account")).unwrap();
    let keys = KeyFile::empty("account");
    let o = options();
    for name in ["peer", "other", "room@chatroom"] {
        let mut fields = RawFields::new();
        fields.insert(
            "nick_name".into(),
            SqlValue::Text(format!("Synthetic {name}")),
        );
        apply_contact_patch(
            &loc,
            &keys,
            &ContactProjection {
                username: name.into(),
                fields,
            },
            &o,
        )
        .unwrap();
    }
    let initial = NativeRosterPatch {
        username: "room@chatroom".into(),
        members: vec![NativeMemberPatch {
            username: "peer".into(),
            display: Some("Per-room label".into()),
        }],
        complete_roster: true,
    };
    apply_native_roster_patch(&loc, &keys, &initial, &o).unwrap();
    // Synthetic opaque native extension fields: keep complete encoded chunks, not
    // only their decoded value. Neither is assigned an invented version meaning.
    let c = Connection::open(loc.root().join("db_storage/contact/contact.db")).unwrap();
    let mut old: Vec<u8> = c
        .query_row("SELECT ext_buffer FROM chat_room", [], |r| r.get(0))
        .unwrap();
    assert_eq!(old[0], 10);
    let nested_len = old[1] as usize;
    assert_eq!(nested_len + 2, old.len());
    old[1] += 5;
    old.extend([24, 9, 34, 1, 42]);
    let root_unknown = [24, 77, 42, 6, b'o', b'p', b'a', b'q', b'u', b'e'];
    old.extend(root_unknown);
    c.execute("UPDATE chat_room SET owner='peer',ext_buffer=?1", [&old])
        .unwrap();
    let partial = NativeRosterPatch {
        username: "room@chatroom".into(),
        members: vec![NativeMemberPatch {
            username: "other".into(),
            display: Some("Other room label".into()),
        }],
        complete_roster: false,
    };
    apply_native_roster_patch(&loc, &keys, &partial, &o).unwrap();
    let packed: Vec<u8> = c
        .query_row("SELECT ext_buffer FROM chat_room", [], |r| r.get(0))
        .unwrap();
    assert!(packed
        .windows(root_unknown.len())
        .any(|w| w == root_unknown));
    assert!(packed.windows(5).any(|w| w == [24, 9, 34, 1, 42]));
    assert_eq!(
        c.query_row("SELECT owner FROM chat_room", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "peer"
    );
    let complete = NativeRosterPatch {
        username: "room@chatroom".into(),
        members: vec![NativeMemberPatch {
            username: "peer".into(),
            display: None,
        }],
        complete_roster: true,
    };
    apply_native_roster_patch(&loc, &keys, &complete, &o).unwrap();
    let packed: Vec<u8> = c
        .query_row("SELECT ext_buffer FROM chat_room", [], |r| r.get(0))
        .unwrap();
    assert_eq!(packed, old);
    let mut patch = RawFields::new();
    patch.insert("remark".into(), SqlValue::Text("Local remark".into()));
    apply_contact_patch(
        &loc,
        &keys,
        &ContactProjection {
            username: "peer".into(),
            fields: patch,
        },
        &o,
    )
    .unwrap();
    assert_eq!(
        c.query_row(
            "SELECT nick_name FROM contact WHERE username='peer'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "Synthetic peer"
    );
    let r = ProfileReader::open(
        loc.clone(),
        keys.clone(),
        ReadOptions {
            stopped_or_consistent_snapshot: true,
            allow_plaintext: true,
            ..Default::default()
        },
    )
    .unwrap();
    let members = r
        .group_members("room@chatroom", &ListQuery::default())
        .unwrap()
        .items;
    assert_eq!(members.len(), 1);
    assert_eq!(
        members[0].room_display_name.as_deref(),
        Some("Per-room label")
    );
    let duplicate = NativeRosterPatch {
        username: "room@chatroom".into(),
        members: vec![
            NativeMemberPatch {
                username: "peer".into(),
                display: None
            };
            2
        ],
        complete_roster: true,
    };
    assert!(apply_native_roster_patch(&loc, &keys, &duplicate, &o).is_err());
}
