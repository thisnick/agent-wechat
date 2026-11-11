use super::*;
use crate::DeviceProfile;

pub(super) fn client() -> NativeClient {
    NativeClient::from_account(
        AccountState {
            schema: 1,
            device: DeviceProfile {
                device_id: [b"A".as_slice(), &[1; 15]].concat(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 1,
            own_username: "synthetic_self".into(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![2; 24],
            response_key: vec![3; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        },
        NativeOptions::default(),
    )
    .unwrap()
}

// Synthetic response framing follows the independent header fixture in
// calls/host_transport.rs; it does not use the request serializer.
fn call_reply(state: &AccountState) -> Vec<u8> {
    let plain = b"synthetic call reply";
    let cipher =
        crate::crypto::cbc(plain, &state.session_key, openssl::symm::Mode::Encrypt).unwrap();
    let cookie = b"new";
    let mut tail = Vec::new();
    for value in [7994, plain.len() as u64, plain.len() as u64, 0, 0] {
        tail.extend(crate::protobuf::varint(value));
    }
    let size = 10 + cookie.len() + tail.len();
    let flags = (5u16 << 12) | ((cookie.len() as u16) << 8) | ((size as u16) << 2) | 2;
    let mut reply = flags.to_le_bytes().to_vec();
    reply.extend(0i32.to_be_bytes());
    reply.extend(state.uin.to_be_bytes());
    reply.extend(cookie);
    reply.extend(tail);
    reply.extend(cipher);
    reply
}

#[tokio::test]
async fn verified_reply_is_retained_before_owned_cookie_adoption() {
    let client = client();
    let retained = Arc::new(Mutex::new(Vec::new()));
    let receipt = retained.clone();
    let reply = client
        .account_request(move |transport, state, _| {
            let reply = call_reply(state);
            let mut transport = transport.clone();
            transport.call_reply_observer = Some(Arc::new(move |plain| {
                *receipt.lock().unwrap() = plain.to_vec();
                Ok(())
            }));
            transport.accept_call_reply(state, 7994, &reply)
        })
        .await
        .unwrap();
    assert_eq!(*retained.lock().unwrap(), b"synthetic call reply");
    assert_eq!(reply, b"synthetic call reply");
    assert_eq!(client.export_account_state().unwrap().cookie, b"new");
}

#[tokio::test]
async fn receipt_storage_failure_never_adopts_cookie_or_returns_allocation() {
    let client = client();
    let result = client
        .account_request(move |transport, state, _| {
            let reply = call_reply(state);
            let mut transport = transport.clone();
            transport.call_reply_observer = Some(Arc::new(|_| {
                anyhow::bail!("Synthetic receipt storage failure")
            }));
            transport.accept_call_reply(state, 7994, &reply)
        })
        .await;
    assert!(result.is_err());
    assert!(client.export_account_state().unwrap().cookie.is_empty());
    assert_eq!(client.account_status().unwrap().revision, 0);
}

#[test]
fn unverified_reply_never_reaches_receipt_observer() {
    let owner = client();
    let mut state = owner.export_account_state().unwrap();
    let mut reply = call_reply(&state);
    // An authenticated command mismatch must be rejected before journal callbacks.
    let observed = Arc::new(AtomicBool::new(false));
    let observer = observed.clone();
    let mut transport = NativeClient::new(NativeOptions::default()).unwrap();
    transport.call_reply_observer = Some(Arc::new(move |_| {
        observer.store(true, Ordering::Release);
        Ok(())
    }));
    assert!(transport
        .accept_call_reply(&mut state, 8069, &reply)
        .is_err());
    reply.pop();
    assert!(transport
        .accept_call_reply(&mut state, 7994, &reply)
        .is_err());
    assert!(!observed.load(Ordering::Acquire));
    assert!(state.cookie.is_empty());
}

#[test]
fn cloned_clients_share_accepted_state_and_ignore_stale_cookie() {
    let client = client();
    let other = client.clone();
    let (old, mut old_state) = client.owned_account().unwrap().begin().unwrap();
    let (new, mut new_state) = other.owned_account().unwrap().begin().unwrap();
    new_state.cookie = vec![2];
    new.accept_cookie(&new_state).unwrap();
    old_state.cookie = vec![1];
    old.accept_cookie(&old_state).unwrap();
    assert_eq!(client.export_account_state().unwrap().cookie, vec![2]);
    assert_eq!(other.account_status().unwrap().revision, 1);
}

#[test]
fn cookie_exchange_cannot_replace_credentials() {
    let client = client();
    let (exchange, mut returned) = client.owned_account().unwrap().begin().unwrap();
    returned.session_key = vec![9; 16];
    assert!(exchange.accept_cookie(&returned).is_err());
    assert_eq!(
        client.export_account_state().unwrap().session_key,
        vec![1; 16]
    );
}

#[test]
fn renewal_fences_old_requests_even_when_key_bytes_do_not_change() {
    let client = client();
    let (old, mut old_state) = client.owned_account().unwrap().begin().unwrap();
    let (renewal, accepted) = client.owned_account().unwrap().begin().unwrap();
    renewal.accept_credentials(accepted).unwrap();
    assert_eq!(client.account_status().unwrap().credential_generation, 1);
    assert!(old.check().is_err());
    old_state.cookie = vec![9];
    assert!(old.accept_cookie(&old_state).is_err());
    assert!(client.export_account_state().unwrap().cookie.is_empty());
    let (next, _) = client.owned_account().unwrap().begin().unwrap();
    next.check().unwrap();
}

#[test]
fn worker_authority_survives_cookie_updates_and_is_revoked_by_renewal_and_stop() {
    let client = client();
    let first = client.account_lease().unwrap();
    let (cookie, mut updated) = client.owned_account().unwrap().begin().unwrap();
    updated.cookie = vec![4];
    cookie.accept_cookie(&updated).unwrap();
    drop(cookie);
    assert!(first.current());
    let (renewal, same_credentials) = client.owned_account().unwrap().begin().unwrap();
    renewal.accept_credentials(same_credentials).unwrap();
    drop(renewal);
    assert!(!first.current());
    let next = client.account_lease().unwrap();
    assert!(next.current());
    client.stop().unwrap();
    assert!(!next.current());
    assert!(client.account_lease().is_err());
}

#[test]
fn concurrent_renewal_and_identity_replacement_are_rejected() {
    let client = client();
    let (first, mut first_state) = client.owned_account().unwrap().begin().unwrap();
    let (second, second_state) = client.owned_account().unwrap().begin().unwrap();
    first_state.own_username = "another_synthetic_account".into();
    assert!(first.accept_credentials(first_state).is_err());
    first
        .accept_credentials(client.export_account_state().unwrap())
        .unwrap();
    assert!(second.accept_credentials(second_state).is_err());
    assert_eq!(
        client.account_identity().unwrap().own_username,
        "synthetic_self"
    );
}

struct RecordingPersistence {
    fail: AtomicBool,
    cookies: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
}

impl AccountPersistence for RecordingPersistence {
    fn persist(&self, previous: &AccountState, accepted: &AccountState) -> Result<()> {
        ensure!(
            !self.fail.load(Ordering::Acquire),
            "Synthetic storage failure"
        );
        self.cookies
            .lock()
            .unwrap()
            .push((previous.cookie.clone(), accepted.cookie.clone()));
        Ok(())
    }
}

#[test]
fn storage_failure_prevents_cookie_and_credential_adoption_without_retry() {
    let storage = Arc::new(RecordingPersistence {
        fail: AtomicBool::new(true),
        cookies: Mutex::new(vec![]),
    });
    let initial = client().export_account_state().unwrap();
    let client = NativeClient::from_account_with_persistence(
        initial,
        NativeOptions::default(),
        storage.clone(),
    )
    .unwrap();
    let (cookie, mut returned) = client.owned_account().unwrap().begin().unwrap();
    returned.cookie = vec![7];
    assert!(cookie
        .accept_cookie(&returned)
        .unwrap_err()
        .is::<AccountPersistenceFailed>());
    let (renewal, mut renewed) = client.owned_account().unwrap().begin().unwrap();
    renewed.session_key = vec![9; 16];
    assert!(renewal
        .accept_credentials(renewed)
        .unwrap_err()
        .is::<AccountPersistenceFailed>());
    let status = client.account_status().unwrap();
    assert_eq!((status.revision, status.credential_generation), (0, 0));
    assert!(client.export_account_state().unwrap().cookie.is_empty());
    assert_eq!(
        client.export_account_state().unwrap().session_key,
        vec![1; 16]
    );
    assert!(storage.cookies.lock().unwrap().is_empty());
}

#[test]
fn accepted_storage_updates_follow_current_state_and_skip_stale_replies() {
    let storage = Arc::new(RecordingPersistence {
        fail: AtomicBool::new(false),
        cookies: Mutex::new(vec![]),
    });
    let initial = client().export_account_state().unwrap();
    let client = NativeClient::from_account_with_persistence(
        initial,
        NativeOptions::default(),
        storage.clone(),
    )
    .unwrap();
    let (old, mut old_state) = client.owned_account().unwrap().begin().unwrap();
    let (first, mut first_state) = client.owned_account().unwrap().begin().unwrap();
    first_state.cookie = vec![1];
    first.accept_cookie(&first_state).unwrap();
    let (second, mut second_state) = client.owned_account().unwrap().begin().unwrap();
    second_state.cookie = vec![2];
    second.accept_cookie(&second_state).unwrap();
    old_state.cookie = vec![9];
    old.accept_cookie(&old_state).unwrap();
    assert_eq!(
        *storage.cookies.lock().unwrap(),
        vec![(vec![], vec![1]), (vec![1], vec![2])]
    );
}

#[tokio::test]
async fn account_fence_preserves_original_interruption_error_for_reconciliation() {
    let client = client();
    let stop_client = client.clone();
    let result: Result<()> = client
        .account_request(move |_, _, _| {
            stop_client.stop()?;
            Err(crate::calls::host_transport::CallLiveSyncInterrupted.into())
        })
        .await;
    assert!(result
        .err()
        .unwrap()
        .is::<crate::calls::host_transport::CallLiveSyncInterrupted>());
    client.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminal_lease_fences_admission_and_handoff_until_exclusive() {
    let client = client();
    let account = client.owned_account().unwrap();
    let (prior, _) = account.begin().unwrap();
    let (terminal, _) = account.begin().unwrap();
    account.stop().unwrap();
    assert!(account.begin().is_err());
    assert!(terminal.check_logout().is_err());
    let handoff_client = client.clone();
    let handoff = tokio::spawn(async move { handoff_client.quiesce_and_export().await });
    tokio::task::yield_now().await;
    assert!(!handoff.is_finished());
    drop(prior);
    account.drain_to(1).await.unwrap();
    terminal.check_logout().unwrap();
    assert!(!handoff.is_finished());
    drop(terminal);
    handoff.await.unwrap().unwrap();
    assert_eq!(
        client.account_status().unwrap().lifecycle,
        AccountLifecycle::Transferred
    );
}

#[test]
fn concurrent_logout_reservation_has_exactly_one_winner() {
    let client = client();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let client = client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.prepare_logout().is_ok()
            })
        })
        .collect();
    let winners = workers
        .into_iter()
        .filter_map(|worker| worker.join().ok())
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
    let status = client.account_status().unwrap();
    assert_eq!(status.lifecycle, AccountLifecycle::Stopped);
    assert_eq!(status.in_flight, 0);
}

#[tokio::test]
async fn dropping_reserved_logout_sends_nothing_and_releases_handoff_barrier() {
    let client = client();
    let pending = client.prepare_logout().unwrap();
    // A daemon may now commit its local logout record before remote submission.
    assert_eq!(
        client.account_status().unwrap().lifecycle,
        AccountLifecycle::Stopped
    );
    assert!(client.prepare_logout().is_err());
    let handoff_client = client.clone();
    let handoff = tokio::spawn(async move { handoff_client.quiesce_and_export().await });
    tokio::task::yield_now().await;
    assert!(!handoff.is_finished());
    drop(pending);
    handoff.await.unwrap().unwrap();
    assert!(client.export_account_state().is_err());
}

#[tokio::test]
async fn public_owned_operations_require_account_admission() {
    let client = NativeClient::new(NativeOptions::default()).unwrap();
    assert!(client
        .sync(&[])
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("no bound account"));
    assert!(client
        .renew_session("test")
        .await
        .unwrap_err()
        .to_string()
        .contains("no bound account"));
}

#[test]
fn snapshot_bridge_cannot_bypass_the_bound_clients_account_owner() {
    let client = client();
    let mut snapshot = client.export_account_state().unwrap();
    let error = client.sync_snapshot(&mut snapshot, &[]).err().unwrap();
    assert!(error
        .to_string()
        .contains("cannot use a bound account client"));
    client.stop().unwrap();
    let error = client
        .lookup_contacts_snapshot(&mut snapshot, &["synthetic_peer".into()])
        .err()
        .unwrap();
    assert!(error
        .to_string()
        .contains("cannot use a bound account client"));
    assert_eq!(client.account_status().unwrap().in_flight, 0);
}

#[tokio::test]
async fn stopped_account_fences_handoff_and_late_reply_then_transfers_once() {
    let client = client();
    let (exchange, mut returned) = client.owned_account().unwrap().begin().unwrap();
    let shutdown_client = client.clone();
    let shutdown = tokio::spawn(async move { shutdown_client.shutdown().await });
    while client.account_status().unwrap().lifecycle == AccountLifecycle::Active {
        tokio::task::yield_now().await;
    }
    assert!(!shutdown.is_finished());
    assert!(client.owned_account().unwrap().begin().is_err());
    assert!(exchange.check().is_err());
    returned.cookie = vec![8];
    assert!(exchange.accept_cookie(&returned).is_err());
    drop(exchange);
    shutdown.await.unwrap().unwrap();
    let state = client.quiesce_and_export().await.unwrap();
    assert!(state.cookie.is_empty());
    assert!(client.clone().export_account_state().is_err());
    assert!(client.quiesce_and_export().await.is_err());
}

#[tokio::test]
async fn blocked_network_worker_does_not_block_other_requests() {
    let client = client();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let worker_client = client.clone();
    let worker = tokio::spawn(async move {
        worker_client
            .account_request(move |_, state, check| {
                check()?;
                let _ = started_tx.send(());
                finish_rx.recv()?;
                state.cookie = vec![1];
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    client
        .account_request(|_, state, check| {
            check()?;
            state.cookie = vec![2];
            Ok(())
        })
        .await
        .unwrap();
    finish_tx.send(()).unwrap();
    worker.await.unwrap().unwrap();
    assert_eq!(client.export_account_state().unwrap().cookie, vec![2]);
}

#[tokio::test]
async fn cancelled_waiter_keeps_worker_in_handoff_barrier() {
    let client = client();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let worker_client = client.clone();
    let worker = tokio::spawn(async move {
        worker_client
            .account_request(move |_, _, check| {
                check()?;
                let _ = started_tx.send(());
                finish_rx.recv()?;
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(client.account_status().unwrap().in_flight, 1);
    finish_tx.send(()).unwrap();
    client.shutdown().await.unwrap();
    assert_eq!(client.account_status().unwrap().in_flight, 0);
}

#[tokio::test]
async fn abandoned_request_cannot_submit_after_preparation() {
    let client = client();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let submitted = Arc::new(AtomicBool::new(false));
    let submitted_worker = submitted.clone();
    let worker_client = client.clone();
    let worker = tokio::spawn(async move {
        worker_client
            .account_request(move |_, _, check| {
                let _ = started_tx.send(());
                finish_rx.recv()?;
                check()?;
                submitted_worker.store(true, Ordering::Release);
                Ok(())
            })
            .await
    });
    started_rx.await.unwrap();
    worker.abort();
    let _ = worker.await;
    finish_tx.send(()).unwrap();
    client.shutdown().await.unwrap();
    assert!(!submitted.load(Ordering::Acquire));
}

#[tokio::test]
async fn stopped_owner_interrupts_poll_without_holding_account_lock() {
    let client = client();
    let owner = client.owned_account().unwrap().clone();
    let poll = tokio::spawn(async move {
        wait_for_poll(
            std::future::pending(),
            std::future::pending(),
            owner.stopped(),
        )
        .await
    });
    client.stop().unwrap();
    let error = poll.await.unwrap().unwrap_err();
    assert!(error.is::<crate::calls::host_transport::CallLiveSyncInterrupted>());
}

#[tokio::test]
async fn cancelled_poll_does_not_apply_a_simultaneously_ready_reply() {
    let result = wait_for_poll(
        async { Ok(vec![1, 2, 3]) },
        async {},
        std::future::pending(),
    )
    .await;
    let error = result.unwrap_err();
    assert!(error.is::<crate::calls::host_transport::CallLiveSyncInterrupted>());
    assert!(!error.is::<NativeRequestNotSubmitted>());
}

#[tokio::test]
async fn closed_admission_is_distinct_from_a_post_submission_fence() {
    let client = client();
    client.stop().unwrap();
    let result = client.account_request(|_, _, _| Ok(())).await;
    assert!(result.unwrap_err().is::<NativeRequestNotSubmitted>());
    assert_eq!(client.account_status().unwrap().in_flight, 0);
}

#[tokio::test]
async fn stopping_a_later_chunk_does_not_claim_the_whole_operation_was_unsent() {
    let client = client();
    let stop = client.clone();
    let error = client
        .account_request(move |_, _, check| {
            check()?;
            stop.stop()?;
            check()
        })
        .await
        .unwrap_err();
    assert!(!error.is::<NativeRequestNotSubmitted>());
    assert_eq!(client.account_status().unwrap().in_flight, 0);
}

#[tokio::test]
async fn verified_reply_survives_when_no_cancellation_is_ready() {
    let result = wait_for_poll(
        async { Ok(vec![1, 2, 3]) },
        std::future::pending(),
        std::future::pending(),
    )
    .await;
    assert_eq!(result.unwrap(), vec![1, 2, 3]);
}

// The envelope header follows the same independent fixture as `call_reply`,
// with a non-zero return code and no payload.
fn rejected_envelope(state: &AccountState, code: i32) -> Vec<u8> {
    let mut tail = Vec::new();
    for value in [7994, 0, 0, 0, 0] {
        tail.extend(crate::protobuf::varint(value));
    }
    let size = 10 + tail.len();
    let flags = (5u16 << 12) | ((size as u16) << 2) | 2;
    let mut reply = flags.to_le_bytes().to_vec();
    reply.extend(code.to_be_bytes());
    reply.extend(state.uin.to_be_bytes());
    reply.extend(tail);
    reply
}

#[test]
fn heartbeat_sync_and_envelope_rejections_carry_a_typed_session_error() {
    use crate::{protobuf as p, Service, ServiceRejected};
    let base = p::bytes(1, &p::int32(1, -13));
    let heartbeat = crate::wire::decode_heartbeat(&base).unwrap_err();
    let sync = crate::wire::decode_sync(&p::int32(1, -13), &[])
        .err()
        .unwrap();
    let state = client().export_account_state().unwrap();
    let envelope = crate::business::AuthenticatedPacket::prepare(&state, 7994, b"synthetic")
        .unwrap()
        .decode_verified_transport_reply(&rejected_envelope(&state, -13))
        .unwrap_err();
    for (error, service, message) in [
        (heartbeat, Service::Heartbeat, "Heartbeat returned -13"),
        (sync, Service::Sync, "Sync service returned -13"),
        (
            envelope,
            Service::NativeEnvelope,
            "Native envelope returned -13",
        ),
    ] {
        let error = error.context("request failed");
        assert_eq!(
            ServiceRejected::find(&error),
            Some(&ServiceRejected::new(service, -13))
        );
        assert!(crate::is_session_rejected(&error));
        assert_eq!(error.root_cause().to_string(), message);
    }
}

#[test]
fn blocking_shutdown_waits_for_admitted_requests() {
    let client = client();
    let (prior, _) = client.owned_account().unwrap().begin().unwrap();
    let waiter = client.clone();
    let shutdown = std::thread::spawn(move || waiter.shutdown_blocking());
    while client.account_status().unwrap().lifecycle == AccountLifecycle::Active {
        std::thread::yield_now();
    }
    std::thread::sleep(Duration::from_millis(20));
    assert!(!shutdown.is_finished());
    drop(prior);
    shutdown.join().unwrap().unwrap();
    let status = client.account_status().unwrap();
    assert_eq!(status.lifecycle, AccountLifecycle::Stopped);
    assert_eq!(status.in_flight, 0);
}

#[test]
fn blocking_logout_waits_until_it_is_the_only_operation() {
    let client = client();
    let account = client.owned_account().unwrap().clone();
    let (prior, _) = account.begin().unwrap();
    let pending = client.prepare_logout().unwrap();
    assert!(pending.exchange.check_logout().is_err());
    let waiting = account.clone();
    let drained = std::thread::spawn(move || waiting.drain_to_blocking(1));
    std::thread::sleep(Duration::from_millis(20));
    assert!(!drained.is_finished());
    drop(prior);
    drained.join().unwrap().unwrap();
    pending.exchange.check_logout().unwrap();
    // No runtime exists on this thread: reservation and the wait need none.
    assert!(tokio::runtime::Handle::try_current().is_err());
    assert!(client.logout_blocking().is_err(), "one reservation only");
    drop(pending);
    assert_eq!(client.account_status().unwrap().in_flight, 0);
}

#[test]
fn logout_guard_passes_once_and_marks_every_refusal_as_not_submitted() {
    let guard_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reserve = |client: &NativeClient| client.prepare_logout().unwrap().exchange;
    let counted = |calls: &Arc<std::sync::atomic::AtomicUsize>| {
        let calls = calls.clone();
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    };
    let idle = || Arc::new(AtomicBool::new(false));

    let client = client();
    let guard = logout_guard(reserve(&client), idle(), counted(&guard_calls));
    guard().unwrap();
    let second = guard().unwrap_err();
    assert!(!second.is::<NativeRequestNotSubmitted>());
    assert_eq!(guard_calls.load(Ordering::SeqCst), 1);

    let client = self::client();
    let refused = logout_guard(reserve(&client), idle(), || anyhow::bail!("local refusal"));
    assert!(refused().unwrap_err().is::<NativeRequestNotSubmitted>());

    let client = self::client();
    let (prior, _) = client.owned_account().unwrap().begin().unwrap();
    let busy = logout_guard(reserve(&client), idle(), counted(&guard_calls));
    assert!(busy().unwrap_err().is::<NativeRequestNotSubmitted>());
    drop(prior);

    let client = self::client();
    let cancelled = logout_guard(
        reserve(&client),
        Arc::new(AtomicBool::new(true)),
        counted(&guard_calls),
    );
    assert!(cancelled().unwrap_err().is::<NativeRequestNotSubmitted>());
    assert_eq!(guard_calls.load(Ordering::SeqCst), 1);
}
