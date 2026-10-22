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
#[test]
fn recalled_share_redacts_structured_metadata_and_blocks_its_thumbnail() {
    let (_temporary, _store, core) = fixture();
    let original = add(49, "peer", "synthetic_self", include_bytes!("../tests/fixtures/shared-link.xml"), 123, 100);
    core.apply_sync_command("one", &original, "original-share", "live").unwrap();
    let before = first(&core);
    let id = before["id"].as_str().unwrap();
    assert_eq!(before["link"]["title"], "Title & preview");
    assert_eq!(core.media_assets("one", id).unwrap().len(), 1);
    core.apply_sync_command("one", &recall(123), "recall-share", "live").unwrap();
    let after = core.message("one", id).unwrap();
    assert!(after["link"].is_null());
    assert_eq!(after["body"], crate::recalls::RECALLED_BODY);
    assert!(!after.to_string().contains("Title & preview"));
    assert!(core.media_assets("one", id).unwrap_err().to_string().contains("message_recalled"));
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
    assert!(parent["quote"].is_null());
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
    let page = core.messages("one", "peer", Some(1), None).unwrap();
    assert_eq!(page["items"][0]["body"], "Current comment");
    assert!(page["items"][0]["quote"].is_null());
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
    assert!(next["items"][0]["quote"].is_null());
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
    assert!(parent["quote"].is_null());
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
        core.normalize_recalled(
            "one",
            original.clone(),
            Some("synthetic_self"),
            reader.generation()
        )
        .unwrap()["body"],
        crate::recalls::RECALLED_BODY
    );
    original
        .raw_fields
        .insert("status".into(), SqlValue::Integer("2".into()));
    assert_eq!(
        core.normalize_recalled(
            "one",
            original.clone(),
            Some("synthetic_self"),
            reader.generation()
        )
        .unwrap()["body"],
        "Own message"
    );
    original
        .raw_fields
        .insert("status".into(), SqlValue::Integer("7".into()));
    original.create_time += 1;
    assert_eq!(
        core.normalize_recalled("one", original, Some("synthetic_self"), reader.generation())
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
