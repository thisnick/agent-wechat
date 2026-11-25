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
    let cipher = crate::crypto::cbc_encrypt(plain, &state.session_key).unwrap();
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

fn synthetic_business_reply(
    state: &AccountState,
    cgi: u64,
    cookie: &[u8],
    plain: &[u8],
) -> Vec<u8> {
    let cipher = crate::crypto::envelope_seal(plain, &state.response_key, b"", [0x77; 12]).unwrap();
    let mut tail = Vec::new();
    for value in [cgi, plain.len() as u64, plain.len() as u64, 0, 0] {
        tail.extend(crate::protobuf::varint(value));
    }
    let size = 10 + cookie.len() + tail.len();
    let flags = (13u16 << 12) | ((cookie.len() as u16) << 8) | ((size as u16) << 2) | 2;
    let mut reply = flags.to_le_bytes().to_vec();
    reply.extend(0i32.to_be_bytes());
    reply.extend(state.uin.to_be_bytes());
    reply.extend(cookie);
    reply.extend(tail);
    reply.extend(cipher);
    reply
}

struct UnusedStreamTransport;
impl crate::transport::StreamTransport for UnusedStreamTransport {
    fn connect<'a>(
        &'a self,
        _host: &'a str,
        _port: u16,
        _timeout: Duration,
    ) -> crate::transport::TransportFuture<'a, Result<Box<dyn crate::transport::AsyncByteStream>>>
    {
        Box::pin(async {
            Err(anyhow::Error::new(
                crate::transport::TransportError::Disconnected,
            ))
        })
    }
}

struct MockClock(AtomicU64);
impl crate::transport::Clock for MockClock {
    fn monotonic_ms(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
    fn unix_time_ms(&self) -> Result<u64> {
        Ok(1_700_000_000_000 + self.monotonic_ms())
    }
}

struct ScriptedShortHttp {
    session: crate::mmtls::ShortSession,
    responses: Mutex<std::collections::VecDeque<Result<Vec<u8>>>>,
    seen_paths: Mutex<Vec<String>>,
    seen_packets: Mutex<Vec<Vec<u8>>>,
}

impl crate::transport::HttpTransport for ScriptedShortHttp {
    fn send<'a>(
        &'a self,
        request: crate::transport::HttpRequest,
    ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>> {
        let next = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(anyhow::anyhow!("unexpected extra HTTP request")));
        let outcome = next.and_then(|reply_payload| {
            let (path, host, packet, wire) = crate::mmtls::tests::handle_mock_short_exchange(
                &self.session,
                &request.body,
                &reply_payload,
            )?;
            ensure!(host == "short.weixin.qq.com", "unexpected host");
            self.seen_paths.lock().unwrap().push(path);
            self.seen_packets.lock().unwrap().push(packet);
            Ok(crate::transport::HttpResponse {
                status: 200,
                headers: vec![],
                body: wire,
            })
        });
        Box::pin(async move { outcome })
    }
}

fn synthetic_short_session() -> crate::mmtls::ShortSession {
    crate::mmtls::ShortSession {
        access_psk: vec![0x42; 32],
        ticket: crate::mmtls::Ticket {
            kind: 1,
            lifetime: 3600,
            reserved: 0,
            nonce: vec![0x24; 12],
            opaque: vec![0x17; 32],
        },
    }
}

#[tokio::test]
async fn async_transport_reuses_short_ticket_persists_cookie_and_enforces_fences() {
    use crate::protobuf as p;

    let initial = client().export_account_state().unwrap();
    let short_session = synthetic_short_session();
    let hb_plain =
        p::message(&[p::bytes(1, &p::uint(1, 0)), p::uint(2, 90), p::uint(3, 0)]).unwrap();
    let hb_reply = synthetic_business_reply(&initial, 518, b"c1", &hb_plain);

    let send_item = p::message(&[
        p::int32(1, 0),
        p::bytes(2, &p::bytes(1, b"peer_user")),
        p::uint(3, 1001),
        p::uint(4, 777),
        p::uint(5, 1_700_000_010),
        p::uint(6, 1),
        p::uint(7, 1),
        p::uint(8, 9_000_000_001),
    ])
    .unwrap();
    let send_plain = p::message(&[
        p::bytes(1, &p::uint(1, 0)),
        p::uint(2, 1),
        p::bytes(3, &send_item),
    ])
    .unwrap();
    let send_reply = synthetic_business_reply(&initial, 522, b"c2", &send_plain);

    let openim_plain = p::message(&[
        p::int32(1, 0),
        p::bytes(
            2,
            &p::message(&[p::bytes(1, b"123@openim"), p::bytes(2, b"WeCom User")]).unwrap(),
        ),
    ])
    .unwrap();
    let openim_reply = synthetic_business_reply(&initial, 881, b"c2", &openim_plain);

    let http = Arc::new(ScriptedShortHttp {
        session: short_session.clone(),
        responses: Mutex::new(
            [
                Ok(hb_reply),
                Ok(send_reply),
                Ok(openim_reply),
                Err(anyhow::Error::new(
                    crate::transport::TransportError::Timeout,
                )),
            ]
            .into_iter()
            .collect(),
        ),
        seen_paths: Mutex::new(Vec::new()),
        seen_packets: Mutex::new(Vec::new()),
    });
    let clock = Arc::new(MockClock(AtomicU64::new(100)));
    let transport = crate::transport::ProtocolTransport::new(
        Arc::new(UnusedStreamTransport),
        http.clone(),
        clock.clone(),
    );
    let storage = Arc::new(RecordingPersistence {
        fail: AtomicBool::new(false),
        cookies: Mutex::new(vec![]),
    });
    let client = NativeClient::from_account_with_transport_and_persistence(
        initial.clone(),
        NativeOptions::default(),
        transport.clone(),
        storage.clone(),
    )
    .unwrap();

    // Blocking helpers must refuse to bypass the configured async transport,
    // and logout_blocking refusal must leave the account Active.
    assert!(client.request_blocking(|_| Ok(())).is_err());
    assert!(client.open_push_connection_blocking().is_err());
    assert!(client.renew_session_blocking("synthetic").is_err());
    assert!(client.logout_blocking().is_err());
    assert_eq!(
        client.account_status().unwrap().lifecycle,
        AccountLifecycle::Active
    );
    let bootstrap = NativeClient::with_transport(NativeOptions::default(), transport).unwrap();
    assert!(bootstrap
        .start_qr_login(initial.device.clone(), "synthetic")
        .is_err());

    // Pre-seed the short-link ticket cache so RPCs reuse it without opening a long link.
    client
        .owned_account()
        .unwrap()
        .short_link
        .put_at_ms("long.weixin.qq.com", short_session, 100);

    let hb = client.heartbeat(&[]).await.unwrap();
    assert_eq!(hb.next_interval, 90);
    assert_eq!(client.export_account_state().unwrap().cookie, b"c1");
    // Verify outgoing heartbeat timestamp came from MockClock (1_700_000_000), not SystemTime.
    let sent_hb = http.seen_packets.lock().unwrap()[0].clone();
    let hb_env = crate::business::split(&sent_hb).unwrap();
    let hb_req_plain =
        crate::crypto::envelope_open(&hb_env.payload, &initial.request_key, b"").unwrap();
    let hb_req_fields = p::decode(&hb_req_plain).unwrap();
    assert_eq!(p::scalar(&hb_req_fields, 2).unwrap(), 1_700_000_000);

    // Pre-submit caller guard refuses before HTTP handoff.
    let refused = client
        .send_text_guarded(
            "peer_user",
            "blocked",
            TextSendOptions {
                created_at: 1_700_000_010,
                client_message_id: 777,
                source: None,
            },
            || anyhow::bail!("guard refused"),
        )
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("guard refused"));
    assert_eq!(http.seen_paths.lock().unwrap().len(), 1);

    let sent = client
        .send_text(
            "peer_user",
            "hello wasm",
            TextSendOptions {
                created_at: 1_700_000_010,
                client_message_id: 777,
                source: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(sent.server_message_id.as_deref(), Some("9000000001"));
    assert_eq!(client.export_account_state().unwrap().cookie, b"c2");

    let openim = client
        .lookup_openim_contact("123@openim", None, None)
        .await
        .unwrap();
    assert_eq!(openim.nickname.as_deref(), Some("WeCom User"));

    // Persistence observed both cookie transitions in order (unchanged c2 skipped).
    assert_eq!(
        *storage.cookies.lock().unwrap(),
        vec![(vec![], b"c1".to_vec()), (b"c1".to_vec(), b"c2".to_vec())]
    );

    // Transport failure invalidates cached short-link ticket without retrying.
    assert!(client.heartbeat(&[]).await.is_err());
    assert!(client
        .owned_account()
        .unwrap()
        .short_link
        .get_at_ms("long.weixin.qq.com", 100)
        .is_none());

    // Blocking logout refuses to bypass async transport and reports NativeRequestNotSubmitted;
    // stop fences future submissions.
    let pending = client.prepare_logout().unwrap();
    let blocking_logout_err = pending.submit_blocking().unwrap_err();
    assert!(blocking_logout_err.is::<NativeRequestNotSubmitted>());
    let stopped_err = client.heartbeat(&[]).await.unwrap_err();
    assert!(stopped_err.is::<NativeRequestNotSubmitted>());

    // Options override preserves account owner and configured transport.
    let updated = client
        .with_request_options(NativeOptions {
            timeout: Duration::from_secs(10),
            ..NativeOptions::default()
        })
        .unwrap();
    assert!(updated.transport.is_some());
    assert_eq!(
        updated.account_status().unwrap().lifecycle,
        AccountLifecycle::Stopped
    );
}

#[tokio::test]
async fn async_transport_multichunk_voice_stop_between_chunks_omits_not_submitted() {
    use crate::protobuf as p;

    let initial = client().export_account_state().unwrap();
    let short_session = synthetic_short_session();
    // Chunk 1 reply: accepted non-final acknowledgement (offset=0, length=32768, end_flag=0).
    let chunk1_plain = p::message(&[
        p::bytes(1, b"synthetic_self"),
        p::bytes(2, b"peer_user"),
        p::uint(3, 0),
        p::uint(4, 32768),
        p::uint(5, 1_700_000_020),
        p::bytes(6, b"voice-client-id-01"),
        p::uint(7, 101),
        p::uint(8, 2000),
        p::uint(9, 0),
        p::bytes(10, &p::uint(1, 0)),
        p::uint(12, 9_000_000_055),
    ])
    .unwrap();
    let chunk1_reply = synthetic_business_reply(&initial, 127, b"v1", &chunk1_plain);

    struct StopAfterFirstHttp {
        session: crate::mmtls::ShortSession,
        first_reply: Mutex<Option<Vec<u8>>>,
        on_first: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    impl crate::transport::HttpTransport for StopAfterFirstHttp {
        fn send<'a>(
            &'a self,
            request: crate::transport::HttpRequest,
        ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>> {
            let reply = self
                .first_reply
                .lock()
                .unwrap()
                .take()
                .expect("second chunk must not be sent after stop");
            let (_, _, _, wire) = crate::mmtls::tests::handle_mock_short_exchange(
                &self.session,
                &request.body,
                &reply,
            )
            .unwrap();
            if let Some(cb) = self.on_first.lock().unwrap().take() {
                cb();
            }
            Box::pin(async move {
                Ok(crate::transport::HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: wire,
                })
            })
        }
    }

    let http = Arc::new(StopAfterFirstHttp {
        session: short_session.clone(),
        first_reply: Mutex::new(Some(chunk1_reply)),
        on_first: Mutex::new(None),
    });
    let clock = Arc::new(MockClock(AtomicU64::new(100)));
    let transport = crate::transport::ProtocolTransport::new(
        Arc::new(UnusedStreamTransport),
        http.clone(),
        clock,
    );
    let client =
        NativeClient::from_account_with_transport(initial, NativeOptions::default(), transport)
            .unwrap();
    client
        .owned_account()
        .unwrap()
        .short_link
        .put_at_ms("long.weixin.qq.com", short_session, 100);
    let stop_handle = client.clone();
    *http.on_first.lock().unwrap() = Some(Box::new(move || {
        stop_handle.stop().unwrap();
    }));

    // Build a valid 2-chunk SILK-framed payload (> 32 KiB).
    let mut encoded = b"\x02#!SILK_V3".to_vec();
    encoded.resize(40_000, 0x11);
    let err = client
        .send_voice(
            "peer_user",
            "voice-client-id-01",
            1_700_000_020,
            2000,
            &encoded,
        )
        .await
        .unwrap_err();
    assert!(
        !err.is::<NativeRequestNotSubmitted>(),
        "stopping after chunk 1 must preserve uncertain submission"
    );
    assert_eq!(
        AccountInterrupted::find(&err),
        Some(AccountInterrupted::Stopped)
    );
}

#[tokio::test]
async fn async_transport_channels_media_rejects_overflow_clock() {
    let initial = client().export_account_state().unwrap();
    let clock = Arc::new(MockClock(AtomicU64::new(u64::MAX - 1_700_000_000_000)));
    let transport = crate::transport::ProtocolTransport::new(
        Arc::new(UnusedStreamTransport),
        Arc::new(ScriptedShortHttp {
            session: synthetic_short_session(),
            responses: Mutex::new(Default::default()),
            seen_paths: Mutex::new(Vec::new()),
            seen_packets: Mutex::new(Vec::new()),
        }),
        clock,
    );
    let client =
        NativeClient::from_account_with_transport(initial, NativeOptions::default(), transport)
            .unwrap();
    let post = crate::ChannelsPost {
        object_id: 1,
        nonce: "123_0_1_1_1_ctx".into(),
    };
    let route_entry = crate::protobuf::message(&[
        crate::protobuf::bytes(1, b"finderLive"),
        crate::protobuf::uint(2, 6673),
        crate::protobuf::bytes(3, b"/cgi-bin/micromsg-bin/transfer/h5exttransfer"),
    ])
    .unwrap();
    let ticket = crate::client::channels::h5_auth_response(
        &crate::protobuf::message(&[
            crate::protobuf::bytes(1, &crate::protobuf::uint(1, 0)),
            crate::protobuf::bytes(2, b"ticket"),
            crate::protobuf::bytes(5, &route_entry),
        ])
        .unwrap(),
    )
    .unwrap();
    let page = crate::client::channels::page_response(
        &crate::protobuf::message(&[
            crate::protobuf::bytes(1, &crate::protobuf::uint(1, 0)),
            crate::protobuf::bytes(2, b"https://channels.weixin.qq.com/web/pages/feed?a=1"),
        ])
        .unwrap(),
    )
    .unwrap();
    let err = client
        .channels_media(&post, &ticket, &page)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Clock timestamp overflow"));
}

#[tokio::test]
async fn async_transport_cancelled_rpc_keeps_barrier_and_adopts_cookie() {
    use crate::protobuf as p;

    struct CountingTokioSpawner(std::sync::atomic::AtomicUsize);
    impl crate::transport::TaskSpawner for CountingTokioSpawner {
        fn spawn(&self, task: crate::transport::TransportFuture<'static, ()>) {
            self.0.fetch_add(1, Ordering::AcqRel);
            tokio::spawn(task);
        }
    }

    struct DelayedShortHttp {
        session: crate::mmtls::ShortSession,
        reply: Mutex<Option<Vec<u8>>>,
        submitted: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }
    impl crate::transport::HttpTransport for DelayedShortHttp {
        fn send<'a>(
            &'a self,
            request: crate::transport::HttpRequest,
        ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>> {
            let reply = self.reply.lock().unwrap().take().unwrap();
            let (_, _, _, wire) = crate::mmtls::tests::handle_mock_short_exchange(
                &self.session,
                &request.body,
                &reply,
            )
            .unwrap();
            if let Some(tx) = self.submitted.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let release = self.release.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let _ = release.await;
                Ok(crate::transport::HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: wire,
                })
            })
        }
    }

    for use_custom_spawner in [false, true] {
        let initial = client().export_account_state().unwrap();
        let short_session = synthetic_short_session();
        let hb_plain =
            p::message(&[p::bytes(1, &p::uint(1, 0)), p::uint(2, 90), p::uint(3, 0)]).unwrap();
        let hb_reply = synthetic_business_reply(&initial, 518, b"c-detached", &hb_plain);
        let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let http = Arc::new(DelayedShortHttp {
            session: short_session.clone(),
            reply: Mutex::new(Some(hb_reply)),
            submitted: Mutex::new(Some(submitted_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        let clock = Arc::new(MockClock(AtomicU64::new(100)));
        let spawner = Arc::new(CountingTokioSpawner(std::sync::atomic::AtomicUsize::new(0)));
        let mut transport =
            crate::transport::ProtocolTransport::new(Arc::new(UnusedStreamTransport), http, clock);
        if use_custom_spawner {
            transport = transport.with_spawner(spawner.clone());
        }
        let client =
            NativeClient::from_account_with_transport(initial, NativeOptions::default(), transport)
                .unwrap();
        client.owned_account().unwrap().short_link.put_at_ms(
            "long.weixin.qq.com",
            short_session,
            100,
        );

        let worker_client = client.clone();
        let worker = tokio::spawn(async move { worker_client.heartbeat(&[]).await });
        submitted_rx.await.unwrap();
        worker.abort();
        let _ = worker.await;

        assert_eq!(client.account_status().unwrap().in_flight, 1);
        let handoff_client = client.clone();
        let handoff = tokio::spawn(async move { handoff_client.quiesce_and_export().await });
        tokio::task::yield_now().await;
        assert!(
            !handoff.is_finished(),
            "handoff must wait for detached transport exchange"
        );

        // Unblock the in-flight HTTP response after stopping; wait for barrier release.
        // Because stop() already happened during quiesce_and_export(), verify barrier
        // waited AND let's verify cookie adoption when caller cancels without stopping!
        release_tx.send(()).unwrap();
        let _ = handoff.await.unwrap().unwrap();
        assert_eq!(
            spawner.0.load(Ordering::Acquire),
            usize::from(use_custom_spawner)
        );
    }

    // Verify rotated cookie IS adopted when caller drops the future while account stays active.
    let initial = client().export_account_state().unwrap();
    let short_session = synthetic_short_session();
    let hb_plain =
        p::message(&[p::bytes(1, &p::uint(1, 0)), p::uint(2, 90), p::uint(3, 0)]).unwrap();
    let hb_reply = synthetic_business_reply(&initial, 518, b"c-adopted", &hb_plain);
    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let http = Arc::new(DelayedShortHttp {
        session: short_session.clone(),
        reply: Mutex::new(Some(hb_reply)),
        submitted: Mutex::new(Some(submitted_tx)),
        release: Mutex::new(Some(release_rx)),
    });
    let clock = Arc::new(MockClock(AtomicU64::new(100)));
    let transport =
        crate::transport::ProtocolTransport::new(Arc::new(UnusedStreamTransport), http, clock);
    let client =
        NativeClient::from_account_with_transport(initial, NativeOptions::default(), transport)
            .unwrap();
    client
        .owned_account()
        .unwrap()
        .short_link
        .put_at_ms("long.weixin.qq.com", short_session, 100);
    let worker_client = client.clone();
    let worker = tokio::spawn(async move { worker_client.heartbeat(&[]).await });
    submitted_rx.await.unwrap();
    worker.abort();
    let _ = worker.await;
    assert_eq!(client.account_status().unwrap().in_flight, 1);
    release_tx.send(()).unwrap();
    while client.account_status().unwrap().in_flight != 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(client.export_account_state().unwrap().cookie, b"c-adopted");
    assert_eq!(client.account_status().unwrap().revision, 1);
}

#[tokio::test]
async fn async_transport_cancelled_multichunk_voice_adopts_chunk1_cookie_and_stops_before_chunk2() {
    use crate::protobuf as p;

    let initial = client().export_account_state().unwrap();
    let short_session = synthetic_short_session();
    let chunk1_plain = p::message(&[
        p::bytes(1, b"synthetic_self"),
        p::bytes(2, b"peer_user"),
        p::uint(3, 0),
        p::uint(4, 32768),
        p::uint(5, 1_700_000_020),
        p::bytes(6, b"voice-client-id-02"),
        p::uint(7, 102),
        p::uint(8, 2000),
        p::uint(9, 0),
        p::bytes(10, &p::uint(1, 0)),
        p::uint(12, 9_000_000_056),
    ])
    .unwrap();
    let chunk1_reply = synthetic_business_reply(&initial, 127, b"v-chunk1", &chunk1_plain);

    struct DelayedFirstChunkHttp {
        session: crate::mmtls::ShortSession,
        first_reply: Mutex<Option<Vec<u8>>>,
        submitted: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl crate::transport::HttpTransport for DelayedFirstChunkHttp {
        fn send<'a>(
            &'a self,
            request: crate::transport::HttpRequest,
        ) -> crate::transport::TransportFuture<'a, Result<crate::transport::HttpResponse>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            let reply = self
                .first_reply
                .lock()
                .unwrap()
                .take()
                .expect("second chunk must not be sent after caller cancellation");
            let (_, _, _, wire) = crate::mmtls::tests::handle_mock_short_exchange(
                &self.session,
                &request.body,
                &reply,
            )
            .unwrap();
            if let Some(tx) = self.submitted.lock().unwrap().take() {
                let _ = tx.send(());
            }
            let release = self.release.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let _ = release.await;
                Ok(crate::transport::HttpResponse {
                    status: 200,
                    headers: vec![],
                    body: wire,
                })
            })
        }
    }

    let (submitted_tx, submitted_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let http = Arc::new(DelayedFirstChunkHttp {
        session: short_session.clone(),
        first_reply: Mutex::new(Some(chunk1_reply)),
        submitted: Mutex::new(Some(submitted_tx)),
        release: Mutex::new(Some(release_rx)),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let clock = Arc::new(MockClock(AtomicU64::new(100)));
    let transport = crate::transport::ProtocolTransport::new(
        Arc::new(UnusedStreamTransport),
        http.clone(),
        clock,
    );
    let client =
        NativeClient::from_account_with_transport(initial, NativeOptions::default(), transport)
            .unwrap();
    client
        .owned_account()
        .unwrap()
        .short_link
        .put_at_ms("long.weixin.qq.com", short_session, 100);

    let mut encoded = b"\x02#!SILK_V3".to_vec();
    encoded.resize(40_000, 0x11);
    let worker_client = client.clone();
    let worker = tokio::spawn(async move {
        worker_client
            .send_voice(
                "peer_user",
                "voice-client-id-02",
                1_700_000_020,
                2000,
                &encoded,
            )
            .await
    });
    submitted_rx.await.unwrap();
    worker.abort();
    let _ = worker.await;

    assert_eq!(client.account_status().unwrap().in_flight, 1);
    release_tx.send(()).unwrap();
    while client.account_status().unwrap().in_flight != 0 {
        tokio::task::yield_now().await;
    }
    assert_eq!(http.calls.load(Ordering::Acquire), 1);
    assert_eq!(client.export_account_state().unwrap().cookie, b"v-chunk1");
}
