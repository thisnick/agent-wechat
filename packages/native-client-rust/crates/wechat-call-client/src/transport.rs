//! Validated HTTP/1.1 WebSocket transport and Calls Protocol v3 control state machine.
//!
//! Shared between the `wx calls` CLI and the desktop Tauri backend. Calling
//! [`ControlClient::connect`] stays passive and never opens local audio/video
//! devices or advertises readiness.

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::header;
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::timeout;
use tokio_tungstenite::{
    tungstenite::{
        handshake::{client::generate_key, derive_accept_key},
        protocol::{Role, WebSocketConfig},
        Message,
    },
    WebSocketStream,
};
use wechat_call_contracts::call_types::{
    CallAudioFormat, CallCapabilities, CallClientHello, CallCommand, CallCommandStatus,
    CallConferenceInvitation, CallEvent, CallInvitationResult, CallOffer, CallParticipantControl,
    CallParticipantStatus, CallPendingOffer, CallState, CallTrack, CallVideoCapabilities, CallView,
    CallsAvailability, CALL_CONTROL_PROTOCOL_VERSION, CALL_CONTROL_SUPPORTED_VERSIONS,
    CALL_PROTOCOL_VERSION,
};

pub type Socket = WebSocketStream<reqwest::Upgraded>;

pub const CONTROL_SOCKET_LIMIT: usize = (64_usize * 1024).div_ceil(3) * 4 + 1024;
pub const MEDIA_SOCKET_LIMIT: usize = wechat_call_contracts::pcm_envelope::MAX_FRAME_BYTES + 4096;
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Structured transport/lifecycle errors so CLI and desktop callers can map
/// auth failures and local cancellations without parsing error strings.
#[derive(Debug)]
pub enum CallClientError {
    SocketRejected { status: u16 },
    Interrupted { message: String },
}

impl fmt::Display for CallClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SocketRejected { status } => write!(f, "call socket upgrade rejected ({status})"),
            Self::Interrupted { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for CallClientError {}

/// Connection parameters for one authenticated account on a hub.
#[derive(Clone)]
pub struct CallEndpoint {
    pub http: reqwest::Client,
    pub base: reqwest::Url,
    pub token: String,
    pub account: String,
}

impl CallEndpoint {
    pub fn account_path(&self, suffix: &str) -> String {
        format!("v1/accounts/{}/{}", encode_segment(&self.account), suffix)
    }
}

/// Percent-encode one URL path segment (matching `wx`'s account/call-ID encoding).
pub fn encode_segment(s: &str) -> String {
    reqwest::Url::parse("http://localhost/")
        .expect("static URL")
        .query_pairs_mut()
        .append_pair("", s)
        .finish()
        .query()
        .expect("query present")
        .trim_start_matches('=')
        .replace('+', "%20")
}

pub fn call_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("WX_CALL_DEBUG").as_deref() == Ok("1"))
}

pub fn debug_call(socket: &str, detail: fmt::Arguments<'_>) {
    if call_debug_enabled() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        eprintln!("[call-debug {}] {socket} {detail}", now.as_millis());
    }
}

/// Keep the caller's verified TLS, custom CA, proxy, and redirect policy. Only
/// WebSocket framing is added after a validated HTTP/1.1 upgrade.
pub async fn open_socket(endpoint: &CallEndpoint, suffix: &str) -> Result<Socket> {
    open_socket_with_limit(endpoint, suffix, CONTROL_SOCKET_LIMIT).await
}

pub async fn open_socket_with_limit(
    endpoint: &CallEndpoint,
    suffix: &str,
    limit: usize,
) -> Result<Socket> {
    let key = generate_key();
    let url = endpoint.base.join(&endpoint.account_path(suffix))?;
    let response = endpoint
        .http
        .get(url)
        .version(reqwest::Version::HTTP_11)
        .bearer_auth(&endpoint.token)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, &key)
        .timeout(STARTUP_TIMEOUT)
        .send()
        .await
        .context("call socket connection failed")?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err(CallClientError::SocketRejected {
            status: response.status().as_u16(),
        }
        .into());
    }
    verify_upgrade(response.headers(), &key)?;
    let stream = timeout(STARTUP_TIMEOUT, response.upgrade())
        .await
        .context("call socket upgrade timed out")??;
    let config = WebSocketConfig::default()
        .read_buffer_size(64 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(limit * 2)
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit));
    Ok(WebSocketStream::from_raw_socket(stream, Role::Client, Some(config)).await)
}

pub fn verify_upgrade(headers: &header::HeaderMap, key: &str) -> Result<()> {
    let one = |name: header::HeaderName| {
        let mut values = headers.get_all(name).iter();
        let value = values.next()?.to_str().ok()?;
        values.next().is_none().then_some(value)
    };
    if !one(header::UPGRADE).is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        || !one(header::CONNECTION).is_some_and(|v| {
            v.split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        })
        || one(header::SEC_WEBSOCKET_ACCEPT) != Some(derive_accept_key(key.as_bytes()).as_str())
        || headers.contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
        || headers.contains_key(header::SEC_WEBSOCKET_PROTOCOL)
    {
        bail!("invalid call socket upgrade response");
    }
    Ok(())
}

pub(crate) fn map_ws_send_error(error: tokio_tungstenite::tungstenite::Error) -> anyhow::Error {
    match error {
        tokio_tungstenite::tungstenite::Error::ConnectionClosed
        | tokio_tungstenite::tungstenite::Error::AlreadyClosed
        | tokio_tungstenite::tungstenite::Error::Protocol(
            tokio_tungstenite::tungstenite::error::ProtocolError::SendAfterClosing,
        ) => anyhow::anyhow!("call socket closed"),
        other => other.into(),
    }
}

pub async fn send_json(socket: &mut Socket, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string(value)?;
    if text.len() > 16 * 1024 {
        bail!("call command exceeds the message limit");
    }
    timeout(WRITE_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .context("call socket write timed out")?
        .map_err(map_ws_send_error)?;
    Ok(())
}

pub async fn receive(socket: &mut Socket, label: &str) -> Result<Message> {
    loop {
        let message = match socket.next().await {
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                debug_call(label, format_args!("read error: {error}"));
                return Err(error.into());
            }
            None => {
                debug_call(label, format_args!("read reached end of stream"));
                bail!("call socket closed");
            }
        };
        match message {
            Message::Ping(data) => {
                if let Err(error) = timeout(WRITE_TIMEOUT, socket.send(Message::Pong(data)))
                    .await
                    .context("call socket pong timed out")
                    .and_then(|result| result.map_err(map_ws_send_error))
                {
                    debug_call(label, format_args!("pong write failed: {error:#}"));
                    return Err(error);
                }
            }
            Message::Pong(_) => {}
            Message::Close(frame) => {
                if let Some(frame) = frame {
                    let code = u16::from(frame.code);
                    debug_call(
                        label,
                        format_args!(
                            "received close frame code={code} reason={:?}",
                            frame.reason.as_str()
                        ),
                    );
                } else {
                    debug_call(
                        label,
                        format_args!("received close frame without a code or reason"),
                    );
                }
                bail!("call socket closed");
            }
            message => return Ok(message),
        }
    }
}

fn is_known_call_event_type(kind: &str) -> bool {
    matches!(
        kind,
        "call.invitation_result"
            | "protocol.error"
            | "conference.invitations"
            | "hello"
            | "command.result"
            | "call.offered"
            | "call.pending"
            | "call.state"
            | "participant.updated"
            | "call.control"
            | "call.peer_status"
            | "calls.availability"
            | "track.added"
            | "track.updated"
            | "track.removed"
            | "media.ready"
            | "video.keyframe_requested"
            | "call.error"
    )
}

/// Parse a JSON text frame from a call WebSocket.
///
/// Returns `Ok(None)` when the server sent a well-formed JSON object whose
/// `type` tag is not recognized by this client, allowing clients to ignore
/// additive future server messages without disconnecting an active call.
pub fn parse_call_event_str(text: &str) -> Result<Option<CallEvent>> {
    if text.len() > 16 * 1024 {
        bail!("call event exceeds the message limit");
    }
    let value: serde_json::Value = serde_json::from_str(text).context("invalid call event")?;
    let Some(kind) = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control))
    else {
        bail!("invalid call event");
    };
    if kind == "protocol.error" {
        let CallEvent::ProtocolError {
            client_versions,
            server_versions,
            code,
        } = serde_json::from_value(value).context("invalid call protocol error")?
        else {
            bail!("invalid call protocol error");
        };
        bail!(
            "call protocol negotiation failed: client versions {client_versions:?}, daemon versions {server_versions:?} ({})",
            serde_json::to_value(code)?
        );
    }
    if kind == "hello" {
        let server_version = value["protocolVersion"].as_u64().unwrap_or(1);
        if !CALL_CONTROL_SUPPORTED_VERSIONS
            .iter()
            .any(|supported| u64::from(*supported) == server_version)
        {
            bail!(
                "unsupported call protocol version: client {CALL_CONTROL_SUPPORTED_VERSIONS:?}, daemon [{server_version}]"
            );
        }
    }
    if !is_known_call_event_type(kind) {
        debug_call(
            "event",
            format_args!("ignoring unknown event type {kind:?}"),
        );
        return Ok(None);
    }
    let event = serde_json::from_value(value).context("invalid call event")?;
    Ok(Some(event))
}

/// Receive one event strictly (used during initial `hello`/`media.ready`
/// handshakes so unknown event types fail closed prior to handshake completion).
pub async fn receive_strict_event(socket: &mut Socket, label: &str) -> Result<CallEvent> {
    let Message::Text(text) = receive(socket, label).await? else {
        bail!("unexpected binary control message");
    };
    let Some(event) = parse_call_event_str(&text)? else {
        bail!("unexpected unknown event before handshake");
    };
    if call_debug_enabled() {
        match event_json(&event) {
            Ok(line) => debug_call(label, format_args!("<- {line}")),
            Err(error) => debug_call(label, format_args!("could not format event: {error}")),
        }
    }
    Ok(event)
}

pub async fn receive_event(socket: &mut Socket, label: &str) -> Result<CallEvent> {
    loop {
        let Message::Text(text) = receive(socket, label).await? else {
            bail!("unexpected binary control message");
        };
        let Some(event) = parse_call_event_str(&text)? else {
            continue;
        };
        if call_debug_enabled() {
            match event_json(&event) {
                Ok(line) => debug_call(label, format_args!("<- {line}")),
                Err(error) => debug_call(label, format_args!("could not format event: {error}")),
            }
        }
        return Ok(event);
    }
}

/// Serialize a [`CallEvent`] to JSON with secret `ownerToken` fields stripped.
pub fn event_json(event: &CallEvent) -> Result<String> {
    let mut value = serde_json::to_value(event)?;
    // Owner tokens authorize media attachment and must stay out of logs and CLI output.
    if let Some(object) = value.as_object_mut() {
        object.remove("ownerToken");
    }
    Ok(serde_json::to_string(&value)?)
}

/// Latched cancellation signal that stays triggered across preparation,
/// attachment, and socket awaits.
#[derive(Clone)]
pub struct Cancellation(tokio::sync::watch::Receiver<bool>);

impl Cancellation {
    pub fn from_receiver(receiver: tokio::sync::watch::Receiver<bool>) -> Self {
        Self(receiver)
    }

    pub fn channel() -> (tokio::sync::watch::Sender<bool>, Self) {
        let (tx, rx) = tokio::sync::watch::channel(false);
        (tx, Self(rx))
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    pub async fn wait(&self) {
        let mut receiver = self.0.clone();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

pub struct OwnerResult {
    pub call_id: Option<String>,
    pub owner_token: Option<String>,
}

/// Calls Protocol v3 control WebSocket client and authoritative state mirror.
pub struct ControlClient {
    pub socket: Socket,
    pub cancellation: Option<Cancellation>,
    pub client_id: String,
    pub capabilities: CallCapabilities,
    pub conference_invitations: Vec<CallConferenceInvitation>,
    pub invitation_results: VecDeque<(String, Vec<CallInvitationResult>)>,
    pub availability: CallsAvailability,
    pub offer: Option<CallOffer>,
    pub pending_offer: Option<CallPendingOffer>,
    pub tracks: BTreeMap<(String, u32), CallTrack>,
    pub call: Option<CallView>,
    pub peer_status: Option<CallParticipantStatus>,
    peer_status_call_id: Option<String>,
    pub format: CallAudioFormat,
    pub video_capabilities: Option<CallVideoCapabilities>,
}

impl ControlClient {
    pub async fn connect(endpoint: &CallEndpoint) -> Result<(Self, CallEvent)> {
        let mut socket = open_socket(endpoint, "calls/socket").await?;
        send_json(
            &mut socket,
            &CallClientHello::Hello {
                supported_protocol_versions: vec![
                    CALL_CONTROL_PROTOCOL_VERSION,
                    CALL_PROTOCOL_VERSION,
                ],
            },
        )
        .await?;
        let event = timeout(
            STARTUP_TIMEOUT,
            receive_strict_event(&mut socket, "control"),
        )
        .await
        .context("call socket hello timed out")??;
        let CallEvent::Hello {
            protocol_version,
            conference_invitations,
            client_id,
            capabilities,
            availability,
            current_offer,
            current_pending_offer,
            current_call,
            audio_format,
            video_capabilities,
        } = &event
        else {
            bail!("unsupported call socket hello");
        };
        if !CALL_CONTROL_SUPPORTED_VERSIONS.contains(protocol_version) {
            bail!("unsupported call socket hello");
        }
        Ok((
            Self {
                socket,
                cancellation: None,
                client_id: client_id.clone(),
                capabilities: *capabilities,
                conference_invitations: conference_invitations.clone(),
                invitation_results: VecDeque::new(),
                availability: *availability,
                offer: current_offer.clone(),
                pending_offer: current_pending_offer.clone(),
                tracks: BTreeMap::new(),
                call: current_call.clone(),
                peer_status: None,
                peer_status_call_id: None,
                format: *audio_format,
                video_capabilities: video_capabilities.clone(),
            },
            event,
        ))
    }

    pub async fn next_event(&mut self) -> Result<CallEvent> {
        receive_event(&mut self.socket, "control").await
    }

    pub fn update(&mut self, event: &CallEvent) {
        match event {
            CallEvent::InvitationResult {
                call_id, results, ..
            } => {
                if self.invitation_results.len() < 128 {
                    self.invitation_results
                        .push_back((call_id.clone(), results.clone()));
                }
            }
            CallEvent::ConferenceInvitations { invitations } => {
                self.conference_invitations = invitations.clone();
            }
            CallEvent::ParticipantUpdated {
                call_id,
                revision,
                participant,
            } => {
                if let Some(call) = self
                    .call
                    .as_mut()
                    .filter(|call| call.call_id == *call_id && *revision >= call.revision)
                {
                    call.revision = *revision;
                    call.participants
                        .retain(|current| current.participant_id != participant.participant_id);
                    call.participants.push(participant.clone());
                }
            }
            CallEvent::TrackRemoved {
                call_id,
                track_id,
                generation,
                ..
            } => {
                let key = (call_id.clone(), *track_id);
                if self
                    .tracks
                    .get(&key)
                    .is_some_and(|track| track.generation() <= *generation)
                {
                    self.tracks.remove(&key);
                }
            }
            CallEvent::Offered { offer } => self.offer = Some(offer.clone()),
            CallEvent::Pending { offer } => self.pending_offer = offer.clone(),
            CallEvent::TrackAdded { call_id, track }
            | CallEvent::TrackUpdated { call_id, track } => {
                let key = (call_id.clone(), track.id());
                if self
                    .tracks
                    .get(&key)
                    .is_none_or(|current| track.generation() >= current.generation())
                {
                    self.tracks.insert(key, track.clone());
                }
            }
            CallEvent::State { call } => {
                if self
                    .peer_status_call_id
                    .as_deref()
                    .is_some_and(|id| id != call.call_id)
                {
                    self.peer_status = None;
                    self.peer_status_call_id = None;
                }
                if self
                    .call
                    .as_ref()
                    .is_none_or(|old| old.call_id != call.call_id || old.revision <= call.revision)
                {
                    self.call = Some(call.clone());
                }
                if call.state == CallState::Ended {
                    self.tracks.retain(|(id, _), _| id != &call.call_id);
                }
            }
            CallEvent::PeerStatus {
                call_id, status, ..
            } => {
                let matches_or_new_call = self
                    .call
                    .as_ref()
                    .is_none_or(|call| call.call_id == *call_id || call.state == CallState::Ended);
                if matches_or_new_call {
                    self.peer_status = Some(*status);
                    self.peer_status_call_id = Some(call_id.clone());
                }
            }
            CallEvent::Control {
                call_id,
                participant_id,
                control,
            } => {
                if let Some(call) = self.call.as_mut().filter(|call| call.call_id == *call_id) {
                    call.controls.retain(|item| {
                        item.participant_id != *participant_id
                            || std::mem::discriminant(&item.control)
                                != std::mem::discriminant(control)
                    });
                    call.controls.push(CallParticipantControl {
                        participant_id: participant_id.clone(),
                        control: control.clone(),
                    });
                }
            }
            CallEvent::Availability { availability } => {
                self.availability = *availability;
                // Availability begins each new snapshot batch; a still-current
                // offer is supplied immediately after it. Never retain an old offer.
                self.offer = None;
                if !availability.busy {
                    self.tracks.clear();
                    self.peer_status = None;
                    self.peer_status_call_id = None;
                }
            }
            _ => {}
        }
    }

    pub async fn command(&mut self, command: CallCommand) -> Result<OwnerResult> {
        let cancellation = self.cancellation.clone();
        if cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            return Err(CallClientError::Interrupted {
                message: "call command cancelled before submission".into(),
            }
            .into());
        }
        let id = command.request_id().to_owned();
        if let Err(error) = send_json(&mut self.socket, &command).await {
            debug_call("control", format_args!("command write failed: {error:#}"));
            return Err(error);
        }
        let response = async {
            loop {
                let event = self.next_event().await?;
                self.update(&event);
                match event {
                    CallEvent::CommandResult {
                        request_id,
                        status,
                        call_id,
                        owner_token,
                        code,
                    } if request_id == id => {
                        if status == CallCommandStatus::Error {
                            bail!("call command rejected: {}", serde_json::to_value(code)?);
                        }
                        return Ok(OwnerResult {
                            call_id,
                            owner_token,
                        });
                    }
                    CallEvent::Error { code } => {
                        bail!("call error: {}", serde_json::to_value(code)?)
                    }
                    _ => {}
                }
            }
        };
        let interrupted = async {
            match cancellation {
                Some(cancellation) => cancellation.wait().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = interrupted => Err(CallClientError::Interrupted {
                message: "call command interrupted; outcome unknown; check calls status before retrying".into(),
            }.into()),
            response = timeout(Duration::from_secs(30), response) => {
                response.context("call command outcome unknown; check calls status before retrying")?
            }
        }
    }

    pub async fn close(&mut self) {
        debug_call("control", format_args!("local close requested"));
        let result = timeout(WRITE_TIMEOUT, self.socket.close(None)).await;
        debug_call("control", format_args!("local close result: {result:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_call_contracts::call_types::{
        CallControl, CallDirection, CallKind, CallState, CallTrackDirection, CallType,
    };

    #[tokio::test]
    async fn control_update_preserves_sibling_controls_and_prunes_tracks_on_end() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await;
            let hello = CallEvent::Hello {
                protocol_version: CALL_PROTOCOL_VERSION,
                client_id: "test-client".into(),
                capabilities: CallCapabilities::default(),
                availability: CallsAvailability {
                    busy: true,
                    owned_by_this_client: true,
                },
                current_offer: None,
                current_pending_offer: None,
                current_call: Some(CallView {
                    call_id: "call-1".into(),
                    revision: 1,
                    kind: CallKind::Direct,
                    call_type: CallType::Video,
                    direction: CallDirection::Outgoing,
                    state: CallState::Connected,
                    group_id: None,
                    participants: vec![],
                    owned_by_this_client: true,
                    controls: vec![],
                }),
                audio_format: CallAudioFormat::default(),
                video_capabilities: None,
                conference_invitations: vec![],
            };
            socket
                .send(Message::Text(serde_json::to_string(&hello).unwrap().into()))
                .await
                .unwrap();
            let _ = socket.next().await;
        });

        let endpoint = CallEndpoint {
            http: reqwest::Client::new(),
            base: reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            token: "synthetic-token".into(),
            account: "acc".into(),
        };
        let (mut control, _) = ControlClient::connect(&endpoint).await.unwrap();

        control.update(&CallEvent::Control {
            call_id: "call-1".into(),
            participant_id: "peer-1".into(),
            control: CallControl::Camera { enabled: false },
        });
        control.update(&CallEvent::Control {
            call_id: "call-1".into(),
            participant_id: "peer-1".into(),
            control: CallControl::Microphone { muted: true },
        });
        let controls = &control.call.as_ref().unwrap().controls;
        assert_eq!(controls.len(), 2);
        assert!(controls
            .iter()
            .any(|c| matches!(c.control, CallControl::Camera { enabled: false })));
        assert!(controls
            .iter()
            .any(|c| matches!(c.control, CallControl::Microphone { muted: true })));

        // Updating camera replaces only Camera, keeping Microphone intact.
        control.update(&CallEvent::Control {
            call_id: "call-1".into(),
            participant_id: "peer-1".into(),
            control: CallControl::Camera { enabled: true },
        });
        let controls = &control.call.as_ref().unwrap().controls;
        assert_eq!(controls.len(), 2);
        assert!(controls
            .iter()
            .any(|c| matches!(c.control, CallControl::Camera { enabled: true })));
        assert!(controls
            .iter()
            .any(|c| matches!(c.control, CallControl::Microphone { muted: true })));

        // Track pruning when call ends.
        control.update(&CallEvent::TrackAdded {
            call_id: "call-1".into(),
            track: CallTrack::Audio {
                id: 1,
                generation: 1,
                direction: CallTrackDirection::Send,
                format: CallAudioFormat::default(),
                participant_id: None,
            },
        });
        assert_eq!(control.tracks.len(), 1);
        control.update(&CallEvent::State {
            call: CallView {
                call_id: "call-1".into(),
                revision: 2,
                kind: CallKind::Direct,
                call_type: CallType::Video,
                direction: CallDirection::Outgoing,
                state: CallState::Ended,
                group_id: None,
                participants: vec![],
                owned_by_this_client: false,
                controls: vec![],
            },
        });
        assert!(control.tracks.is_empty());
        control.close().await;
        server.await.unwrap();
    }

    #[test]
    fn parse_call_event_ignores_unknown_server_types_and_parses_peer_status() {
        let unknown = parse_call_event_str(
            r#"{"type":"call.future_telemetry","callId":"call-1","metric":42}"#,
        )
        .unwrap();
        assert!(unknown.is_none());

        let parsed = parse_call_event_str(
            r#"{"type":"call.peer_status","callId":"call-1","participantId":"bob","status":{"code":3,"kind":"acknowledged"}}"#,
        )
        .unwrap()
        .expect("peer_status event");
        match parsed {
            CallEvent::PeerStatus {
                call_id,
                participant_id,
                status,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(participant_id, "bob");
                assert_eq!(status.code, 3);
                assert_eq!(
                    status.kind,
                    wechat_call_contracts::call_types::CallParticipantStatusKind::Acknowledged
                );
            }
            other => panic!("unexpected event: {}", event_json(&other).unwrap()),
        }

        // Known event types with malformed payloads must still fail closed.
        assert!(parse_call_event_str(r#"{"type":"call.peer_status","callId":"call-1"}"#).is_err());
        assert!(parse_call_event_str(r#"{"notType":"x"}"#).is_err());
    }
}
