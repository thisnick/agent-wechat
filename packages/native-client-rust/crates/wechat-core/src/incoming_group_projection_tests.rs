use super::tests::{contact, ready_refresh, ROOM};
use super::*;
use wechat_protocol::wire::{SyncBatch, SyncCommand};

fn batch(commands: Vec<SyncCommand>) -> SyncBatch {
    SyncBatch {
        code: 0,
        flags: 0,
        cursor: p::uint(1, 0),
        commands,
        continuation: false,
    }
}

fn unknown() -> SyncCommand {
    SyncCommand {
        id: 999,
        payload: vec![],
    }
}

fn group_command(unresolved_control: bool) -> SyncCommand {
    let mut payload = contact("Phone title", unresolved_control);
    if !unresolved_control {
        payload.extend(p::uint(53, 7));
        payload.extend(p::bytes(57, &p::uint(1, 2)));
        payload.extend(p::uint(64, 3));
    }
    SyncCommand { id: 2, payload }
}

fn stage(core: &Core, id: &str, commands: Vec<SyncCommand>, contact_stream: bool) {
    let raw = if contact_stream {
        serde_json::to_vec(&wechat_protocol::contact_sync::ContactSyncBatch {
            cursor: p::uint(1, 0),
            continuation: vec![],
            commands,
            raw: vec![],
        })
        .unwrap()
    } else {
        serde_json::to_vec(&batch(commands)).unwrap()
    };
    let sealed = core
        .vault
        .seal("one", &format!("apply:{id}"), &raw)
        .unwrap();
    let purpose = if contact_stream {
        "contact-received-cursor"
    } else {
        "received-cursor"
    };
    let cursor = core.vault.seal("one", purpose, &p::uint(1, 0)).unwrap();
    if contact_stream {
        core.store
            .stage_contact_sync("one", id, &sealed, &cursor)
            .unwrap();
    } else {
        core.store.stage_sync("one", id, &sealed, &cursor).unwrap();
    }
}

fn fixture() -> (tempfile::TempDir, Arc<Core>) {
    let result = super::super::groups::tests::fixture();
    super::super::groups::tests::make_group(&result.1);
    result
}

fn stage_source(core: &Core, contact_stream: bool, neighbors: usize, unresolved: bool) -> String {
    let mut commands: Vec<_> = (0..neighbors).map(|_| unknown()).collect();
    commands.push(group_command(unresolved));
    stage(core, "source", commands, contact_stream);
    core.project_pending("one").unwrap();
    let source = format!("projected:source:{neighbors}");
    let (key, job) = ready_refresh(core, &source);
    core.save_incoming_group("one", &key, &job).unwrap();
    source
}

fn attempts(core: &Core) -> usize {
    core.projection_command_attempts.load(Ordering::Relaxed)
}

struct IdleHeartbeat;

impl crate::upstream::Upstream for IdleHeartbeat {
    fn kind(&self) -> crate::upstream::UpstreamKind {
        crate::upstream::UpstreamKind::Native
    }

    fn heartbeat(
        &self,
        _: &mut AccountState,
        _: &[u8],
    ) -> Result<wechat_protocol::wire::Heartbeat> {
        Ok(wechat_protocol::wire::Heartbeat {
            next_interval: 5,
            selector: 0,
        })
    }
}

#[tokio::test]
async fn group_refresh_projection_attempts_do_not_scale_with_unrelated_backlog() {
    for contact_stream in [false, true] {
        // Include a backlog larger than pending_sync's 100-batch page. Its
        // unknown commands were never attempted, so any batch-level retry
        // would show in the attempt count. The source batch's unknown
        // neighbors are skipped as handled when it is first projected.
        for backlog in [0, 128] {
            let (_temp, core) = fixture();
            for index in 0..backlog {
                let id = format!("unrelated-{index}");
                stage(&core, &id, (0..8).map(|_| unknown()).collect(), false);
                core.store.block_sync("one", &id).unwrap();
            }
            let source = stage_source(&core, contact_stream, 7, false);
            core.push_health
                .lock()
                .unwrap()
                .insert("one".into(), Arc::new(AtomicBool::new(true)));
            core.projection_command_attempts.store(0, Ordering::Relaxed);

            core.tick("one").await.unwrap();

            // The source was acknowledged when it arrived. The refresh writes
            // the group without replaying it or touching the backlog.
            assert_eq!(
                attempts(&core),
                0,
                "nothing may be replayed; backlog={backlog}, contact_stream={contact_stream}"
            );
            assert!(core.store.runtime("one", &source).unwrap().is_some());
            assert_eq!(core.store.pending_sync_count("one").unwrap(), backlog);
            assert!(core.store.pending_sync("one").unwrap().is_empty());
            assert!(core.store.pending_contact_sync("one").unwrap().is_empty());
            assert_eq!(
                core.group_info("one", ROOM).unwrap()["name"],
                "Fresh phone title"
            );
            for index in 0..7 {
                assert!(core
                    .store
                    .runtime("one", &format!("skipped-control:projected:source:{index}"))
                    .unwrap()
                    .is_some());
            }
            core.projection_command_attempts.store(0, Ordering::Relaxed);
            core.tick("one").await.unwrap();
            assert_eq!(
                attempts(&core),
                0,
                "idle ticks must leave blocked commands alone"
            );
        }
    }
}

#[tokio::test]
async fn heartbeat_polling_retries_only_the_refreshed_source_command() {
    let (_temp, mut core) = fixture();
    Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
        .unwrap()
        .upstream = Arc::new(IdleHeartbeat);
    for index in 0..128 {
        let id = format!("unrelated-{index}");
        stage(&core, &id, (0..8).map(|_| unknown()).collect(), false);
        core.store.block_sync("one", &id).unwrap();
    }
    let source = stage_source(&core, false, 7, false);
    // The mock heartbeat reports no new sync; all exercised work is local.
    core.store
        .put_runtime("one", "history-caught-up", b"1")
        .unwrap();
    core.projection_command_attempts.store(0, Ordering::Relaxed);
    assert!(!core.push_connected("one"));
    core.tick("one").await.unwrap();
    // The source was acknowledged when it arrived; nothing is replayed.
    assert_eq!(attempts(&core), 0);
    assert!(core.store.runtime("one", &source).unwrap().is_some());
    // The never-attempted backlog stays blocked.
    assert_eq!(core.store.pending_sync_count("one").unwrap(), 128);
    assert!(core.store.pending_sync("one").unwrap().is_empty());
    core.projection_command_attempts.store(0, Ordering::Relaxed);
    core.tick("one").await.unwrap();
    assert_eq!(attempts(&core), 0);
}

#[test]
fn group_updates_finish_on_arrival_and_the_refresh_never_replays_them() {
    for contact_stream in [false, true] {
        for unresolved in [false, true] {
            let (_temp, core) = fixture();
            let source = stage_source(&core, contact_stream, 0, unresolved);
            // Acknowledged on arrival, with or without unresolved controls.
            assert!(core.store.runtime("one", &source).unwrap().is_some());
            assert_eq!(core.store.pending_sync_count("one").unwrap(), 0);
            core.projection_command_attempts.store(0, Ordering::Relaxed);
            assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 1);
            assert_eq!(attempts(&core), 0);
            assert_eq!(
                core.group_info("one", ROOM).unwrap()["name"],
                "Fresh phone title"
            );
            assert!(core.store.pending_sync("one").unwrap().is_empty());
            assert!(core.store.pending_contact_sync("one").unwrap().is_empty());
        }
    }
}

#[test]
fn restart_after_group_completion_resumes_the_source_once() {
    for contact_stream in [false, true] {
        let (_temp, core) = fixture();
        let source = stage_source(&core, contact_stream, 0, false);
        let (key, job) = ready_refresh(&core, &source);
        // Simulate interruption after metadata was saved, before source replay.
        core.apply_incoming_group("one", &key, &job).unwrap();
        core.store.put_runtime("one", &done(&source), b"1").unwrap();
        let store = core.store.clone();
        drop(core);
        let recovered = Core::new(store, None).unwrap();
        // The source was acknowledged on arrival, so it isn't replayed.
        assert_eq!(attempts(&recovered), 0);
        assert!(recovered.store.runtime("one", &source).unwrap().is_some());
        assert!(recovered
            .store
            .incoming_group_refreshes("one")
            .unwrap()
            .is_empty());
        assert_eq!(recovered.store.pending_sync_count("one").unwrap(), 0);
        recovered.project_pending("one").unwrap();
        assert_eq!(
            attempts(&recovered),
            0,
            "completed sources must not run again"
        );
    }
}

#[test]
fn logout_generation_change_prevents_source_replay() {
    let (_temp, core) = fixture();
    let source = stage_source(&core, false, 0, false);
    core.store
        .put_runtime("one", "logout-epoch", b"changed")
        .unwrap();
    core.projection_command_attempts.store(0, Ordering::Relaxed);
    assert_eq!(core.refresh_incoming_groups("one", false).unwrap(), 0);
    assert_eq!(attempts(&core), 0);
    // The source was acknowledged on arrival; the refresh from the earlier
    // sign-in stays queued and unused.
    assert!(core.store.runtime("one", &source).unwrap().is_some());
    assert_eq!(core.store.incoming_group_refreshes("one").unwrap().len(), 1);
    assert_eq!(core.group_info("one", ROOM).unwrap()["name"], "Phone title");
}

#[tokio::test]
async fn early_wake_in_polling_mode_refreshes_groups_without_sending_heartbeat() {
    struct NoHeartbeat;
    impl crate::upstream::Upstream for NoHeartbeat {
        fn kind(&self) -> crate::upstream::UpstreamKind {
            crate::upstream::UpstreamKind::Native
        }
        fn heartbeat(
            &self,
            _: &mut AccountState,
            _: &[u8],
        ) -> Result<wechat_protocol::wire::Heartbeat> {
            bail!("unexpected heartbeat on early wake")
        }
    }

    let (_temp, mut core) = fixture();
    Arc::get_mut(&mut Arc::get_mut(&mut core).unwrap().shared)
        .unwrap()
        .upstream = Arc::new(NoHeartbeat);
    let _source = stage_source(&core, false, 0, false);
    assert!(!core.push_connected("one"));

    let outcome = core.tick_with_poll_heartbeat("one", false).await.unwrap();
    assert!(!outcome.push_connected);
    assert_eq!(outcome.heartbeat_interval, None);
    assert!(core
        .store
        .incoming_group_refreshes("one")
        .unwrap()
        .is_empty());
    assert_eq!(
        core.group_info("one", ROOM).unwrap()["name"],
        "Fresh phone title"
    );
}
