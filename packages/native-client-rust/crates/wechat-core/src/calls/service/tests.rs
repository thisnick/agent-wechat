use super::*;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Barrier, Notify};
#[cfg(feature = "native-calls")]
use wechat_calls::{
    audio::{AudioDecoder, AudioEncoder},
    opus::{OpusDecoder, OpusEncoder},
    tone::PulseTone,
};

struct Attempt {
    request: BackendRequest,
    sink: BackendSink,
    reply: oneshot::Sender<Result<BackendOutcome>>,
}
struct FakeBackend {
    authorities: Arc<Mutex<BTreeMap<String, AccountAuthority>>>,
    attempts: mpsc::UnboundedSender<Attempt>,
    sinks: Mutex<BTreeMap<String, BackendSink>>,
    submitted_pcm: Arc<Mutex<Vec<Vec<i16>>>>,
    cleared: Arc<Mutex<Vec<(CallIdentity, u32)>>>,
    #[cfg(feature = "native-calls")]
    codec: Arc<Mutex<Option<(OpusEncoder, OpusDecoder)>>>,
    held_audio: Mutex<Option<oneshot::Receiver<()>>>,
    audio_started: Arc<Notify>,
    stopped: Arc<Mutex<Vec<CallIdentity>>>,
    stop_outcome: Mutex<BackendOutcome>,
}
impl FakeBackend {
    fn new(loopback: bool) -> (Arc<Self>, mpsc::UnboundedReceiver<Attempt>) {
        let (attempts, incoming) = mpsc::unbounded_channel();
        let authorities = ["a", "b"]
            .into_iter()
            .map(|id| (id.into(), auth(id, "generation1")))
            .collect();
        #[cfg(feature = "native-calls")]
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        #[cfg(not(feature = "native-calls"))]
        let _ = loopback;
        #[cfg(feature = "native-calls")]
        let codec = loopback.then(|| {
            (
                OpusEncoder::new(format, 20000).unwrap(),
                OpusDecoder::new(format).unwrap(),
            )
        });
        (
            Arc::new(Self {
                authorities: Arc::new(Mutex::new(authorities)),
                attempts,
                sinks: Mutex::new(BTreeMap::new()),
                submitted_pcm: Arc::new(Mutex::new(Vec::new())),
                cleared: Arc::new(Mutex::new(Vec::new())),
                #[cfg(feature = "native-calls")]
                codec: Arc::new(Mutex::new(codec)),
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
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        self.authorities.lock().unwrap().get(account).cloned()
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        CallCapabilities {
            direct_audio: self.authority(account).is_some(),
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
        #[cfg(feature = "native-calls")]
        let codec = self.codec.clone();
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
            #[cfg(not(feature = "native-calls"))]
            let decoded: Option<Vec<i16>> = None;
            #[cfg(feature = "native-calls")]
            let decoded = codec.lock().unwrap().as_mut().map(|(encoder, decoder)| {
                let packet = encoder.encode(&request.samples).unwrap();
                decoder.decode(&packet).unwrap()
            });
            if let (Some(sink), Some(decoded)) = (sink, decoded) {
                sink.audio(&decoded).await?;
            }
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
        media: CallMedia::default(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn simultaneous_answers_dispatch_once_and_other_accounts_only_see_busy() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let mut one = ready(&service, "a", "one").await;
    let mut two = ready(&service, "a", "two").await;
    let mut other = ready(&service, "b", "three").await;
    let identity = service
        .offer(
            auth("a", "generation1"),
            "incoming".into(),
            "synthetic-contact".into(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let workers = [one.client_id.clone(), two.client_id.clone()]
        .into_iter()
        .map(|client| {
            let service = service.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                service
                    .command(
                        &client,
                        CallCommand::Answer {
                            request_id: "answer".into(),
                            call_id: "incoming".into(),
                            expected_revision: 1,
                            media: CallMedia::default(),
                        },
                    )
                    .await
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.await.unwrap().unwrap();
    }
    let first = result(&mut one, "answer").await;
    let second = result(&mut two, "answer").await;
    assert_eq!(
        [first.0, second.0]
            .into_iter()
            .filter(|status| *status == CallCommandStatus::Accepted)
            .count(),
        1
    );
    assert_eq!(
        [first.0, second.0]
            .into_iter()
            .filter(|status| *status == CallCommandStatus::Error)
            .count(),
        1
    );
    let attempt = next_attempt(&mut attempts).await;
    assert_eq!(attempt.request.ticket.operation(), Operation::Answer);
    assert_eq!(attempt.request.ticket.identity(), identity);
    assert!(attempts.try_recv().is_err());
    let snap = service.current("b", "three").await.unwrap();
    assert!(snap.availability.busy);
    assert!(snap.current_offer.is_none());
    assert!(snap.current_call.is_none());
    service
        .command(&other.client_id, dial("busy"))
        .await
        .unwrap();
    assert_eq!(
        result(&mut other, "busy").await.3,
        Some(CallErrorCode::Busy)
    );
}

#[tokio::test]
async fn media_attachment_requires_winning_connection_principal_account_and_lease() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let observer = ready(&service, "a", "observer").await;
    let (_attempt, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    assert!(matches!(
        service
            .attach_media("a", "observer", &call, &owner.client_id, &token)
            .await,
        Err(CallServiceError(CallErrorCode::Forbidden))
    ));
    assert!(matches!(
        service
            .attach_media("a", "observer", &call, &observer.client_id, &token)
            .await,
        Err(CallServiceError(CallErrorCode::NotOwner))
    ));
    assert!(matches!(
        service
            .attach_media("b", "owner", &call, &owner.client_id, &token)
            .await,
        Err(CallServiceError(CallErrorCode::Forbidden))
    ));
    assert!(service
        .attach_media("a", "owner", &call, &owner.client_id, &"x".repeat(64))
        .await
        .is_err());
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    assert!(matches!(
        media.ready,
        CallEvent::MediaReady {
            send_track: CallAudioTrack {
                id: 1,
                direction: CallTrackDirection::Send,
                ..
            },
            receive_track: CallAudioTrack {
                id: 2,
                direction: CallTrackDirection::Receive,
                ..
            }
        }
    ));
}

#[cfg(feature = "native-calls")]
#[tokio::test]
async fn real_opus_loopback_preserves_audio_across_service_pcm_sockets() {
    let (backend, mut attempts) = FakeBackend::new(true);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (_attempt, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let mut generator = PulseTone::new(
        format,
        880.0,
        0.2,
        Duration::from_millis(500),
        Duration::from_millis(250),
    )
    .unwrap();
    let pcm = generator.next_frame().unwrap();
    let packet = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        format,
        &pcm,
    )
    .unwrap();
    service.media_frame(&media.id, &packet).await.unwrap();
    let received = tokio::time::timeout(Duration::from_secs(1), media.frames.recv())
        .await
        .unwrap()
        .unwrap();
    let decoded = MediaFrame::decode(&received, format).unwrap();
    assert_eq!(decoded.header.track_id, 2);
    assert_eq!(decoded.header.sequence, 0);
    assert_eq!(decoded.samples().len(), 320);
    assert!(decoded.samples().any(|sample| sample.unsigned_abs() > 100));
    assert_eq!(backend.submitted_pcm.lock().unwrap().as_slice(), &[pcm]);
}

#[tokio::test]
async fn disconnected_or_replaced_authority_cannot_deliver_or_submit_more_audio() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    backend
        .authorities
        .lock()
        .unwrap()
        .insert("a".into(), auth("a", "generation2"));
    assert!(sink.audio(&[0; 320]).await.is_err());
    let packet = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 0,
            timestamp_micros: 0,
        },
        AudioFormat::new(16000, 1, 20).unwrap(),
        &[0; 320],
    )
    .unwrap();
    assert!(service.media_frame(&media.id, &packet).await.is_err());
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    service.disconnect(&owner.client_id).await;
    assert!(sink.audio(&[0; 320]).await.is_err());
}

#[tokio::test]
async fn unanswered_offer_expires_without_a_client_or_backend_submission() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    service
        .offer(
            auth("a", "generation1"),
            "incoming".into(),
            "synthetic-contact".into(),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let snap = service.current("a", "observer").await.unwrap();
    assert!(!snap.availability.busy);
    assert!(snap.current_offer.is_none());
    assert!(attempts.try_recv().is_err());
}

#[tokio::test]
async fn clear_audio_updates_tracks_and_drops_old_generation_without_revoking_owner() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (_attempt, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    while owner.events.try_recv().is_ok() {}
    service
        .command(
            &owner.client_id,
            CallCommand::ClearAudio {
                request_id: "clear".into(),
                call_id: call,
            },
        )
        .await
        .unwrap();
    let mut send_generation = None;
    while let Ok(event) = owner.events.try_recv() {
        if let CallEvent::TrackUpdated { track, .. } = event {
            if track.direction == CallTrackDirection::Send {
                send_generation = Some(track.generation);
            }
        }
    }
    assert_eq!(send_generation, Some(2));
    assert_eq!(backend.cleared.lock().unwrap().len(), 1);
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let old = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 1,
            sequence: 1,
            timestamp_micros: 20000,
        },
        format,
        &[0; 320],
    )
    .unwrap();
    service.media_frame(&media.id, &old).await.unwrap();
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
    assert!(service.validate_media(&media.id).await.is_ok());
    let current = frame::encode_pcm(
        MediaHeader {
            track_id: 1,
            generation: 2,
            sequence: 0,
            timestamp_micros: 0,
        },
        format,
        &[0; 320],
    )
    .unwrap();
    service.media_frame(&media.id, &current).await.unwrap();
}

#[tokio::test]
async fn duplicate_dial_before_and_after_terminal_never_dispatches_again() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    service
        .command(&owner.client_id, dial("same"))
        .await
        .unwrap();
    let original = result(&mut owner, "same").await;
    let attempt = next_attempt(&mut attempts).await;
    service
        .command(&owner.client_id, dial("same"))
        .await
        .unwrap();
    assert_eq!(
        result(&mut owner, "same").await.0,
        CallCommandStatus::Replayed
    );
    assert!(attempts.try_recv().is_err());
    attempt
        .reply
        .send(Ok(BackendOutcome::Ended))
        .unwrap_or_else(|_| panic!("active operation"));
    state(&mut owner, CallState::Ended).await;
    service
        .command(&owner.client_id, dial("same"))
        .await
        .unwrap();
    let replayed = result(&mut owner, "same").await;
    assert_eq!(replayed.0, CallCommandStatus::Replayed);
    assert_eq!(replayed.1, original.1);
    assert!(attempts.try_recv().is_err());
    assert!(
        !service
            .current("a", "owner")
            .await
            .unwrap()
            .availability
            .busy
    );
}

#[tokio::test]
async fn clear_audio_fences_a_frame_already_waiting_in_the_backend_queue() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (_sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
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
            CallCommand::ClearAudio {
                request_id: "clear".into(),
                call_id: call,
            },
        )
        .await
        .unwrap();
    release.send(()).unwrap();
    sending.await.unwrap().unwrap();
    assert!(backend.submitted_pcm.lock().unwrap().is_empty());
}

#[tokio::test]
async fn mute_interrupts_queued_audio_once_and_sends_silence_in_the_new_generation() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (_, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
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
}

#[tokio::test]
async fn held_initial_start_is_revoked_by_cancel_or_disconnect_and_cannot_revive() {
    for (cancel, terminal) in [(true, false), (false, false), (true, true)] {
        let (backend, mut attempts) = FakeBackend::new(false);
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
            let snapshot = service.current("a", "observer").await.unwrap();
            let call_state = service.lock().unwrap().call.as_ref().map(|call| call.state);
            if terminal && !snapshot.availability.busy {
                break;
            }
            if !terminal && call_state == Some(CallState::Uncertain) {
                break;
            }
        }
        let snapshot = service.current("a", "observer").await.unwrap();
        if terminal {
            assert!(!snapshot.availability.busy);
        } else {
            assert!(snapshot.availability.busy);
            assert_eq!(
                service.lock().unwrap().call.as_ref().unwrap().state,
                CallState::Uncertain
            );
        }
    }
}

#[tokio::test]
async fn owner_disconnect_terminates_once_after_grace_and_unknown_stop_keeps_slot() {
    let (backend, mut attempts) = FakeBackend::new(false);
    *backend.stop_outcome.lock().unwrap() = BackendOutcome::Uncertain;
    let service = Arc::new(CallService::new(backend.clone()));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    service.disconnect(&owner.client_id).await;
    assert!(service.validate_media(&media.id).await.is_err());
    assert!(sink.audio(&[0; 320]).await.is_err());
    assert!(
        service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    tokio::time::sleep(Duration::from_millis(5100)).await;
    assert!(
        service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
    assert_eq!(backend.stopped.lock().unwrap().len(), 1);
    // Repeated reads and a duplicate close cannot trigger another remote stop.
    service.disconnect(&owner.client_id).await;
    service.current("a", "owner").await.unwrap();
    assert_eq!(backend.stopped.lock().unwrap().len(), 1);
    sink.ended().await.unwrap();
    assert!(
        !service
            .current("b", "observer")
            .await
            .unwrap()
            .availability
            .busy
    );
}

#[tokio::test]
async fn slow_owner_control_reader_is_disconnected_and_queued_media_is_not_authorized() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
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
    // The receiver can still hold a frame after its sender was dropped. The API
    // must apply validate_media before writing this frame to the old socket.
    assert!(media.frames.recv().await.is_some());
    assert!(sink.audio(&[0; 320]).await.is_err());
}

#[tokio::test]
async fn maintenance_worker_does_not_keep_the_service_alive() {
    let (backend, _attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let connection = service.connect("a", "observer", false).await.unwrap();
    let weak = Arc::downgrade(&service);
    drop(connection);
    drop(service);
    tokio::time::timeout(Duration::from_secs(1), async {
        while weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("background maintenance must own only a weak reference");
}

#[tokio::test]
async fn clear_audio_does_not_reset_the_incoming_track_epoch_or_clock() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let (sink, call, token) = connected_call(&service, &mut attempts, &mut owner).await;
    let mut media = service
        .attach_media("a", "owner", &call, &owner.client_id, &token)
        .await
        .unwrap();
    sink.audio(&[100; 320]).await.unwrap();
    let before = media.frames.recv().await.unwrap();
    let format = AudioFormat::new(16000, 1, 20).unwrap();
    let before = MediaFrame::decode(&before, format).unwrap();
    assert_eq!((before.header.generation, before.header.sequence), (1, 0));
    service
        .command(
            &owner.client_id,
            CallCommand::ClearAudio {
                request_id: "clear".into(),
                call_id: call,
            },
        )
        .await
        .unwrap();
    sink.audio(&[200; 320]).await.unwrap();
    let after = media.frames.recv().await.unwrap();
    let after = MediaFrame::decode(&after, format).unwrap();
    assert_eq!(
        (
            after.header.generation,
            after.header.sequence,
            after.header.timestamp_micros
        ),
        (1, 1, 20000)
    );
}

#[tokio::test]
async fn hangup_fences_queued_audio_before_the_remote_stop_result() {
    let (backend, mut attempts) = FakeBackend::new(false);
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
    let (backend, mut attempts) = FakeBackend::new(false);
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
    let (backend, mut attempts) = FakeBackend::new(false);
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
        let (backend, mut attempts) = FakeBackend::new(false);
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
async fn admitted_answer_waiting_for_media_accepts_one_owner_hangup() {
    let (backend, mut attempts) = FakeBackend::new(false);
    let service = Arc::new(CallService::new(backend));
    let mut owner = ready(&service, "a", "owner").await;
    let mut other = ready(&service, "a", "other").await;
    service
        .offer(
            auth("a", "generation1"),
            "answering".into(),
            "peer".into(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    service
        .command(
            &owner.client_id,
            CallCommand::Answer {
                request_id: "answer".into(),
                call_id: "answering".into(),
                expected_revision: 1,
                media: CallMedia::default(),
            },
        )
        .await
        .unwrap();
    let answering = next_attempt(&mut attempts).await;
    answering
        .reply
        .send(Ok(BackendOutcome::Admitted))
        .unwrap_or_else(|_| panic!("answer admission receiver"));
    let revision = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            {
                let state = service.lock().unwrap();
                let call = state.call.as_ref().unwrap();
                if state
                    .ownership
                    .pending_start(&call.authority, &call.identity)
                    .is_none()
                {
                    break call.revision;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    service
        .command(
            &other.client_id,
            CallCommand::Hangup {
                request_id: "not-owner".into(),
                call_id: "answering".into(),
                expected_revision: revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut other, "not-owner").await.0,
        CallCommandStatus::Error
    );
    service
        .command(
            &owner.client_id,
            CallCommand::Hangup {
                request_id: "owner-hangup".into(),
                call_id: "answering".into(),
                expected_revision: revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        result(&mut owner, "owner-hangup").await.0,
        CallCommandStatus::Accepted
    );
    let ending = next_attempt(&mut attempts).await;
    assert_eq!(ending.request.ticket.operation(), Operation::Hangup);
    assert!(ending.sink.is_terminal_current());
    assert!(!answering.sink.is_current());
    service
        .command(
            &owner.client_id,
            CallCommand::Hangup {
                request_id: "owner-hangup".into(),
                call_id: "answering".into(),
                expected_revision: revision,
            },
        )
        .await
        .unwrap();
    assert!(attempts.try_recv().is_err());
    ending
        .reply
        .send(Ok(BackendOutcome::Ended))
        .unwrap_or_else(|_| panic!("hangup receiver"));
    state(&mut owner, CallState::Ended).await;
}
