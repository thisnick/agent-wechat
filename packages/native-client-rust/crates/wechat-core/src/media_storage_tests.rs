//! Where attachment bytes live, how earlier databases are read and moved, and
//! what is cleaned up. All values are synthetic.
use super::*;

fn media_files(dir: &tempfile::TempDir) -> Vec<String> {
    let path = dir.path().join("data/accounts/one/agent/media");
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}
fn control(dir: &tempfile::TempDir) -> rusqlite::Connection {
    rusqlite::Connection::open(dir.path().join("data/accounts/one/agent/control.sqlite")).unwrap()
}
/// An account with a managed official profile that accepts writes.
pub(super) fn profile_core() -> (tempfile::TempDir, Arc<Core>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(wechat_store::Store::open(&dir.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store, None).unwrap();
    let mut state = tests::accepted_media_state();
    state.device.device_id = vec![b'A'; 16];
    state.uin = 7;
    state.cookie = vec![1; 15];
    state.auto_auth_key = wechat_protocol::protobuf::message(&[
        wechat_protocol::protobuf::bytes(1, &wechat_protocol::protobuf::buffer(b"synthetic-a")),
        wechat_protocol::protobuf::bytes(2, &wechat_protocol::protobuf::buffer(b"synthetic-b")),
    ])
    .unwrap();
    state.key_info = vec![4; 180];
    state.database_password = vec![42; 32];
    core.test_managed_profile("one", &state).unwrap();
    (dir, core)
}
/// An attachment the official profile has no place for.
fn sidecar_asset(core: &Core, id: &str, bytes: &[u8]) -> MediaAsset {
    let asset: MediaAsset = serde_json::from_value(json!({
        "id":id, "accountId":"one", "kind":"sticker", "variant":"original",
        "state":"pending", "retryable":true, "officialWriteback":"unsupported"
    }))
    .unwrap();
    core.store
        .put_media_asset(
            "one",
            &MediaAssetRecord {
                id: id.into(),
                message_id: None,
                variant: "original".into(),
                kind: "sticker".into(),
                metadata: serde_json::to_value(&asset).unwrap(),
                plan: vec![],
                payload: None,
            },
        )
        .unwrap();
    core.cache_asset(
        "one",
        id,
        bytes,
        "image/png",
        Some(MediaVariant::Original),
        None,
    )
    .unwrap()
}
/// A voice note that exists in the official profile, with its two attachments.
fn profile_voice(core: &Core) -> (Vec<u8>, Vec<MediaAsset>) {
    let pcm: Vec<i16> = (0..16_000)
        .map(|n| ((n % 80) * 200 - 8000) as i16)
        .collect();
    let encoded = wechat_protocol::silk::encode(&pcm).unwrap();
    let content = wechat_protocol::media_send::local_voice_xml(encoded.len(), 1000).unwrap();
    let receipt = "storage-test";
    let source = core
        .project_accepted_media(
            "one",
            "filehelper",
            34,
            &content,
            "",
            1_700_000_000,
            "9000000000000000001",
            receipt,
        )
        .unwrap();
    let options = core.write_options("one", receipt).unwrap();
    let (location, keys) = core.profile_location_keys("one").unwrap();
    wechat_profile::cache_voice_projection(
        &location,
        &keys,
        &source,
        "filehelper",
        &encoded,
        &options,
    )
    .unwrap();
    let message = crate::message_id(&source).unwrap();
    let assets = core.media_assets("one", &message).unwrap();
    (encoded, assets)
}

/// Cache both attachments of the voice note and record the profile write, as
/// the send and download paths do, without releasing this store's copy.
fn cache_complete(
    core: &Core,
    encoded: &[u8],
    assets: &[MediaAsset],
) -> Vec<(MediaAsset, Vec<u8>)> {
    assets
        .iter()
        .map(|asset| {
            let (bytes, mime) = if asset.variant == MediaVariant::Encoded {
                (encoded.to_vec(), "audio/silk")
            } else {
                (
                    wechat_protocol::silk::wav(&wechat_protocol::silk::decode(encoded).unwrap())
                        .unwrap(),
                    "audio/wav",
                )
            };
            let mut cached = core
                .cache_asset("one", &asset.id, &bytes, mime, Some(asset.variant), None)
                .unwrap();
            cached.official_writeback = "complete".into();
            core.store
                .save_media_asset(
                    "one",
                    &asset.id,
                    &serde_json::to_value(&cached).unwrap(),
                    None,
                )
                .unwrap();
            (cached, bytes)
        })
        .collect()
}
/// Rewrite the account database as the version before payload files wrote it:
/// every payload inside its row, no payload files and the earlier schema.
fn downgrade_to_embedded_payloads(dir: &tempfile::TempDir) {
    let db = control(dir);
    let media = dir.path().join("data/accounts/one/agent/media");
    for (table, cleared) in [("media_assets", false), ("media_sources", true)] {
        let rows: Vec<(String, String)> = db
            .prepare(&format!(
                "SELECT id,payload_file FROM {table} WHERE payload_file IS NOT NULL"
            ))
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for (id, file) in rows {
            let sealed = std::fs::read(media.join(&file)).unwrap();
            db.execute(
                &format!("UPDATE {table} SET payload=?2,payload_file=NULL WHERE id=?1"),
                rusqlite::params![id, sealed],
            )
            .unwrap();
            std::fs::remove_file(media.join(file)).unwrap();
        }
        let _ = cleared;
    }
    db.execute_batch(
        "UPDATE media_assets SET profile_key=NULL;ALTER TABLE media_assets DROP COLUMN payload_file;ALTER TABLE media_assets DROP COLUMN profile_key;ALTER TABLE media_sources DROP COLUMN payload_file;PRAGMA user_version=12;",
    )
    .unwrap();
}
fn reopen(dir: &tempfile::TempDir) -> Arc<Core> {
    Core::new(
        Arc::new(wechat_store::Store::open_existing(&dir.path().join("data")).unwrap()),
        None,
    )
    .unwrap()
}
fn error_code<T>(result: Result<T>) -> String {
    result.err().unwrap().to_string()
}

#[test]
fn attachment_written_to_the_profile_is_served_from_it_without_a_second_copy() {
    let (dir, core) = profile_core();
    let (encoded, assets) = profile_voice(&core);
    assert_eq!(assets.len(), 2);
    let cached = cache_complete(&core, &encoded, &assets);
    // Until the profile is confirmed to return the bytes, a copy is kept here.
    assert_eq!(media_files(&dir).len(), 2);
    for (asset, bytes) in &cached {
        assert!(core.release_to_profile("one", asset, bytes).unwrap());
        assert_eq!(
            core.store
                .media_asset_profile_key("one", &asset.id)
                .unwrap()
                .as_deref(),
            Some("message")
        );
        assert!(core
            .store
            .media_asset_with_payload("one", &asset.id)
            .unwrap()
            .payload
            .is_none());
        let content = core.media_content("one", &asset.id).unwrap();
        assert_eq!(&content.bytes, bytes);
        assert_eq!(content.sha256, digest(bytes));
    }
    assert!(media_files(&dir).is_empty());
    let ids: Vec<String> = cached.iter().map(|(asset, _)| asset.id.clone()).collect();
    drop(core);
    let core = reopen(&dir);
    for ((_, bytes), id) in cached.iter().zip(&ids) {
        assert_eq!(&core.media_content("one", id).unwrap().bytes, bytes);
    }
    // Bytes that no longer match what was verified are refused, not replaced.
    let mut changed = core.media_asset("one", &ids[0]).unwrap();
    changed.sha256 = Some("0".repeat(64));
    core.store
        .save_media_asset(
            "one",
            &ids[0],
            &serde_json::to_value(&changed).unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(
        error_code(core.media_content("one", &ids[0])),
        "media_integrity_failed"
    );
}

#[test]
fn a_copy_is_kept_when_the_profile_cannot_return_the_bytes() {
    let (dir, core) = profile_core();
    let (encoded, assets) = profile_voice(&core);
    let cached = cache_complete(&core, &encoded, &assets);
    let (asset, bytes) = &cached[0];
    // Not written to the profile yet, different bytes, or a different copy.
    let mut pending = asset.clone();
    pending.official_writeback = "blocked".into();
    assert!(!core.release_to_profile("one", &pending, bytes).unwrap());
    assert!(!core
        .release_to_profile("one", asset, b"other bytes")
        .unwrap());
    let mut other = asset.clone();
    other.actual_variant = None;
    assert!(!core.release_to_profile("one", &other, bytes).unwrap());
    // The profile holds different bytes than this attachment recorded.
    let sticker = sidecar_asset(&core, "sticker", b"synthetic sticker");
    let mut claimed = sticker.clone();
    claimed.kind = MediaKind::Voice;
    claimed.variant = asset.variant;
    claimed.actual_variant = Some(asset.variant);
    claimed.message_id = asset.message_id.clone();
    claimed.official_writeback = "complete".into();
    assert!(!core
        .release_to_profile("one", &claimed, b"synthetic sticker")
        .unwrap());
    assert_eq!(media_files(&dir).len(), 3);
    assert_eq!(
        core.media_content("one", "sticker").unwrap().bytes,
        b"synthetic sticker"
    );
    // A recorded profile location whose bytes are gone is an error.
    core.store
        .save_media_asset_in_profile(
            "one",
            "sticker",
            &serde_json::to_value(&sticker).unwrap(),
            "message",
        )
        .unwrap();
    assert_eq!(
        error_code(core.media_content("one", "sticker")),
        "media_not_cached"
    );
}

#[test]
fn media_without_a_profile_location_keeps_one_encrypted_file_and_a_deleted_file_is_an_error() {
    let (dir, core) = tests::test_core();
    let raw = b"synthetic sticker bytes that must not appear on disk";
    sidecar_asset(&core, "sticker", raw);
    let first = media_files(&dir);
    assert_eq!(first.len(), 1);
    let media = dir.path().join("data/accounts/one/agent/media");
    let stored = std::fs::read(media.join(&first[0])).unwrap();
    assert!(!stored.windows(raw.len()).any(|window| window == raw));
    assert_eq!(core.media_content("one", "sticker").unwrap().bytes, raw);
    // Nothing is written into the database row.
    let embedded: i64 = control(&dir)
        .query_row(
            "SELECT count(*) FROM media_assets WHERE payload IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(embedded, 0);
    // Caching again replaces the file instead of adding one.
    core.cache_asset(
        "one",
        "sticker",
        raw,
        "image/png",
        Some(MediaVariant::Original),
        None,
    )
    .unwrap();
    let second = media_files(&dir);
    assert_eq!(second.len(), 1);
    assert_ne!(first, second);
    let source = core
        .upload_source("one", "notes.txt", "text/plain", raw)
        .unwrap();
    assert_eq!(media_files(&dir).len(), 2);
    assert_eq!(core.source_bytes("one", &source.id).unwrap().1, raw);
    // A deleted file is reported; the rows and the other account are untouched.
    for name in media_files(&dir) {
        std::fs::remove_file(media.join(name)).unwrap();
    }
    assert_eq!(
        error_code(core.media_content("one", "sticker")),
        "media_not_cached"
    );
    assert_eq!(
        error_code(core.source_bytes("one", &source.id)),
        "media_not_cached"
    );
    assert_eq!(
        core.media_asset("one", "sticker").unwrap().state,
        "available"
    );
    assert!(core.store.media_source_metadata("one", &source.id).is_ok());
    assert_eq!(core.maintain_media_storage("one").unwrap()["sweptFiles"], 0);
}

#[test]
fn earlier_database_payloads_load_unchanged_then_move_to_files_or_the_profile() {
    let (dir, core) = profile_core();
    let (encoded, assets) = profile_voice(&core);
    let cached = cache_complete(&core, &encoded, &assets);
    let sticker = b"synthetic sticker";
    sidecar_asset(&core, "sticker", sticker);
    let upload = b"synthetic upload";
    let source = core
        .upload_source("one", "notes.txt", "text/plain", upload)
        .unwrap();
    // Earlier versions stored each image preview as an upload.
    let preview = format!("{WXGF_PREVIEW_PREFIX}{}", "a".repeat(64));
    core.store
        .create_media_source(
            "one",
            &preview,
            "p.jpg",
            "image/jpeg",
            7,
            &"b".repeat(64),
            b"preview",
        )
        .unwrap();
    drop(core);
    downgrade_to_embedded_payloads(&dir);
    assert!(media_files(&dir).is_empty());

    // The earlier database opens, migrates and serves every payload as it is.
    let core = reopen(&dir);
    let version: i64 = control(&dir)
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 13);
    let check = |core: &Core| {
        for (asset, bytes) in &cached {
            assert_eq!(&core.media_content("one", &asset.id).unwrap().bytes, bytes);
        }
        assert_eq!(core.media_content("one", "sticker").unwrap().bytes, sticker);
        assert_eq!(core.source_bytes("one", &source.id).unwrap().1, upload);
    };
    check(&core);
    assert!(media_files(&dir).is_empty());
    assert_eq!(
        core.store
            .embedded_media_payloads("one", 100)
            .unwrap()
            .len(),
        5
    );

    let report = core.maintain_media_storage("one").unwrap();
    assert_eq!(report["releasedToProfile"], 2);
    assert_eq!(report["movedPayloads"], 2);
    assert_eq!(report["removedPreviews"], 1);
    assert_eq!(report["removedUploads"], 0);
    assert_eq!(report["sweptFiles"], 0);
    check(&core);
    // The sticker and the upload each have a file; the voice note has none.
    assert_eq!(media_files(&dir).len(), 2);
    assert!(core
        .store
        .embedded_media_payloads("one", 100)
        .unwrap()
        .is_empty());
    let db = control(&dir);
    let embedded: i64 = db
        .query_row(
            "SELECT (SELECT count(*) FROM media_assets WHERE payload IS NOT NULL)+(SELECT count(*) FROM media_sources WHERE length(payload)>0)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(embedded, 0);
    assert!(core.store.media_source_metadata("one", &preview).is_err());
    drop(db);

    // A second pass and a restart change nothing.
    let again = core.maintain_media_storage("one").unwrap();
    for key in [
        "releasedToProfile",
        "movedPayloads",
        "removedPreviews",
        "sweptFiles",
    ] {
        assert_eq!(again[key], 0, "{key}");
    }
    drop(core);
    let core = reopen(&dir);
    check(&core);
}

#[test]
fn finished_send_uploads_are_deleted_after_a_day_and_needed_or_unused_ones_stay() {
    let (dir, core) = tests::test_core();
    let upload = |name: &str| {
        core.upload_source("one", name, "text/plain", name.as_bytes())
            .unwrap()
            .id
    };
    let job = |id: &str,
               source: &str,
               thumbnail: Option<&str>,
               state: &str,
               result: serde_json::Value,
               aged: bool| {
        let mut plan = tests::file_plan();
        plan.request.source_id = source.into();
        plan.request.thumbnail_source_id = thumbnail.map(str::to_owned);
        tests::plan_job(&core, id, &plan);
        let mut row = core.store.media_job("one", id).unwrap();
        row.state = state.into();
        row.retryable = false;
        row.data = json!({"sourceId":source,"result":result});
        core.store.save_media_job("one", &row).unwrap();
        if aged {
            control(&dir)
                .execute(
                    "UPDATE media_jobs SET updated_at='2020-01-01T00:00:00+00:00' WHERE id=?1",
                    [id],
                )
                .unwrap();
        }
    };
    let complete = json!({"profileProjection":"complete","officialMediaWriteback":"complete"});
    let (sent, thumbnail, failed) = (upload("sent"), upload("thumbnail"), upload("failed"));
    job(
        "sent",
        &sent,
        Some(&thumbnail),
        "accepted",
        complete.clone(),
        true,
    );
    job("failed", &failed, None, "failed", json!({}), true);
    // Finished moments ago: a client may still reuse the source ID.
    let recent = upload("recent");
    job("recent", &recent, None, "accepted", complete.clone(), false);
    // Accepted, but the profile write has not completed.
    let unwritten = upload("unwritten");
    job(
        "unwritten",
        &unwritten,
        None,
        "accepted",
        json!({"profileProjection":"complete","officialMediaWriteback":"blocked"}),
        true,
    );
    // One finished job and one that still needs review share an upload.
    let shared = upload("shared");
    job("shared-done", &shared, None, "accepted", complete, true);
    job(
        "shared-review",
        &shared,
        None,
        "needs_review",
        json!({}),
        true,
    );
    let unused = upload("unused");

    let report = core.maintain_media_storage("one").unwrap();
    assert_eq!(report["removedUploads"], 3);
    for id in [&sent, &thumbnail, &failed] {
        assert!(core.store.media_source_metadata("one", id).is_err());
    }
    for id in [&recent, &unwritten, &shared, &unused] {
        assert_eq!(core.source_bytes("one", id).unwrap().0.id, *id);
    }
    assert_eq!(media_files(&dir).len(), 4);
    assert_eq!(
        core.maintain_media_storage("one").unwrap()["removedUploads"],
        0
    );
}

#[test]
fn files_that_nothing_refers_to_are_removed_and_others_are_left_alone() {
    let (dir, core) = tests::test_core();
    sidecar_asset(&core, "sticker", b"synthetic sticker");
    let media = dir.path().join("data/accounts/one/agent/media");
    // Left by a crash between writing a file and committing its row.
    std::fs::write(media.join(format!("{}.bin", "c".repeat(32))), b"orphan").unwrap();
    std::fs::write(media.join(".tmp-unfinished"), b"partial").unwrap();
    std::fs::write(media.join("README.txt"), b"not ours").unwrap();
    assert_eq!(core.maintain_media_storage("one").unwrap()["sweptFiles"], 2);
    let left = media_files(&dir);
    assert_eq!(left.len(), 2);
    assert!(left.contains(&"README.txt".to_string()));
    assert_eq!(
        core.media_content("one", "sticker").unwrap().bytes,
        b"synthetic sticker"
    );
}

#[test]
fn a_sent_file_is_read_back_from_the_profile_and_its_upload_is_then_released() {
    let (dir, core) = profile_core();
    let bytes = b"synthetic report contents".repeat(64);
    let source = core
        .upload_source("one", "report.txt", "text/plain", &bytes)
        .unwrap();
    let mut plan = tests::file_plan();
    plan.request.source_id = source.id.clone();
    initialize_file_upload_ids(&mut plan, bytes.len()).unwrap();
    plan.material = Some(UploadMaterial {
        key: vec![4; 16],
        file_key: "5".repeat(32),
        task_id: 17,
    });
    plan.reservation = Some(tests::reservation());
    let md5 = hex_string(&wechat_protocol::crypto::md5(&bytes));
    plan.uploaded_file = Some(UploadedMedia {
        descriptor: MediaDescriptor {
            file_id: "synthetic-owned-object".into(),
            aes_key: vec![4; 16],
            quality: file_upload_quality(bytes.len()),
            expected_bytes: Some(bytes.len() as u64),
            expected_md5: Some(md5.clone()),
            strict_size: true,
            expires_at: None,
        },
        plaintext_bytes: bytes.len() as u32,
        md5: md5.clone(),
        sha256: source.sha256.clone(),
        file_name: source.name.clone(),
        width: None,
        height: None,
    });
    tests::plan_job(&core, "send", &plan);
    let mut row = core.store.media_job("one", "send").unwrap();
    row.state = "accepted".into();
    row.data = json!({"sourceId":source.id,"threadId":"filehelper","serverMessageId":"9000000000000000002"});
    core.store.save_media_job("one", &row).unwrap();

    core.project_media_send_receipts("one", "send").unwrap();
    let result = core.store.media_job("one", "send").unwrap().data["result"].clone();
    assert_eq!(result["profileProjection"], "complete");
    assert_eq!(result["officialMediaWriteback"], "complete");
    let message = result["messageIds"][0].as_str().unwrap();
    let assets = core.media_assets("one", message).unwrap();
    assert!(!assets.is_empty());
    for asset in &assets {
        assert_eq!(asset.state, "available");
        assert_eq!(asset.official_writeback, "complete");
        // Files are located in the profile by the MD5 of their contents.
        assert_eq!(
            core.store
                .media_asset_profile_key("one", &asset.id)
                .unwrap()
                .as_deref(),
            Some(md5.as_str())
        );
        assert_eq!(core.media_content("one", &asset.id).unwrap().bytes, bytes);
    }
    // Only the upload still has a file here; the sent copy lives in the profile.
    assert_eq!(media_files(&dir).len(), 1);
    assert_eq!(
        core.maintain_media_storage("one").unwrap()["removedUploads"],
        0
    );
    control(&dir)
        .execute(
            "UPDATE media_jobs SET updated_at='2020-01-01T00:00:00+00:00'",
            [],
        )
        .unwrap();
    assert_eq!(
        core.maintain_media_storage("one").unwrap()["removedUploads"],
        1
    );
    assert!(media_files(&dir).is_empty());
    for asset in &assets {
        assert_eq!(core.media_content("one", &asset.id).unwrap().bytes, bytes);
    }
    // Repair of an already complete job no longer needs the upload.
    assert_eq!(
        core.repair_media_writeback("one", 10).unwrap()["scanned"],
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_converts_an_earlier_database_in_the_background_without_blocking_use() {
    let (dir, core) = tests::test_core();
    let mut expected = vec![];
    for n in 0..20 {
        let id = format!("sticker-{n:02}");
        let bytes = format!("synthetic sticker {n}").into_bytes();
        sidecar_asset(&core, &id, &bytes);
        expected.push((id, bytes));
    }
    drop(core);
    downgrade_to_embedded_payloads(&dir);
    let core = reopen(&dir);
    core.spawn_media_storage_maintenance().unwrap();
    // Reads work while the conversion runs, whichever form a row is in.
    for (id, bytes) in &expected {
        assert_eq!(&core.media_content("one", id).unwrap().bytes, bytes);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !core
        .store
        .embedded_media_payloads("one", 100)
        .unwrap()
        .is_empty()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "conversion did not finish"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(media_files(&dir).len(), expected.len());
    for (id, bytes) in &expected {
        assert_eq!(&core.media_content("one", id).unwrap().bytes, bytes);
    }
    // It runs once per process.
    core.spawn_media_storage_maintenance().unwrap();
}
