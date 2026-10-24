use super::encrypted_roundtrip::{add, state};
use super::*;
use crate::message_id;

fn fixture() -> (tempfile::TempDir, Arc<Store>, Arc<Core>) {
    let temporary = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&temporary.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store.clone(), None).unwrap();
    let state = state();
    core.configure_profile_format_key(&[9; 16]).unwrap();
    core.install_login_state("one", &state, None).unwrap();
    core.ensure_managed_profile("one", &state).unwrap();
    (temporary, store, core)
}
fn recall(server: u64) -> SyncCommand {
    add(10002,"peer","synthetic_self",format!("<sysmsg type='revokemsg'><revokemsg><session>peer</session><msgid>12</msgid><newmsgid>{server}</newmsgid><replacemsg><![CDATA[Localized recall notice]]></replacemsg><revoketime>0</revoketime></revokemsg></sysmsg>").as_bytes(),999,110)
}
fn first(core: &Core) -> Value {
    core.messages("one", "peer", Some(50), None).unwrap()["items"][0].clone()
}
fn recall_count(store: &Store) -> usize {
    store
        .events("one", 0, 200)
        .unwrap()
        .iter()
        .filter(|e| e.kind == "message.recalled")
        .count()
}

// Production stages the original batch before projection. Direct command tests
// retain that existing journal separately so attribution has the same evidence.
fn retain_original(core: &Core, store: &Store, id: &str, command: SyncCommand) {
    let batch = wechat_protocol::SyncBatch {
        code: 0,
        flags: 0,
        cursor: vec![1],
        commands: vec![command],
        continuation: false,
    };
    let sealed = core
        .vault
        .seal(
            "one",
            &format!("apply:{id}"),
            &serde_json::to_vec(&batch).unwrap(),
        )
        .unwrap();
    store
        .stage_sync("one", id, &sealed, b"synthetic-cursor")
        .unwrap();
    store.finish_sync("one", id).unwrap();
}

#[test]
fn recall_api_derives_own_and_group_authors_from_existing_sync_receipts() {
    for (thread, from, to, body, author, direction) in [
        (
            "peer",
            "synthetic_self",
            "peer",
            "own from another device",
            "synthetic_self",
            "outgoing",
        ),
        (
            "room@chatroom",
            "room@chatroom",
            "synthetic_self",
            "member:\nmember message",
            "member",
            "incoming",
        ),
        (
            "room@chatroom",
            "synthetic_self",
            "room@chatroom",
            "own group message",
            "synthetic_self",
            "outgoing",
        ),
    ] {
        let (_temporary, store, core) = fixture();
        retain_original(
            &core,
            &store,
            "original-author-batch",
            add(1, from, to, body.as_bytes(), 123, 100),
        );
        core.apply_sync_command(
            "one",
            &add(1, from, to, body.as_bytes(), 123, 100),
            "original-author",
            "live",
        )
        .unwrap();
        let before = core.messages("one", thread, Some(50), None).unwrap()["items"][0].clone();
        let id = before["id"].as_str().unwrap();
        let notice = "Someone & <another> recalled \"a message\"";
        let xml = format!("<sysmsg type='revokemsg'><revokemsg><session>{thread}</session><newmsgid>123</newmsgid><replacemsg><![CDATA[{notice}]]></replacemsg></revokemsg></sysmsg>");
        let xml = if thread.ends_with("@chatroom") {
            format!("other_member:\n{xml}")
        } else {
            xml
        };
        core.apply_sync_command(
            "one",
            &add(10002, thread, "synthetic_self", xml.as_bytes(), 999, 110),
            "recall-author",
            "live",
        )
        .unwrap();
        let value = core.message("one", id).unwrap();
        assert_eq!(value["senderId"], author);
        assert_eq!(value["direction"], direction);
        assert_eq!(value["recall"]["replacementText"], notice);
        let raw = core
            .profile_reader("one")
            .unwrap()
            .message(
                &crate::source_message_id(id).unwrap(),
                Some("synthetic_self"),
            )
            .unwrap();
        assert_eq!(raw.sender.as_deref(), Some(""));
        let doc = roxmltree::Document::parse(raw.content.text.as_deref().unwrap()).unwrap();
        assert_eq!(
            doc.descendants()
                .find(|node| node.has_tag_name("content"))
                .unwrap()
                .text(),
            Some(notice)
        );
        let thread_row = core
            .profile_reader("one")
            .unwrap()
            .threads(&wechat_profile::ListQuery::default())
            .unwrap()
            .items
            .into_iter()
            .find(|row| row.username == thread)
            .unwrap();
        assert_eq!(
            thread_row.raw_fields.get("summary"),
            Some(&SqlValue::Text(notice.into()))
        );
        let event = store
            .events("one", 0, 200)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == "message.recalled")
            .unwrap();
        assert!(event.data.get("originalSenderId").is_none());
        assert!(event.data.get("originalDirection").is_none());
        assert!(event.data.get("replacementText").is_none());
        core.reconcile_recall_markers("one").unwrap();
        assert_eq!(core.message("one", id).unwrap()["senderId"], author);
        assert_eq!(recall_count(&store), 1);
    }
}

#[test]
fn imported_cleared_row_keeps_notice_and_unknown_author_until_a_late_original() {
    for recovery in 0..3 {
        let (temporary, store, core) = fixture();
        core.apply_sync_command(
            "one",
            &add(1, "synthetic_self", "peer", b"original", 123, 100),
            "original-import",
            "live",
        )
        .unwrap();
        let id = first(&core)["id"].as_str().unwrap().to_owned();
        let source = crate::source_message_id(&id).unwrap();
        let db = synthetic_database(&core, &temporary, &source.database);
        db.execute("INSERT INTO Name2Id(user_name,is_session) VALUES('',0)", [])
            .unwrap();
        db.execute(&format!("UPDATE {} SET local_type=10000,status=4,real_sender_id=(SELECT rowid FROM Name2Id WHERE user_name=''),message_content=?1 WHERE local_id=?2", source.table), rusqlite::params!["<sysmsg type='revokemsg'><revokemsg><content>You recalled a message</content><revoketime>110</revoketime></revokemsg></sysmsg>", source.local_id.parse::<i64>().unwrap()]).unwrap();
        drop(db);
        match recovery {
            1 => {
                core.apply_confirmed_recall_with_notice(
                    "one",
                    "peer",
                    "123",
                    Some(110),
                    Some("Later control notice"),
                    "reconciled",
                    "imported-recall",
                )
                .unwrap();
            }
            2 => {
                store
                    .record_recall("one", "peer", "123", None, None, Some(110), "reconciled")
                    .unwrap();
                core.reconcile_recall_markers("one").unwrap();
            }
            _ => {}
        }
        let value = core.message("one", &id).unwrap();
        assert!(value["senderId"].is_null());
        assert!(value["direction"].is_null());
        assert!(value["recall"]["originalSenderId"].is_null());
        assert_eq!(value["recall"]["replacementText"], "You recalled a message");
        retain_original(
            &core,
            &store,
            "late-import-batch",
            add(1, "synthetic_self", "peer", b"original", 123, 100),
        );
        core.apply_sync_command(
            "one",
            &add(1, "synthetic_self", "peer", b"original", 123, 100),
            "late-import-original",
            "catchup",
        )
        .unwrap();
        let value = core.message("one", &id).unwrap();
        assert_eq!(value["senderId"], "synthetic_self");
        assert_eq!(value["direction"], "outgoing");
        assert_eq!(recall_count(&store), 1);
    }
}

#[test]
fn replacement_notice_is_bounded_text_and_replay_keeps_the_first_notice() {
    let (_temporary, store, core) = fixture();
    for replacement in ["<b>markup</b>".into(), "x".repeat(16 * 1024 + 1)] {
        let xml = format!("<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>123</newmsgid><replacemsg>{replacement}</replacemsg></revokemsg></sysmsg>");
        let command = add(10002, "peer", "synthetic_self", xml.as_bytes(), 999, 110);
        assert!(core
            .apply_sync_command("one", &command, "invalid-notice", "live")
            .is_err());
        assert!(store.recall_marker("one", "peer", "123").unwrap().is_none());
    }
    core.apply_sync_command("one", &recall(123), "first-notice", "live")
        .unwrap();
    core.apply_confirmed_recall_with_notice(
        "one",
        "peer",
        "123",
        None,
        Some("Different replay notice"),
        "catchup",
        "other-notice",
    )
    .unwrap();
    let marker = store.recall_marker("one", "peer", "123").unwrap().unwrap();
    assert_eq!(
        core.recall_preview("one", &marker).unwrap(),
        "Localized recall notice"
    );
    assert_eq!(recall_count(&store), 1);
}

#[test]
fn replacement_notice_preserves_xml_character_references_and_line_endings() {
    let (_temporary, _store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"original", 123, 100),
        "notice-original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let xml = b"<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>123</newmsgid><replacemsg>First&#13;\n&lt;notice&gt; &amp; detail\t</replacemsg></revokemsg></sysmsg>";
    core.apply_sync_command(
        "one",
        &add(10002, "peer", "synthetic_self", xml, 999, 110),
        "notice-control",
        "live",
    )
    .unwrap();
    let expected = "First\r\n<notice> & detail\t";
    assert_eq!(
        core.message("one", &id).unwrap()["recall"]["replacementText"],
        expected
    );
    let raw = core
        .profile_reader("one")
        .unwrap()
        .message(&crate::source_message_id(&id).unwrap(), None)
        .unwrap();
    let doc = roxmltree::Document::parse(raw.content.text.as_deref().unwrap()).unwrap();
    assert_eq!(
        doc.descendants()
            .find(|node| node.has_tag_name("content"))
            .unwrap()
            .text(),
        Some(expected)
    );
}

#[test]
fn accepted_text_and_media_repair_keep_recalled_group_notice_and_blank_preview_sender() {
    for media in [false, true] {
        for existing in [false, true] {
            let (_temporary, store, core) = fixture();
            let project = |receipt| {
                if media {
                    core.project_accepted_media(
                        "one",
                        "room@chatroom",
                        49,
                        std::str::from_utf8(include_bytes!("../tests/fixtures/shared-link.xml"))
                            .unwrap(),
                        "Original preview",
                        100,
                        "123",
                        receipt,
                    )
                } else {
                    core.project_accepted_text(
                        "one",
                        "room@chatroom",
                        "Original text",
                        100,
                        "123",
                        receipt,
                    )
                }
            };
            if existing {
                project("before-recall").unwrap();
            }
            let control = add(10002, "room@chatroom", "synthetic_self", b"<sysmsg type='revokemsg'><revokemsg><session>room@chatroom</session><newmsgid>123</newmsgid><replacemsg>Server group notice</replacemsg></revokemsg></sysmsg>", 999, 110);
            core.apply_sync_command("one", &control, "accepted-recall", "live")
                .unwrap();
            let source = project("accepted-repair").unwrap();
            let value = core
                .message("one", &crate::message_id(&source).unwrap())
                .unwrap();
            // Direct projection helpers are not accepted-send receipts.
            assert!(value["senderId"].is_null());
            assert!(value["direction"].is_null());
            assert_eq!(value["recall"]["replacementText"], "Server group notice");
            let thread = core
                .profile_reader("one")
                .unwrap()
                .threads(&wechat_profile::ListQuery::default())
                .unwrap()
                .items
                .into_iter()
                .find(|row| row.username == "room@chatroom")
                .unwrap();
            assert_eq!(
                thread.raw_fields.get("summary"),
                Some(&SqlValue::Text("Server group notice".into()))
            );
            assert_eq!(
                thread.raw_fields.get("last_msg_sender"),
                Some(&SqlValue::Text("".into()))
            );
            assert_eq!(thread.unread_count, Some(0));
            assert_eq!(recall_count(&store), 1);
        }
    }
}

#[test]
fn unavailable_search_index_keeps_author_notice_and_suppression_pending_without_clearing_source() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(
            1,
            "synthetic_self",
            "peer",
            b"Original before index cleanup",
            123,
            100,
        ),
        "index-original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let (location, _) = core.profile_location_keys("one").unwrap();
    let path = location.root().join("db_storage/message/message_fts.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE unsupported_index(payload TEXT); INSERT INTO unsupported_index VALUES('synthetic retained data');").unwrap();
    drop(db);
    for receipt in ["index-recall", "index-retry"] {
        core.retain_control_raw("one", &recall(123), receipt)
            .unwrap();
        let error = core
            .apply_confirmed_recall_with_notice(
                "one",
                "peer",
                "123",
                None,
                Some("Localized recall notice"),
                "live",
                receipt,
            )
            .unwrap_err();
        assert!(
            store.recall_marker("one", "peer", "123").unwrap().is_some(),
            "{error:#}"
        );
        let value = core.message("one", &id).unwrap();
        assert_eq!(value["body"], crate::recalls::RECALLED_BODY);
        assert_eq!(value["senderId"], "synthetic_self");
        assert_eq!(value["direction"], "outgoing");
        assert_eq!(
            value["recall"]["replacementText"],
            "Localized recall notice"
        );
        let raw = core
            .profile_reader("one")
            .unwrap()
            .message(&crate::source_message_id(&id).unwrap(), None)
            .unwrap();
        assert_eq!(
            raw.content.text.as_deref(),
            Some("Original before index cleanup")
        );
        assert_eq!(store.pending_recall_projections("one").unwrap(), 1);
        assert_eq!(recall_count(&store), 1);
    }
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT payload FROM unsupported_index", [], |row| row
            .get::<_, String>(0))
            .unwrap(),
        "synthetic retained data"
    );
}
#[test]
fn recalled_share_redacts_structured_metadata_and_blocks_its_thumbnail() {
    let (_temporary, _store, core) = fixture();
    let original = add(
        49,
        "peer",
        "synthetic_self",
        include_bytes!("../tests/fixtures/shared-link.xml"),
        123,
        100,
    );
    core.apply_sync_command("one", &original, "original-share", "live")
        .unwrap();
    let before = first(&core);
    let id = before["id"].as_str().unwrap();
    assert_eq!(before["link"]["title"], "Title & preview");
    assert_eq!(core.media_assets("one", id).unwrap().len(), 1);
    core.apply_sync_command("one", &recall(123), "recall-share", "live")
        .unwrap();
    let after = core.message("one", id).unwrap();
    assert!(after["link"].is_null());
    assert_eq!(after["body"], crate::recalls::RECALLED_BODY);
    assert!(!after.to_string().contains("Title & preview"));
    assert!(core
        .media_assets("one", id)
        .unwrap_err()
        .to_string()
        .contains("message_recalled"));
}
fn synthetic_database(
    core: &Core,
    temporary: &tempfile::TempDir,
    relative: &str,
) -> rusqlite::Connection {
    use base64::Engine;
    let (location, keys) = core.profile_location_keys("one").unwrap();
    let exported = temporary.path().join("data/synthetic-format-keys.json");
    if !exported.exists() {
        keys.write_new(&exported).unwrap();
    }
    let value: Value = serde_json::from_slice(&std::fs::read(exported).unwrap()).unwrap();
    let encoded = value["databases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["relativePath"] == relative)
        .unwrap()["rawPageKeyBase64"]
        .as_str()
        .unwrap();
    let raw = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let hex = raw
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let db = rusqlite::Connection::open(location.root().join("db_storage").join(relative)).unwrap();
    db.execute_batch(&format!(
        "PRAGMA key=\"x'{hex}'\";PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;"
    ))
    .unwrap();
    db
}
fn synthetic_cached_asset(
    core: &Core,
    store: &Store,
    mut asset: crate::MediaAsset,
    bytes: &[u8],
) -> crate::MediaAsset {
    use sha2::Digest;
    asset.state = "available".into();
    asset.actual_variant = Some(asset.variant);
    asset.byte_length = Some(bytes.len() as u64);
    asset.sha256 = Some(format!("{:x}", sha2::Sha256::digest(bytes)));
    asset.content_type = Some("application/octet-stream".into());
    asset.retryable = false;
    let sealed = core
        .vault
        .seal("one", &format!("asset:{}", asset.id), bytes)
        .unwrap();
    store
        .save_media_asset(
            "one",
            &asset.id,
            &serde_json::to_value(&asset).unwrap(),
            Some(&sealed),
        )
        .unwrap();
    asset
}

#[test]
fn wire_recall_uses_explicit_target_and_does_not_assume_official_row_format() {
    let command = recall(u64::MAX);
    let message =
        wechat_protocol::wire::decode_add_message(&command.payload, "synthetic_self").unwrap();
    let parsed = crate::recalls::incoming_recall(&message, "synthetic_self")
        .unwrap()
        .unwrap();
    assert_eq!(parsed.thread, "peer");
    assert_eq!(parsed.server, u64::MAX.to_string());
    assert_ne!(message.server_message_id, Some(parsed.server));
    assert!(parsed.recalled_at.is_none());
    assert_eq!(
        parsed.replacement_text.as_deref(),
        Some("Localized recall notice")
    );
    for xml in [
        "<sysmsg type='revokemsg'><revokemsg><session>peer</session></revokemsg></sysmsg>",
        "<sysmsg type='revokemsg'><revokemsg><session>other</session><newmsgid>10</newmsgid></revokemsg></sysmsg>",
        "<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>10</newmsgid><newmsgid>11</newmsgid></revokemsg></sysmsg>",
        "<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>10</newmsgid><unknown-native-action/></revokemsg></sysmsg>",
        "<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>10</newmsgid></revokemsg><unknown-native-action/></sysmsg>",
    ] {
        let command=add(10002,"peer","synthetic_self",xml.as_bytes(),999,110);
        let message=wechat_protocol::wire::decode_add_message(&command.payload,"synthetic_self").unwrap();
        assert!(crate::recalls::incoming_recall(&message,"synthetic_self").is_err());
    }
    let official = add(
        10000,
        "peer",
        "synthetic_self",
        b"<sysmsg type='revokemsg'><revokemsg><revoketime>0</revoketime></revokemsg></sysmsg>",
        999,
        110,
    );
    let message =
        wechat_protocol::wire::decode_add_message(&official.payload, "synthetic_self").unwrap();
    assert!(crate::recalls::incoming_recall(&message, "synthetic_self").is_err());
}

#[test]
fn sender_prefixed_group_recall_keeps_the_chat_and_target_checks() {
    let xml = "<sysmsg type='revokemsg'><revokemsg><session>room@chatroom</session><newmsgid>123</newmsgid></revokemsg></sysmsg>";
    let command = add(
        10002,
        "room@chatroom",
        "synthetic_self",
        format!("member:\n{xml}").as_bytes(),
        999,
        110,
    );
    let message =
        wechat_protocol::wire::decode_add_message(&command.payload, "synthetic_self").unwrap();
    let parsed = crate::recalls::incoming_recall(&message, "synthetic_self")
        .unwrap()
        .unwrap();
    assert_eq!(parsed.thread, "room@chatroom");
    assert_eq!(parsed.server, "123");
    let mut with_empty_announcement = message.clone();
    with_empty_announcement.content = String::from_utf8(with_empty_announcement.content)
        .unwrap()
        .replace("</revokemsg>", "<announcement_id/></revokemsg>")
        .into_bytes();
    assert_eq!(
        crate::recalls::incoming_recall(&with_empty_announcement, "synthetic_self")
            .unwrap()
            .unwrap()
            .server,
        "123"
    );
    let mut with_empty_cdata = with_empty_announcement.clone();
    with_empty_cdata.content = String::from_utf8(with_empty_cdata.content)
        .unwrap()
        .replace(
            "<announcement_id/>",
            "<announcement_id><![CDATA[]]></announcement_id>",
        )
        .into_bytes();
    assert_eq!(
        crate::recalls::incoming_recall(&with_empty_cdata, "synthetic_self")
            .unwrap()
            .unwrap()
            .server,
        "123"
    );
    for field in [
        "<announcement_id>123</announcement_id>",
        "<announcement_id> </announcement_id>",
        "<announcement_id unknown='1'/>",
        "<announcement_id><unknown/></announcement_id>",
        "<announcement_id><!--unknown--></announcement_id>",
        "<announcement_id/><announcement_id/>",
        "<announcement_id/><revoketime>110</revoketime>",
    ] {
        let mut invalid = with_empty_announcement.clone();
        invalid.content = String::from_utf8(invalid.content)
            .unwrap()
            .replace("<announcement_id/>", field)
            .into_bytes();
        assert!(crate::recalls::incoming_recall(&invalid, "synthetic_self").is_err());
    }
    for prefix in [
        "",
        "member@chatroom",
        "two words",
        "member\r",
        "member\n",
        &"x".repeat(513),
    ] {
        let mut invalid = message.clone();
        invalid.content = format!("{prefix}:\n{xml}").into_bytes();
        assert!(crate::recalls::incoming_recall(&invalid, "synthetic_self")
            .unwrap()
            .is_none());
    }
    for mutation in 0..5 {
        let mut invalid = message.clone();
        match mutation {
            0 => invalid.from = "peer".into(),
            1 => invalid.peer = "other@chatroom".into(),
            2 => invalid.to = "foreign".into(),
            3 => invalid.outgoing = true,
            _ => invalid.sender_id = Some("other-member".into()),
        }
        assert!(crate::recalls::incoming_recall(&invalid, "synthetic_self")
            .unwrap()
            .is_none());
    }
    let mut wrong_target = message.clone();
    wrong_target.content = String::from_utf8(wrong_target.content)
        .unwrap()
        .replace(
            "<session>room@chatroom</session>",
            "<session>other@chatroom</session>",
        )
        .into_bytes();
    assert!(
        crate::recalls::incoming_recall(&wrong_target, "synthetic_self")
            .err()
            .unwrap()
            .to_string()
            .contains("recall_target_thread_mismatch")
    );
}

#[test]
fn sender_prefixed_group_recall_projects_once_without_creating_a_control_message() {
    let (_temporary, store, core) = fixture();
    let original = add(
        1,
        "room@chatroom",
        "synthetic_self",
        b"member:\nOriginal group message",
        123,
        100,
    );
    retain_original(&core, &store, "original-batch", original.clone());
    core.apply_sync_command("one", &original, "group-original", "live")
        .unwrap();
    let before = core.messages("one", "room@chatroom", None, None).unwrap()["items"][0].clone();
    let control = add(10002, "room@chatroom", "synthetic_self", b"member:\n<sysmsg type='revokemsg'><revokemsg><session>room@chatroom</session><newmsgid>123</newmsgid><announcement_id><![CDATA[]]></announcement_id></revokemsg></sysmsg>", 999, 110);
    for _ in 0..2 {
        core.apply_sync_command("one", &control, "group-recall", "live")
            .unwrap();
    }
    let id = before["id"].as_str().unwrap();
    let after = core.message("one", id).unwrap();
    assert_eq!(after["id"], before["id"]);
    assert_eq!(after["serverId"], "123");
    assert_eq!(after["body"], crate::recalls::RECALLED_BODY);
    assert_eq!(after["type"], 10000);
    assert_eq!(after["senderId"], "member");
    assert_eq!(after["direction"], "incoming");
    assert!(after["recall"]["replacementText"].is_null());
    assert_eq!(
        core.messages("one", "room@chatroom", None, None).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(recall_count(&store), 1);
    let key = "sync-control:group-recall";
    let sealed = store.runtime("one", key).unwrap().unwrap();
    assert_eq!(
        *core.vault.open_record("one", key, &sealed).unwrap(),
        control.payload
    );
    assert!(store.runtime("one", "group-recall").unwrap().is_some());
    assert!(store
        .recall_marker("one", "room@chatroom", "999")
        .unwrap()
        .is_none());
    drop(core);
    let core = Core::new(store.clone(), None).unwrap();
    core.configure_profile_format_key(&[9; 16]).unwrap();
    core.apply_sync_command("one", &control, "group-recall", "catchup")
        .unwrap();
    assert_eq!(
        core.message("one", id).unwrap()["body"],
        crate::recalls::RECALLED_BODY
    );
    assert_eq!(recall_count(&store), 1);
}

#[test]
fn incoming_recall_preserves_identity_and_blocks_replayed_history_quotes_and_cached_bytes() {
    let (_temporary, store, core) = fixture();
    let server = u64::MAX - 1;
    let original = add(
        1,
        "peer",
        "synthetic_self",
        b"Original private body",
        server,
        100,
    );
    retain_original(&core, &store, "original-batch", original.clone());
    core.apply_sync_command("one", &original, "original", "live")
        .unwrap();
    let before = first(&core);
    let id = before["id"].as_str().unwrap();
    let source = crate::source_message_id(id).unwrap();
    let raw_before = core
        .profile_reader("one")
        .unwrap()
        .message(&source, None)
        .unwrap();
    core.apply_sync_command("one", &recall(server), "recall", "live")
        .unwrap();
    let after = core.message("one", id).unwrap();
    assert_eq!(after["id"], before["id"]);
    assert_eq!(after["serverId"], server.to_string());
    assert_eq!(after["body"], crate::recalls::RECALLED_BODY);
    assert_eq!(after["type"], 10000);
    assert_eq!(after["senderId"], "peer");
    assert_eq!(after["direction"], "incoming");
    assert_eq!(
        after["recall"]["replacementText"],
        "Localized recall notice"
    );
    assert!(after["recall"]["observedAt"].is_string());
    assert!(after["recall"]["recalledAt"].is_null());
    let raw_after = core
        .profile_reader("one")
        .unwrap()
        .message(&source, None)
        .unwrap();
    assert_eq!(raw_after.create_time, raw_before.create_time);
    assert_eq!(raw_after.sort_seq, raw_before.sort_seq);
    assert_eq!(
        raw_after.raw_fields.get("status"),
        Some(&SqlValue::Integer("4".into()))
    );
    core.apply_sync_command("one", &recall(server), "recall-replay", "catchup")
        .unwrap();
    core.apply_sync_command("one", &original, "original-replay", "catchup")
        .unwrap();
    assert_eq!(
        core.message("one", id).unwrap()["body"],
        crate::recalls::RECALLED_BODY
    );
    assert_eq!(recall_count(&store), 1);

    let quote=format!("<msg><appmsg><type>57</type><title>Own comment</title><refermsg><type>1</type><svrid>{server}</svrid><content>Original private body</content></refermsg></appmsg></msg>");
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", quote.as_bytes(), 42, 111),
        "quote",
        "live",
    )
    .unwrap();
    let parent = first(&core);
    let parent_id = parent["id"].as_str().unwrap();
    assert_eq!(parent["body"], "Own comment");
    assert_eq!(parent["quote"]["state"], "recalled");
    assert!(parent["quote"]["embeddedText"].is_null());
    assert_eq!(
        core.quoted_message("one", parent_id).unwrap()["state"],
        "recalled"
    );
    assert_eq!(
        core.media_assets("one", parent_id).unwrap_err().to_string(),
        "message_recalled"
    );
    let bytes = b"Cached attachment bytes";
    store
        .put_media_asset(
            "one",
            &wechat_store::MediaAssetRecord {
                id: "cached".into(),
                message_id: Some(id.into()),
                variant: "normal".into(),
                kind: "file".into(),
                metadata: Value::Null,
                plan: vec![],
                payload: Some(core.vault.seal("one", "asset:cached", bytes).unwrap()),
            },
        )
        .unwrap();
    assert_eq!(
        core.media_content("one", "cached")
            .err()
            .unwrap()
            .to_string(),
        "media_identity_unavailable"
    );
    assert_eq!(
        core.media_asset("one", "cached").unwrap_err().to_string(),
        "media_identity_unavailable"
    );
    assert!(store
        .recall_marker("one", "other", &server.to_string())
        .unwrap()
        .is_none());
}

#[test]
fn out_of_order_recall_suppresses_later_original_without_duplicate_events_or_unread() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command("one", &recall(200), "early-recall", "live")
        .unwrap();
    let marker = store.recall_marker("one", "peer", "200").unwrap().unwrap();
    assert!(marker.message_id.is_none());
    assert_eq!(marker.projection_state, "target_absent");
    retain_original(
        &core,
        &store,
        "late-original-batch",
        add(1, "peer", "synthetic_self", b"Late original body", 200, 100),
    );
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Late original body", 200, 100),
        "late-original",
        "catchup",
    )
    .unwrap();
    let value = first(&core);
    assert_eq!(value["body"], crate::recalls::RECALLED_BODY);
    assert_eq!(value["serverId"], "200");
    assert_eq!(recall_count(&store), 1);
    let marker = store.recall_marker("one", "peer", "200").unwrap().unwrap();
    assert_eq!(marker.message_id.as_deref(), value["id"].as_str());
    assert_eq!(marker.projection_state, "applied");
    assert_eq!(value["senderId"], "peer");
    assert_eq!(value["direction"], "incoming");
    let threads = core
        .profile_reader("one")
        .unwrap()
        .threads(&wechat_profile::ListQuery::default())
        .unwrap();
    assert_eq!(threads.items[0].unread_count, Some(0));
}

#[test]
fn unsupported_controls_remain_sealed_and_block_complete_cursor_after_known_recall() {
    let (_temporary, store, core) = fixture();
    let batch = wechat_protocol::SyncBatch {
        code: 0,
        flags: 0,
        cursor: vec![1],
        commands: vec![
            recall(200),
            SyncCommand {
                id: 777,
                payload: vec![8, 1],
            },
        ],
        continuation: false,
    };
    let payload = core
        .vault
        .seal(
            "one",
            "apply:recall-batch",
            &serde_json::to_vec(&batch).unwrap(),
        )
        .unwrap();
    store
        .stage_sync("one", "recall-batch", &payload, b"received-cursor")
        .unwrap();
    let before = store.runtime("one", "projected-cursor").unwrap();
    assert_eq!(
        core.recover_projection("one").unwrap()["pendingCommands"],
        1
    );
    assert_eq!(store.pending_sync_count("one").unwrap(), 1);
    assert_eq!(store.runtime("one", "projected-cursor").unwrap(), before);
    assert_eq!(
        store.runtime("one", "received-cursor").unwrap().unwrap(),
        b"received-cursor"
    );
    assert_eq!(store.media_sync_receipts("one").unwrap()[0].1, payload);
    core.recover_projection("one").unwrap();
    assert_eq!(recall_count(&store), 1);
}

#[test]
fn projection_failure_keeps_suppression_and_repair_never_resubmits_a_command() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Original", 200, 100),
        "original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let binding = store
        .runtime("one", "profile-write-binding")
        .unwrap()
        .unwrap();
    store.clear_runtime("one", "profile-write-binding").unwrap();
    assert!(core
        .apply_confirmed_recall(
            "one",
            "peer",
            "200",
            None,
            "local_recall",
            "confirmed-receipt"
        )
        .is_err());
    assert_eq!(store.pending_recall_projections("one").unwrap(), 1);
    assert_eq!(
        core.message("one", &id).unwrap()["body"],
        crate::recalls::RECALLED_BODY
    );
    store
        .put_runtime("one", "received-cursor", b"received")
        .unwrap();
    assert!(!store
        .stage_cursor_projection("one", b"received", b"cursor-journal")
        .unwrap());
    store
        .put_runtime("one", "profile-write-binding", &binding)
        .unwrap();
    core.reconcile_recall_markers("one").unwrap();
    assert_eq!(store.pending_recall_projections("one").unwrap(), 0);
    assert_eq!(recall_count(&store), 1);
    assert_eq!(
        core.profile_reader("one")
            .unwrap()
            .message(&crate::source_message_id(&id).unwrap(), None)
            .unwrap()
            .message_type,
        10000
    );
    // A failed recheck after an older applied receipt must become a pending
    // obligation again, rather than permitting complete profile handoff.
    store.clear_runtime("one", "profile-write-binding").unwrap();
    store
        .clear_runtime("one", "recall-reconciled-generation")
        .unwrap();
    assert!(core.reconcile_recall_markers("one").is_err());
    assert_eq!(store.pending_recall_projections("one").unwrap(), 1);
    store
        .put_runtime("one", "profile-write-binding", &binding)
        .unwrap();
    core.reconcile_recall_markers("one").unwrap();
    assert_eq!(store.pending_recall_projections("one").unwrap(), 0);
    assert_eq!(recall_count(&store), 1);
}

#[test]
fn native_official_recall_outside_quote_page_is_observed_before_embedded_fallback() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Original", 200, 100),
        "original",
        "live",
    )
    .unwrap();
    let quote=b"<msg><appmsg><type>57</type><title>Current comment</title><refermsg><type>1</type><svrid>200</svrid><content>Original</content></refermsg></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", quote, 300, 101),
        "quote",
        "live",
    )
    .unwrap();
    let (location, keys) = core.profile_location_keys("one").unwrap();
    wechat_profile::apply_recall_projection(
        &location,
        &keys,
        "peer",
        "200",
        None,
        &core.write_options("one", "official-fixture").unwrap(),
    )
    .unwrap();
    assert!(store.recall_marker("one", "peer", "200").unwrap().is_none());
    let page = core
        .catalog_page(
            "one",
            "messages",
            Some("peer"),
            &crate::CatalogQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["items"][0]["body"], "Current comment");
    assert_eq!(page["items"][0]["quote"]["state"], "recalled");
    assert!(page["items"][0]["quote"]["embeddedText"].is_null());
    assert_eq!(
        core.quoted_message("one", page["items"][0]["id"].as_str().unwrap())
            .unwrap()["state"],
        "recalled"
    );
    assert_eq!(recall_count(&store), 1);
}

#[test]
fn retained_search_pages_resolve_current_recall_state_instead_of_retaining_bodies() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(
            1,
            "peer",
            "synthetic_self",
            b"search original private body",
            200,
            100,
        ),
        "original",
        "live",
    )
    .unwrap();
    let quote=b"<msg><appmsg><type>57</type><title>search own comment</title><refermsg><type>1</type><svrid>200</svrid><content>search original private body</content></refermsg></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", quote, 300, 101),
        "quote",
        "live",
    )
    .unwrap();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"search newest", 400, 102),
        "latest",
        "live",
    )
    .unwrap();
    let page = core
        .catalog_page(
            "one",
            "messages",
            None,
            &crate::CatalogQuery {
                limit: Some(1),
                search: Some("search".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["items"][0]["serverId"], "400");
    let target = core
        .profile_reader("one")
        .unwrap()
        .messages_by_server_id("peer", "200", Some("synthetic_self"))
        .unwrap()
        .pop()
        .unwrap();
    // A failed/pending projection deliberately leaves the source body intact.
    // Neither retained views nor later reads may use it over the durable marker.
    store
        .record_recall(
            "one",
            "peer",
            "200",
            Some(&message_id(&target.id).unwrap()),
            Some(100),
            None,
            "live",
        )
        .unwrap();
    let next = core
        .catalog_page(
            "one",
            "messages",
            None,
            &crate::CatalogQuery {
                limit: Some(2),
                search: Some("search".into()),
                cursor: Some(page["nextCursor"].as_str().unwrap().into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(next["items"][0]["body"], "search own comment");
    assert_eq!(next["items"][0]["quote"]["state"], "recalled");
    assert!(next["items"][0]["quote"]["embeddedText"].is_null());
    assert_eq!(next["items"][1]["body"], crate::recalls::RECALLED_BODY);
    assert!(next["items"][1]["recall"].is_object());
    assert!(!next.to_string().contains("original private body"));
    assert_eq!(
        core.quoted_message("one", next["items"][0]["id"].as_str().unwrap())
            .unwrap()["state"],
        "recalled"
    );
    let fresh = core
        .catalog_page(
            "one",
            "messages",
            None,
            &crate::CatalogQuery {
                search: Some("original private body".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(fresh["items"].as_array().unwrap().is_empty());
    let no_title=b"<msg><appmsg><type>57</type><refermsg><type>1</type><svrid>200</svrid><content>search original private body</content></refermsg></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", no_title, 500, 103),
        "quote-without-title",
        "live",
    )
    .unwrap();
    let parent = first(&core);
    assert!(parent["body"].is_null());
    assert_eq!(parent["quote"]["state"], "recalled");
    assert!(parent["quote"]["embeddedText"].is_null());
}

#[test]
fn pending_own_replay_uses_source_tombstone_but_acknowledged_new_identity_remains_distinct() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "synthetic_self", "peer", b"Own message", 200, 100),
        "own-message",
        "live",
    )
    .unwrap();
    let reader = core.profile_reader("one").unwrap();
    let mut original = reader
        .messages_by_server_id("peer", "200", Some("synthetic_self"))
        .unwrap()
        .pop()
        .unwrap();
    let id = message_id(&original.id).unwrap();
    store
        .record_recall("one", "peer", "200", Some(&id), Some(100), None, "outgoing")
        .unwrap();
    original.server_message_id = Some("199".into());
    original.outgoing = Some(true);
    original
        .raw_fields
        .insert("status".into(), SqlValue::Integer("7".into()));
    assert_eq!(
        core.normalize_recalled("one", original.clone(), Some("synthetic_self"), &reader)
            .unwrap()["body"],
        crate::recalls::RECALLED_BODY
    );
    original
        .raw_fields
        .insert("status".into(), SqlValue::Integer("2".into()));
    assert_eq!(
        core.normalize_recalled("one", original.clone(), Some("synthetic_self"), &reader)
            .unwrap()["body"],
        "Own message"
    );
    original
        .raw_fields
        .insert("status".into(), SqlValue::Integer("7".into()));
    original.create_time += 1;
    assert_eq!(
        core.normalize_recalled("one", original, Some("synthetic_self"), &reader)
            .unwrap()["body"],
        "Own message"
    );
}

#[test]
fn bound_cached_assets_and_queued_downloads_cannot_follow_a_reused_local_id() {
    let (temporary, store, core) = fixture();
    let xml=b"<msg><appmsg><title>synthetic.pdf</title><type>6</type><md5>0123456789abcdef0123456789abcdef</md5><appattach><totallen>8260</totallen><fileext>pdf</fileext><cdnattachurl>synthetic-file-id</cdnattachurl><aeskey>01010101010101010101010101010101</aeskey><encryver>0</encryver></appattach></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", xml, 200, 100),
        "original-file",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let old = synthetic_cached_asset(
        &core,
        &store,
        core.media_assets("one", &id).unwrap().pop().unwrap(),
        b"Old retained attachment",
    );
    assert_eq!(
        core.media_content("one", &old.id).unwrap().bytes,
        b"Old retained attachment"
    );
    store
        .create_media_job(
            "one",
            "old-download",
            "download:old",
            &"a".repeat(64),
            "download",
            &json!({"assetId":old.id}),
            b"",
        )
        .unwrap();
    core.apply_sync_command("one", &recall(200), "recall-file", "live")
        .unwrap();
    let source = crate::source_message_id(&id).unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute(&format!("UPDATE {} SET server_id=201,local_type=49,status=3,real_sender_id=(SELECT rowid FROM Name2Id WHERE user_name='peer'),message_content=?1 WHERE local_id=?2",source.table),rusqlite::params![std::str::from_utf8(xml).unwrap(),source.local_id.parse::<i64>().unwrap()]).unwrap();
    assert_eq!(core.message("one", &id).unwrap()["serverId"], "201");
    assert!(core.message("one", &id).unwrap().get("recall").is_none());
    assert_eq!(
        core.media_asset("one", &old.id).unwrap_err().to_string(),
        "message_recalled"
    );
    assert_eq!(
        core.media_content("one", &old.id)
            .err()
            .unwrap()
            .to_string(),
        "message_recalled"
    );
    assert_eq!(
        core.perform_download("one", "old-download")
            .unwrap_err()
            .to_string(),
        "message_recalled"
    );
    assert_eq!(
        store.media_job("one", "old-download").unwrap().phase,
        "queued"
    );
    assert_eq!(
        core.start_media_transfer(
            "one",
            crate::MediaTransferRequest {
                message_id: id.clone(),
                asset_id: Some(old.id.clone()),
                quality: crate::MediaVariant::Best
            }
        )
        .unwrap_err()
        .to_string(),
        "message_recalled"
    );
    let new = core.media_assets("one", &id).unwrap().pop().unwrap();
    assert_ne!(new.id, old.id);
    let new = synthetic_cached_asset(&core, &store, new, b"New occupant attachment");
    assert_eq!(
        core.media_content("one", &new.id).unwrap().bytes,
        b"New occupant attachment"
    );
    store
        .create_media_job(
            "one",
            "new-download",
            "download:new",
            &"b".repeat(64),
            "download",
            &json!({"assetId":new.id}),
            b"",
        )
        .unwrap();
    core.perform_download("one", "new-download").unwrap();
    assert_eq!(
        store.media_job("one", "new-download").unwrap().state,
        "available"
    );
    // A non-recalled handle is still invalid after a later unrelated occupant.
    db.execute(
        &format!(
            "UPDATE {} SET server_id=202 WHERE local_id=?1",
            source.table
        ),
        [source.local_id.parse::<i64>().unwrap()],
    )
    .unwrap();
    assert_eq!(
        core.media_content("one", &new.id)
            .err()
            .unwrap()
            .to_string(),
        "media_source_changed"
    );
    db.execute(
        &format!("DELETE FROM {} WHERE local_id=?1", source.table),
        [source.local_id.parse::<i64>().unwrap()],
    )
    .unwrap();
    assert_eq!(
        core.media_content("one", &new.id)
            .err()
            .unwrap()
            .to_string(),
        "media_source_changed"
    );
}

#[test]
fn captured_quoted_target_blocks_old_asset_even_after_parent_disappears() {
    let (temporary, store, core) = fixture();
    let image="<msg><img cdnmidimgurl='synthetic-image' aeskey='01010101010101010101010101010101' length='12'/></msg>";
    core.apply_sync_command(
        "one",
        &add(3, "peer", "synthetic_self", image.as_bytes(), 200, 100),
        "image",
        "live",
    )
    .unwrap();
    let quote=format!("<msg><appmsg><type>57</type><title>Own comment</title><refermsg><type>3</type><svrid>200</svrid><content><![CDATA[{image}]]></content></refermsg></appmsg></msg>");
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", quote.as_bytes(), 300, 101),
        "quoted-image",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let asset = synthetic_cached_asset(
        &core,
        &store,
        core.media_assets("one", &id).unwrap().pop().unwrap(),
        b"Quoted retained image",
    );
    core.apply_sync_command("one", &recall(200), "image-recall", "live")
        .unwrap();
    let source = crate::source_message_id(&id).unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute(
        &format!("DELETE FROM {} WHERE local_id=?1", source.table),
        [source.local_id.parse::<i64>().unwrap()],
    )
    .unwrap();
    assert_eq!(
        core.media_content("one", &asset.id)
            .err()
            .unwrap()
            .to_string(),
        "message_recalled"
    );
    assert_eq!(
        core.media_asset("one", &asset.id).unwrap_err().to_string(),
        "message_recalled"
    );
}

#[test]
fn exact_message_recall_survives_removed_peer_metadata_without_masking_a_new_server_id() {
    let (temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Private original", 200, 100),
        "original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    let source = crate::source_message_id(&id).unwrap();
    store
        .record_recall("one", "peer", "200", Some(&id), Some(100), None, "live")
        .unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute("DELETE FROM Name2Id", []).unwrap();
    synthetic_database(&core, &temporary, "contact/contact.db")
        .execute("DELETE FROM contact", [])
        .unwrap();
    assert!(core
        .profile_reader("one")
        .unwrap()
        .message(&source, None)
        .unwrap()
        .peer
        .is_none());
    let recalled = core.message("one", &id).unwrap();
    assert_eq!(recalled["body"], crate::recalls::RECALLED_BODY);
    assert_eq!(recalled["threadId"], "peer");
    assert_eq!(
        core.media_assets("one", &id).unwrap_err().to_string(),
        "message_recalled"
    );
    db.execute(
        &format!(
            "UPDATE {} SET server_id=201 WHERE local_id=?1",
            source.table
        ),
        [source.local_id.parse::<i64>().unwrap()],
    )
    .unwrap();
    assert_eq!(
        core.message("one", &id).unwrap()["body"],
        "Private original"
    );
}

#[test]
fn recall_api_author_evidence_is_exact_and_conflicts_remain_unknown() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"original", 123, 100),
        "original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    core.apply_sync_command("one", &recall(123), "recall", "live")
        .unwrap();
    // A cleared row and a human-readable notice cannot establish ownership.
    assert!(core.message("one", &id).unwrap()["senderId"].is_null());
    for (key, command) in [
        (
            "wrong-time",
            add(1, "peer", "synthetic_self", b"other", 123, 101),
        ),
        (
            "wrong-server",
            add(1, "peer", "synthetic_self", b"other", 124, 100),
        ),
        (
            "wrong-thread",
            add(1, "different", "synthetic_self", b"other", 123, 100),
        ),
        (
            "wrong-recipient",
            add(1, "peer", "foreign", b"other", 123, 100),
        ),
    ] {
        retain_original(&core, &store, key, command);
        assert!(
            core.message("one", &id).unwrap()["senderId"].is_null(),
            "{key}"
        );
    }
    retain_original(
        &core,
        &store,
        "matching",
        add(1, "peer", "synthetic_self", b"original", 123, 100),
    );
    let events = store.events("one", 0, 200).unwrap().len();
    let markers = store.recall_markers("one").unwrap();
    let revision = store.recall_sync_evidence_revision("one").unwrap();
    let evidence = store.recall_sync_evidence("one").unwrap();
    for _ in 0..2 {
        let value = core.message("one", &id).unwrap();
        assert_eq!(value["senderId"], "peer");
        assert_eq!(value["direction"], "incoming");
    }
    assert_eq!(store.events("one", 0, 200).unwrap().len(), events);
    assert_eq!(
        serde_json::to_value(store.recall_markers("one").unwrap()).unwrap(),
        serde_json::to_value(markers).unwrap()
    );
    assert_eq!(
        store.recall_sync_evidence_revision("one").unwrap(),
        revision
    );
    assert_eq!(store.recall_sync_evidence("one").unwrap(), evidence);
    let raw = core
        .profile_reader("one")
        .unwrap()
        .message(&crate::source_message_id(&id).unwrap(), None)
        .unwrap();
    assert_eq!(raw.sender.as_deref(), Some(""));
    retain_original(
        &core,
        &store,
        "contradiction",
        add(
            1,
            "synthetic_self",
            "peer",
            b"contradictory original",
            123,
            100,
        ),
    );
    let value = core.message("one", &id).unwrap();
    assert!(value["senderId"].is_null());
    assert!(value["direction"].is_null());
}

#[test]
fn settled_official_notice_is_read_on_each_api_request() {
    let (temporary, _store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"original", 123, 100),
        "original",
        "live",
    )
    .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    core.apply_sync_command("one", &recall(123), "recall", "live")
        .unwrap();
    assert_eq!(
        core.message("one", &id).unwrap()["recall"]["replacementText"],
        "Localized recall notice"
    );
    let source = crate::source_message_id(&id).unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute(&format!("UPDATE {} SET message_content=?1 WHERE local_id=?2", source.table), rusqlite::params!["<sysmsg type='revokemsg'><revokemsg><content>Official profile notice</content><revoketime>0</revoketime></revokemsg></sysmsg>", source.local_id.parse::<i64>().unwrap()]).unwrap();
    drop(db);
    core.reconcile_recall_markers("one").unwrap();
    assert_eq!(
        core.message("one", &id).unwrap()["recall"]["replacementText"],
        "Official profile notice"
    );
}

#[test]
fn unavailable_optional_author_evidence_still_returns_a_redacted_recall() {
    let (_temporary, store, core) = fixture();
    let original = add(1, "peer", "synthetic_self", b"private body", 123, 100);
    retain_original(&core, &store, "original-batch", original.clone());
    core.apply_sync_command("one", &original, "original", "live")
        .unwrap();
    let id = first(&core)["id"].as_str().unwrap().to_owned();
    core.apply_sync_command("one", &recall(123), "recall", "live")
        .unwrap();
    assert_eq!(core.message("one", &id).unwrap()["senderId"], "peer");
    store
        .create_media_job(
            "one",
            "oversized-evidence",
            "send:oversized",
            &"a".repeat(64),
            "send",
            &json!({"threadId":"peer", "serverMessageId":"123"}),
            &vec![0; 4 * 1024 * 1024 + 1],
        )
        .unwrap();
    let mut row = store.media_job("one", "oversized-evidence").unwrap();
    row.state = "accepted".into();
    store.save_media_job("one", &row).unwrap();
    let value = core.message("one", &id).unwrap();
    assert_eq!(value["body"], crate::recalls::RECALLED_BODY);
    assert!(value["senderId"].is_null());
    assert!(value["direction"].is_null());
    assert_eq!(
        value["recall"]["replacementText"],
        "Localized recall notice"
    );
    assert!(!value.to_string().contains("private body"));
}

#[test]
fn quote_normalization_reuses_the_request_snapshot_and_still_checks_recall_receipts() {
    let (_temporary, store, core) = fixture();
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Original", 123, 100),
        "original",
        "live",
    )
    .unwrap();
    let xml = b"<msg><appmsg><type>57</type><title>Reply</title><refermsg><type>1</type><svrid>123</svrid><content>Original</content></refermsg></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", xml, 124, 101),
        "reply",
        "live",
    )
    .unwrap();
    let reader = core.profile_reader("one").unwrap();
    let parent = reader
        .messages_by_server_id("peer", "124", Some("synthetic_self"))
        .unwrap()
        .pop()
        .unwrap();
    let (location, _) = core.profile_location_keys("one").unwrap();
    let source = location.root().join("db_storage");
    let moved = location.root().join("request-source-unavailable");
    std::fs::rename(&source, &moved).unwrap();
    // Once captured, this request can resolve quotes without touching the
    // source again. Recall receipts remain live suppression evidence.
    let first = core.normalize_recalled("one", parent.clone(), Some("synthetic_self"), &reader);
    store
        .record_recall("one", "peer", "123", None, None, None, "live")
        .unwrap();
    let recalled = core.normalize_recalled("one", parent, Some("synthetic_self"), &reader);
    std::fs::rename(moved, source).unwrap();
    let first = first.unwrap();
    assert_eq!(first["quote"]["serverId"], "123");
    assert_eq!(first["version"], reader.generation());
    let recalled = recalled.unwrap();
    assert_eq!(recalled["quote"]["state"], "recalled");
    assert!(recalled["quote"]["embeddedText"].is_null());
    assert_eq!(recalled["body"], "Reply");
}

#[test]
fn unfiltered_history_keeps_membership_while_search_and_later_pages_resolve_content() {
    for thread in [Some("peer"), None] {
        let (_temporary, _store, core) = fixture();
        for (index, command) in [
            add(1, "peer", "synthetic_self", b"Original", 123, 100),
            add(49, "peer", "synthetic_self", b"<msg><appmsg><type>57</type><title>Needle reply</title><refermsg><type>1</type><svrid>123</svrid><content>Original</content></refermsg></appmsg></msg>", 124, 101),
            add(1, "peer", "synthetic_self", b"Newest", 125, 102),
        ].iter().enumerate() {
            core.apply_sync_command("one", command, &format!("seed-{index}"), "live").unwrap();
        }
        let first = core
            .catalog_page(
                "one",
                "messages",
                thread,
                &crate::CatalogQuery {
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(first["items"][0]["serverId"], "125");
        assert_eq!(first["items"].as_array().unwrap().len(), 1);
        let search = core
            .catalog_page(
                "one",
                "messages",
                thread,
                &crate::CatalogQuery {
                    limit: Some(1),
                    search: Some("Needle".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(search["items"][0]["serverId"], "124");
        assert_eq!(search["items"][0]["body"], "Needle reply");
        assert_eq!(search["items"][0]["quote"]["serverId"], "123");
        core.apply_confirmed_recall_with_notice(
            "one",
            "peer",
            "124",
            None,
            Some("Server recall notice"),
            "live",
            "between-pages",
        )
        .unwrap();
        core.apply_sync_command(
            "one",
            &add(1, "peer", "synthetic_self", b"Late arrival", 126, 200),
            "late",
            "live",
        )
        .unwrap();
        let next = core
            .catalog_page(
                "one",
                "messages",
                thread,
                &crate::CatalogQuery {
                    limit: Some(2),
                    cursor: Some(first["nextCursor"].as_str().unwrap().into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(next["items"][0]["serverId"], "124");
        assert_eq!(next["items"][0]["body"], crate::recalls::RECALLED_BODY);
        assert_eq!(
            next["items"][0]["recall"]["replacementText"],
            "Server recall notice"
        );
        assert_eq!(next["items"][1]["serverId"], "123");
        assert_eq!(next["items"][1]["body"], "Original");
        assert!(next["nextCursor"].is_null());
    }
}

#[test]
fn history_does_not_load_off_page_bodies() {
    let (temporary, _store, core) = fixture();
    for (server, time) in [(123, 100), (124, 101)] {
        core.apply_sync_command(
            "one",
            &add(1, "peer", "synthetic_self", b"body", server, time),
            &format!("seed-{server}"),
            "live",
        )
        .unwrap();
    }
    let newest = first(&core);
    let source = crate::source_message_id(newest["id"].as_str().unwrap()).unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute(
        &format!(
            "UPDATE {} SET message_content=zeroblob(17825792) WHERE server_id=123",
            source.table
        ),
        [],
    )
    .unwrap();
    drop(db);
    let page = core
        .catalog_page(
            "one",
            "messages",
            None,
            &crate::CatalogQuery {
                limit: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(page["items"][0]["serverId"], "124");
    assert_eq!(page["items"][0]["body"], "body");
    // The oversized row still fails when actually requested; its contents must
    // not be silently truncated, dropped from membership, or cached as a body.
    assert!(core
        .catalog_page(
            "one",
            "messages",
            None,
            &crate::CatalogQuery {
                limit: Some(1),
                cursor: Some(page["nextCursor"].as_str().unwrap().into()),
                ..Default::default()
            }
        )
        .is_err());
}

/// Opt-in timing gate for a quiet release builder. Ordinary CI exercises the
/// deterministic off-page-body and snapshot-reuse regressions above instead.
#[test]
#[ignore = "run on a quiet release builder; see FEATURES.md"]
fn history_latency_benchmark() {
    if cfg!(debug_assertions) {
        eprintln!("Use --release for comparable history timings.");
    }
    let (temporary, _store, core) = fixture();
    let quote = b"<msg><appmsg><type>57</type><title>Benchmark reply</title><refermsg><type>1</type><svrid>123</svrid><content>Original</content></refermsg></appmsg></msg>";
    core.apply_sync_command(
        "one",
        &add(1, "peer", "synthetic_self", b"Original", 123, 100),
        "original",
        "live",
    )
    .unwrap();
    core.apply_sync_command(
        "one",
        &add(49, "peer", "synthetic_self", quote, 124, 101),
        "quote",
        "live",
    )
    .unwrap();
    let source = crate::source_message_id(first(&core)["id"].as_str().unwrap()).unwrap();
    let db = synthetic_database(&core, &temporary, &source.database);
    db.execute_batch(&format!("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<2000) INSERT INTO {}(server_id,local_type,real_sender_id,create_time,message_content,source,WCDB_CT_message_content,WCDB_CT_source) SELECT 1000+x,local_type,real_sender_id,101+x,message_content,source,WCDB_CT_message_content,WCDB_CT_source FROM {},n WHERE server_id=124",source.table,source.table)).unwrap();
    drop(db);
    let budget = std::env::var("WECHAT_HISTORY_P95_MS")
        .map(|s| s.parse::<u128>().expect("millisecond budget"))
        .unwrap_or(100);
    for thread in [Some("peer"), None] {
        let query = crate::CatalogQuery {
            limit: Some(20),
            ..Default::default()
        };
        core.catalog_page("one", "messages", thread, &query)
            .unwrap();
        let mut timings = Vec::new();
        for _ in 0..20 {
            let started = std::time::Instant::now();
            let first = core
                .catalog_page("one", "messages", thread, &query)
                .unwrap();
            timings.push(started.elapsed().as_micros());
            assert_eq!(first["items"].as_array().unwrap().len(), 20);
            assert_eq!(first["items"][0]["quote"]["serverId"], "123");
        }
        timings.sort_unstable();
        eprintln!(
            "history scope={} rows=2002 page=20 samples=20 p50_us={} p95_us={}",
            thread.unwrap_or("account"),
            timings[9],
            timings[18]
        );
        assert!(
            timings[18] < budget * 1000,
            "history p95 exceeded {budget} ms"
        );
    }
}
