use super::*;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Notify};

struct Attempt {
    request: BackendRequest,
    sink: BackendSink,
    reply: oneshot::Sender<Result<BackendOutcome>>,
}
struct FakeBackend {
    conference_enabled: AtomicBool,
    invited: Arc<Mutex<Vec<Vec<String>>>>,
    authorities: Arc<Mutex<BTreeMap<String, AccountAuthority>>>,
    attempts: mpsc::UnboundedSender<Attempt>,
    sinks: Mutex<BTreeMap<String, BackendSink>>,
    submitted_pcm: Arc<Mutex<Vec<Vec<i16>>>>,
    cleared: Arc<Mutex<Vec<(CallIdentity, u32)>>>,
    held_audio: Mutex<Option<oneshot::Receiver<()>>>,
    audio_started: Arc<Notify>,
    stopped: Arc<Mutex<Vec<CallIdentity>>>,
    stop_outcome: Mutex<BackendOutcome>,
}
impl FakeBackend {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Attempt>) {
        let (attempts, incoming) = mpsc::unbounded_channel();
        let authorities = ["a", "b"]
            .into_iter()
            .map(|id| (id.into(), auth(id, "generation1")))
            .collect();
        (
            Arc::new(Self {
                conference_enabled: AtomicBool::new(false),
                invited: Arc::new(Mutex::new(Vec::new())),
                authorities: Arc::new(Mutex::new(authorities)),
                attempts,
                sinks: Mutex::new(BTreeMap::new()),
                submitted_pcm: Arc::new(Mutex::new(Vec::new())),
                cleared: Arc::new(Mutex::new(Vec::new())),
                held_audio: Mutex::new(None),
                audio_started: Arc::new(Notify::new()),
                stopped: Arc::new(Mutex::new(Vec::new())),
                stop_outcome: Mutex::new(BackendOutcome::Ended),
            }),
            incoming,
        )
    }
}
impl CallBackend for FakeBackend {
    fn invite(
        &self,
        request: BackendInviteRequest,
    ) -> BackendFuture<Result<Vec<CallInvitationResult>>> {
        let enabled = self.conference_enabled.load(Ordering::Acquire);
        let invited = self.invited.clone();
        Box::pin(async move {
            if !enabled {
                return Err(error(CallErrorCode::Unsupported));
            }
            invited.lock().unwrap().push(request.contacts.clone());
            Ok(request
                .contacts
                .into_iter()
                .map(|contact_id| {
                    let rejected = contact_id == "blocked";
                    CallInvitationResult {
                        contact_id,
                        status: if rejected {
                            CallInvitationStatus::Rejected
                        } else {
                            CallInvitationStatus::Submitted
                        },
                        service_code: Some(if rejected { 5 } else { 0 }),
                        verification_required: false,
                    }
                })
                .collect())
        })
    }
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        self.authorities.lock().unwrap().get(account).cloned()
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: self.authority(account).is_some(),
            conference_create: self.conference_enabled.load(Ordering::Acquire),
            conference_invite: self.conference_enabled.load(Ordering::Acquire),
            ..CallCapabilities::default()
        }
    }
    fn submit(
        &self,
        request: BackendRequest,
        sink: BackendSink,
    ) -> BackendFuture<Result<BackendOutcome>> {
        let (reply, result) = oneshot::channel();
        self.sinks
            .lock()
            .unwrap()
            .insert(request.ticket.call_id().into(), sink.clone());
        self.attempts
            .send(Attempt {
                request,
                sink,
                reply,
            })
            .unwrap_or_else(|_| panic!("test receiver must remain alive"));
        Box::pin(async {
            result
                .await
                .unwrap_or_else(|_| Err(error(CallErrorCode::OutcomeUnknown)))
        })
    }
    fn send_audio(&self, request: BackendAudioRequest) -> BackendFuture<Result<()>> {
        let sink = self
            .sinks
            .lock()
            .unwrap()
            .get(request.identity.call_id())
            .cloned();
        let submitted = self.submitted_pcm.clone();
        let authorities = self.authorities.clone();
        let held = self.held_audio.lock().unwrap().take();
        let started = self.audio_started.clone();
        Box::pin(async move {
            if let Some(held) = held {
                started.notify_one();
                let _ = held.await;
            }
            if !request.is_current() {
                return Err(error(CallErrorCode::Stale));
            }
            if let Some(sink) = &sink {
                let expected = sink.ticket.authority();
                if authorities.lock().unwrap().get(&expected.account_id) != Some(expected) {
                    return Err(error(CallErrorCode::Stale));
                }
            }
            submitted.lock().unwrap().push(request.samples.clone());
            Ok(())
        })
    }
    fn clear_audio(&self, identity: CallIdentity, generation: u32) -> BackendFuture<Result<()>> {
        self.cleared.lock().unwrap().push((identity, generation));
        Box::pin(async { Ok(()) })
    }
    fn stop(&self, identity: CallIdentity) -> BackendFuture<Result<BackendOutcome>> {
        self.stopped.lock().unwrap().push(identity);
        let outcome = *self.stop_outcome.lock().unwrap();
        Box::pin(async move { Ok(outcome) })
    }
    fn close_local(
        &self,
        _: CallIdentity,
        _: wechat_store::CallLocalEndReason,
    ) -> BackendFuture<Result<()>> {
        // This policy fixture has no actor or remote request to keep running.
        Box::pin(async { Ok(()) })
    }
}
fn auth(account: &str, generation: &str) -> AccountAuthority {
    AccountAuthority {
        account_id: account.into(),
        generation: generation.into(),
    }
}
fn dial(request: &str) -> CallCommand {
    CallCommand::Dial {
        request_id: request.into(),
        target: CallTarget::Direct {
            contact_id: "synthetic-contact".into(),
        },
        call_type: CallType::Voice,
    }
}
async fn next_attempt(receiver: &mut mpsc::UnboundedReceiver<Attempt>) -> Attempt {
    tokio::time::timeout(Duration::from_secs(1), receiver.recv())
        .await
        .expect("backend dispatch deadline")
        .expect("backend receiver")
}
async fn result(
    connection: &mut ControlConnection,
    request: &str,
) -> (
    CallCommandStatus,
    Option<String>,
    Option<String>,
    Option<CallErrorCode>,
) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(event) = connection.events.recv().await {
            if let CallEvent::CommandResult {
                request_id,
                status,
                call_id,
                owner_token,
                code,
            } = event
            {
                if request_id == request {
                    return (status, call_id, owner_token, code);
                }
            }
        }
        panic!("control connection closed before result");
    })
    .await
    .expect("command result deadline")
}
async fn state(connection: &mut ControlConnection, expected: CallState) -> CallView {
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(event) = connection.events.recv().await {
            if let CallEvent::State { call } = event {
                if call.state == expected {
                    return call;
                }
            }
        }
        panic!("control connection closed before state");
    })
    .await
    .expect("call state deadline")
}
async fn ready(service: &Arc<CallService>, account: &str, principal: &str) -> ControlConnection {
    let mut connection = service.connect(account, principal, true).await.unwrap();
    assert!(matches!(
        connection.events.recv().await,
        Some(CallEvent::Hello { .. })
    ));
    service
        .command(
            &connection.client_id,
            CallCommand::Ready {
                request_id: "ready".into(),
                ready: true,
                format: CallAudioFormat::default(),
                scope: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut connection, "ready").await.0,
        CallCommandStatus::Accepted
    );
    connection
}
async fn connected_call(
    service: &Arc<CallService>,
    attempts: &mut mpsc::UnboundedReceiver<Attempt>,
    connection: &mut ControlConnection,
) -> (BackendSink, String, String) {
    service
        .command(&connection.client_id, dial("dial"))
        .await
        .unwrap();
    let (_, id, token, _) = result(connection, "dial").await;
    let attempt = next_attempt(attempts).await;
    let Attempt { sink, reply, .. } = attempt;
    reply
        .send(Ok(BackendOutcome::Connected))
        .unwrap_or_else(|_| panic!("active operation"));
    state(connection, CallState::Connected).await;
    (sink, id.unwrap(), token.unwrap())
}

#[tokio::test]
async fn slow_media_reader_retains_recent_audio_and_call_ownership() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();

    let overflow = 4;
    let sent = MEDIA_RECEIVE_CAPACITY + overflow;
    for sequence in 0..sent {
        sink.audio(&[sequence as i16; 320]).await.unwrap();
    }
    assert_eq!(media.frames.pending_frames(), MEDIA_RECEIVE_CAPACITY);
    assert!(matches!(
        media.frames.recv_frame().await,
        Err(broadcast::error::RecvError::Lagged(frames)) if frames == overflow as u64
    ));
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    for sequence in overflow..sent {
        let bytes = media.frames.try_recv().unwrap();
        let frame = MediaFrame::decode(&bytes, format).unwrap();
        assert_eq!(frame.header.sequence, sequence as u64);
        assert_eq!(frame.header.timestamp_micros, sequence as u64 * 20_000);
        assert_eq!(frame.samples().next(), Some(sequence as i16));
    }
    assert_eq!(media.frames.pending_frames(), 0);
    assert!(service.media_delivery_allowed(&media.id).await.unwrap());
    service.validate_control(&owner.client_id).await.unwrap();

    sink.audio(&[123; 320]).await.unwrap();
    let bytes = media.frames.try_recv().unwrap();
    let frame = MediaFrame::decode(&bytes, format).unwrap();
    assert_eq!(frame.header.sequence, sent as u64);
    assert_eq!(frame.samples().next(), Some(123));

    // Losing the actual receiver still revokes the control owner.
    drop(media.frames);
    sink.audio(&[123; 320]).await.unwrap();
    assert!(service.validate_control(&owner.client_id).await.is_err());
}

#[tokio::test]
async fn mute_interrupts_queued_audio_once_and_sends_silence_in_the_new_generation() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut observer = service.connect("a", "observer", false).await.unwrap();
    let mut other = service.connect("b", "other", false).await.unwrap();
    assert!(matches!(
        observer.events.recv().await.unwrap(),
        CallEvent::Hello { .. }
    ));
    assert!(matches!(
        other.events.recv().await.unwrap(),
        CallEvent::Hello {
            current_call: None,
            ..
        }
    ));
    for muted in [false, true, false] {
        sink.peer_microphone(muted).await.unwrap();
        for connection in [&mut owner, &mut observer] {
            let event = tokio::time::timeout(Duration::from_secs(1), connection.events.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(event, CallEvent::Control {
                call_id, participant_id, control: CallControl::Microphone { muted: value }
            } if call_id == call && participant_id == "synthetic-contact" && value == muted));
        }
        assert!(other.events.try_recv().is_err());
        let snapshot = service.current("a", "observer").await.unwrap();
        let control = &snapshot.current_call.unwrap().controls[0];
        assert_eq!(control.participant_id, "synthetic-contact");
        assert_eq!(control.control, CallControl::Microphone { muted });
        sink.peer_microphone(muted).await.unwrap();
        assert!(owner.events.try_recv().is_err());
        assert!(observer.events.try_recv().is_err());
    }
    let mut late = service.connect("a", "late observer", false).await.unwrap();
    let CallEvent::Hello {
        current_call: Some(view),
        ..
    } = late.events.recv().await.unwrap()
    else {
        panic!("Current controls must be included for a late observer")
    };
    assert_eq!(
        view.controls[0].control,
        CallControl::Microphone { muted: false }
    );
    assert!(!view.owned_by_this_client);
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    sink.audio(&[100; 320]).await.unwrap();
    let received = media.frames.recv().await.unwrap();
    let before = MediaFrame::decode(&received, AudioFormat::new(16000, 1, 20).unwrap()).unwrap();
    assert_eq!((before.header.generation, before.header.sequence), (1, 0));
    let (release, held) = oneshot::channel();
    *backend.held_audio.lock().unwrap() = Some(held);
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let bytes = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        format,
        &[1234; 320],
    )
    .unwrap();
    let sender = service.clone();
    let id = media.id.clone();
    let sending = tokio::spawn(async move { sender.media_frame(&id, &bytes).await });
    tokio::time::timeout(Duration::from_secs(1), backend.audio_started.notified())
        .await
        .unwrap();
    for request in ["mute-one", "mute-again"] {
        service
            .command(
                &owner.client_id,
                CallCommand::Mute {
                    request_id: request.into(),
                    call_id: call.clone(),
                    muted: true,
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(backend.cleared.lock().unwrap().len(), 1);
    release.send(()).unwrap();
    sending.await.unwrap().unwrap();
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    let bytes = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 2,
            sequence: 0,
            timestamp_micros: 0,
        },
        format,
        &[1234; 320],
    )
    .unwrap();
    service.media_frame(&media.id, &bytes).await.unwrap();
    assert_eq!(
        backend.submitted_pcm.lock().unwrap().as_slice(),
        &[vec![0; 320]]
    );

    service
        .command(
            &owner.client_id,
            CallCommand::ClearAudio {
                request_id: "clear-muted".into(),
                call_id: call.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(backend.cleared.lock().unwrap().len(), 2);
    sink.audio(&[200; 320]).await.unwrap();
    let received = media.frames.recv().await.unwrap();
    let after = MediaFrame::decode(&received, format).unwrap();
    assert_eq!(
        (
            after.header.generation,
            after.header.sequence,
            after.header.timestamp_micros
        ),
        (1, 1, 20_000)
    );
    // Old TX generation is dropped without revoking the owner or rewinding RX.
    service.media_frame(&media.id, &bytes).await.unwrap();
    assert_eq!(backend.submitted_pcm.lock().unwrap().len(), 1);
    service.validate_media(&media.id).await.unwrap();
    sink.ended().await.unwrap();
    assert!(sink.peer_microphone(true).await.is_err());
}

#[tokio::test]
async fn held_initial_start_is_revoked_by_cancel_or_disconnect_and_cannot_revive() {
    for (cancel, terminal) in [(true, false), (false, false), (true, true)] {
        let (backend, mut attempts) = FakeBackend::new();
        *backend.stop_outcome.lock().unwrap() = BackendOutcome::Uncertain;
        let service = Arc::new(CallService::new(backend.clone()));
        let mut owner = ready(&service, "a", "owner").await;
        service
            .command(&owner.client_id, dial("held-start"))
            .await
            .unwrap();
        let (_, call, _, _) = result(&mut owner, "held-start").await;
        let view = state(&mut owner, CallState::Dialing).await;
        let attempt = next_attempt(&mut attempts).await;
        assert!(attempt.sink.is_current());
        if cancel {
            service
                .command(
                    &owner.client_id,
                    CallCommand::Cancel {
                        request_id: "cancel-held".into(),
                        call_id: call.unwrap(),
                        expected_revision: view.revision,
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                result(&mut owner, "cancel-held").await.0,
                CallCommandStatus::Accepted
            );
            assert!(attempts.try_recv().is_err());
        } else {
            service.disconnect(&owner.client_id).await;
        }
        assert!(!attempt.sink.is_current());
        attempt
            .reply
            .send(Ok(if terminal {
                BackendOutcome::Ended
            } else {
                BackendOutcome::Connected
            }))
            .unwrap_or_else(|_| panic!("held start outcome"));
        for _ in 0..100 {
            tokio::task::yield_now().await;
            if !service
                .current("a", "observer")
                .await
                .unwrap()
                .availability
                .busy
            {
                break;
            }
        }
        assert!(
            !service
                .current("a", "observer")
                .await
                .unwrap()
                .availability
                .busy
        );
        assert!(!attempt.sink.is_current());
    }
}

#[tokio::test]
async fn slow_owner_control_reader_is_disconnected_and_queued_media_is_not_authorized() {
    let (backend, mut attempts) = FakeBackend::new();
    *backend.stop_outcome.lock().unwrap() = BackendOutcome::Uncertain;
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    sink.audio(&[0; 320]).await.unwrap();
    for index in 0..40 {
        if service
            .command(
                &owner.client_id,
                CallCommand::Ready {
                    request_id: format!("unread-{index}"),
                    ready: true,
                    format: CallAudioFormat::default(),
                    scope: None,
                },
            )
            .await
            .is_err()
        {
            break;
        }
    }
    assert!(service.validate_control(&owner.client_id).await.is_err());
    assert!(service.validate_media(&media.id).await.is_err());
    // Revoking the owner closes the queue immediately. No retained frame may
    // escape on the old attachment, even before its API writer revalidates.
    assert!(media.frames.recv().await.is_none());
    assert!(sink.audio(&[0; 320]).await.is_err());

    service.disconnect(&owner.client_id).await;
    for _ in 0..100 {
        if !service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        !service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    assert_eq!(backend.stopped.lock().unwrap().len(), 1);
    service.disconnect(&owner.client_id).await;
    service.current("a", "owner").await.unwrap();
    assert_eq!(backend.stopped.lock().unwrap().len(), 1);
    assert!(sink.ended().await.is_err());

    // A fresh controller can own a new call. Graceful shutdown stops it once
    // and a late callback cannot recreate a session in this daemon.
    let mut next_owner = ready(&service, "a", "next owner").await;
    let (next_sink, _, _) = connected_call(&service, &mut attempts, &mut next_owner).await;
    service.shutdown().await;
    service.shutdown().await;
    assert_eq!(backend.stopped.lock().unwrap().len(), 2);
    assert!(
        !service
            .current("a", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    assert!(service.connect("a", "new", false).await.is_err());
    assert!(next_sink.connected().await.is_err());
}

#[tokio::test]
async fn hangup_fences_queued_audio_before_the_remote_stop_result() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    sink.audio(&[123; 320]).await.unwrap();
    let (release, held) = oneshot::channel();
    *backend.held_audio.lock().unwrap() = Some(held);
    let bytes = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        AudioFormat::new(16000, 1, 20).unwrap(),
        &[1000; 320],
    )
    .unwrap();
    let sender_service = service.clone();
    let media_id = media.id.clone();
    let sending = tokio::spawn(async move { sender_service.media_frame(&media_id, &bytes).await });
    tokio::time::timeout(Duration::from_secs(1), backend.audio_started.notified())
        .await
        .unwrap();
    service
        .command(
            &owner.client_id,
            CallCommand::Hangup {
                request_id: "hangup".into(),
                call_id: call.clone(),
                expected_revision: 2,
            },
        )
        .await
        .unwrap();
    let stopping = next_attempt(&mut attempts).await;
    assert_eq!(stopping.request.ticket.operation(), Operation::Hangup);
    // A queued receive frame remains owned but is no longer deliverable. The
    // existing attachment stays live until the backend confirms terminal state.
    assert!(media.frames.try_recv().is_ok());
    assert!(!service.media_delivery_allowed(&media.id).await.unwrap());
    service.validate_media(&media.id).await.unwrap();
    service.validate_control(&owner.client_id).await.unwrap();
    assert!(service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .is_err());
    let late = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 1,
            timestamp_micros: 20_000,
        },
        AudioFormat::new(16000, 1, 20).unwrap(),
        &[2000; 320],
    )
    .unwrap();
    service.media_frame(&media.id, &late).await.unwrap();
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    assert!(sink.audio(&[456; 320]).await.is_err());
    release.send(()).unwrap();
    sending.await.unwrap().unwrap();
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    service.validate_media(&media.id).await.unwrap();
    assert!(
        service
            .current("a", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    assert!(backend.stopped.lock().unwrap().is_empty());
    stopping
        .reply
        .send(Ok(BackendOutcome::Ended))
        .unwrap_or_else(|_| panic!("active stop"));
    state(&mut owner, CallState::Ended).await;
    assert!(service.validate_media(&media.id).await.is_err());
}

#[tokio::test]
async fn ending_transport_still_rejects_foreign_frames_and_revoked_authority() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (_, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    service
        .command(
            &owner.client_id,
            CallCommand::Hangup {
                request_id: "held-hangup".into(),
                call_id: call,
                expected_revision: 2,
            },
        )
        .await
        .unwrap();
    let stop = next_attempt(&mut attempts).await;
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    for (track_id, generation) in [(2, 1), (1, 2)] {
        let bytes = frame::encode_pcm(
            MediaHeader {
                track_id,
                generation,
                sequence: 0,
                timestamp_micros: 0,
            },
            format,
            &[1; 320],
        )
        .unwrap();
        assert!(service.media_frame(&media.id, &bytes).await.is_err());
    }
    service.validate_media(&media.id).await.unwrap();
    backend
        .authorities
        .lock()
        .unwrap()
        .insert("a".into(), auth("a", "replacement"));
    assert!(service.validate_media(&media.id).await.is_err());
    assert!(service.media_delivery_allowed(&media.id).await.is_err());
    assert!(service.validate_control(&owner.client_id).await.is_err());
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    drop(stop);
}

#[tokio::test]
async fn recovered_unknown_calls_block_all_accounts_without_fabricating_an_owner_or_sending() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend.clone()));
    service.recover_unknown("a", "old-a").unwrap();
    service.recover_unknown("b", "old-b").unwrap();
    service.recover_unknown("a", "old-a").unwrap();
    let mut client = service.connect("a", "observer", true).await.unwrap();
    assert!(matches!(
        client.events.recv().await,
        Some(CallEvent::Hello {
            availability: CallsAvailability {
                busy: true,
                owned_by_this_client: false
            },
            current_offer: None,
            current_call: None,
            ..
        })
    ));
    assert!(matches!(
        client.events.recv().await,
        Some(CallEvent::Error {
            code: CallErrorCode::OutcomeUnknown
        })
    ));
    service
        .command(
            &client.client_id,
            CallCommand::Ready {
                request_id: "ready".into(),
                ready: true,
                format: CallAudioFormat::default(),
                scope: None,
            },
        )
        .await
        .unwrap();
    result(&mut client, "ready").await;
    service
        .command(&client.client_id, dial("blocked"))
        .await
        .unwrap();
    assert_eq!(
        result(&mut client, "blocked").await.3,
        Some(CallErrorCode::Busy)
    );
    assert!(matches!(
        service
            .offer(
                auth("a", "generation1"),
                "new".into(),
                "synthetic-contact".into(),
                Duration::from_secs(1)
            )
            .await,
        Err(CallServiceError(CallErrorCode::Busy))
    ));
    assert!(
        service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    assert_eq!(service.lock().unwrap().recovered_unknown.len(), 2);
    assert!(attempts.try_recv().is_err());
    assert!(backend.stopped.lock().unwrap().is_empty());
}

#[tokio::test]
async fn explicit_reject_guard_is_pending_ticket_bound_and_stops_on_owner_loss_or_replacement() {
    for revoke in [false, true] {
        let (backend, mut attempts) = FakeBackend::new();
        let service = Arc::new(CallService::new(backend.clone()));
        let owner = ready(&service, "a", "owner").await;
        service
            .offer(
                auth("a", "generation1"),
                "reused-id".into(),
                "peer".into(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        service
            .command(
                &owner.client_id,
                CallCommand::Reject {
                    request_id: "reject".into(),
                    call_id: "reused-id".into(),
                    expected_revision: 1,
                },
            )
            .await
            .unwrap();
        let rejecting = next_attempt(&mut attempts).await;
        assert!(!rejecting.sink.is_current());
        assert!(rejecting.sink.is_terminal_current());
        if revoke {
            backend
                .authorities
                .lock()
                .unwrap()
                .insert("a".into(), auth("a", "replacement"));
            assert!(!rejecting.sink.is_terminal_current());
            continue;
        }
        service.disconnect(&owner.client_id).await;
        assert!(!rejecting.sink.is_terminal_current());
        rejecting
            .reply
            .send(Ok(BackendOutcome::Ended))
            .unwrap_or_else(|_| panic!("reject receiver"));
        for _ in 0..100 {
            if !service
                .current("a", "fixture")
                .await
                .unwrap()
                .availability
                .busy
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        let mut next = ready(&service, "a", "new-owner").await;
        service
            .offer(
                auth("a", "generation1"),
                "reused-id".into(),
                "peer".into(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        service
            .command(
                &next.client_id,
                CallCommand::Reject {
                    request_id: "new-reject".into(),
                    call_id: "reused-id".into(),
                    expected_revision: 1,
                },
            )
            .await
            .unwrap();
        let newer = next_attempt(&mut attempts).await;
        assert!(newer.sink.is_terminal_current());
        assert!(!rejecting.sink.is_terminal_current());
        newer
            .reply
            .send(Ok(BackendOutcome::Ended))
            .unwrap_or_else(|_| panic!("new reject receiver"));
        state(&mut next, CallState::Ended).await;
    }
}

#[tokio::test]
async fn track_controls_precede_pcm_and_retired_generations_cannot_return() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    let track = |generation| CallAudioTrack {
        id: 3,
        generation,
        direction: CallTrackDirection::Receive,
        format: CallAudioFormat::default(),
        participant_id: Some("participant-a".into()),
    };
    sink.receive_tracks(vec![track(1)]).await.unwrap();
    sink.track_audio(
        MediaHeader {
            track_id: 3,
            generation: 1,
            sequence: 0,
            timestamp_micros: 100_000,
        },
        &[123; 320],
    )
    .await
    .unwrap();
    assert!(matches!(
        media.frames.recv_delivery().await.unwrap(),
        MediaDelivery::Control(CallEvent::TrackRemoved { track_id: 2, .. })
    ));
    assert!(matches!(
        media.frames.recv_delivery().await.unwrap(),
        MediaDelivery::Control(CallEvent::TrackAdded {
            track: CallAudioTrack {
                id: 3,
                generation: 1,
                ..
            },
            ..
        })
    ));
    assert!(matches!(
        media.frames.recv_delivery().await.unwrap(),
        MediaDelivery::Audio(_)
    ));
    sink.track_audio(
        MediaHeader {
            track_id: 3,
            generation: 1,
            sequence: 1,
            timestamp_micros: 120_000,
        },
        &[456; 320],
    )
    .await
    .unwrap();
    sink.receive_tracks(vec![]).await.unwrap();
    assert_eq!(media.frames.pending_frames(), 0);
    assert!(matches!(
        media.frames.recv_delivery().await.unwrap(),
        MediaDelivery::Control(CallEvent::TrackRemoved {
            track_id: 3,
            generation: 1,
            ..
        })
    ));
    assert!(sink.receive_tracks(vec![track(1)]).await.is_err());
    assert!(sink
        .track_audio(
            MediaHeader {
                track_id: 3,
                generation: 1,
                sequence: 2,
                timestamp_micros: 140_000
            },
            &[789; 320]
        )
        .await
        .is_err());
    service.validate_media(&media.id).await.unwrap();
    sink.receive_tracks(vec![track(2)]).await.unwrap();
    assert!(matches!(
        media.frames.recv_delivery().await.unwrap(),
        MediaDelivery::Control(CallEvent::TrackAdded {
            track: CallAudioTrack {
                id: 3,
                generation: 2,
                ..
            },
            ..
        })
    ));
    service.validate_control(&owner.client_id).await.unwrap();
}

#[tokio::test]
async fn participant_microphone_and_departure_are_separate_ordered_updates() {
    let (backend, mut attempts) = FakeBackend::new();
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, _, _) = connected_call(&service, &mut attempts, &mut owner).await;
    let participant = |microphone_enabled| CallParticipant {
        participant_id: "participant-a".into(),
        contact_id: "contact-a".into(),
        membership: CallMembership::Joined,
        microphone_enabled,
    };
    sink.participants(vec![participant(None)]).await.unwrap();
    sink.participants(vec![participant(Some(false))])
        .await
        .unwrap();
    sink.participants(vec![]).await.unwrap();
    let mut updates = Vec::new();
    while let Ok(event) = owner.events.try_recv() {
        if let CallEvent::ParticipantUpdated {
            revision,
            participant,
            ..
        } = event
        {
            updates.push((revision, participant));
        }
    }
    assert_eq!(updates.len(), 3);
    assert!(updates.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert_eq!(updates[1].1.membership, CallMembership::Joined);
    assert_eq!(updates[1].1.microphone_enabled, Some(false));
    assert_eq!(updates[2].1.membership, CallMembership::Left);
    assert_eq!(updates[2].1.microphone_enabled, None);
}

#[tokio::test]
async fn leave_survives_roster_revisions_and_repeated_requests_submit_only_once() {
    for racing_roster in [false, true] {
        let (backend, mut attempts) = FakeBackend::new();
        let service = Arc::new(CallService::new(backend));
        let mut owner = ready(&service, "a", "owner").await;
        let (sink, call, _) = connected_call(&service, &mut attempts, &mut owner).await;
        let observed = service
            .current("a", "owner")
            .await
            .unwrap()
            .current_call
            .unwrap();
        sink.participants(vec![CallParticipant {
            participant_id: "p-a".into(),
            contact_id: "contact-a".into(),
            membership: CallMembership::Joined,
            microphone_enabled: Some(false),
        }])
        .await
        .unwrap();
        sink.participants(vec![]).await.unwrap();
        let newer = service
            .current("a", "owner")
            .await
            .unwrap()
            .current_call
            .unwrap();
        assert!(newer.revision > observed.revision);
        service
            .command(
                &owner.client_id,
                CallCommand::Hangup {
                    request_id: "leave".into(),
                    call_id: call.clone(),
                    expected_revision: if racing_roster {
                        observed.revision
                    } else {
                        newer.revision
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result(&mut owner, "leave").await.0,
            CallCommandStatus::Accepted
        );
        let stopping = next_attempt(&mut attempts).await;
        assert_eq!(stopping.request.ticket.operation(), Operation::Hangup);
        let ending = service
            .current("a", "owner")
            .await
            .unwrap()
            .current_call
            .unwrap();
        assert!(ending.revision > newer.revision);
        service
            .command(
                &owner.client_id,
                CallCommand::Hangup {
                    request_id: "leave-again".into(),
                    call_id: call,
                    expected_revision: observed.revision,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result(&mut owner, "leave-again").await.0,
            CallCommandStatus::Accepted
        );
        assert!(matches!(
            attempts.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        stopping
            .reply
            .send(Ok(BackendOutcome::Ended))
            .unwrap_or_else(|_| panic!("active stopping operation"));
        state(&mut owner, CallState::Ended).await;
    }
}

#[tokio::test]
async fn conference_creation_and_later_invitation_keep_admission_outcomes_separate_from_membership()
{
    let (backend, mut attempts) = FakeBackend::new();
    backend.conference_enabled.store(true, Ordering::Release);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    service
        .command(
            &owner.client_id,
            CallCommand::Dial {
                request_id: "create".into(),
                target: CallTarget::Conference {
                    group_id: "test@chatroom".into(),
                    invitees: vec!["contact-a".into()],
                },
                call_type: CallType::Voice,
            },
        )
        .await
        .unwrap();
    let (_, call, _, _) = result(&mut owner, "create").await;
    let call = call.unwrap();
    let attempt = next_attempt(&mut attempts).await;
    assert_eq!(attempt.request.peer_id, "test@chatroom");
    assert_eq!(
        attempt.request.conference_invitees,
        Some(vec!["contact-a".into()])
    );
    assert!(attempt.request.conference_invitation.is_none());
    attempt
        .reply
        .send(Ok(BackendOutcome::Connected))
        .unwrap_or_else(|_| panic!("active create"));
    state(&mut owner, CallState::Connected).await;
    let invite = CallCommand::Invite {
        request_id: "invite".into(),
        call_id: call.clone(),
        contact_ids: vec!["contact-b".into(), "blocked".into()],
    };
    service
        .command(&owner.client_id, invite.clone())
        .await
        .unwrap();
    assert_eq!(
        result(&mut owner, "invite").await.0,
        CallCommandStatus::Accepted
    );
    let results = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = owner
                .events
                .recv()
                .await
                .expect("invitation owner remains connected");
            if let CallEvent::InvitationResult {
                request_id,
                results,
                ..
            } = event
            {
                if request_id == "invite" {
                    break results;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(results[0].status, CallInvitationStatus::Submitted);
    assert_eq!(results[1].status, CallInvitationStatus::Rejected);
    assert!(service
        .current("a", "owner")
        .await
        .unwrap()
        .current_call
        .unwrap()
        .participants
        .is_empty());
    service.command(&owner.client_id, invite).await.unwrap();
    assert_eq!(
        result(&mut owner, "invite").await.0,
        CallCommandStatus::Replayed
    );
    assert_eq!(backend.invited.lock().unwrap().len(), 1);
}
