use super::*;
use clap::Parser;
use tokio::sync::oneshot;
use wechat_core::call_types::{
    CallAudioFormat, CallCapabilities, CallCommandStatus, CallsAvailability,
};

#[tokio::test]
async fn actual_parent_help_needs_no_config_token_stdin_or_network() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("not-created");
    let cli = super::super::Cli::try_parse_from([
        "wx".to_string(),
        "--config".into(),
        absent.join("client.toml").display().to_string(),
        "--server-config".into(),
        absent.join("server.toml").display().to_string(),
        "--data-dir".into(),
        absent.display().to_string(),
        "--token-stdin".into(),
        "calls".into(),
    ])
    .unwrap();
    timeout(Duration::from_secs(2), super::super::run(cli))
        .await
        .unwrap()
        .unwrap();
    assert!(!absent.exists());
}

fn upgrade_headers(key: &str) -> header::HeaderMap {
    let mut headers = header::HeaderMap::new();
    headers.insert(header::UPGRADE, "websocket".parse().unwrap());
    headers.insert(header::CONNECTION, "keep-alive, Upgrade".parse().unwrap());
    headers.insert(
        header::SEC_WEBSOCKET_ACCEPT,
        derive_accept_key(key.as_bytes()).parse().unwrap(),
    );
    headers
}

#[tokio::test]
async fn websocket_upgrade_requires_accept_key_and_rejects_unrequested_extensions() {
    let key = generate_key();
    let mut headers = upgrade_headers(&key);
    assert!(verify_upgrade(&headers, &key).is_ok());
    assert!(verify_upgrade(&headers, "different-key").is_err());
    headers.insert(
        header::SEC_WEBSOCKET_EXTENSIONS,
        "permessage-deflate".parse().unwrap(),
    );
    assert!(verify_upgrade(&headers, &key).is_err());
    headers.remove(header::SEC_WEBSOCKET_EXTENSIONS);
    headers.append(header::SEC_WEBSOCKET_ACCEPT, "duplicate".parse().unwrap());
    assert!(verify_upgrade(&headers, &key).is_err());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/v1/accounts/test-account/calls/socket",
        axum::routing::get(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let error = match open_socket(&remote(address), "calls/socket").await {
        Ok(_) => panic!("rejected socket unexpectedly opened"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("401"));
    assert!(!format!("{error:?}").contains("synthetic-bearer"));
    server.abort();
}

pub(super) fn remote(address: std::net::SocketAddr) -> Remote {
    Remote {
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        base: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
        token: "synthetic-bearer".into(),
        account: Some("test-account".into()),
    }
}

pub(super) fn hello() -> CallEvent {
    CallEvent::Hello {
        protocol_version: 1,
        client_id: "fixture-client".into(),
        capabilities: CallCapabilities::default(),
        availability: CallsAvailability {
            busy: false,
            owned_by_this_client: false,
        },
        current_offer: None,
        current_pending_offer: None,
        current_call: None,
        audio_format: CallAudioFormat::default(),
    }
}

#[tokio::test]
async fn json_monitor_uses_header_auth_pongs_and_never_registers_or_claims() {
    let text = event_json(&CallEvent::CommandResult {
        request_id: "fixture-request".into(),
        status: CallCommandStatus::Accepted,
        call_id: Some("fixture-call".into()),
        owner_token: Some("fixture-owner-secret".into()),
        code: None,
    })
    .unwrap();
    assert!(!text.contains("fixture-owner-secret"));
    assert!(!text.contains("ownerToken"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (done, stop) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.uri().path(),
                    "/v1/accounts/test-account/calls/socket"
                );
                assert!(request.uri().query().is_none());
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer synthetic-bearer"
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&hello()).unwrap().into(),
            ))
            .await
            .unwrap();
        // A passive monitor sends no readiness, claim or media request.
        assert!(timeout(Duration::from_millis(50), socket.next())
            .await
            .is_err());
        socket
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .unwrap();
        let pong = timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(pong,Message::Pong(bytes) if bytes.as_ref()==[1,2,3]));
        done.send(()).unwrap();
        let close = timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(close, Message::Close(_)));
    });
    observe_until(&remote(address), async {
        stop.await.unwrap();
    })
    .await
    .unwrap();
    server.await.unwrap();
}

#[cfg(feature = "call-audio")]
#[tokio::test]
async fn winning_claim_attaches_generated_pcm_without_opening_devices() {
    use wechat_calls::{audio::AudioFormat, frame::MediaFrame};
    use wechat_core::call_types::{CallAudioTrack, CallMedia, CallTrackDirection};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut control = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut initial = hello();
        if let CallEvent::Hello { capabilities, .. } = &mut initial {
            capabilities.direct_audio = true;
        }
        control
            .send(Message::Text(
                serde_json::to_string(&initial).unwrap().into(),
            ))
            .await
            .unwrap();
        let Message::Text(text) = control.next().await.unwrap().unwrap() else {
            panic!("expected dial");
        };
        let command: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(command["type"], "call.dial");
        assert_eq!(command["target"]["contactId"], "synthetic-contact");
        let result = CallEvent::CommandResult {
            request_id: command["requestId"].as_str().unwrap().into(),
            status: CallCommandStatus::Accepted,
            call_id: Some("owned-call".into()),
            owner_token: Some("synthetic-owner-proof".into()),
            code: None,
        };
        control
            .send(Message::Text(
                serde_json::to_string(&result).unwrap().into(),
            ))
            .await
            .unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let mut media = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.uri().path(),
                    "/v1/accounts/test-account/calls/owned-call/media"
                );
                assert!(request.uri().query().is_none());
                assert_eq!(
                    request.headers()["authorization"],
                    "Bearer synthetic-bearer"
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let Message::Text(text) = media.next().await.unwrap().unwrap() else {
            panic!("expected media attachment");
        };
        let attach: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(attach["type"], "media.attach");
        assert_eq!(attach["clientId"], "fixture-client");
        assert_eq!(attach["ownerToken"], "synthetic-owner-proof");
        let ready = CallEvent::MediaReady {
            send_track: CallAudioTrack {
                id: 1,
                generation: 3,
                direction: CallTrackDirection::Send,
                format: CallAudioFormat::default(),
            },
            receive_track: CallAudioTrack {
                id: 2,
                generation: 1,
                direction: CallTrackDirection::Receive,
                format: CallAudioFormat::default(),
            },
        };
        media
            .send(Message::Text(serde_json::to_string(&ready).unwrap().into()))
            .await
            .unwrap();
        let Message::Binary(bytes) = media.next().await.unwrap().unwrap() else {
            panic!("expected generated PCM");
        };
        let frame = MediaFrame::decode(&bytes, AudioFormat::new(16000, 1, 20).unwrap()).unwrap();
        assert_eq!(frame.header.track_id, 1);
        assert_eq!(frame.header.generation, 3);
        assert_eq!(frame.samples().collect::<Vec<_>>(), vec![123; 320]);
        let Message::Text(text) = control.next().await.unwrap().unwrap() else {
            panic!("expected mute");
        };
        let mute: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(mute["type"], "call.mute");
        let update = CallEvent::TrackUpdated {
            call_id: "owned-call".into(),
            track: CallAudioTrack {
                id: 1,
                generation: 4,
                direction: CallTrackDirection::Send,
                format: CallAudioFormat::default(),
            },
        };
        control
            .send(Message::Text(
                serde_json::to_string(&update).unwrap().into(),
            ))
            .await
            .unwrap();
        let result = CallEvent::CommandResult {
            request_id: mute["requestId"].as_str().unwrap().into(),
            status: CallCommandStatus::Accepted,
            call_id: Some("owned-call".into()),
            owner_token: None,
            code: None,
        };
        control
            .send(Message::Text(
                serde_json::to_string(&result).unwrap().into(),
            ))
            .await
            .unwrap();
        let Message::Binary(bytes) = media.next().await.unwrap().unwrap() else {
            panic!("expected generation-four PCM");
        };
        let frame = MediaFrame::decode(&bytes, AudioFormat::new(16000, 1, 20).unwrap()).unwrap();
        assert_eq!(frame.header.generation, 4);
        assert_eq!(frame.header.sequence, 0);
        use wechat_core::call_types::{CallDirection, CallKind, CallState, CallView};
        let view = CallView {
            call_id: "owned-call".into(),
            revision: 4,
            kind: CallKind::Direct,
            direction: CallDirection::Outgoing,
            state: CallState::Connected,
            owned_by_this_client: true,
        };
        control
            .send(Message::Text(
                serde_json::to_string(&CallEvent::State { call: view.clone() })
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        let message = tokio::select! {
            message = control.next() => message.unwrap().unwrap(),
            _ = media.next() => panic!("media detached before explicit end"),
        };
        let Message::Text(message) = message else {
            panic!("expected hangup");
        };
        let command: serde_json::Value = serde_json::from_str(&message).unwrap();
        assert_eq!(command["type"], "call.hangup");
        control
            .send(Message::Text(
                serde_json::to_string(&CallEvent::CommandResult {
                    request_id: command["requestId"].as_str().unwrap().into(),
                    status: CallCommandStatus::Accepted,
                    call_id: Some("owned-call".into()),
                    owner_token: None,
                    code: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        assert!(timeout(Duration::from_millis(30), media.next())
            .await
            .is_err());
        control
            .send(Message::Text(
                serde_json::to_string(&CallEvent::State {
                    call: CallView {
                        state: CallState::Ended,
                        ..view
                    },
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        assert!(!matches!(
            timeout(Duration::from_secs(1), media.next()).await.unwrap(),
            Some(Ok(Message::Text(_) | Message::Binary(_)))
        ));
        assert!(matches!(
            control.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let remote = remote(address);
    let (mut control, _) = Control::connect(&remote).await.unwrap();
    let claim = control
        .command(CallCommand::Dial {
            request_id: "dial-one".into(),
            target: wechat_core::call_types::CallTarget::Direct {
                contact_id: "synthetic-contact".into(),
            },
            media: CallMedia::default(),
        })
        .await
        .unwrap();
    let mut media = super::audio::Media::attach(
        &remote,
        &control,
        claim.call_id.as_deref().unwrap(),
        claim.owner_token.as_deref().unwrap(),
    )
    .await
    .unwrap();
    media
        .send(&[123; 320], AudioFormat::new(16000, 1, 20).unwrap())
        .await
        .unwrap();
    control
        .command(CallCommand::Mute {
            request_id: "mute-consumed-track".into(),
            call_id: "owned-call".into(),
            muted: true,
        })
        .await
        .unwrap();
    assert_eq!(
        media.synchronize_tracks(&control, "owned-call").unwrap(),
        (true, false)
    );
    assert_eq!(
        media.synchronize_tracks(&control, "owned-call").unwrap(),
        (false, false)
    );
    media
        .send(&[0; 320], AudioFormat::new(16000, 1, 20).unwrap())
        .await
        .unwrap();
    let mut owned = super::audio::OwnedCall {
        call_id: "owned-call".into(),
        owner_token: "synthetic-owner-proof".into(),
        media: Some(media),
    };
    let (_cancel, signal) = tokio::sync::watch::channel(true);
    control.cancellation = Some(Cancellation(signal));
    owned.finish(&mut control, false).await.unwrap();
    assert!(owned.media.is_none());
    owned.finish(&mut control, false).await.unwrap();
    control.close().await;
    server.await.unwrap();
}

#[cfg(feature = "call-audio")]
#[tokio::test]
async fn cancellation_stays_latched_across_preparation_and_prevents_a_claim() {
    let (cancel, signal) = tokio::sync::watch::channel(false);
    let cancellation = Cancellation(signal);
    // Simulate a signal during an await that has no active signal waiter.
    tokio::time::sleep(Duration::from_millis(1)).await;
    cancel.send(true).unwrap();
    timeout(Duration::from_millis(20), cancellation.wait())
        .await
        .unwrap();
    assert!(cancellation.is_cancelled());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (checked, wait_checked) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&hello()).unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(timeout(Duration::from_millis(50), socket.next())
            .await
            .is_err());
        checked.send(()).unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let (mut control, _) = Control::connect(&remote(address)).await.unwrap();
    control.cancellation = Some(cancellation);
    let result = control
        .command(CallCommand::Answer {
            request_id: "cancelled-claim".into(),
            call_id: "pinned-offer".into(),
            expected_revision: 1,
            media: wechat_core::call_types::CallMedia::default(),
        })
        .await;
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("cancelled claim submitted"),
    };
    assert_eq!(super::super::outcome::classify(&error).0, 130);
    wait_checked.await.unwrap();
    control.close().await;
    server.await.unwrap();
}

#[cfg(feature = "call-audio")]
#[tokio::test]
async fn interactive_monitor_renews_all_readiness_for_a_second_offer() {
    use wechat_core::call_types::{CallKind, CallMedia, CallOffer, CallReadyScope};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let first = CallOffer {
            call_id: "offer-a".into(),
            revision: 3,
            kind: CallKind::Direct,
            caller_id: "alice".into(),
            offered_media: CallMedia::default(),
        };
        let mut initial = hello();
        if let CallEvent::Hello { current_offer, .. } = &mut initial {
            *current_offer = Some(first);
        }
        socket
            .send(Message::Text(
                serde_json::to_string(&initial).unwrap().into(),
            ))
            .await
            .unwrap();
        for expected in ["offer", "all"] {
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                panic!("expected readiness");
            };
            let command: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(command["type"], "call.ready");
            assert_eq!(command["scope"]["kind"], expected);
            let result = CallEvent::CommandResult {
                request_id: command["requestId"].as_str().unwrap().into(),
                status: CallCommandStatus::Accepted,
                call_id: None,
                owner_token: None,
                code: None,
            };
            socket
                .send(Message::Text(
                    serde_json::to_string(&result).unwrap().into(),
                ))
                .await
                .unwrap();
        }
        let second = CallEvent::Offered {
            offer: CallOffer {
                call_id: "offer-b".into(),
                revision: 3,
                kind: CallKind::Direct,
                caller_id: "bob".into(),
                offered_media: CallMedia::default(),
            },
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&second).unwrap().into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let (mut control, _) = Control::connect(&remote(address)).await.unwrap();
    assert_eq!(control.offer.as_ref().unwrap().call_id, "offer-a");
    control
        .command(CallCommand::Ready {
            request_id: "prepare-a".into(),
            ready: true,
            format: CallAudioFormat::default(),
            scope: Some(CallReadyScope::Offer {
                call_id: "offer-a".into(),
                expected_revision: 3,
            }),
        })
        .await
        .unwrap();
    // Successful local participation returns to the monitor's general policy.
    super::audio::restore_monitor_readiness(&mut control)
        .await
        .unwrap();
    let event = receive_event(&mut control.socket).await.unwrap();
    control.update(&event);
    assert_eq!(control.offer.as_ref().unwrap().call_id, "offer-b");
    control.close().await;
    server.await.unwrap();
}

#[cfg(feature = "call-audio")]
#[tokio::test]
async fn quit_after_claim_waits_for_its_exact_state_before_cancelling_once() {
    use wechat_core::call_types::{
        CallDirection, CallKind, CallMedia, CallState, CallTarget, CallView,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&hello()).unwrap().into(),
            ))
            .await
            .unwrap();
        let Message::Text(dial) = socket.next().await.unwrap().unwrap() else {
            panic!("expected dial");
        };
        let dial: serde_json::Value = serde_json::from_str(&dial).unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&CallEvent::CommandResult {
                    request_id: dial["requestId"].as_str().unwrap().into(),
                    status: CallCommandStatus::Accepted,
                    call_id: Some("exact-call".into()),
                    owner_token: Some("synthetic-owner".into()),
                    code: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        let call = CallView {
            call_id: "exact-call".into(),
            revision: 9,
            kind: CallKind::Direct,
            direction: CallDirection::Outgoing,
            state: CallState::Dialing,
            owned_by_this_client: true,
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&CallEvent::State { call: call.clone() })
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
        let Message::Text(cancel) = socket.next().await.unwrap().unwrap() else {
            panic!("expected cancel");
        };
        let cancel: serde_json::Value = serde_json::from_str(&cancel).unwrap();
        assert_eq!(cancel["type"], "call.cancel");
        assert_eq!(cancel["callId"], "exact-call");
        assert_eq!(cancel["expectedRevision"], 9);
        socket
            .send(Message::Text(
                serde_json::to_string(&CallEvent::CommandResult {
                    request_id: cancel["requestId"].as_str().unwrap().into(),
                    status: CallCommandStatus::Accepted,
                    call_id: Some("exact-call".into()),
                    owner_token: None,
                    code: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                serde_json::to_string(&CallEvent::State {
                    call: CallView {
                        state: CallState::Ended,
                        ..call
                    },
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let (mut control, _) = Control::connect(&remote(address)).await.unwrap();
    control
        .command(CallCommand::Dial {
            request_id: "dial".into(),
            target: CallTarget::Direct {
                contact_id: "synthetic-peer".into(),
            },
            media: CallMedia::default(),
        })
        .await
        .unwrap();
    assert!(control.call.is_none());
    let (_cancel, signal) = tokio::sync::watch::channel(true);
    control.cancellation = Some(Cancellation(signal));
    super::audio::terminate_owned(&mut control, "exact-call")
        .await
        .unwrap();
    super::audio::terminate_owned(&mut control, "exact-call")
        .await
        .unwrap();
    control.close().await;
    server.await.unwrap();
}
