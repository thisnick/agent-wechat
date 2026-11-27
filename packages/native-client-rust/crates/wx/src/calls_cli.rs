//! Call control on the CLI's machine. Merely observing never opens audio devices.
use super::{failure, Remote};
use anyhow::Result;
use clap::{Args, Subcommand};
#[cfg(test)]
use futures_util::{SinkExt, StreamExt};
#[cfg(test)]
use reqwest::header;
use std::{
    ops::{Deref, DerefMut},
    time::Duration,
};
#[cfg(test)]
use tokio::time::timeout;
#[cfg(test)]
use tokio_tungstenite::tungstenite::{
    handshake::{client::generate_key, derive_accept_key},
    Message,
};
#[cfg(test)]
use wechat_call_client::transport::{self, verify_upgrade};
use wechat_call_client::{
    audio::AudioDeviceOptions,
    transport::{
        debug_call, event_json, send_json, CallClientError, CallEndpoint, Cancellation,
        ControlClient, OwnerResult,
    },
};
use wechat_core::call_types::{CallAudioFormat, CallCommand, CallEvent, CallOffer};

#[path = "calls_cli_audio.rs"]
mod audio;

#[path = "calls_cli_interaction.rs"]
mod interaction;

#[path = "calls_cli_receive.rs"]
mod receive_audio;

#[path = "calls_cli_levels.rs"]
mod levels;

#[path = "calls_cli_video.rs"]
mod video;

#[path = "calls_cli_session.rs"]
pub(crate) mod session;

#[derive(Subcommand)]
pub(crate) enum CallsCommand {
    /// List local microphone, speaker and camera devices without connecting to a daemon.
    Devices,
    /// Read the daemon's current call state and exit.
    Status,
    /// Wait for incoming voice or video calls. With --json, observe only without local devices.
    Monitor {
        #[command(flatten)]
        audio: AudioOptions,
        #[command(flatten)]
        media: IncomingMediaOptions,
    },
    /// Start or join a voice or video conference with local devices and participant meters.
    Conference {
        #[command(subcommand)]
        command: Option<ConferenceCommand>,
    },
    /// Call one contact using this computer's microphone, speakers and optional camera.
    Dial {
        /// Contact ID to call, from wx contacts list.
        contact: String,
        #[command(flatten)]
        audio: AudioOptions,
        #[command(flatten)]
        media: OutgoingMediaOptions,
    },
    /// Answer the current offer using this computer's microphone, speakers and optional camera.
    Answer {
        /// Incoming call ID to answer; omit to answer the current offer.
        call_id: Option<String>,
        #[command(flatten)]
        audio: AudioOptions,
        #[command(flatten)]
        media: IncomingMediaOptions,
    },
    /// Run and drive headless call sessions with synthetic media, event waits, recording and inspection.
    Session {
        #[command(subcommand)]
        command: Option<session::SessionCommand>,
    },
}

#[derive(Subcommand)]
pub(crate) enum ConferenceCommand {
    /// Start a group voice or video conference; use i inside the call for later invitations.
    Start {
        /// Group ID, ending in @chatroom, to start the conference in.
        group: String,
        /// Contact ID to invite; repeat for several.
        #[arg(long = "invite", required = true)]
        invitees: Vec<String>,
        #[command(flatten)]
        audio: AudioOptions,
        #[command(flatten)]
        media: OutgoingMediaOptions,
    },
    /// Join a current invitation for this group; never creates a room.
    Join {
        /// Group ID, ending in @chatroom, that has a current conference invitation.
        group: String,
        #[command(flatten)]
        audio: AudioOptions,
        #[command(flatten)]
        media: OutgoingMediaOptions,
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

impl From<&AudioOptions> for AudioDeviceOptions {
    fn from(options: &AudioOptions) -> Self {
        Self {
            input_device: options.input_device.clone(),
            output_device: options.output_device.clone(),
        }
    }
}

impl From<&AudioDeviceOptions> for AudioOptions {
    fn from(options: &AudioDeviceOptions) -> Self {
        Self {
            input_device: options.input_device.clone(),
            output_device: options.output_device.clone(),
        }
    }
}

#[derive(Args, Clone, Default)]
pub(crate) struct OutgoingMediaOptions {
    /// Enable video for this call (camera and microphone start on unless overridden).
    #[arg(long)]
    video: bool,
    /// Local camera source: auto, off, test, file:PATH, or a device path.
    #[arg(long)]
    camera: Option<String>,
    /// Start with the camera turned off; toggle with v during the call.
    #[arg(long)]
    camera_off: bool,
    /// Start with the microphone muted; toggle with m during the call.
    #[arg(long)]
    muted: bool,
}

#[derive(Args, Clone, Default)]
pub(crate) struct IncomingMediaOptions {
    /// Local camera source for video calls: auto, off, test, file:PATH, or a device path.
    #[arg(long)]
    camera: Option<String>,
    /// Start video calls with the camera turned off; toggle with v before or during the call.
    #[arg(long)]
    camera_off: bool,
    /// Start with the microphone muted; toggle with m before or during the call.
    #[arg(long)]
    muted: bool,
}

pub(crate) async fn run(
    remote: &Remote,
    spawn: &session::SessionSpawnContext,
    command: CallsCommand,
    json: bool,
    parent: Option<crate::parent_watch::ParentWatch>,
) -> Result<()> {
    debug_call("session", format_args!("debug enabled"));
    match command {
        CallsCommand::Devices => devices(json).await,
        CallsCommand::Status => {
            let value = remote.get(&remote.path("calls/current")?).await?;
            super::Output::new(json).print(&value)
        }
        CallsCommand::Monitor { .. } if json => observe(remote).await,
        CallsCommand::Session { command: Some(cmd) } => {
            session::run_with_daemon(remote, spawn, cmd, json, parent).await
        }
        CallsCommand::Session { command: None } => anyhow::bail!("choose a session subcommand"),
        command => audio::run(remote, command, json).await,
    }
}

pub(crate) async fn devices(json: bool) -> Result<()> {
    {
        let (devices, cameras) = tokio::task::spawn_blocking(|| {
            let audio = wechat_audio::AudioSession::devices()?;
            let cameras = video::list_cameras();
            Ok::<_, anyhow::Error>((audio, cameras))
        })
        .await??;
        let mut values: Vec<_> = devices
            .iter()
            .map(|device| {
                serde_json::json!({
                    "kind": if device.input { "input" } else { "output" }, "id": device.id,
                    "name": device.name, "default": device.is_default,
                })
            })
            .collect();
        for camera in &cameras {
            values.push(serde_json::json!({
                "kind": "video_input",
                "id": camera.id,
                "name": camera.name,
                "default": camera.is_default,
            }));
        }
        if json {
            return super::Output::new(json).print(&serde_json::json!({"devices": values}));
        }
        println!("Local audio and camera devices (no capture or playback started):");
        let clean = |text: &str| {
            text.chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect::<String>()
        };
        for device in devices {
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
        if cameras.is_empty() {
            println!("Camera: None detected\n  Pass --camera test for a synthetic test pattern");
        } else {
            for camera in cameras {
                println!(
                    "Camera{}: {}\n  {}",
                    if camera.is_default { " (default)" } else { "" },
                    clean(&camera.name),
                    clean(&camera.id)
                );
            }
        }
        Ok(())
    }
}

fn endpoint(remote: &Remote) -> Result<CallEndpoint> {
    Ok(CallEndpoint {
        http: remote.http.clone(),
        base: remote.base.clone(),
        token: remote.token.clone(),
        account: remote.account()?.to_owned(),
    })
}

fn map_client_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(client_error) = error.downcast_ref::<CallClientError>() {
        return match client_error {
            CallClientError::SocketRejected { status } => {
                let display = reqwest::StatusCode::from_u16(*status)
                    .map(|code| code.to_string())
                    .unwrap_or_else(|_| status.to_string());
                failure(
                    if matches!(status, 401 | 403) { 4 } else { 3 },
                    "call_socket_rejected",
                    &format!("call socket upgrade rejected ({display})"),
                )
            }
            CallClientError::Interrupted { message } => failure(130, "client_interrupted", message),
        };
    }
    error
}

#[cfg(test)]
async fn open_socket(remote: &Remote, suffix: &str) -> Result<transport::Socket> {
    transport::open_socket(&endpoint(remote)?, suffix)
        .await
        .map_err(map_client_error)
}

struct Control(ControlClient);

impl Deref for Control {
    type Target = ControlClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Control {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Control {
    async fn connect(remote: &Remote) -> Result<(Self, CallEvent)> {
        let (inner, hello) = ControlClient::connect(&endpoint(remote)?)
            .await
            .map_err(map_client_error)?;
        Ok((Self(inner), hello))
    }

    async fn command(&mut self, command: CallCommand) -> Result<OwnerResult> {
        self.0.command(command).await.map_err(map_client_error)
    }
}

fn listen_cancellation() -> (Cancellation, SignalWatcher) {
    let (sender, cancellation) = Cancellation::channel();
    let task = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = sender.send(true);
    });
    (cancellation, SignalWatcher(task))
}

struct SignalWatcher(tokio::task::JoinHandle<()>);

impl Drop for SignalWatcher {
    fn drop(&mut self) {
        self.0.abort();
    }
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
                Ok(event) => { println!("{}", event_json(&event)?); }
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
