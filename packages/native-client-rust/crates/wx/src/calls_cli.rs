//! Call control on the CLI's machine. Merely observing never opens audio devices.
use super::{failure, Remote};
use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use futures_util::{SinkExt, StreamExt};
use reqwest::header;
use std::{
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
use wechat_core::call_types::CallEvent;

use wechat_core::call_types::{
    CallAudioFormat, CallCapabilities, CallCommand, CallOffer, CallPendingOffer, CallView,
    CallsAvailability,
};

#[path = "calls_cli_audio.rs"]
mod audio;

#[path = "calls_cli_levels.rs"]
mod levels;

type Socket = WebSocketStream<reqwest::Upgraded>;
const SOCKET_LIMIT: usize = 64 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

fn call_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("WX_CALL_DEBUG").as_deref() == Ok("1"))
}

fn debug_call(socket: &str, detail: fmt::Arguments<'_>) {
    if call_debug_enabled() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        eprintln!("[call-debug {}] {socket} {detail}", now.as_millis());
    }
}

#[derive(Subcommand)]
pub(crate) enum CallsCommand {
    /// List local microphone/speaker devices without connecting to a daemon.
    Devices,
    /// Read the daemon's current call state and exit.
    Status,
    /// Wait for incoming calls. With --json, observe only without audio devices.
    Monitor {
        #[command(flatten)]
        audio: AudioOptions,
    },
    /// Call one contact using this computer's microphone and speakers.
    Dial {
        contact: String,
        #[command(flatten)]
        audio: AudioOptions,
    },
    /// Answer the current offer using this computer's microphone and speakers.
    Answer {
        call_id: Option<String>,
        #[command(flatten)]
        audio: AudioOptions,
    },
}

#[derive(Args, Clone, Default)]
pub(crate) struct AudioOptions {
    /// Local backend source name or endpoint ID; defaults to the OS microphone.
    #[arg(long)]
    input_device: Option<String>,
    /// Local backend sink name or endpoint ID; defaults to the OS speakers.
    #[arg(long)]
    output_device: Option<String>,
}

pub(crate) async fn run(remote: &Remote, command: CallsCommand, json: bool) -> Result<()> {
    debug_call("session", format_args!("debug enabled"));
    match command {
        CallsCommand::Devices => devices(json).await,
        CallsCommand::Status => {
            let value = remote.get(&remote.path("calls/current")?).await?;
            super::Output::new(json).print(&value)
        }
        CallsCommand::Monitor { .. } if json => observe(remote).await,
        command => audio::run(remote, command, json).await,
    }
}

pub(crate) async fn devices(json: bool) -> Result<()> {
    {
        let devices = tokio::task::spawn_blocking(wechat_audio::AudioSession::devices).await??;
        let values: Vec<_> = devices
            .iter()
            .map(|device| {
                serde_json::json!({
                    "kind": if device.input { "input" } else { "output" }, "id": device.id,
                    "name": device.name, "default": device.is_default,
                })
            })
            .collect();
        if json {
            return super::Output::new(json).print(&serde_json::json!({"devices": values}));
        }
        println!("Local audio devices (no microphone capture or playback started):");
        for device in devices {
            let clean = |text: &str| {
                text.chars()
                    .filter(|c| !c.is_control())
                    .take(256)
                    .collect::<String>()
            };
            println!(
                "{}{}: {}\n  {}",
                if device.input {
                    "Microphone"
                } else {
                    "Speaker"
                },
                if device.is_default { " (default)" } else { "" },
                clean(&device.name),
                device
                    .id
                    .as_deref()
                    .map(clean)
                    .unwrap_or_else(|| "Use the operating system's default device".into())
            );
        }
        Ok(())
    }
}

/// Keep the existing HTTP client's verified TLS, custom CA, proxy and redirect
/// policy. Only WebSocket framing is added after a validated HTTP/1.1 upgrade.
async fn open_socket(remote: &Remote, suffix: &str) -> Result<Socket> {
    let key = generate_key();
    let response = remote
        .http
        .get(remote.base.join(&remote.path(suffix)?)?)
        .version(reqwest::Version::HTTP_11)
        .bearer_auth(&remote.token)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, &key)
        .timeout(STARTUP_TIMEOUT)
        .send()
        .await
        .context("call socket connection failed")?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        let status = response.status();
        return Err(failure(
            if matches!(status.as_u16(), 401 | 403) {
                4
            } else {
                3
            },
            "call_socket_rejected",
            &format!("call socket upgrade rejected ({status})"),
        ));
    }
    verify_upgrade(response.headers(), &key)?;
    let stream = timeout(STARTUP_TIMEOUT, response.upgrade())
        .await
        .context("call socket upgrade timed out")??;
    let config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(SOCKET_LIMIT * 2)
        .max_message_size(Some(SOCKET_LIMIT))
        .max_frame_size(Some(SOCKET_LIMIT));
    Ok(WebSocketStream::from_raw_socket(stream, Role::Client, Some(config)).await)
}

fn verify_upgrade(headers: &header::HeaderMap, key: &str) -> Result<()> {
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

async fn send_json(socket: &mut Socket, value: &impl serde::Serialize) -> Result<()> {
    let text = serde_json::to_string(value)?;
    if text.len() > 16 * 1024 {
        bail!("call command exceeds the message limit");
    }
    timeout(WRITE_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .context("call socket write timed out")??;
    Ok(())
}

async fn receive(socket: &mut Socket, label: &str) -> Result<Message> {
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
                    .and_then(|result| result.map_err(Into::into))
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

async fn receive_event(socket: &mut Socket, label: &str) -> Result<CallEvent> {
    let Message::Text(text) = receive(socket, label).await? else {
        bail!("unexpected binary control message");
    };
    if text.len() > 16 * 1024 {
        bail!("call event exceeds the message limit");
    }
    let event = serde_json::from_str(&text).context("invalid call event")?;
    if call_debug_enabled() {
        match event_json(&event) {
            Ok(line) => debug_call(label, format_args!("<- {line}")),
            Err(error) => debug_call(label, format_args!("could not format event: {error}")),
        }
    }
    Ok(event)
}

struct Control {
    socket: Socket,

    cancellation: Option<Cancellation>,

    client_id: String,

    capabilities: CallCapabilities,

    availability: CallsAvailability,

    offer: Option<CallOffer>,

    pending_offer: Option<CallPendingOffer>,

    tracks: std::collections::BTreeMap<(String, u32), wechat_core::call_types::CallAudioTrack>,

    call: Option<CallView>,

    format: CallAudioFormat,
}
impl Control {
    async fn connect(remote: &Remote) -> Result<(Self, CallEvent)> {
        let mut socket = open_socket(remote, "calls/socket").await?;
        let event = timeout(STARTUP_TIMEOUT, receive_event(&mut socket, "control"))
            .await
            .context("call socket hello timed out")??;
        let CallEvent::Hello {
            protocol_version: 1,
            client_id: _client_id,
            capabilities: _capabilities,
            availability: _availability,
            current_offer: _current_offer,
            current_pending_offer: _current_pending_offer,
            current_call: _current_call,
            audio_format: _audio_format,
        } = &event
        else {
            bail!("unsupported call socket hello");
        };
        Ok((
            Self {
                socket,

                cancellation: None,

                client_id: _client_id.clone(),

                capabilities: *_capabilities,

                availability: *_availability,

                offer: _current_offer.clone(),

                pending_offer: _current_pending_offer.clone(),

                tracks: Default::default(),

                call: _current_call.clone(),

                format: *_audio_format,
            },
            event,
        ))
    }

    async fn next_event(&mut self) -> Result<CallEvent> {
        receive_event(&mut self.socket, "control").await
    }

    fn update(&mut self, event: &CallEvent) {
        match event {
            CallEvent::Offered { offer } => self.offer = Some(offer.clone()),
            CallEvent::Pending { offer } => self.pending_offer = offer.clone(),
            CallEvent::TrackAdded { call_id, track }
            | CallEvent::TrackUpdated { call_id, track } => {
                let key = (call_id.clone(), track.id);
                if self
                    .tracks
                    .get(&key)
                    .is_none_or(|current| track.generation >= current.generation)
                {
                    self.tracks.insert(key, track.clone());
                }
            }
            CallEvent::State { call } => self.call = Some(call.clone()),
            CallEvent::Control {
                call_id,
                participant_id,
                control,
            } => {
                if let Some(call) = self.call.as_mut().filter(|call| call.call_id == *call_id) {
                    call.controls
                        .retain(|item| item.participant_id != *participant_id);
                    call.controls
                        .push(wechat_core::call_types::CallParticipantControl {
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
            }
            _ => {}
        }
    }

    async fn command(&mut self, command: CallCommand) -> Result<OwnerResult> {
        let cancellation = self.cancellation.clone();
        if cancellation
            .as_ref()
            .is_some_and(Cancellation::is_cancelled)
        {
            return Err(failure(
                130,
                "client_interrupted",
                "call command cancelled before submission",
            ));
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
                        if status == wechat_core::call_types::CallCommandStatus::Error {
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
            _ = interrupted => Err(failure(130,"client_interrupted","call command interrupted; outcome unknown; check calls status before retrying")),
            response = timeout(Duration::from_secs(30),response) => response.context("call command outcome unknown; check calls status before retrying")?,
        }
    }
    async fn close(&mut self) {
        debug_call("control", format_args!("local close requested"));
        let result = timeout(WRITE_TIMEOUT, self.socket.close(None)).await;
        debug_call("control", format_args!("local close result: {result:?}"));
    }
}

/// A signal stays latched across device preparation, attachment and socket
/// awaits. A new waiter cannot miss a signal consumed by an earlier phase.
#[derive(Clone)]
struct Cancellation(tokio::sync::watch::Receiver<bool>);

impl Cancellation {
    fn listen() -> (Self, SignalWatcher) {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = sender.send(true);
        });
        (Self(receiver), SignalWatcher(task))
    }
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    async fn wait(&self) {
        let mut receiver = self.0.clone();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }
}

struct SignalWatcher(tokio::task::JoinHandle<()>);

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct OwnerResult {
    call_id: Option<String>,
    owner_token: Option<String>,
}

fn event_json(event: &CallEvent) -> Result<String> {
    let mut value = serde_json::to_value(event)?;
    // Owner tokens authorize media attachment and must stay out of CLI output.
    if let Some(object) = value.as_object_mut() {
        object.remove("ownerToken");
    }
    Ok(serde_json::to_string(&value)?)
}

async fn observe(remote: &Remote) -> Result<()> {
    observe_until(remote, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

async fn observe_until(remote: &Remote, stop: impl std::future::Future<Output = ()>) -> Result<()> {
    let (mut control, hello) = Control::connect(remote).await?;
    println!("{}", event_json(&hello)?);
    tokio::pin!(stop);
    let result = loop {
        tokio::select! {
            _ = &mut stop => break Ok(()),
            event = control.next_event() => match event {
                Ok(event) => { println!("{}",event_json(&event)?); }
                Err(error) => break Err(error),
            }
        }
    };
    control.close().await;
    result
}

#[cfg(test)]
#[path = "calls_cli_tests.rs"]
mod tests;
