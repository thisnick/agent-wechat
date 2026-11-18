use super::encrypted_roundtrip::{add, state};
use super::*;
use crate::account_network::AccountNetworkPermit;
use std::sync::mpsc;
use tokio::{sync::oneshot, time::timeout};
use wechat_protocol::{protobuf as p, wire::SyncBatch};

const DEADLINE: Duration = Duration::from_secs(20);

fn fixture() -> (tempfile::TempDir, Arc<Core>) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&directory.path().join("data")).unwrap());
    store.create_account("one").unwrap();
    let core = Core::new(store, None).unwrap();
    core.test_managed_profile("one", &state()).unwrap();
    core.store
        .put_runtime("one", "desired", b"connected")
        .unwrap();
    (directory, core)
}

fn contact_uses_rollback_journal(core: &Core) {
    let (location, _) = core.profile_location_keys("one").unwrap();
    let db =
        rusqlite::Connection::open(location.root().join("db_storage/contact/contact.db")).unwrap();
    let password = &state().database_password;
    assert_eq!(
        unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
        0
    );
    db.execute_batch(
        "PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;",
    )
    .unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
}

fn batch(value: u32, commands: Vec<SyncCommand>) -> SyncBatch {
    let key = p::message(&[p::uint(1, 1), p::uint(2, value.into())]).unwrap();
    SyncBatch {
        code: 0,
        flags: 0,
        cursor: p::message(&[p::uint(1, 1), p::bytes(2, &key)]).unwrap(),
        continuation: false,
        commands,
    }
}

fn text_commands(start: u32, count: usize) -> Vec<SyncCommand> {
    (start..start + count as u32)
        .map(|id| add(1, "peer", "synthetic_self", b"Replay text", id.into(), id))
        .collect()
}

fn profile_cursor(core: &Core) -> Vec<u8> {
    let binding = core.identity_binding("one").unwrap().unwrap();
    let identity = core.read_bound_identity("one", &binding).unwrap();
    let (location, _) = core.profile_location_keys("one").unwrap();
    wechat_profile::read_account_sync_key(
        &location,
        &identity,
        &wechat_profile::ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        },
    )
    .unwrap()
    .to_vec()
}

fn stage(core: &Core, id: &str, batch: &SyncBatch) {
    core.store
        .stage_sync(
            "one",
            id,
            &core
                .vault
                .seal(
                    "one",
                    &format!("apply:{id}"),
                    &serde_json::to_vec(batch).unwrap(),
                )
                .unwrap(),
            &core
                .vault
                .seal("one", "received-cursor", &batch.cursor)
                .unwrap(),
        )
        .unwrap();
}

fn committed_events(core: &Core) -> usize {
    core.store
        .events("one", 0, 100)
        .unwrap()
        .iter()
        .filter(|event| event.kind == "message.committed")
        .count()
}

async fn replay(
    core: Arc<Core>,
    batch: SyncBatch,
    first_release: impl FnOnce(&Core) -> Result<()> + Send + 'static,
) -> Result<Value> {
    let replay = core.lock_sync_replay("one").await?;
    let guard = core.lock_account("one").await?;
    blocking(move || {
        let _replay = replay;
        let mut permit = AccountNetworkPermit {
            guard: Some(guard),
            runtime: tokio::runtime::Handle::current(),
        };
        let fence = core.session_fence("one")?;
        let mut first_release = Some(first_release);
        core.commit_sync_batch_with_yield(
            "one",
            batch,
            "fixture",
            "fixture",
            Some(&mut || {
                if let Some(first_release) = first_release.take() {
                    // Pause outside the actual writer permit, before the next unit validates.
                    permit.transfer(&core, "one", || first_release(&core))?;
                    core.check_sync_replay_session("one", &fence, Some(b"connected"))
                } else {
                    core.yield_sync_writer("one", &mut permit, &fence, Some(b"connected"))
                }
            }),
        )
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_reads_finish_between_replay_chunks_in_wal_and_rollback_profiles() {
    for parallel in [true, false] {
        let (_directory, core) = fixture();
        if !parallel {
            contact_uses_rollback_journal(&core);
        }
        let contact = SyncCommand {
            id: 2,
            payload: p::message(&[
                p::bytes(1, &p::bytes(1, b"peer")),
                p::bytes(2, &p::bytes(1, b"Fixture peer")),
                p::uint(7, u64::from(u32::MAX)),
                p::uint(8, 1),
            ])
            .unwrap(),
        };
        let before = batch(
            1,
            vec![
                contact,
                add(
                    1,
                    "peer",
                    "synthetic_self",
                    b"Committed direct text",
                    77,
                    77,
                ),
                add(
                    1,
                    "room@chatroom",
                    "synthetic_self",
                    b"member:\nCommitted group text",
                    78,
                    78,
                ),
            ],
        );
        let original_cursor = before.cursor.clone();
        core.commit_sync_batch("one", before, "fixture", "fixture")
            .unwrap();
        let direct_id = core.messages("one", "peer", None, None).unwrap()["items"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let group_id = core.messages("one", "room@chatroom", None, None).unwrap()["items"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(profile_cursor(&core), original_cursor);

        let later = batch(2, text_commands(100, 2 * SYNC_REPLAY_CHUNK_SIZE + 1));
        let next_cursor = later.cursor.clone();
        let (paused, at_boundary) = oneshot::channel();
        let (resume, can_continue) = mpsc::channel();
        let writer = tokio::spawn(replay(core.clone(), later, move |_| {
            paused.send(()).unwrap();
            can_continue.recv_timeout(DEADLINE)?;
            Ok(())
        }));
        timeout(DEADLINE, at_boundary).await.unwrap().unwrap();
        assert_eq!(committed_events(&core), 2 + SYNC_REPLAY_CHUNK_SIZE);
        assert_eq!(core.cursor("one").unwrap(), next_cursor);
        assert_eq!(profile_cursor(&core), original_cursor);
        assert_eq!(core.store.pending_sync_count("one").unwrap(), 1);
        assert!(core.store.pending_cursor_projection("one").unwrap());
        assert!(
            timeout(Duration::ZERO, core.lock_sync_replay("one"))
                .await
                .is_err(),
            "another replay must not take over while the writer lock is free"
        );

        let (direct, group, contacts) = timeout(DEADLINE, async {
            tokio::try_join!(
                core.read_timed("one", move |view| view.message("one", &direct_id)),
                core.read_timed("one", move |view| view.message("one", &group_id)),
                core.read_timed("one", move |view| view.contacts("one", &Default::default())),
            )
        })
        .await
        .expect("all reads must return while replay is paused")
        .unwrap();
        assert_eq!(direct.0["body"], "Committed direct text");
        assert_eq!(direct.0["senderId"], "peer");
        assert_eq!(group.0["body"], "Committed group text");
        assert_eq!(group.0["senderId"], "member");
        assert!(contacts.0["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| { item["id"] == "peer" && item["displayName"] == "Fixture peer" }));
        assert_eq!(
            [direct.1.parallel, group.1.parallel, contacts.1.parallel],
            [parallel; 3]
        );

        resume.send(()).unwrap();
        let result = timeout(DEADLINE, writer).await.unwrap().unwrap().unwrap();
        assert_eq!(
            result["projection"]["projectedCommands"],
            2 * SYNC_REPLAY_CHUNK_SIZE + 1
        );
        assert_eq!(result["projection"]["pendingBatches"], 0);
        assert_eq!(profile_cursor(&core), next_cursor);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_stops_at_session_fences_and_resumes_from_durable_receipts() {
    for fence_change in ["logout", "relogin", "credentials", "disconnect"] {
        let (directory, core) = fixture();
        let first = batch(1, vec![]);
        let original_cursor = first.cursor.clone();
        core.commit_sync_batch("one", first, "fixture", "fixture")
            .unwrap();
        let later = batch(2, text_commands(100, SYNC_REPLAY_CHUNK_SIZE + 1));
        let next_cursor = later.cursor.clone();
        let error = replay(core.clone(), later, move |core| {
            tokio::runtime::Handle::current().block_on(async {
                let _guard = core.lock_account("one").await?;
                match fence_change {
                    "logout" => core.logout_local("one"),
                    "relogin" => {
                        core.logout_local("one")?;
                        let epoch = core.store.runtime("one", "logout-epoch")?.unwrap();
                        core.install_login_state("one", &state(), Some(&epoch))?;
                        core.store.put_runtime("one", "desired", b"connected")
                    }
                    "credentials" => {
                        let mut refreshed = core.account_state("one")?;
                        refreshed.request_key[0] ^= 1;
                        core.save_state("one", &refreshed)
                    }
                    "disconnect" => core.disconnect("one"),
                    _ => unreachable!(),
                }
            })
        })
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "session_superseded");
        assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE);
        assert_eq!(profile_cursor(&core), original_cursor);
        assert_eq!(core.store.pending_sync_count("one").unwrap(), 1);
        assert!(core.store.pending_cursor_projection("one").unwrap());
        drop(core);

        let store = Arc::new(Store::open_existing(&directory.path().join("data")).unwrap());
        let recovered = Core::new(store, None).unwrap();
        if fence_change == "logout" {
            let epoch = recovered
                .store
                .runtime("one", "logout-epoch")
                .unwrap()
                .unwrap();
            recovered
                .install_login_state("one", &state(), Some(&epoch))
                .unwrap();
        }
        let result = recovered.recover_projection("one").unwrap();
        assert_eq!(result["projectedCommands"], 1);
        assert_eq!(result["pendingBatches"], 0);
        assert_eq!(profile_cursor(&recovered), next_cursor);
        assert_eq!(committed_events(&recovered), SYNC_REPLAY_CHUNK_SIZE + 1);
        assert_eq!(
            recovered.recover_projection("one").unwrap()["projectedCommands"],
            0
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_masks_already_staged_recalls_before_releasing_the_writer() {
    let (_directory, core) = fixture();
    let original = batch(
        1,
        vec![
            add(1, "peer", "synthetic_self", b"Private body one", 77, 77),
            add(1, "peer", "synthetic_self", b"Private body two", 78, 78),
        ],
    );
    core.commit_sync_batch("one", original, "fixture", "fixture")
        .unwrap();
    let originals = core.messages("one", "peer", None, None).unwrap();
    let targets: Vec<_> = originals["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["serverId"].as_str().unwrap().to_owned(),
                row["id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(targets.len(), 2);
    for (id, cursor, start, count) in [("older-a", 2, 100, 3), ("older-b", 3, 103, 6)] {
        stage(&core, id, &batch(cursor, text_commands(start, count)));
    }
    let mut commands = text_commands(200, SYNC_REPLAY_CHUNK_SIZE);
    for (server, command) in [(77, 299), (78, 300)] {
        let xml = format!("<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>{server}</newmsgid><replacemsg>Fixture recall</replacemsg></revokemsg></sysmsg>");
        commands.push(add(
            10002,
            "peer",
            "synthetic_self",
            xml.as_bytes(),
            command.into(),
            command,
        ));
    }
    let (read, checked) = oneshot::channel();
    replay(core.clone(), batch(4, commands), move |core| {
        let _guard = tokio::runtime::Handle::current().block_on(core.lock_account("one"))?;
        assert_eq!(committed_events(core), 2 + SYNC_REPLAY_CHUNK_SIZE);
        assert_eq!(core.store.pending_sync_count("one")?, 2);
        for (server, id) in &targets {
            let marker = core.store.recall_marker("one", "peer", server)?.unwrap();
            assert!(marker.awaits_sync_command());
            assert_eq!(marker.message_id.as_deref(), Some(id.as_str()));

            let message = core.message("one", id)?;
            assert_eq!(message["body"], "[Message recalled]");
            assert_eq!(message["recall"]["replacementText"], "Fixture recall");
            assert!(!message.to_string().contains("Private body"));
        }
        assert!(
            !core
                .store
                .events("one", 0, 100)?
                .iter()
                .any(|event| event.kind == "message.recalled"),
            "a reader can observe the mask before the recall event exists"
        );
        read.send(()).unwrap();
        Ok(())
    })
    .await
    .unwrap();
    checked
        .await
        .expect("must yield while the final recall is still unprojected");
    assert_eq!(
        core.store
            .events("one", 0, 100)
            .unwrap()
            .iter()
            .filter(|event| event.kind == "message.recalled")
            .count(),
        2,
    );
    assert_eq!(core.store.pending_sync_count("one").unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_recall_uses_the_retained_notice_in_the_native_session_preview() {
    let (_directory, core) = fixture();
    let target = 100 + SYNC_REPLAY_CHUNK_SIZE as u32 - 1;
    let xml = format!("<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>{target}</newmsgid><replacemsg>Fixture recall</replacemsg></revokemsg></sysmsg>");
    let mut commands = text_commands(100, SYNC_REPLAY_CHUNK_SIZE);
    commands.push(add(
        10002,
        "peer",
        "synthetic_self",
        xml.as_bytes(),
        199,
        199,
    ));
    replay(core, batch(1, commands), move |core| {
        let _guard = tokio::runtime::Handle::current().block_on(core.lock_account("one"))?;
        let marker = core
            .store
            .recall_marker("one", "peer", &target.to_string())?
            .context("missing staged recall")?;
        assert!(marker.awaits_sync_command());
        let thread = core
            .profile_reader("one")?
            .threads(&Default::default())?
            .items
            .into_iter()
            .find(|row| row.username == "peer")
            .context("missing native session")?;
        assert_eq!(
            thread.raw_fields.get("summary"),
            Some(&wechat_profile::SqlValue::Text("Fixture recall".into()))
        );
        Ok(())
    })
    .await
    .unwrap();
}

fn recall_command(target: u32) -> SyncCommand {
    let xml = format!("<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>{target}</newmsgid><replacemsg>Fixture recall</replacemsg></revokemsg></sysmsg>");
    add(10002, "peer", "synthetic_self", xml.as_bytes(), 199, 199)
}

#[tokio::test]
async fn staged_recall_preview_rejects_an_unreadable_business_shard() {
    let (_directory, core) = fixture();
    let _guard = core.lock_account("one").await.unwrap();
    let (location, _) = core.profile_location_keys("one").unwrap();
    let messages = location.root().join("db_storage/message");
    std::fs::copy(
        messages.join("message_0.db"),
        messages.join("biz_message_0.db"),
    )
    .unwrap();
    core.mask_sync_recall_before_yield(
        "one",
        &recall_command(100),
        "projected:fixture:1",
        "synthetic_self",
        "fixture",
        &mut None,
    )
    .unwrap();
    let marker = core
        .store
        .recall_marker("one", "peer", "100")
        .unwrap()
        .unwrap();
    assert!(marker.awaits_sync_command());

    let error = core
        .projected_sync_recall_preview("one", &marker)
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<wechat_profile::ProfileError>(),
            Some(wechat_profile::ProfileError::KeyUnavailable(name)) if name == "message shards"
        ),
        "{error:#}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_defers_an_ambiguous_recall_until_its_source_is_unique() {
    let (directory, core) = fixture();
    let (_donor_directory, donor) = fixture();
    let target = 100 + SYNC_REPLAY_CHUNK_SIZE as u32 - 1;
    donor
        .commit_sync_batch(
            "one",
            batch(1, text_commands(target, 1)),
            "fixture",
            "fixture",
        )
        .unwrap();

    // Copy a valid keyed shard containing the same target that replay will
    // insert in the ordinary message database.
    let (donor_location, donor_keys) = donor.profile_location_keys("one").unwrap();
    let (location, mut keys) = core.profile_location_keys("one").unwrap();
    let original = "message/message_0.db";
    let business = "message/biz_message_0.db";
    let source = donor_location.root().join("db_storage").join(original);
    {
        let db = rusqlite::Connection::open(&source).unwrap();
        let password = &state().database_password;
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
            0
        );
        db.execute_batch(
            "PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;",
        )
        .unwrap();
        let busy: i32 = db
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(busy, 0);
    }
    let business_path = location.root().join("db_storage").join(business);
    std::fs::copy(source, &business_path).unwrap();
    let mut key = donor_keys.databases[original].clone();
    key.relative_path = business.into();
    keys.databases.insert(business.into(), key);
    let original_keys = core.store.keys_file("one").unwrap().unwrap();
    let key_file = original_keys.with_file_name("business-shard-keys.json");
    keys.write_new(&key_file).unwrap();
    core.store.set_keys_file("one", &key_file).unwrap();

    let mut commands = text_commands(100, SYNC_REPLAY_CHUNK_SIZE);
    commands.push(recall_command(target));
    let (checked, observed) = oneshot::channel();
    let result = replay(core.clone(), batch(1, commands), move |core| {
        let _guard = tokio::runtime::Handle::current().block_on(core.lock_account("one"))?;
        let reader = core.profile_reader("one")?;
        let copies = reader.messages_by_server_id("peer", &target.to_string(), None)?;
        assert_eq!(copies.len(), 2);
        for copy in copies {
            let visible = core.message("one", &crate::message_id(&copy.id)?)?;
            assert_eq!(visible["body"], "[Message recalled]");
        }
        let thread = reader
            .threads(&Default::default())?
            .items
            .into_iter()
            .find(|row| row.username == "peer")
            .context("missing native session")?;
        assert_eq!(
            thread.raw_fields.get("summary"),
            Some(&wechat_profile::SqlValue::Text("Replay text".into()))
        );
        assert_eq!(committed_events(core), SYNC_REPLAY_CHUNK_SIZE - 1);
        assert!(core
            .store
            .events("one", 0, 100)?
            .iter()
            .all(|event| event.kind != "message.recalled"));
        checked.send(()).unwrap();
        Ok(())
    })
    .await
    .unwrap();
    observed
        .await
        .expect("must yield before applying the recall");
    assert_eq!(result["projection"]["pendingBatches"], 1);
    assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE - 1);
    let events = core.store.events("one", 0, 100).unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == "projection.blocked"));
    assert!(events.iter().all(|event| event.kind != "message.recalled"));
    let target = target.to_string();
    let marker = core
        .store
        .recall_marker("one", "peer", &target)
        .unwrap()
        .unwrap();
    assert!(marker.awaits_sync_command());

    // Quarantine the duplicate in this fixture and restore its original keys.
    drop(core.profile_readers.lock().unwrap().remove("one"));
    std::fs::rename(
        business_path,
        directory.path().join("quarantined-business.db"),
    )
    .unwrap();
    core.store.set_keys_file("one", &original_keys).unwrap();
    let originals = core
        .profile_reader("one")
        .unwrap()
        .messages_by_server_id("peer", &target, None)
        .unwrap();
    assert_eq!(originals.len(), 1);
    assert_eq!(originals[0].id.database, original);
    let original_id = crate::message_id(&originals[0].id).unwrap();

    let result = core.recover_projection_online("one").await.unwrap();
    assert_eq!(result["projectedCommands"], 2);
    assert_eq!(result["pendingBatches"], 0);
    let events = core.store.events("one", 0, 100).unwrap();
    let commits: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "message.committed" && event.data["messageId"] == original_id)
        .collect();
    let recalls: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "message.recalled")
        .collect();
    assert_eq!(commits.len(), 1);
    assert_eq!(recalls.len(), 1);
    assert!(recalls[0].cursor.parse::<u64>().unwrap() > commits[0].cursor.parse::<u64>().unwrap());
    assert_eq!(recalls[0].data["messageId"], original_id);
    assert_eq!(recalls[0].data["serverId"], target);
    assert_eq!(
        core.message("one", &original_id).unwrap()["body"],
        "[Message recalled]"
    );
    let last_cursor = events.last().unwrap().cursor.parse().unwrap();
    assert_eq!(
        core.recover_projection_online("one").await.unwrap()["projectedCommands"],
        0
    );
    assert!(core
        .store
        .events("one", last_cursor, 100)
        .unwrap()
        .iter()
        .all(|event| {
            !matches!(
                event.kind.as_str(),
                "message.committed" | "message.recalled"
            )
        }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staged_recall_resumes_after_restart_and_emits_once_after_the_original_commit() {
    let (directory, core) = fixture();
    let xml = b"<sysmsg type='revokemsg'><revokemsg><session>peer</session><newmsgid>100</newmsgid></revokemsg></sysmsg>";
    let mut commands = text_commands(100, SYNC_REPLAY_CHUNK_SIZE);
    commands.push(add(10002, "peer", "synthetic_self", xml, 199, 199));
    // The mask exists before the original row, and the command follows the
    // first yield. Interrupt replay while the mask is the only recall evidence.
    let error = replay(core.clone(), batch(1, commands), |core| {
        let _guard = tokio::runtime::Handle::current().block_on(core.lock_account("one"))?;
        let events = core.store.events("one", 0, 100)?;
        let original = events
            .iter()
            .find(|event| event.kind == "message.committed")
            .unwrap();
        let id = original.data["messageId"].as_str().unwrap();
        let message = core.message("one", id)?;
        assert_eq!(message["body"], "[Message recalled]");
        assert!(core
            .apply_confirmed_recall("one", "peer", "100", None, "outgoing", "other")
            .is_err());
        assert!(!core
            .store
            .events("one", 0, 100)?
            .iter()
            .any(|event| event.kind == "message.recalled"));
        let native = core
            .profile_reader("one")?
            .message(&crate::source_message_id(id)?, None)?;
        assert_eq!(native.content.text.as_deref(), Some("Replay text"));
        core.disconnect("one")
    })
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "session_superseded");
    drop(core);

    let store = Arc::new(Store::open_existing(&directory.path().join("data")).unwrap());
    let recovered = Core::new(store, None).unwrap();
    let marker = recovered
        .store
        .recall_marker("one", "peer", "100")
        .unwrap()
        .unwrap();
    assert!(marker.awaits_sync_command());
    assert!(recovered
        .store
        .recall_control_receipt("one", "peer", "100")
        .unwrap()
        .is_none());
    assert_eq!(
        recovered.recover_projection("one").unwrap()["projectedCommands"],
        1
    );
    assert_eq!(
        recovered.recover_projection("one").unwrap()["projectedCommands"],
        0
    );
    let events = recovered.store.events("one", 0, 100).unwrap();
    let commits: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "message.committed")
        .collect();
    assert_eq!(commits.len(), SYNC_REPLAY_CHUNK_SIZE);
    let recalls: Vec<_> = events
        .iter()
        .filter(|event| event.kind == "message.recalled")
        .collect();
    assert_eq!(recalls.len(), 1);
    assert!(
        recalls[0].cursor.parse::<u64>().unwrap()
            > commits.last().unwrap().cursor.parse::<u64>().unwrap()
    );
    assert_eq!(recalls[0].data["messageId"], commits[0].data["messageId"]);
    assert_eq!(recalls[0].data["serverId"], "100");
    assert_eq!(recalls[0].data["observedAt"], marker.observed_at);
    let id = recalls[0].data["messageId"].as_str().unwrap();
    assert_eq!(
        recovered.message("one", id).unwrap()["body"],
        "[Message recalled]"
    );
    let native = recovered
        .profile_reader("one")
        .unwrap()
        .message(&crate::source_message_id(id).unwrap(), None)
        .unwrap();
    assert!(crate::recalls::official_recall(&native).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_budget_ignores_existing_receipts_and_empty_batches() {
    let (_directory, core) = fixture();
    let skipped = 2 * SYNC_REPLAY_CHUNK_SIZE;
    // Retention clears the payloads of receipted commands in old blocked
    // batches while preserving each command's position for a later retry.
    let compacted = (0..skipped)
        .map(|_| SyncCommand {
            id: 5,
            payload: vec![],
        })
        .collect();
    stage(&core, "receipted", &batch(1, compacted));
    for index in 0..skipped {
        core.store
            .put_runtime("one", &format!("projected:receipted:{index}"), b"1")
            .unwrap();
    }
    stage(&core, "empty", &batch(2, vec![]));

    let result = replay(
        core.clone(),
        batch(3, text_commands(200, SYNC_REPLAY_CHUNK_SIZE + 1)),
        |core| {
            assert_eq!(committed_events(core), SYNC_REPLAY_CHUNK_SIZE);
            assert_eq!(
                core.projection_command_attempts.load(Ordering::Relaxed),
                SYNC_REPLAY_CHUNK_SIZE
            );
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(
        result["projection"]["projectedCommands"],
        SYNC_REPLAY_CHUNK_SIZE + 1
    );
    assert_eq!(result["projection"]["pendingBatches"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_link_catchup_yields_and_checks_the_push_validation_generation() {
    struct Catchup(Option<SyncBatch>);

    impl crate::upstream::PushConnection for Catchup {
        fn sync(
            &mut self,
            _state: &mut AccountState,
            _cursor: &[u8],
            scene: u32,
        ) -> Result<SyncBatch> {
            assert_eq!(scene, 6);
            self.0.take().context("unexpected second sync")
        }

        fn heartbeat(
            &mut self,
            _state: &mut AccountState,
            _cursor: &[u8],
        ) -> Result<wechat_protocol::wire::Heartbeat> {
            bail!("unexpected heartbeat")
        }

        fn poll(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<wechat_protocol::push::Notification>> {
            bail!("unexpected poll")
        }
    }

    let (_directory, core) = fixture();
    core.store
        .mark_validated_if_active("one", &core.ownership_generation)
        .unwrap();
    let fence = core.session_fence("one").unwrap();
    let writer = core.clone();
    let connection = Catchup(Some(batch(
        1,
        text_commands(100, SYNC_REPLAY_CHUNK_SIZE + 1),
    )));
    let drain = tokio::spawn(async move {
        writer
            .drain_push_sync("one", Box::new(connection), &fence, 6, "catchup")
            .await
            .err()
    });

    timeout(DEADLINE, async {
        while core.projection_command_attempts.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
        // A waiter queued after projection starts must acquire the writer at
        // the chunk boundary, while push still owns the replay operation.
        let _guard = core.lock_account("one").await.unwrap();
        assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE);
        assert_eq!(core.store.pending_sync_count("one").unwrap(), 1);
        core.store
            .clear_runtime("one", "validated-generation")
            .unwrap();
    })
    .await
    .unwrap();
    let error = timeout(DEADLINE, drain)
        .await
        .unwrap()
        .unwrap()
        .expect("push should reject a superseded validation");
    assert_eq!(error.to_string(), "push_session_superseded");
    assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE);
    assert_eq!(
        core.recover_projection("one").unwrap()["projectedCommands"],
        1
    );
    assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completing_sticker_controls_yields_between_pending_message_chunks() {
    let (_directory, core) = fixture();
    let mut commands = vec![SyncCommand {
        id: 999,
        payload: vec![],
    }];
    commands.extend(text_commands(100, SYNC_REPLAY_CHUNK_SIZE + 1));
    let source = batch(1, commands);
    stage(&core, "sticker", &source);
    let control = crate::sticker_control::StickerRefresh {
        key: "sticker-refresh:fixture".into(),
        value: b"fixture".to_vec(),
        receipt: Some("projected:sticker:0".into()),
    };
    core.store
        .put_runtime("one", &control.key, &control.value)
        .unwrap();
    let replay_guard = core.lock_sync_replay("one").await.unwrap();
    let guard = core.lock_account("one").await.unwrap();
    let writer = core.clone();
    let (released, checked) = oneshot::channel();
    blocking(move || {
        let _replay = replay_guard;
        let runtime = tokio::runtime::Handle::current();
        let mut permit = AccountNetworkPermit {
            guard: Some(guard),
            runtime: runtime.clone(),
        };
        let fence = writer.session_fence("one")?;
        let mut released = Some(released);
        writer.finish_sticker_controls(
            "one",
            &[control],
            Some(&mut || {
                permit.transfer(&writer, "one", || {
                    let _guard = runtime.block_on(writer.lock_account("one"))?;
                    assert_eq!(committed_events(&writer), SYNC_REPLAY_CHUNK_SIZE);
                    assert_eq!(writer.store.pending_sync_count("one")?, 1);
                    released.take().unwrap().send(()).unwrap();
                    Ok(())
                })?;
                writer.check_sync_replay_session("one", &fence, Some(b"connected"))
            }),
        )
    })
    .await
    .unwrap();
    checked
        .await
        .expect("sticker completion must yield during replay");
    assert_eq!(committed_events(&core), SYNC_REPLAY_CHUNK_SIZE + 1);
    assert_eq!(core.store.pending_sync_count("one").unwrap(), 0);
}
