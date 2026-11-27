//! Headless, agent-drivable call session engine and local IPC CLI (`wx calls session`).
//!
//! Runs a persistent Calls Protocol v3 client without requiring a TTY, alternate screen
//! buffer, or physical audio/camera hardware. Agents can drive one or many named sessions
//! via sequential shell commands over local IPC (`start`, `dial`, `answer`, `reject`,
//! `hangup`, `conference`, `mute`, `unmute`, `camera`, `audio`, `video`, `wait`,
//! `status`, `events`, `snapshot`, `record`, `stop`) or drive a single process over
//! `--stdio` NDJSON.

use super::{endpoint, map_client_error, Control};
use crate::{failure, parent_watch::ParentWatch, Output, Remote};
use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::{Args, Subcommand};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::{broadcast, mpsc, oneshot, watch, Semaphore},
    time::timeout,
};
use tokio_tungstenite::tungstenite::Message;
use wechat_call_client::{
    inspect::{CallMediaInspector, MediaInspectionReport, RecordingReport},
    media::MediaClient,
    synthetic_audio::{AudioSinkSpec, AudioSourceSpec, CallAudioIo},
    transport::{event_json, parse_call_event_str, send_json},
    video::{client_video_capabilities_with_receive, CameraSource, LocalVideoCapture},
};
use wechat_calls::audio::AudioFormat;
use wechat_core::{
    call_types::{
        CallAudioFormat, CallCommand, CallCommandStatus, CallConferenceInvitation, CallDirection,
        CallEvent, CallReadyScope, CallState, CallTarget, CallTrack, CallTrackDirection, CallType,
        CallVideoCodec,
    },
    calls::pcm_envelope::{MediaPayloadKind, WireMediaFrame},
};

const EVENT_BUFFER_CAP: usize = 4096;
const MEDIA_CLOSED_END_GRACE: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_IPC_LINE_BYTES: usize = 256 * 1024;

/// Connection details captured by `main.rs` so `wx calls session start` can spawn
/// `wx calls session run` without placing bearer tokens on command-line arguments.
#[derive(Clone, Debug)]
pub(crate) struct SessionSpawnContext {
    pub(crate) endpoint: String,
    pub(crate) account: String,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) ephemeral_token: Option<String>,
    pub(crate) ca_cert_file: Option<PathBuf>,
}

/// Identifies a running local call session by name or explicit directory.
#[derive(Args, Clone, Debug, Default)]
pub(crate) struct SessionTargetArgs {
    /// Named session to target (defaults to "default").
    #[arg(long, default_value = "default")]
    pub(crate) session: String,
    /// Explicit private session directory instead of the default runtime directory.
    #[arg(long)]
    pub(crate) dir: Option<PathBuf>,
}

/// Shared options for starting or running a headless call session.
#[derive(Args, Clone, Debug)]
pub(crate) struct SessionRunOptions {
    /// Session name used to address this session from other wx calls session commands.
    #[arg(long, default_value = "default")]
    pub(crate) name: String,
    /// Explicit private directory for this session's socket, event log, snapshots and recordings.
    #[arg(long)]
    pub(crate) dir: Option<PathBuf>,
    /// Advertise call readiness immediately so incoming direct offers and conference invitations arrive.
    #[arg(long)]
    pub(crate) ready: bool,
    /// Outgoing audio source: silence, tone[:HZ[:AMP[:ON_MS:OFF_MS]]], file:PATH[:loop|:once], or device[:ID].
    #[arg(long, default_value = "silence")]
    pub(crate) audio: String,
    /// Incoming audio sink: null (headless metering/recording only) or device[:ID].
    #[arg(long, default_value = "null")]
    pub(crate) output: String,
    /// Outgoing video source: test, smpte, color:NAME|#RRGGBB, file:PATH, off, auto, or a device path.
    #[arg(long, default_value = "test")]
    pub(crate) video: String,
    /// Preferred incoming video codec: jpeg (decoded RGB inspection/snapshots), h265, or h264.
    #[arg(long, default_value = "jpeg", value_parser = ["jpeg", "h265", "h264"])]
    pub(crate) receive_video_codec: String,
    /// Automatically record each call's TX/RX WAV audio and RX video into this directory.
    #[arg(long)]
    pub(crate) record_dir: Option<PathBuf>,
    /// Periodically save decoded remote video snapshots every N milliseconds (0 disables).
    #[arg(long, default_value_t = 0)]
    pub(crate) snapshot_interval: u64,
    /// Emit media.stats events every N milliseconds while media is attached (0 disables).
    #[arg(long, default_value_t = 1000)]
    pub(crate) stats_interval: u64,
    /// Stop automatically after this many idle seconds with no active call or commands (0 disables).
    #[arg(long, default_value_t = 600)]
    pub(crate) idle_timeout: u64,
}

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum SessionCommand {
    /// Start a detached headless call session in the background and wait until ready.
    Start {
        #[command(flatten)]
        options: SessionRunOptions,
    },
    /// Run a headless call session in the foreground, optionally driven over NDJSON stdin/stdout.
    Run {
        #[command(flatten)]
        options: SessionRunOptions,
        /// Read NDJSON commands from stdin and emit NDJSON events and replies on stdout.
        #[arg(long)]
        stdio: bool,
        /// Disable the local IPC socket (for isolated --stdio runs).
        #[arg(long)]
        no_ipc: bool,
        /// Stop gracefully when the parent process that started this foreground session exits.
        #[arg(long)]
        exit_with_parent: bool,
    },
    /// List local call sessions and report whether each is running.
    List {
        /// Parent directory containing named session folders; defaults to the standard runtime folder.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Stop a running headless call session cleanly.
    Stop {
        /// Session name to stop (defaults to "default").
        #[arg(default_value = "default")]
        name: String,
        /// Explicit private session directory to stop instead of resolving by name.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Disconnect without first hanging up any active owned call.
        #[arg(long)]
        no_hangup: bool,
        /// Seconds to wait for the session process to exit (1 to 120).
        #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=120))]
        timeout: u64,
    },
    /// Show the session's current call state, offers, peer status, controls and live media metrics.
    Status {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
    /// Read buffered session events or follow live events as NDJSON.
    Events {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Return only events with sequence number strictly greater than this value.
        #[arg(long, default_value_t = 0)]
        after: u64,
        /// Maximum number of buffered events to return when not following.
        #[arg(long, default_value_t = 200)]
        limit: usize,
        /// Keep streaming new events as NDJSON lines until interrupted or the session stops.
        #[arg(long)]
        follow: bool,
    },
    /// Block until a matching call event or media threshold is observed, or fail on timeout.
    Wait(Box<SessionWaitArgs>),
    /// Enable or disable readiness for incoming offers and conference invitations.
    Ready {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Desired readiness state: on or off.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Place a direct voice or video call from a running session.
    Dial {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Contact ID to call, from wx contacts list.
        contact: String,
        /// Start a video call instead of a voice call.
        #[arg(long)]
        video: bool,
        /// Start with the microphone muted.
        #[arg(long)]
        muted: bool,
        /// Start a video call with the camera turned off.
        #[arg(long)]
        camera_off: bool,
        /// Override the outgoing audio source for this call.
        #[arg(long)]
        audio: Option<String>,
        /// Override the outgoing video source for this call.
        #[arg(long)]
        video_source: Option<String>,
    },
    /// Answer the current incoming direct call offer in a running session.
    Answer {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Incoming call ID to answer; omit to answer the current offer.
        call_id: Option<String>,
        /// Start with the microphone muted.
        #[arg(long)]
        muted: bool,
        /// Start a video call with the camera turned off.
        #[arg(long)]
        camera_off: bool,
        /// Override the outgoing audio source for this call.
        #[arg(long)]
        audio: Option<String>,
        /// Override the outgoing video source for this call.
        #[arg(long)]
        video_source: Option<String>,
    },
    /// Reject the current incoming direct call offer in a running session.
    Reject {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Incoming call ID to reject; omit to reject the current offer.
        call_id: Option<String>,
    },
    /// Cancel or hang up the session's active owned call and wait for terminal cleanup.
    Hangup {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
    /// Start, join, or invite contacts into a group conference from a running session.
    Conference {
        #[command(subcommand)]
        command: Option<SessionConferenceCommand>,
    },
    /// Mute the local microphone and notify the peer or conference.
    Mute {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
    /// Unmute the local microphone and notify the peer or conference.
    Unmute {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
    /// Turn the local camera on or off during a video call.
    Camera {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Desired camera state: on or off.
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Clear queued outgoing audio and advance the send audio track generation.
    ClearAudio {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
    /// Switch the outgoing audio source (silence, tone:..., file:..., device[:ID]) live.
    Audio {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Outgoing audio source specification.
        source: String,
    },
    /// Switch the outgoing video source (test, smpte, color:..., file:..., off, auto, device) live.
    Video {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Outgoing video source specification.
        source: String,
    },
    /// Request a remote video keyframe from the daemon.
    Keyframe {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Receive video track ID; omit to request on every active receive video track.
        #[arg(long)]
        track: Option<u32>,
    },
    /// Save the latest decoded remote video frame(s) to JPEG or PNG for visual inspection.
    Snapshot {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Receive video track ID; omit to snapshot all decoded remote video tracks.
        #[arg(long)]
        track: Option<u32>,
        /// Output image file path (.jpg or .png) or directory; defaults to the session snapshots folder.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Start or stop recording call audio and video streams.
    Record {
        #[command(subcommand)]
        command: Option<SessionRecordCommand>,
    },
}

impl SessionCommand {
    /// Returns true only for commands that connect to a WeChat daemon directly.
    pub(crate) fn needs_daemon(&self) -> bool {
        matches!(self, Self::Start { .. } | Self::Run { .. })
    }
}

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum SessionConferenceCommand {
    /// Start a group voice or video conference from a running session.
    Start {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Group ID, ending in @chatroom, to start the conference in.
        group: String,
        /// Contact ID to invite; repeat for several.
        #[arg(long = "invite", required = true)]
        invitees: Vec<String>,
        /// Start a video conference instead of voice-only.
        #[arg(long)]
        video: bool,
        /// Start with the microphone muted.
        #[arg(long)]
        muted: bool,
        /// Start a video conference with the camera turned off.
        #[arg(long)]
        camera_off: bool,
        /// Override the outgoing audio source for this call.
        #[arg(long)]
        audio: Option<String>,
        /// Override the outgoing video source for this call.
        #[arg(long)]
        video_source: Option<String>,
    },
    /// Join a current conference invitation for a group from a running session.
    Join {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Group ID, ending in @chatroom, with an active conference invitation.
        group: String,
        /// Exact invitation ID when more than one invitation exists for the group.
        #[arg(long)]
        invitation_id: Option<String>,
        /// Enable video when joining the conference.
        #[arg(long)]
        video: bool,
        /// Start with the microphone muted.
        #[arg(long)]
        muted: bool,
        /// Join with the camera turned off even when --video is passed.
        #[arg(long)]
        camera_off: bool,
        /// Override the outgoing audio source for this call.
        #[arg(long)]
        audio: Option<String>,
        /// Override the outgoing video source for this call.
        #[arg(long)]
        video_source: Option<String>,
    },
    /// Invite one or more contacts into the session's active conference.
    Invite {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Contact IDs to invite into the active conference.
        #[arg(required = true)]
        contacts: Vec<String>,
    },
}

#[derive(Subcommand, Clone, Debug)]
pub(crate) enum SessionRecordCommand {
    /// Start recording TX/RX WAV audio and RX video for the active call.
    Start {
        #[command(flatten)]
        target: SessionTargetArgs,
        /// Output directory for recordings; defaults to a timestamped folder inside the session directory.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Stop recording and finalize WAV headers, video remuxing and manifest.json.
    Stop {
        #[command(flatten)]
        target: SessionTargetArgs,
    },
}

fn default_wait_timeout() -> u64 {
    30
}

fn default_hz_tolerance() -> f64 {
    25.0
}

#[derive(Args, Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SessionWaitArgs {
    #[command(flatten)]
    #[serde(skip)]
    pub(crate) target: SessionTargetArgs,
    /// Only consider events with sequence number strictly greater than this value.
    #[arg(long, default_value_t = 0)]
    pub(crate) after: u64,
    /// Seconds to wait before failing with a timeout error.
    #[arg(long, default_value_t = 30)]
    #[serde(default = "default_wait_timeout")]
    pub(crate) timeout: u64,
    /// Match events whose type equals one of these values (repeatable).
    #[arg(long = "event")]
    pub(crate) events: Vec<String>,
    /// Restrict matching to events for this call ID.
    #[arg(long)]
    pub(crate) call_id: Option<String>,
    /// Match call.state events reaching one of these states (repeatable).
    #[arg(long = "state", value_parser = ["offered", "dialing", "ringing", "connecting", "connected", "ending", "ended", "uncertain"])]
    pub(crate) states: Vec<String>,
    /// Match call.peer_status events reaching one of these kinds (repeatable).
    #[arg(long = "peer-status", value_parser = ["undefined", "joined", "invited", "acknowledged", "left", "cancelled", "busy", "rejected", "no_response", "unknown"])]
    pub(crate) peer_statuses: Vec<String>,
    /// Match call.control events with this control kind (microphone or camera).
    #[arg(long, value_parser = ["microphone", "camera"])]
    pub(crate) control_kind: Option<String>,
    /// Match call.control events with this muted/enabled boolean value.
    #[arg(long)]
    pub(crate) control_value: Option<bool>,
    /// Match participant.updated, call.control, or call.peer_status for this participant or contact ID.
    #[arg(long)]
    pub(crate) participant: Option<String>,
    /// Match participant.updated events with this membership (invited, joined, left, unknown).
    #[arg(long, value_parser = ["invited", "joined", "left", "unknown"])]
    pub(crate) membership: Option<String>,
    /// Match participant.updated events with this status kind.
    #[arg(long, value_parser = ["undefined", "joined", "invited", "acknowledged", "left", "cancelled", "busy", "rejected", "no_response", "unknown"])]
    pub(crate) participant_status: Option<String>,
    /// Match participant.updated events where microphoneEnabled equals this value.
    #[arg(long)]
    pub(crate) mic: Option<bool>,
    /// Match participant.updated events where cameraEnabled equals this value.
    #[arg(long)]
    pub(crate) camera: Option<bool>,
    /// Match conference.invitations events containing an invitation for this group ID.
    #[arg(long)]
    pub(crate) group: Option<String>,
    /// Wait until received audio frame count reaches at least this value.
    #[arg(long)]
    pub(crate) audio_frames: Option<u64>,
    /// Wait until received audio RMS level reaches at least this dBFS value (e.g. -35).
    #[arg(long, allow_negative_numbers = true)]
    pub(crate) min_rms_db: Option<f64>,
    /// Wait until received audio dominant tone frequency is within --hz-tolerance of this Hz value.
    #[arg(long)]
    pub(crate) dominant_hz: Option<f64>,
    /// Frequency tolerance in Hz used with --dominant-hz.
    #[arg(long, default_value_t = 25.0)]
    #[serde(default = "default_hz_tolerance")]
    pub(crate) hz_tolerance: f64,
    /// Wait until received video frame count reaches at least this value.
    #[arg(long)]
    pub(crate) video_frames: Option<u64>,
    /// Restrict --audio-frames, --min-rms-db, --dominant-hz, or --video-frames to this track ID.
    #[arg(long)]
    pub(crate) track: Option<u32>,
}

impl SessionWaitArgs {
    fn has_media_predicate(&self) -> bool {
        self.audio_frames.is_some()
            || self.min_rms_db.is_some()
            || self.dominant_hz.is_some()
            || self.video_frames.is_some()
    }

    fn has_event_predicate(&self) -> bool {
        !self.events.is_empty()
            || !self.states.is_empty()
            || !self.peer_statuses.is_empty()
            || self.control_kind.is_some()
            || self.control_value.is_some()
            || self.participant.is_some()
            || self.membership.is_some()
            || self.participant_status.is_some()
            || self.mic.is_some()
            || self.camera.is_some()
            || self.group.is_some()
    }

    fn matches_media(&self, report: &MediaInspectionReport) -> bool {
        if let Some(min_frames) = self.audio_frames {
            let frames = match self.track {
                Some(id) => report
                    .audio_receive
                    .iter()
                    .find(|t| t.track_id == id)
                    .map_or(0, |t| t.frames),
                None => report.audio_receive.iter().map(|t| t.frames).sum(),
            };
            if frames < min_frames {
                return false;
            }
        }
        if let Some(min_rms) = self.min_rms_db {
            let rms = match self.track {
                Some(id) => report
                    .audio_receive
                    .iter()
                    .find(|t| t.track_id == id)
                    .map_or(-90.0, |t| t.rms_db),
                None => report
                    .audio_receive
                    .iter()
                    .map(|t| t.rms_db)
                    .fold(-90.0f64, f64::max),
            };
            if rms < min_rms {
                return false;
            }
        }
        if let Some(target_hz) = self.dominant_hz {
            let tol = self.hz_tolerance.abs().max(1.0);
            let matched = match self.track {
                Some(id) => report
                    .audio_receive
                    .iter()
                    .find(|t| t.track_id == id)
                    .and_then(|t| t.dominant_hz)
                    .is_some_and(|hz| (hz - target_hz).abs() <= tol),
                None => report
                    .audio_receive
                    .iter()
                    .filter_map(|t| t.dominant_hz)
                    .any(|hz| (hz - target_hz).abs() <= tol),
            };
            if !matched {
                return false;
            }
        }
        if let Some(min_vframes) = self.video_frames {
            let frames = match self.track {
                Some(id) => report
                    .video_receive
                    .iter()
                    .find(|t| t.track_id == id)
                    .map_or(0, |t| t.frames),
                None => report.video_receive.iter().map(|t| t.frames).sum(),
            };
            if frames < min_vframes {
                return false;
            }
        }
        true
    }

    fn matches_event(&self, event: &Value) -> bool {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return false;
        };
        if let Some(want_call) = self.call_id.as_deref() {
            let ev_call = event
                .get("callId")
                .and_then(Value::as_str)
                .or_else(|| event.pointer("/call/callId").and_then(Value::as_str))
                .or_else(|| event.pointer("/offer/callId").and_then(Value::as_str));
            if ev_call != Some(want_call) {
                return false;
            }
        }
        if !self.events.is_empty() && !self.events.iter().any(|e| e == kind) {
            return false;
        }
        if !self.states.is_empty() {
            if kind != "call.state" {
                return false;
            }
            let Some(state) = event.pointer("/call/state").and_then(Value::as_str) else {
                return false;
            };
            if !self.states.iter().any(|s| s == state) {
                return false;
            }
        }
        if !self.peer_statuses.is_empty() {
            if kind != "call.peer_status" {
                return false;
            }
            let Some(status_kind) = event.pointer("/status/kind").and_then(Value::as_str) else {
                return false;
            };
            if !self.peer_statuses.iter().any(|s| s == status_kind) {
                return false;
            }
        }
        if self.control_kind.is_some() || self.control_value.is_some() {
            if kind != "call.control" {
                return false;
            }
            let ev_ctrl_kind = event.pointer("/control/kind").and_then(Value::as_str);
            if let Some(want) = self.control_kind.as_deref() {
                if ev_ctrl_kind != Some(want) {
                    return false;
                }
            }
            if let Some(want_val) = self.control_value {
                let actual = match ev_ctrl_kind {
                    Some("microphone") => event.pointer("/control/muted").and_then(Value::as_bool),
                    Some("camera") => event.pointer("/control/enabled").and_then(Value::as_bool),
                    _ => None,
                };
                if actual != Some(want_val) {
                    return false;
                }
            }
        }
        if self.membership.is_some()
            || self.participant_status.is_some()
            || self.mic.is_some()
            || self.camera.is_some()
        {
            if kind != "participant.updated" {
                return false;
            }
            if let Some(want) = self.membership.as_deref() {
                if event
                    .pointer("/participant/membership")
                    .and_then(Value::as_str)
                    != Some(want)
                {
                    return false;
                }
            }
            if let Some(want) = self.participant_status.as_deref() {
                if event
                    .pointer("/participant/status/kind")
                    .and_then(Value::as_str)
                    != Some(want)
                {
                    return false;
                }
            }
            if let Some(want) = self.mic {
                if event
                    .pointer("/participant/microphoneEnabled")
                    .and_then(Value::as_bool)
                    != Some(want)
                {
                    return false;
                }
            }
            if let Some(want) = self.camera {
                if event
                    .pointer("/participant/cameraEnabled")
                    .and_then(Value::as_bool)
                    != Some(want)
                {
                    return false;
                }
            }
        }
        if let Some(want_participant) = self.participant.as_deref() {
            let matched = match kind {
                "participant.updated" => {
                    event
                        .pointer("/participant/participantId")
                        .and_then(Value::as_str)
                        == Some(want_participant)
                        || event
                            .pointer("/participant/contactId")
                            .and_then(Value::as_str)
                            == Some(want_participant)
                }
                "call.control" | "call.peer_status" => {
                    event.get("participantId").and_then(Value::as_str) == Some(want_participant)
                }
                _ => false,
            };
            if !matched {
                return false;
            }
        }
        if let Some(want_group) = self.group.as_deref() {
            if kind != "conference.invitations" {
                return false;
            }
            let Some(list) = event.get("invitations").and_then(Value::as_array) else {
                return false;
            };
            if !list
                .iter()
                .any(|inv| inv.get("groupId").and_then(Value::as_str) == Some(want_group))
            {
                return false;
            }
        }
        true
    }
}

/// Entrypoint for daemon-backed session commands (`start`, `run`).
pub(crate) async fn run_with_daemon(
    remote: &Remote,
    spawn: &SessionSpawnContext,
    command: SessionCommand,
    json: bool,
    parent: Option<ParentWatch>,
) -> Result<()> {
    match command {
        SessionCommand::Start { options } => start_detached(spawn, options, json).await,
        SessionCommand::Run {
            options,
            stdio,
            no_ipc,
            exit_with_parent,
        } => {
            let parent = if exit_with_parent {
                parent.or_else(|| ParentWatch::new().ok())
            } else {
                None
            };
            run_foreground(remote, options, stdio, no_ipc, parent).await
        }
        other => run_local(other, json).await,
    }
}

/// Entrypoint for local-IPC session commands that require no daemon flags or token.
pub(crate) async fn run_local(command: SessionCommand, json: bool) -> Result<()> {
    let output = Output::new(json);
    match command {
        SessionCommand::List { dir } => {
            let root = dir.unwrap_or_else(default_sessions_root);
            let sessions = list_sessions(&root).await?;
            output.print(&json!({ "directory": root, "sessions": sessions }))
        }
        SessionCommand::Stop {
            name,
            dir,
            no_hangup,
            timeout: wait_secs,
        } => {
            let session_dir = resolve_session_dir(&name, dir.as_deref())?;
            let result =
                stop_session(&session_dir, !no_hangup, Duration::from_secs(wait_secs)).await?;
            output.print(&result)
        }
        SessionCommand::Status { target } => {
            let resp =
                ipc_call(&target, json!({ "op": "status" }), Duration::from_secs(15)).await?;
            output.print(&resp)
        }
        SessionCommand::Events {
            target,
            after,
            limit,
            follow,
        } => {
            let session_dir = resolve_session_dir(&target.session, target.dir.as_deref())?;
            if follow {
                ipc_follow_events(&session_dir, after).await
            } else {
                let resp = ipc_call_dir(
                    &session_dir,
                    json!({ "op": "events", "after": after, "limit": limit }),
                    Duration::from_secs(15),
                )
                .await?;
                if json {
                    output.print(&resp)
                } else {
                    if let Some(items) = resp["events"].as_array() {
                        for item in items {
                            println!("{}", serde_json::to_string(item)?);
                        }
                    }
                    Ok(())
                }
            }
        }
        SessionCommand::Wait(args) => {
            let wait_secs = args.timeout.max(1);
            let mut body = serde_json::to_value(&*args)?;
            body["op"] = json!("wait");
            let resp = ipc_call(
                &args.target,
                body,
                Duration::from_secs(wait_secs.saturating_add(10)),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Ready { target, state } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "ready", "ready": state == "on" }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Dial {
            target,
            contact,
            video,
            muted,
            camera_off,
            audio,
            video_source,
        } => {
            let resp = ipc_call(
                &target,
                json!({
                    "op": "dial",
                    "contact": contact,
                    "video": video,
                    "muted": muted,
                    "cameraOff": camera_off,
                    "audio": audio.map(resolve_audio_spec_path),
                    "videoSource": video_source.map(resolve_video_spec_path),
                }),
                Duration::from_secs(40),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Answer {
            target,
            call_id,
            muted,
            camera_off,
            audio,
            video_source,
        } => {
            let resp = ipc_call(
                &target,
                json!({
                    "op": "answer",
                    "callId": call_id,
                    "muted": muted,
                    "cameraOff": camera_off,
                    "audio": audio.map(resolve_audio_spec_path),
                    "videoSource": video_source.map(resolve_video_spec_path),
                }),
                Duration::from_secs(40),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Reject { target, call_id } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "reject", "callId": call_id }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Hangup { target } => {
            let resp =
                ipc_call(&target, json!({ "op": "hangup" }), Duration::from_secs(20)).await?;
            output.print(&resp)
        }
        SessionCommand::Conference { command: Some(cmd) } => match cmd {
            SessionConferenceCommand::Start {
                target,
                group,
                invitees,
                video,
                muted,
                camera_off,
                audio,
                video_source,
            } => {
                let resp = ipc_call(
                    &target,
                    json!({
                        "op": "conference.start",
                        "group": group,
                        "invitees": invitees,
                        "video": video,
                        "muted": muted,
                        "cameraOff": camera_off,
                        "audio": audio.map(resolve_audio_spec_path),
                        "videoSource": video_source.map(resolve_video_spec_path),
                    }),
                    Duration::from_secs(40),
                )
                .await?;
                output.print(&resp)
            }
            SessionConferenceCommand::Join {
                target,
                group,
                invitation_id,
                video,
                muted,
                camera_off,
                audio,
                video_source,
            } => {
                let resp = ipc_call(
                    &target,
                    json!({
                        "op": "conference.join",
                        "group": group,
                        "invitationId": invitation_id,
                        "video": video,
                        "muted": muted,
                        "cameraOff": camera_off,
                        "audio": audio.map(resolve_audio_spec_path),
                        "videoSource": video_source.map(resolve_video_spec_path),
                    }),
                    Duration::from_secs(40),
                )
                .await?;
                output.print(&resp)
            }
            SessionConferenceCommand::Invite { target, contacts } => {
                let resp = ipc_call(
                    &target,
                    json!({ "op": "conference.invite", "contacts": contacts }),
                    Duration::from_secs(35),
                )
                .await?;
                output.print(&resp)
            }
        },
        SessionCommand::Mute { target } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "mute", "muted": true }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Unmute { target } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "mute", "muted": false }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Camera { target, state } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "camera", "enabled": state == "on" }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::ClearAudio { target } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "clear_audio" }),
                Duration::from_secs(35),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Audio { target, source } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "audio", "source": resolve_audio_spec_path(source) }),
                Duration::from_secs(20),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Video { target, source } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "video", "source": resolve_video_spec_path(source) }),
                Duration::from_secs(20),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Keyframe { target, track } => {
            let resp = ipc_call(
                &target,
                json!({ "op": "keyframe", "track": track }),
                Duration::from_secs(15),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Snapshot { target, track, out } => {
            let resp = ipc_call(
                &target,
                json!({
                    "op": "snapshot",
                    "track": track,
                    "out": out.map(|p| resolve_client_path(p).display().to_string()),
                }),
                Duration::from_secs(15),
            )
            .await?;
            output.print(&resp)
        }
        SessionCommand::Record { command: Some(cmd) } => match cmd {
            SessionRecordCommand::Start { target, out } => {
                let resp = ipc_call(
                    &target,
                    json!({
                        "op": "record.start",
                        "dir": out.map(|p| resolve_client_path(p).display().to_string()),
                    }),
                    Duration::from_secs(15),
                )
                .await?;
                output.print(&resp)
            }
            SessionRecordCommand::Stop { target } => {
                let resp = ipc_call(
                    &target,
                    json!({ "op": "record.stop" }),
                    Duration::from_secs(30),
                )
                .await?;
                output.print(&resp)
            }
        },
        SessionCommand::Conference { command: None } | SessionCommand::Record { command: None } => {
            bail!("choose a subcommand")
        }
        SessionCommand::Start { .. } | SessionCommand::Run { .. } => {
            bail!("internal error: daemon-backed session command reached local handler")
        }
    }
}

// ---------------------------------------------------------------------------
// Session directory, metadata, and process management
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetadata {
    name: String,
    pid: u32,
    start_ticks: Option<String>,
    executable: Option<PathBuf>,
    account: String,
    endpoint: String,
    started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ipc_socket: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ipc_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ipc_secret: Option<String>,
}

fn validate_session_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "session name must be 1..=64 ASCII letters, digits, '-' or '_'"
    );
    Ok(())
}

fn default_sessions_root() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(runtime);
        if path.is_absolute() && path.is_dir() {
            return path.join("agent-wechat").join("call-sessions");
        }
    }
    std::env::temp_dir()
        .join(format!("agent-wechat-{}", wechat_platform::user_key()))
        .join("call-sessions")
}

fn resolve_session_dir(name: &str, explicit_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = explicit_dir {
        ensure!(
            !dir.as_os_str().is_empty(),
            "session directory cannot be empty"
        );
        return Ok(dir.to_path_buf());
    }
    validate_session_name(name)?;
    Ok(default_sessions_root().join(name))
}

fn resolve_client_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path
    }
}

fn resolve_audio_spec_path(raw: String) -> String {
    let Some(rest) = raw.strip_prefix("file:") else {
        return raw;
    };
    let (path_part, suffix) = if let Some(prefix) = rest.strip_suffix(":loop") {
        (prefix, ":loop")
    } else if let Some(prefix) = rest.strip_suffix(":once") {
        (prefix, ":once")
    } else {
        (rest, "")
    };
    if path_part.is_empty() {
        return raw;
    }
    let resolved = resolve_client_path(PathBuf::from(path_part));
    format!("file:{}{suffix}", resolved.display())
}

fn resolve_video_spec_path(raw: String) -> String {
    let Some(rest) = raw.strip_prefix("file:") else {
        return raw;
    };
    if rest.is_empty() {
        return raw;
    }
    let resolved = resolve_client_path(PathBuf::from(rest));
    format!("file:{}", resolved.display())
}

fn ensure_private_dir_exact(dir: &Path) -> Result<()> {
    if dir.exists() {
        ensure!(
            dir.is_dir() && wechat_platform::owned_path(dir)?,
            "session directory must be an owned directory and not a link: {}",
            dir.display()
        );
        wechat_platform::protect_path(dir, true)?;
    } else {
        wechat_platform::private_dir(dir)?;
    }
    Ok(())
}

fn ensure_private_session_dir(dir: &Path) -> Result<()> {
    // When creating under our default temp_dir fallback, validate ownership and
    // private permissions on each managed ancestor directory, not just the leaf.
    let default_root = default_sessions_root();
    if dir.starts_with(&default_root) {
        if let Some(root_parent) = default_root.parent() {
            ensure_private_dir_exact(root_parent)?;
        }
        ensure_private_dir_exact(&default_root)?;
    }
    ensure_private_dir_exact(dir)
}

#[cfg(unix)]
fn allocate_ipc_socket_path(session_dir: &Path) -> Result<PathBuf> {
    let direct = session_dir.join("s.sock");
    // macOS sockaddr_un.sun_path is 104 bytes (103 chars + NUL); keep margin for safety.
    if direct.as_os_str().len() <= 96 {
        return Ok(direct);
    }
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(session_dir.as_os_str().as_encoded_bytes());
    let hex: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    let base = std::env::temp_dir().join(format!("wx-{}", wechat_platform::user_key()));
    ensure_private_dir_exact(&base)?;
    Ok(base.join(format!("{hex}.sock")))
}

struct TempFileGuard(Option<PathBuf>);

impl TempFileGuard {
    fn new(path: Option<PathBuf>) -> Self {
        Self(path)
    }

    fn path(&self) -> Option<&PathBuf> {
        self.0.as_ref()
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

fn read_private_tail(path: &Path, max_lines: usize) -> String {
    let Ok(file) = wechat_platform::OpenOptions::new()
        .read(true)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(path)
    else {
        return String::new();
    };
    if !file.metadata().is_ok_and(|m| m.is_file())
        || !wechat_platform::private_file(&file).unwrap_or(false)
    {
        return String::new();
    }
    let mut text = String::new();
    let _ = std::io::Read::take(file, 64 * 1024).read_to_string(&mut text);
    text.lines()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = wechat_platform::private_tempfile_in(parent, ".wx-session-")?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|e| anyhow!("failed to persist {}: {}", path.display(), e.error))?;
    Ok(())
}

fn read_metadata(dir: &Path) -> Result<Option<SessionMetadata>> {
    let path = dir.join("session.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = crate::private_input(&path, 32 * 1024)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn session_process_alive(meta: &SessionMetadata) -> bool {
    match (
        wechat_platform::process_identity(meta.pid),
        &meta.start_ticks,
        &meta.executable,
    ) {
        (Some((created, exe)), Some(want_ticks), Some(want_exe)) => {
            &created == want_ticks && &exe == want_exe
        }
        _ => false,
    }
}

async fn start_detached(
    spawn: &SessionSpawnContext,
    mut options: SessionRunOptions,
    json: bool,
) -> Result<()> {
    options.audio = resolve_audio_spec_path(options.audio);
    options.video = resolve_video_spec_path(options.video);
    options.record_dir = options.record_dir.map(resolve_client_path);
    options.dir = options.dir.map(resolve_client_path);

    // Validate specs eagerly before spawning so invalid args fail fast and clearly.
    AudioSourceSpec::parse(&options.audio)?;
    AudioSinkSpec::parse(&options.output)?;
    CameraSource::parse(&options.video)?;
    let session_dir = resolve_session_dir(&options.name, options.dir.as_deref())?;
    ensure_private_session_dir(&session_dir)?;

    // Serialize concurrent `session start` invocations for the same directory.
    let start_lock = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(session_dir.join("start.lock"))?;
    ensure!(
        start_lock.metadata()?.is_file() && wechat_platform::private_file(&start_lock)?,
        "session start lock must be a private owned regular file"
    );
    start_lock.try_lock_exclusive().with_context(|| {
        format!(
            "another `wx calls session start` is already starting {:?}",
            options.name
        )
    })?;

    // Refuse starting a duplicate if an active session already owns this directory.
    if let Ok(Some(existing)) = read_metadata(&session_dir) {
        if session_process_alive(&existing)
            && ipc_call_dir(
                &session_dir,
                json!({ "op": "status" }),
                Duration::from_secs(2),
            )
            .await
            .is_ok()
        {
            bail!(
                "call session {:?} is already running (pid {}); stop it first or pass --name",
                existing.name,
                existing.pid
            );
        }
    }

    let log_path = session_dir.join("session.log");
    let log_file = wechat_platform::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(&log_path)?;
    ensure!(
        log_file.metadata()?.is_file() && wechat_platform::private_file(&log_file)?,
        "session log must be a private owned regular file"
    );

    let ephemeral_token_guard =
        TempFileGuard::new(if let Some(secret) = spawn.ephemeral_token.as_deref() {
            let path = session_dir.join("auth.token");
            write_private_file(&path, format!("{secret}\n").as_bytes())?;
            Some(path)
        } else {
            None
        });

    let token_file = ephemeral_token_guard
        .path()
        .or(spawn.token_file.as_ref())
        .context("API token file is unavailable for detached call session")?;

    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let mut cmd = ProcessCommand::new(&executable);
    cmd.env_remove("AGENT_WECHAT_TOKEN");
    cmd.arg("--server")
        .arg(&spawn.endpoint)
        .arg("--token-file")
        .arg(token_file)
        .arg("--account")
        .arg(&spawn.account);
    if let Some(ca) = &spawn.ca_cert_file {
        cmd.arg("--ca-cert-file")
            .arg(resolve_client_path(ca.clone()));
    }
    cmd.args(["calls", "session", "run"])
        .arg("--name")
        .arg(&options.name)
        .arg("--dir")
        .arg(&session_dir)
        .arg("--audio")
        .arg(&options.audio)
        .arg("--output")
        .arg(&options.output)
        .arg("--video")
        .arg(&options.video)
        .arg("--receive-video-codec")
        .arg(&options.receive_video_codec)
        .arg("--snapshot-interval")
        .arg(options.snapshot_interval.to_string())
        .arg("--stats-interval")
        .arg(options.stats_interval.to_string())
        .arg("--idle-timeout")
        .arg(options.idle_timeout.to_string());
    if options.ready {
        cmd.arg("--ready");
    }
    if let Some(rec) = &options.record_dir {
        cmd.arg("--record-dir").arg(rec);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    wechat_platform::detach(&mut cmd);

    let mut child = cmd.spawn().context("cannot launch call session process")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let wait_for_ready = options.ready;
    let startup = async {
        loop {
            if let Some(status) = child.try_wait()? {
                let tail = read_private_tail(&log_path, 8);
                bail!(
                    "call session exited before becoming ready ({status}){}",
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(":\n{tail}")
                    }
                );
            }
            if Instant::now() >= deadline {
                bail!("call session readiness deadline reached");
            }
            if let Ok(Some(meta)) = read_metadata(&session_dir) {
                if meta.pid == child.id() {
                    if let Ok(status) = ipc_call_dir(
                        &session_dir,
                        json!({ "op": "status" }),
                        Duration::from_millis(800),
                    )
                    .await
                    {
                        if let Some(err) = status
                            .pointer("/session/readyError")
                            .and_then(Value::as_str)
                        {
                            bail!("call session readiness rejected: {err}");
                        }
                        if !wait_for_ready
                            || status.pointer("/session/ready").and_then(Value::as_bool)
                                == Some(true)
                        {
                            return Ok(status);
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(75)).await;
        }
    }
    .await;

    drop(ephemeral_token_guard);
    drop(start_lock);

    match startup {
        Ok(status) => Output::new(json).print(&status),
        Err(error) => {
            if child.try_wait()?.is_none() {
                #[cfg(unix)]
                unsafe {
                    libc::kill(child.id() as i32, libc::SIGTERM);
                }
                #[cfg(windows)]
                {
                    let _ = child.kill();
                }
                for _ in 0..30 {
                    if child.try_wait()?.is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(error)
        }
    }
}

async fn list_sessions(root: &Path) -> Result<Vec<Value>> {
    let mut items = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(items);
    };
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for dir in dirs {
        let Ok(Some(meta)) = read_metadata(&dir) else {
            continue;
        };
        let alive = session_process_alive(&meta);
        if alive {
            if let Ok(status) =
                ipc_call_dir(&dir, json!({ "op": "status" }), Duration::from_millis(500)).await
            {
                items.push(json!({
                    "name": meta.name,
                    "state": "running",
                    "pid": meta.pid,
                    "dir": dir,
                    "account": meta.account,
                    "endpoint": meta.endpoint,
                    "ready": status["session"]["ready"],
                    "callState": status["call"]["state"],
                    "callId": status["call"]["callId"],
                }));
                continue;
            }
        }
        items.push(json!({
            "name": meta.name,
            "state": if alive { "unresponsive" } else { "stopped" },
            "pid": meta.pid,
            "dir": dir,
            "account": meta.account,
            "endpoint": meta.endpoint,
        }));
    }
    Ok(items)
}

async fn stop_session(session_dir: &Path, hangup: bool, wait_timeout: Duration) -> Result<Value> {
    let Some(meta) = read_metadata(session_dir)? else {
        return Ok(json!({ "state": "stopped", "dir": session_dir, "wasRunning": false }));
    };
    if !session_process_alive(&meta) {
        let _ = fs::remove_file(session_dir.join("session.json"));
        if let Some(sock) = &meta.ipc_socket {
            let _ = fs::remove_file(sock);
        }
        let _ = fs::remove_file(session_dir.join("control.sock"));
        let _ = fs::remove_file(session_dir.join("s.sock"));
        return Ok(json!({
            "name": meta.name,
            "state": "stopped",
            "dir": session_dir,
            "wasRunning": false,
        }));
    }
    let reply = ipc_call_dir(
        session_dir,
        json!({ "op": "stop", "hangup": hangup }),
        wait_timeout.min(Duration::from_secs(15)),
    )
    .await
    .ok();
    let deadline = Instant::now() + wait_timeout;
    let mut signaled = false;
    while session_process_alive(&meta) {
        if Instant::now() >= deadline {
            bail!(
                "call session {:?} (pid {}) did not exit within {}s",
                meta.name,
                meta.pid,
                wait_timeout.as_secs()
            );
        }
        if reply.is_none() && !signaled {
            signaled = true;
            #[cfg(unix)]
            unsafe {
                libc::kill(meta.pid as i32, libc::SIGTERM);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(json!({
        "name": meta.name,
        "pid": meta.pid,
        "dir": session_dir,
        "state": "stopped",
        "wasRunning": true,
        "recording": reply.as_ref().and_then(|v| v.get("recording")).cloned(),
    }))
}

// ---------------------------------------------------------------------------
// Local IPC client helpers
// ---------------------------------------------------------------------------

async fn ipc_call(target: &SessionTargetArgs, request: Value, max_wait: Duration) -> Result<Value> {
    let dir = resolve_session_dir(&target.session, target.dir.as_deref())?;
    ipc_call_dir(&dir, request, max_wait).await
}

async fn ipc_call_dir(session_dir: &Path, mut request: Value, max_wait: Duration) -> Result<Value> {
    let meta = read_metadata(session_dir)?.with_context(|| {
        format!(
            "call session not running (missing {})",
            session_dir.join("session.json").display()
        )
    })?;
    if let Some(secret) = meta.ipc_secret.as_deref() {
        request["secret"] = Value::String(secret.to_owned());
    }
    let line = format!("{}\n", serde_json::to_string(&request)?);
    let response_line = timeout(max_wait, ipc_roundtrip(session_dir, &meta, &line))
        .await
        .context("call session IPC request timed out")??;
    let envelope: Value =
        serde_json::from_str(&response_line).context("invalid call session IPC reply")?;
    if envelope["ok"].as_bool() == Some(true) {
        Ok(envelope["result"].clone())
    } else {
        let code = envelope["code"].as_str().unwrap_or("session_error");
        let message = envelope["error"]
            .as_str()
            .unwrap_or("call session request failed");
        let exit = match code {
            "wait_timeout" => 3,
            "invalid_arguments" => 2,
            _ => 9,
        };
        Err(failure(exit, code, message))
    }
}

async fn ipc_follow_events(session_dir: &Path, after: u64) -> Result<()> {
    let meta = read_metadata(session_dir)?.with_context(|| {
        format!(
            "call session not running (missing {})",
            session_dir.join("session.json").display()
        )
    })?;
    let mut request = json!({ "op": "events.follow", "after": after });
    if let Some(secret) = meta.ipc_secret.as_deref() {
        request["secret"] = Value::String(secret.to_owned());
    }
    let payload = format!("{}\n", serde_json::to_string(&request)?);
    #[cfg(unix)]
    {
        let sock = meta
            .ipc_socket
            .clone()
            .unwrap_or_else(|| session_dir.join("control.sock"));
        let stream = tokio::net::UnixStream::connect(&sock)
            .await
            .with_context(|| format!("cannot connect to call session {}", sock.display()))?;
        let (reader, mut writer) = stream.into_split();
        writer.write_all(payload.as_bytes()).await?;
        writer.flush().await?;
        let mut lines = BufReader::new(reader).lines();
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(()),
                line = lines.next_line() => {
                    let Some(line) = line? else { return Ok(()); };
                    println!("{line}");
                }
            }
        }
    }
    #[cfg(windows)]
    {
        let port = meta.ipc_port.context("missing session IPC port")?;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let (reader, mut writer) = stream.into_split();
        writer.write_all(payload.as_bytes()).await?;
        writer.flush().await?;
        let mut lines = BufReader::new(reader).lines();
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(()),
                line = lines.next_line() => {
                    let Some(line) = line? else { return Ok(()); };
                    println!("{line}");
                }
            }
        }
    }
}

async fn ipc_roundtrip(
    session_dir: &Path,
    meta: &SessionMetadata,
    payload: &str,
) -> Result<String> {
    #[cfg(unix)]
    {
        let sock = meta
            .ipc_socket
            .clone()
            .unwrap_or_else(|| session_dir.join("control.sock"));
        let stream = tokio::net::UnixStream::connect(&sock)
            .await
            .with_context(|| format!("cannot connect to call session {}", sock.display()))?;
        let (reader, mut writer) = stream.into_split();
        writer.write_all(payload.as_bytes()).await?;
        writer.flush().await?;
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await?
            .context("call session closed IPC connection without a reply")
    }
    #[cfg(windows)]
    {
        let _ = session_dir;
        let port = meta.ipc_port.context("missing session IPC port")?;
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        let (reader, mut writer) = stream.into_split();
        writer.write_all(payload.as_bytes()).await?;
        writer.flush().await?;
        let mut lines = BufReader::new(reader).lines();
        lines
            .next_line()
            .await?
            .context("call session closed IPC connection without a reply")
    }
}

// ---------------------------------------------------------------------------
// Foreground session engine & event bus
// ---------------------------------------------------------------------------

struct EventBusState {
    seq: u64,
    ring: VecDeque<Value>,
    log_file: Option<BufWriter<File>>,
}

struct SharedEventBus {
    inner: Mutex<EventBusState>,
    tx: broadcast::Sender<Value>,
    media_watch: watch::Sender<Option<(String, MediaInspectionReport)>>,
    stdio_tx: Option<mpsc::UnboundedSender<String>>,
}

impl SharedEventBus {
    fn new(
        log_path: Option<&Path>,
        stdio_tx: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<Arc<Self>> {
        let log_file = match log_path {
            Some(path) => {
                let file = wechat_platform::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .mode(0o600)
                    .custom_flags(wechat_platform::NOFOLLOW)
                    .open(path)?;
                Some(BufWriter::new(file))
            }
            None => None,
        };
        let (tx, _) = broadcast::channel(512);
        let (media_watch, _) = watch::channel(None);
        Ok(Arc::new(Self {
            inner: Mutex::new(EventBusState {
                seq: 0,
                ring: VecDeque::with_capacity(256),
                log_file,
            }),
            tx,
            media_watch,
            stdio_tx,
        }))
    }

    fn current_seq(&self) -> u64 {
        self.inner.lock().map(|g| g.seq).unwrap_or(0)
    }

    fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.tx.subscribe()
    }

    fn subscribe_media(&self) -> watch::Receiver<Option<(String, MediaInspectionReport)>> {
        self.media_watch.subscribe()
    }

    fn publish_media(&self, report: Option<(String, MediaInspectionReport)>) {
        let _ = self.media_watch.send(report);
    }

    fn events_after(&self, after: u64, limit: usize) -> Vec<Value> {
        let Ok(guard) = self.inner.lock() else {
            return Vec::new();
        };
        guard
            .ring
            .iter()
            .filter(|ev| ev["seq"].as_u64().is_some_and(|s| s > after))
            .take(limit)
            .cloned()
            .collect()
    }

    fn emit(&self, mut payload: Value) -> u64 {
        let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (seq, serialized) = {
            let mut guard = self.inner.lock().expect("event bus lock");
            guard.seq += 1;
            let seq = guard.seq;
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("seq".into(), json!(seq));
                obj.insert("ts".into(), json!(ts));
                obj.remove("ownerToken");
            }
            let line = serde_json::to_string(&payload).unwrap_or_default();
            if let Some(file) = guard.log_file.as_mut() {
                let _ = writeln!(file, "{line}");
                let _ = file.flush();
            }
            if guard.ring.len() >= EVENT_BUFFER_CAP {
                guard.ring.pop_front();
            }
            guard.ring.push_back(payload.clone());
            (seq, line)
        };
        let _ = self.tx.send(payload);
        if let Some(tx) = &self.stdio_tx {
            let _ = tx.send(serialized);
        }
        seq
    }

    fn emit_call_event(&self, event: &CallEvent) -> u64 {
        let raw = event_json(event).unwrap_or_else(|_| "{}".into());
        let value: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
        self.emit(value)
    }
}

struct EngineCommand {
    request: Value,
    reply: oneshot::Sender<Result<Value>>,
    flushed: Option<oneshot::Receiver<()>>,
}

struct ActiveSessionCall {
    call_id: String,
    owner_token: String,
    is_conference: bool,
    video_allowed: bool,
    muted: bool,
    camera_enabled: bool,
    media: Option<MediaClient>,
    video_capture: Option<LocalVideoCapture>,
    media_closed_at: Option<Instant>,
    media_closed_err: Option<String>,
    media_attach_failed: bool,
    inspector: CallMediaInspector,
    last_snapshot_at: Instant,
    stopping: bool,
    pending_hangup: bool,
    hangup_started_at: Option<Instant>,
    end_waiters: Vec<oneshot::Sender<Result<Value>>>,
}

enum PendingKind {
    Ready {
        scope: Option<CallReadyScope>,
        initial: bool,
        for_waiting_offer: bool,
        followup: Option<Box<StagedAction>>,
    },
    Dial {
        is_conference: bool,
        video_allowed: bool,
        muted: bool,
        camera_enabled: bool,
    },
    Answer {
        video_allowed: bool,
        muted: bool,
        camera_enabled: bool,
    },
    Reject,
    Hangup,
    Mute {
        old_muted: bool,
        new_muted: bool,
    },
    Camera {
        old_enabled: bool,
        new_enabled: bool,
    },
    Invite,
    ClearAudio,
}

enum StagedAction {
    Dial {
        contact: String,
        call_type: CallType,
        muted: bool,
        camera_enabled: bool,
    },
    ConferenceStart {
        group: String,
        invitees: Vec<String>,
        call_type: CallType,
        muted: bool,
        camera_enabled: bool,
    },
    ConferenceJoin {
        invitation: CallConferenceInvitation,
        muted: bool,
        camera_enabled: bool,
    },
    Answer {
        call_id: Option<String>,
        muted: bool,
        camera_off: bool,
    },
}

struct PendingRequest {
    kind: PendingKind,
    event_seq_before: u64,
    reply: Option<oneshot::Sender<Result<Value>>>,
    sent_at: Instant,
}

struct WaitingPendingOffer {
    call_id: String,
    muted: bool,
    camera_off: bool,
    event_seq_before: u64,
    reply: oneshot::Sender<Result<Value>>,
    deadline: Instant,
}

fn parse_receive_codec(raw: &str) -> Result<CallVideoCodec> {
    match raw {
        "jpeg" => Ok(CallVideoCodec::Jpeg),
        "h265" => Ok(CallVideoCodec::H265),
        "h264" => Ok(CallVideoCodec::H264),
        other => bail!("unsupported receive video codec {other:?}"),
    }
}

fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn ipc_ok(id: Option<&Value>, result: Value) -> Value {
    let mut obj = json!({ "reply": true, "ok": true, "result": result });
    if let Some(id) = id {
        obj["id"] = id.clone();
    }
    obj
}

fn ipc_err(id: Option<&Value>, code: &str, message: impl Into<String>) -> Value {
    let mut obj = json!({
        "reply": true,
        "ok": false,
        "code": code,
        "error": message.into(),
    });
    if let Some(id) = id {
        obj["id"] = id.clone();
    }
    obj
}

async fn run_foreground(
    remote: &Remote,
    options: SessionRunOptions,
    stdio: bool,
    no_ipc: bool,
    parent: Option<ParentWatch>,
) -> Result<()> {
    let audio_source = AudioSourceSpec::parse(&options.audio)?;
    let audio_sink = AudioSinkSpec::parse(&options.output)?;
    let video_source = CameraSource::parse(&options.video)?;
    let receive_codec = parse_receive_codec(&options.receive_video_codec)?;

    let session_dir = resolve_session_dir(&options.name, options.dir.as_deref())?;
    ensure_private_session_dir(&session_dir)?;
    let lock_file = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(session_dir.join("session.lock"))?;
    if lock_file.try_lock_exclusive().is_err() {
        bail!(
            "call session {:?} already holds lock in {}",
            options.name,
            session_dir.display()
        );
    }

    let (mut control, hello) = Control::connect(remote).await?;
    // Remove any ephemeral bearer token file written by `start_detached` now that we connected.
    let _ = fs::remove_file(session_dir.join("auth.token"));
    if control.format != CallAudioFormat::default() {
        control.close().await;
        bail!("unsupported negotiated call audio format");
    }
    let audio_format = AudioFormat::new(
        control.format.sample_rate,
        control.format.channels,
        control.format.frame_ms,
    )?;
    let mut audio_io = CallAudioIo::open(audio_source, audio_sink, audio_format).await?;

    let (stdio_tx, mut stdio_writer_task) = if stdio {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            let mut out = tokio::io::stdout();
            while let Some(line) = rx.recv().await {
                if out.write_all(line.as_bytes()).await.is_err()
                    || out.write_all(b"\n").await.is_err()
                    || out.flush().await.is_err()
                {
                    break;
                }
            }
        });
        (Some(tx), Some(writer))
    } else {
        (None, None)
    };
    let bus = SharedEventBus::new(Some(&session_dir.join("events.jsonl")), stdio_tx.clone())?;

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<EngineCommand>(128);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    // Prepare local IPC listener unless disabled.
    #[allow(unused_mut)]
    let mut ipc_secret: Option<String> = None;
    #[allow(unused_mut)]
    let mut ipc_socket_path: Option<PathBuf> = None;
    #[allow(unused_mut)]
    let mut ipc_port: Option<u16> = None;
    let mut ipc_task: Option<tokio::task::JoinHandle<()>> = None;

    if !no_ipc {
        let ipc_permits = Arc::new(Semaphore::new(64));
        #[cfg(unix)]
        {
            let sock_path = allocate_ipc_socket_path(&session_dir)?;
            let _ = fs::remove_file(&sock_path);
            let listener = tokio::net::UnixListener::bind(&sock_path).with_context(|| {
                format!("failed to bind session IPC socket {}", sock_path.display())
            })?;
            wechat_platform::protect_path(&sock_path, false)?;
            ipc_socket_path = Some(sock_path);
            let bus_c = bus.clone();
            let cmd_tx_c = cmd_tx.clone();
            let mut stop_c = shutdown_rx.clone();
            ipc_task = Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop_c.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { continue; };
                            let Ok(permit) = ipc_permits.clone().try_acquire_owned() else {
                                drop(stream);
                                continue;
                            };
                            let bus_h = bus_c.clone();
                            let cmd_h = cmd_tx_c.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                let (r, w) = stream.into_split();
                                handle_ipc_stream(r, w, bus_h, cmd_h, None).await;
                            });
                        }
                    }
                }
            }));
        }
        #[cfg(windows)]
        {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
            let port = listener.local_addr()?.port();
            let secret = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            ipc_port = Some(port);
            ipc_secret = Some(secret.clone());
            let bus_c = bus.clone();
            let cmd_tx_c = cmd_tx.clone();
            let mut stop_c = shutdown_rx.clone();
            ipc_task = Some(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop_c.changed() => break,
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { continue; };
                            let Ok(permit) = ipc_permits.clone().try_acquire_owned() else {
                                drop(stream);
                                continue;
                            };
                            let bus_h = bus_c.clone();
                            let cmd_h = cmd_tx_c.clone();
                            let sec = Some(secret.clone());
                            tokio::spawn(async move {
                                let _permit = permit;
                                let (r, w) = stream.into_split();
                                handle_ipc_stream(r, w, bus_h, cmd_h, sec).await;
                            });
                        }
                    }
                }
            }));
        }
    }

    let pid = std::process::id();
    let (start_ticks, executable) = wechat_platform::process_identity(pid)
        .map(|(t, e)| (Some(t), Some(e)))
        .unwrap_or((None, None));
    let account = remote.account()?.to_owned();
    let endpoint_str = remote.base.to_string();
    let metadata = SessionMetadata {
        name: options.name.clone(),
        pid,
        start_ticks,
        executable,
        account: account.clone(),
        endpoint: endpoint_str.clone(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ipc_socket: ipc_socket_path.clone(),
        ipc_port,
        ipc_secret,
    };
    write_private_file(
        &session_dir.join("session.json"),
        &serde_json::to_vec_pretty(&metadata)?,
    )?;

    bus.emit(json!({
        "type": "session.started",
        "name": options.name,
        "pid": pid,
        "dir": session_dir,
        "account": account,
        "endpoint": endpoint_str,
        "clientId": control.client_id,
        "audioSource": audio_io.input_spec().label(),
        "audioSink": audio_io.output_spec().label(),
        "videoSource": video_source.label(),
        "receiveVideoCodec": options.receive_video_codec,
    }));
    bus.emit_call_event(&hello);

    // Spawn stdin NDJSON command reader when `--stdio` is enabled.
    let mut stdio_task = if stdio {
        let cmd_tx_c = cmd_tx.clone();
        let bus_c = bus.clone();
        let out_tx = stdio_tx.clone().expect("stdio stdout tx");
        let shutdown_c = shutdown_tx.clone();
        Some(tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: Result<Value, _> = serde_json::from_str(trimmed);
                let mut stop_flushed_tx: Option<oneshot::Sender<()>> = None;
                let reply_json = match parsed {
                    Ok(req) => {
                        let req_id = req.get("id").cloned();
                        let op = req.get("op").and_then(Value::as_str).unwrap_or("");
                        let res = if op == "wait" {
                            run_wait_request(&bus_c, &req).await
                        } else if op == "events" {
                            let after = req.get("after").and_then(Value::as_u64).unwrap_or(0);
                            let limit = req
                                .get("limit")
                                .and_then(Value::as_u64)
                                .unwrap_or(200)
                                .min(4096) as usize;
                            Ok(json!({
                                "eventSeq": bus_c.current_seq(),
                                "events": bus_c.events_after(after, limit),
                            }))
                        } else {
                            let (tx, rx) = oneshot::channel();
                            let flushed = if op == "stop" {
                                let (ftx, frx) = oneshot::channel();
                                stop_flushed_tx = Some(ftx);
                                Some(frx)
                            } else {
                                None
                            };
                            if cmd_tx_c
                                .send(EngineCommand {
                                    request: req,
                                    reply: tx,
                                    flushed,
                                })
                                .await
                                .is_err()
                            {
                                Err(anyhow!("session stopped"))
                            } else {
                                match timeout(Duration::from_secs(45), rx).await {
                                    Ok(Ok(res)) => res,
                                    Ok(Err(_)) => Err(anyhow!("session stopped")),
                                    Err(_) => {
                                        Err(anyhow!("command timed out waiting for session reply"))
                                    }
                                }
                            }
                        };
                        match res {
                            Ok(val) => ipc_ok(req_id.as_ref(), val),
                            Err(err) => {
                                let (_, code) = crate::outcome::classify(&err);
                                ipc_err(req_id.as_ref(), &code, err.to_string())
                            }
                        }
                    }
                    Err(err) => ipc_err(None, "invalid_arguments", format!("invalid JSON: {err}")),
                };
                let line = serde_json::to_string(&reply_json).unwrap_or_default();
                let _ = out_tx.send(line);
                if let Some(ftx) = stop_flushed_tx {
                    let _ = ftx.send(());
                    return;
                }
            }
            // Stdin closed -> initiate clean shutdown.
            let _ = shutdown_c.send(true);
        }))
    } else {
        None
    };

    // Main session state.
    let mut desired_ready = options.ready;
    let mut ready_scope: Option<CallReadyScope> = None;
    let mut startup_ready_error: Option<String> = None;
    let mut current_video_source = video_source;
    let mut active_call: Option<ActiveSessionCall> = None;
    let mut pending: HashMap<String, PendingRequest> = HashMap::new();
    let mut waiting_offer: Option<WaitingPendingOffer> = None;
    let mut last_media_report: Option<MediaInspectionReport> = None;
    let mut last_recording: Option<RecordingReport> = None;
    let mut last_activity = Instant::now();
    let mut last_stats_emitted = Instant::now();
    let mut last_wav_flush = Instant::now();
    let mut stop_reason: Option<String> = None;
    let mut stop_hangup = true;
    let mut stop_reply: Option<oneshot::Sender<Result<Value>>> = None;
    let mut stop_flushed: Option<oneshot::Receiver<()>> = None;

    // If `--ready` was set at startup, advertise readiness right away.
    if options.ready {
        let daemon_video =
            control.capabilities.direct_video || control.capabilities.conference_video;
        let video_caps = tokio::task::spawn_blocking(move || {
            client_video_capabilities_with_receive(daemon_video, None, Some(vec![receive_codec]))
        })
        .await
        .context("video capability probe failed")?;
        let rid = request_id();
        let before = bus.current_seq();
        let format = control.format;
        send_json(
            &mut control.socket,
            &CallCommand::Ready {
                request_id: rid.clone(),
                ready: true,
                format,
                scope: Some(CallReadyScope::All),
                video: video_caps,
            },
        )
        .await?;
        pending.insert(
            rid,
            PendingRequest {
                kind: PendingKind::Ready {
                    scope: Some(CallReadyScope::All),
                    initial: true,
                    for_waiting_offer: false,
                    followup: None,
                },
                event_seq_before: before,
                reply: None,
                sent_at: Instant::now(),
            },
        );
    }

    let mut audio_tick =
        tokio::time::interval(Duration::from_millis(u64::from(control.format.frame_ms)));
    audio_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut housekeep_tick = tokio::time::interval(Duration::from_millis(100));
    housekeep_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    let loop_result: Result<()> = loop {
        // Check if active call reached Connected and needs /media attachment.
        if let Some(call) = active_call.as_mut() {
            let connected = control
                .call
                .as_ref()
                .is_some_and(|v| v.call_id == call.call_id && v.state == CallState::Connected);
            if connected
                && call.media.is_none()
                && call.media_closed_at.is_none()
                && !call.media_attach_failed
                && !call.stopping
            {
                let ep = endpoint(remote)?;
                let client_id = control.client_id.clone();
                let fmt = control.format;
                match MediaClient::attach_parts(
                    &ep,
                    &client_id,
                    fmt,
                    &call.call_id,
                    &call.owner_token,
                )
                .await
                .map_err(map_client_error)
                {
                    Ok(mut attached) => {
                        if let Some(view) =
                            control.call.as_ref().filter(|v| v.call_id == call.call_id)
                        {
                            let _ = attached.receive.roster(view);
                            attached.receive_video.roster(view);
                            call.inspector.apply_roster(view);
                        }
                        call.inspector.sync_send_tracks(
                            &attached.send_track,
                            attached.video_send_track.as_ref(),
                        );
                        for track in control.tracks.values() {
                            if track.direction() == CallTrackDirection::Receive {
                                call.inspector.upsert_receive_track(track);
                            }
                        }
                        for rvt in attached.receive_video.tracks().values() {
                            call.inspector.upsert_receive_track(&rvt.track);
                        }
                        audio_io.discard_capture();
                        let send_tracks_json: Vec<Value> = std::iter::once(&attached.send_track)
                            .chain(attached.video_send_track.iter())
                            .filter_map(|t| serde_json::to_value(t).ok())
                            .collect();
                        let rx_video_tracks: Vec<Value> = attached
                            .receive_video
                            .tracks()
                            .values()
                            .filter_map(|t| serde_json::to_value(&t.track).ok())
                            .collect();
                        call.media = Some(attached);
                        bus.emit(json!({
                            "type": "media.attached",
                            "callId": call.call_id,
                            "sendTracks": send_tracks_json,
                            "receiveVideoTracks": rx_video_tracks,
                        }));
                        // Start local video capture if camera is enabled.
                        if call.camera_enabled && call.video_allowed {
                            if let Err(err) = start_session_video_capture(
                                call,
                                &current_video_source,
                                &mut control,
                                &bus,
                            )
                            .await
                            {
                                bus.emit(json!({
                                    "type": "session.error",
                                    "message": format!("camera startup failed: {err}"),
                                }));
                            }
                        }
                    }
                    Err(err) => {
                        bus.emit(json!({
                            "type": "session.error",
                            "message": format!("media attachment failed: {err}"),
                        }));
                        call.media_closed_at = Some(Instant::now());
                        call.media_closed_err = Some(err.to_string());
                    }
                }
            }
        }

        // Sync tracks if media is attached.
        if let Some(call) = active_call.as_mut() {
            if let Some(media) = call.media.as_mut() {
                if let Ok((audio_changed, video_changed)) =
                    media.synchronize_tracks(&control, &call.call_id)
                {
                    if audio_changed {
                        audio_io.discard_capture();
                    }
                    call.inspector
                        .sync_send_tracks(&media.send_track, media.video_send_track.as_ref());
                    if video_changed && call.camera_enabled {
                        if let Some(cap) = call.video_capture.as_mut() {
                            let _ = cap.request_keyframe();
                        }
                    }
                }
            }
        }

        // Handle waiting pending offer promotion if `control.offer` is now available.
        if let Some(staged) = waiting_offer.take() {
            if let Some(offer) = control
                .offer
                .as_ref()
                .filter(|o| o.call_id == staged.call_id)
                .cloned()
            {
                let is_video =
                    offer.call_type == CallType::Video && control.capabilities.direct_video;
                let camera_enabled =
                    is_video && !staged.camera_off && !current_video_source.is_off();
                let rid = request_id();
                let cmd = CallCommand::Answer {
                    request_id: rid.clone(),
                    call_id: offer.call_id,
                    expected_revision: offer.revision,
                    camera_enabled: (offer.call_type == CallType::Video).then_some(camera_enabled),
                    muted: staged.muted.then_some(true),
                };
                match send_json(&mut control.socket, &cmd).await {
                    Ok(()) => {
                        pending.insert(
                            rid,
                            PendingRequest {
                                kind: PendingKind::Answer {
                                    video_allowed: is_video,
                                    muted: staged.muted,
                                    camera_enabled,
                                },
                                event_seq_before: staged.event_seq_before,
                                reply: Some(staged.reply),
                                sent_at: Instant::now(),
                            },
                        );
                    }
                    Err(err) => {
                        let _ = staged.reply.send(Err(err));
                    }
                }
            } else if Instant::now() >= staged.deadline
                || control
                    .pending_offer
                    .as_ref()
                    .is_none_or(|o| o.call_id != staged.call_id)
            {
                let _ = staged.reply.send(Err(anyhow!(
                    "offer_expired: the selected call ended or was replaced"
                )));
            } else {
                waiting_offer = Some(staged);
            }
        }

        #[cfg(unix)]
        let term_fut = sigterm.recv();
        #[cfg(windows)]
        let term_fut = std::future::pending::<Option<()>>();

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                stop_reason = Some("sigint".into());
                break Ok(());
            }
            _ = term_fut => {
                stop_reason = Some("sigterm".into());
                break Ok(());
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    stop_reason = Some("stdin_eof".into());
                    break Ok(());
                }
            }
            Some(eng_cmd) = cmd_rx.recv() => {
                last_activity = Instant::now();
                let op = eng_cmd.request.get("op").and_then(Value::as_str).unwrap_or("");
                if op == "stop" {
                    stop_hangup = eng_cmd.request.get("hangup").and_then(Value::as_bool).unwrap_or(true);
                    stop_reason = Some("requested".into());
                    stop_reply = Some(eng_cmd.reply);
                    stop_flushed = eng_cmd.flushed;
                    break Ok(());
                }
                let ctx = CommandContext {
                    options: &options,
                    session_dir: &session_dir,
                    account: &account,
                    endpoint: &endpoint_str,
                    receive_codec,
                    audio_format,
                    bus: &bus,
                    control: &mut control,
                    audio_io: &mut audio_io,
                    current_video_source: &mut current_video_source,
                    desired_ready: &mut desired_ready,
                    ready_scope: &mut ready_scope,
                    startup_ready_error: &startup_ready_error,
                    active_call: &mut active_call,
                    pending: &mut pending,
                    waiting_offer: &mut waiting_offer,
                    last_media_report: &last_media_report,
                    last_recording: &mut last_recording,
                };
                handle_engine_command(ctx, eng_cmd.request, eng_cmd.reply).await;
            }
            event = control.next_event() => {
                let event = match event {
                    Ok(ev) => ev,
                    Err(err) => {
                        bus.emit(json!({
                            "type": "session.error",
                            "message": format!("control socket closed: {err}"),
                        }));
                        break Err(err);
                    }
                };
                control.update(&event);
                bus.emit_call_event(&event);
                handle_control_event(
                    &event,
                    &mut control,
                    &mut active_call,
                    &mut audio_io,
                    &current_video_source,
                    &mut pending,
                    &mut waiting_offer,
                    &mut ready_scope,
                    &mut startup_ready_error,
                    desired_ready,
                    receive_codec,
                    &options,
                    &session_dir,
                    &bus,
                    &mut last_media_report,
                    &mut last_recording,
                )
                .await;
            }
            media_msg = next_active_media(&mut active_call) => {
                let Some(call) = active_call.as_mut() else { continue; };
                match media_msg {
                    Ok(Message::Binary(bytes)) => {
                        match WireMediaFrame::decode(&bytes) {
                            Ok(wire) => {
                                let now = Instant::now();
                                if wire.kind == MediaPayloadKind::PcmS16le {
                                    if let Some(media) = call.media.as_mut() {
                                        let elapsed = media.attached_elapsed();
                                        let inspector = &mut call.inspector;
                                        let _ = media.receive.wire_frame(
                                            &wire,
                                            elapsed,
                                            |track_id, participant_id, samples| {
                                                inspector.observe_receive_audio(
                                                    track_id,
                                                    participant_id,
                                                    samples,
                                                    now,
                                                );
                                            },
                                        );
                                    }
                                } else if let Some(media) = call.media.as_mut() {
                                    let admitted = if wire.kind == MediaPayloadKind::Jpeg {
                                        media.receive_video.binary_frame(&wire)
                                    } else {
                                        media
                                            .receive_video
                                            .tracks()
                                            .get(&wire.header.track_id)
                                            .is_some_and(|t| {
                                                t.track.generation() == wire.header.generation
                                            })
                                    };
                                    if admitted {
                                        call.inspector.observe_receive_video_wire(&wire, now);
                                    }
                                }
                            }
                            Err(err) => {
                                bus.emit(json!({
                                    "type": "session.error",
                                    "message": format!("invalid binary media frame: {err}"),
                                }));
                            }
                        }
                    }
                    Ok(Message::Text(text)) => {
                        match parse_call_event_str(&text) {
                            Ok(Some(event)) => {
                                bus.emit_call_event(&event);
                                match &event {
                                    CallEvent::TrackAdded { track, .. }
                                    | CallEvent::TrackUpdated { track, .. }
                                        if track.direction() == CallTrackDirection::Send =>
                                    {
                                        control.update(&event);
                                    }
                                    CallEvent::TrackAdded { call_id: id, track }
                                    | CallEvent::TrackUpdated { call_id: id, track }
                                        if id == &call.call_id =>
                                    {
                                        call.inspector.upsert_receive_track(track);
                                        if let Some(media) = call.media.as_mut() {
                                            media.receive_video.event(&event, &call.call_id);
                                            if let Some(view) = control
                                                .call
                                                .as_ref()
                                                .filter(|v| v.call_id == call.call_id)
                                            {
                                                media.receive_video.roster(view);
                                                call.inspector.apply_roster(view);
                                            }
                                            let _ = media.receive.event(&event, &call.call_id);
                                        }
                                    }
                                    CallEvent::TrackRemoved {
                                        call_id: id,
                                        track_id,
                                        ..
                                    } if id == &call.call_id => {
                                        call.inspector.mark_receive_track_removed(*track_id);
                                        if let Some(media) = call.media.as_mut() {
                                            media.receive_video.event(&event, &call.call_id);
                                            if media.receive.event(&event, &call.call_id).unwrap_or(false) {
                                                let _ = audio_io.clear_playback();
                                            }
                                        }
                                    }
                                    CallEvent::VideoKeyframeRequested { .. } => {
                                        if let Some(cap) = call.video_capture.as_mut() {
                                            let _ = cap.request_keyframe();
                                        }
                                    }
                                    CallEvent::Error { code } => {
                                        bus.emit(json!({
                                            "type": "session.error",
                                            "message": format!("media error: {}", serde_json::to_value(code).unwrap_or(Value::Null)),
                                        }));
                                    }
                                    _ => {}
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                bus.emit(json!({
                                    "type": "session.error",
                                    "message": format!("media text event error: {err}"),
                                }));
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        call.media = None;
                        call.video_capture.take();
                        call.media_closed_at.get_or_insert_with(Instant::now);
                        call.media_closed_err.get_or_insert_with(|| err.to_string());
                        bus.emit(json!({
                            "type": "media.detached",
                            "callId": call.call_id,
                            "reason": err.to_string(),
                        }));
                    }
                }
            }
            _ = audio_tick.tick() => {
                if let Some(call) = active_call.as_mut() {
                    if let Some(media) = call.media.as_mut() {
                        let now = Instant::now();
                        if let Ok((samples, finished_once)) = audio_io.next_frame(audio_format, call.muted) {
                            if finished_once {
                                bus.emit(json!({
                                    "type": "media.audio_finished",
                                    "callId": call.call_id,
                                    "source": audio_io.input_spec().label(),
                                }));
                            }
                            call.inspector.observe_send_audio(&samples, now);
                            if let Err(err) = media.send_pcm(&samples, audio_format).await {
                                call.media = None;
                                call.video_capture.take();
                                call.media_closed_at.get_or_insert_with(Instant::now);
                                call.media_closed_err.get_or_insert_with(|| err.to_string());
                                bus.emit(json!({
                                    "type": "media.detached",
                                    "callId": call.call_id,
                                    "reason": err.to_string(),
                                }));
                                continue;
                            }
                        }
                        if let Some(media) = call.media.as_mut() {
                            if let Ok(frames) = media.receive.render_due(media.attached_elapsed()) {
                                for mixed in frames {
                                    call.inspector.observe_receive_mixed_audio(&mixed, now);
                                    let _ = audio_io.enqueue_playback(&mixed, audio_format.sample_rate());
                                }
                            }
                            let camera_pending = pending
                                .values()
                                .any(|p| matches!(p.kind, PendingKind::Camera { .. }));
                            if call.camera_enabled && !camera_pending {
                                if let Some(frame) = call.video_capture.as_mut().and_then(|c| c.try_recv()) {
                                    call.inspector.observe_send_video(frame.bytes.len(), frame.flags, now);
                                    if let Err(err) = media.send_video(&frame).await {
                                        call.media = None;
                                        call.video_capture.take();
                                        call.media_closed_at.get_or_insert_with(Instant::now);
                                        call.media_closed_err.get_or_insert_with(|| err.to_string());
                                        bus.emit(json!({
                                            "type": "media.detached",
                                            "callId": call.call_id,
                                            "reason": err.to_string(),
                                        }));
                                        continue;
                                    }
                                }
                            }
                            let decoded_ids = media.receive_video.poll_decoded_ids();
                            for id in decoded_ids {
                                if let Some(frame) = media
                                    .receive_video
                                    .tracks()
                                    .get(&id)
                                    .and_then(|t| t.frame.as_ref())
                                {
                                    call.inspector.observe_receive_video_decoded(id, frame, now);
                                }
                            }
                        }
                    }
                }
            }
            _ = housekeep_tick.tick() => {
                let now = Instant::now();
                if let Some(p) = &parent {
                    if p.exited() {
                        stop_reason = Some("parent_exited".into());
                        break Ok(());
                    }
                }
                // Expire timed-out pending control commands.
                let expired: Vec<String> = pending
                    .iter()
                    .filter(|(_, p)| now.saturating_duration_since(p.sent_at) > COMMAND_TIMEOUT)
                    .map(|(k, _)| k.clone())
                    .collect();
                for key in expired {
                    if let Some(mut req) = pending.remove(&key) {
                        if matches!(req.kind, PendingKind::Hangup) {
                            if let Some(call) = active_call.as_mut() {
                                for waiter in call.end_waiters.drain(..) {
                                    let _ = waiter.send(Err(anyhow!(
                                        "hangup timed out waiting for daemon confirmation"
                                    )));
                                }
                            }
                        } else if matches!(req.kind, PendingKind::Ready { for_waiting_offer: true, .. }) {
                            if let Some(staged) = waiting_offer.take() {
                                let _ = staged.reply.send(Err(anyhow!(
                                    "call readiness timed out while preparing incoming offer"
                                )));
                            }
                        } else if matches!(req.kind, PendingKind::Ready { initial: true, .. }) {
                            startup_ready_error
                                .get_or_insert_with(|| "initial call.ready timed out".into());
                        }
                        if let Some(reply) = req.reply.take() {
                            let _ = reply.send(Err(anyhow!(
                                "call command timed out; check session status before retrying"
                            )));
                        }
                    }
                }
                if let Some(call) = active_call.as_mut() {
                    last_activity = now;
                    if call
                        .hangup_started_at
                        .is_some_and(|at| now.saturating_duration_since(at) > COMMAND_TIMEOUT)
                        && !call.end_waiters.is_empty()
                    {
                        for waiter in call.end_waiters.drain(..) {
                            let _ = waiter.send(Err(anyhow!(
                                "hangup timed out waiting for terminal call.state"
                            )));
                        }
                    }
                    // Update live media watch for concurrent `wait` requests.
                    let report = call.inspector.report(now);
                    bus.publish_media(Some((call.call_id.clone(), report.clone())));
                    if options.stats_interval > 0
                        && call.media.is_some()
                        && now.saturating_duration_since(last_stats_emitted)
                            >= Duration::from_millis(options.stats_interval)
                    {
                        last_stats_emitted = now;
                        bus.emit(json!({
                            "type": "media.stats",
                            "callId": call.call_id,
                            "media": report,
                        }));
                    }
                    if options.snapshot_interval > 0
                        && call.media.is_some()
                        && now.saturating_duration_since(call.last_snapshot_at)
                            >= Duration::from_millis(options.snapshot_interval)
                    {
                        call.last_snapshot_at = now;
                        let snap_dir = session_dir.join("snapshots");
                        if let Ok(snaps) = call.inspector.save_snapshots(None, &snap_dir, false) {
                            for snap in snaps {
                                bus.emit(json!({
                                    "type": "media.snapshot",
                                    "callId": call.call_id,
                                    "snapshot": snap,
                                }));
                            }
                        }
                    }
                    if now.saturating_duration_since(last_wav_flush) >= Duration::from_secs(1) {
                        last_wav_flush = now;
                        call.inspector.flush_recording();
                    }
                    if call
                        .media_closed_at
                        .is_some_and(|at| now.saturating_duration_since(at) >= MEDIA_CLOSED_END_GRACE)
                    {
                        let err = call
                            .media_closed_err
                            .clone()
                            .unwrap_or_else(|| "call media socket closed".into());
                        bus.emit(json!({
                            "type": "session.error",
                            "message": err,
                        }));
                        call.media_closed_at = None;
                        call.media_attach_failed = true;
                    }
                } else if options.idle_timeout > 0
                    && now.saturating_duration_since(last_activity)
                        >= Duration::from_secs(options.idle_timeout)
                {
                    stop_reason = Some("idle_timeout".into());
                    break Ok(());
                }
            }
        }
    };

    // Clean teardown: hang up active owned call if requested, finalize recordings, close sockets.
    let mut final_recording = last_recording.clone();
    if let Some(mut call) = active_call.take() {
        call.video_capture.take();
        if stop_hangup {
            let _ = super::audio::terminate_owned(&mut control, &call.call_id).await;
        }
        if let Some(rec) = stop_inspector_recording(&mut call.inspector).await {
            bus.emit(json!({
                "type": "recording.stopped",
                "callId": call.call_id,
                "recording": rec,
            }));
            final_recording = Some(rec);
        }
        for waiter in call.end_waiters.drain(..) {
            let _ = waiter.send(Ok(json!({
                "callId": call.call_id,
                "hadCall": true,
                "ended": true,
                "eventSeq": bus.current_seq(),
                "recording": final_recording,
            })));
        }
        drop(call.media);
    }

    let _ = audio_io.stop().await;
    control.close().await;
    let reason = stop_reason.unwrap_or_else(|| "finished".into());
    bus.emit(json!({
        "type": "session.stopped",
        "reason": reason,
        "recording": final_recording,
    }));

    if let Some(reply) = stop_reply {
        let _ = reply.send(Ok(json!({
            "state": "stopped",
            "reason": reason,
            "eventSeq": bus.current_seq(),
            "recording": final_recording,
        })));
    }
    if let Some(flushed) = stop_flushed {
        let _ = timeout(Duration::from_secs(2), flushed).await;
    }

    let _ = shutdown_tx.send(true);
    if let Some(task) = ipc_task.take() {
        task.abort();
    }
    if let Some(task) = stdio_task.take() {
        task.abort();
    }
    drop(stdio_tx);
    drop(bus);
    if let Some(writer) = stdio_writer_task.take() {
        let _ = timeout(Duration::from_secs(2), writer).await;
    }
    if let Some(sock) = ipc_socket_path {
        let _ = fs::remove_file(sock);
    }
    let _ = fs::remove_file(session_dir.join("session.json"));
    let _ = fs::remove_file(session_dir.join("auth.token"));
    drop(lock_file);

    loop_result
}

async fn stop_inspector_recording(inspector: &mut CallMediaInspector) -> Option<RecordingReport> {
    let recorder = inspector.take_recorder()?;
    let report = tokio::task::spawn_blocking(move || recorder.finish())
        .await
        .ok()?
        .ok()?;
    inspector.record_finished_report(report.clone());
    Some(report)
}

async fn next_active_media(active: &mut Option<ActiveSessionCall>) -> Result<Message> {
    match active.as_mut().and_then(|c| c.media.as_mut()) {
        Some(media) => media.next_message().await,
        None => std::future::pending().await,
    }
}

async fn start_session_video_capture(
    call: &mut ActiveSessionCall,
    source: &CameraSource,
    control: &mut Control,
    bus: &SharedEventBus,
) -> Result<()> {
    if !call.camera_enabled || !call.video_allowed || call.video_capture.is_some() {
        return Ok(());
    }
    let Some(media) = call.media.as_ref() else {
        return Ok(());
    };
    let Some(format) = media
        .video_send_track
        .as_ref()
        .and_then(CallTrack::video_format)
    else {
        call.camera_enabled = false;
        let _ = send_json(
            &mut control.socket,
            &CallCommand::Camera {
                request_id: request_id(),
                call_id: call.call_id.clone(),
                enabled: false,
            },
        )
        .await;
        bail!("outgoing video track unavailable");
    };
    let src = source.clone();
    let started = tokio::task::spawn_blocking(move || LocalVideoCapture::start(&src, format))
        .await
        .map_err(|err| anyhow!("camera worker panicked: {err}"))??;
    call.video_capture = Some(started);
    bus.emit(json!({
        "type": "media.video_capture_started",
        "callId": call.call_id,
        "source": source.label(),
        "codec": format.codec,
    }));
    Ok(())
}

// ---------------------------------------------------------------------------
// Command dispatch inside the foreground session actor
// ---------------------------------------------------------------------------

struct CommandContext<'a> {
    options: &'a SessionRunOptions,
    session_dir: &'a Path,
    account: &'a str,
    endpoint: &'a str,
    receive_codec: CallVideoCodec,
    audio_format: AudioFormat,
    bus: &'a Arc<SharedEventBus>,
    control: &'a mut Control,
    audio_io: &'a mut CallAudioIo,
    current_video_source: &'a mut CameraSource,
    desired_ready: &'a mut bool,
    ready_scope: &'a mut Option<CallReadyScope>,
    startup_ready_error: &'a Option<String>,
    active_call: &'a mut Option<ActiveSessionCall>,
    pending: &'a mut HashMap<String, PendingRequest>,
    waiting_offer: &'a mut Option<WaitingPendingOffer>,
    last_media_report: &'a Option<MediaInspectionReport>,
    last_recording: &'a mut Option<RecordingReport>,
}

async fn handle_engine_command(
    ctx: CommandContext<'_>,
    request: Value,
    reply: oneshot::Sender<Result<Value>>,
) {
    let op = request.get("op").and_then(Value::as_str).unwrap_or("");
    let before_seq = ctx.bus.current_seq();
    match op {
        "status" => {
            let now = Instant::now();
            let media_report = ctx
                .active_call
                .as_ref()
                .map(|c| c.inspector.report(now))
                .or_else(|| ctx.last_media_report.clone());
            let local = match ctx.active_call.as_ref() {
                Some(call) => json!({
                    "owned": true,
                    "callId": call.call_id,
                    "isConference": call.is_conference,
                    "videoAllowed": call.video_allowed,
                    "muted": call.muted,
                    "cameraEnabled": call.camera_enabled,
                    "mediaAttached": call.media.is_some(),
                }),
                None => json!({
                    "owned": false,
                    "callId": Value::Null,
                    "muted": false,
                    "cameraEnabled": false,
                    "mediaAttached": false,
                }),
            };
            let _ = reply.send(Ok(json!({
                "session": {
                    "name": ctx.options.name,
                    "pid": std::process::id(),
                    "dir": ctx.session_dir,
                    "account": ctx.account,
                    "endpoint": ctx.endpoint,
                    "clientId": ctx.control.client_id,
                    "ready": matches!(ctx.ready_scope, Some(CallReadyScope::All)),
                    "readyError": ctx.startup_ready_error,
                    "eventSeq": before_seq,
                    "audioSource": ctx.audio_io.input_spec().label(),
                    "audioSink": ctx.audio_io.output_spec().label(),
                    "videoSource": ctx.current_video_source.label(),
                    "receiveVideoCodec": ctx.options.receive_video_codec,
                },
                "capabilities": ctx.control.capabilities,
                "availability": ctx.control.availability,
                "offer": ctx.control.offer,
                "pendingOffer": ctx.control.pending_offer,
                "conferenceInvitations": ctx.control.conference_invitations,
                "call": ctx.control.call,
                "peerStatus": ctx.control.peer_status,
                "local": local,
                "media": media_report,
                "lastRecording": ctx.last_recording,
            })));
        }
        "ready" => {
            let enable = request
                .get("ready")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            *ctx.desired_ready = enable;
            if let Err(err) = submit_ready(
                ctx.control,
                ctx.pending,
                enable,
                CallReadyScope::All,
                ctx.receive_codec,
                false,
                false,
                None,
                before_seq,
                Some(reply),
            )
            .await
            {
                let _ = ctx
                    .bus
                    .emit(json!({"type": "session.error", "message": err.to_string()}));
            }
        }
        "dial" => {
            let contact = match request
                .get("contact")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(c) => c.to_owned(),
                None => {
                    let _ = reply.send(Err(anyhow!("contact is required")));
                    return;
                }
            };
            let is_video = request
                .get("video")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let muted = request
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let camera_off = request
                .get("cameraOff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !ctx.control.capabilities.direct_audio {
                let _ = reply.send(Err(anyhow!("direct calling unavailable on this daemon")));
                return;
            }
            let call_type = if is_video {
                if !ctx.control.capabilities.direct_video {
                    let _ = reply.send(Err(anyhow!(
                        "direct video calling unavailable on this daemon"
                    )));
                    return;
                }
                CallType::Video
            } else {
                CallType::Voice
            };
            if ctx.control.availability.busy || ctx.active_call.is_some() {
                let _ = reply.send(Err(anyhow!(
                    "busy: the daemon already has an ongoing or unresolved call"
                )));
                return;
            }
            if let Err(err) = apply_media_overrides(
                &request,
                ctx.audio_io,
                ctx.current_video_source,
                ctx.audio_format,
            )
            .await
            {
                let _ = reply.send(Err(err));
                return;
            }
            let camera_enabled =
                call_type == CallType::Video && !camera_off && !ctx.current_video_source.is_off();
            let staged = StagedAction::Dial {
                contact,
                call_type,
                muted,
                camera_enabled,
            };
            if let Err(err) = submit_ready_or_action(
                ctx.control,
                ctx.pending,
                ctx.ready_scope.as_ref(),
                *ctx.desired_ready,
                CallReadyScope::Outgoing,
                ctx.receive_codec,
                staged,
                before_seq,
                reply,
            )
            .await
            {
                ctx.bus
                    .emit(json!({"type": "session.error", "message": err.to_string()}));
            }
        }
        "answer" => {
            let call_id = request
                .get("callId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let muted = request
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let camera_off = request
                .get("cameraOff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(offer) = ctx
                .control
                .offer
                .as_ref()
                .filter(|o| call_id.as_deref().is_none_or(|id| id == o.call_id))
                .cloned()
            {
                if let Err(err) = apply_media_overrides(
                    &request,
                    ctx.audio_io,
                    ctx.current_video_source,
                    ctx.audio_format,
                )
                .await
                {
                    let _ = reply.send(Err(err));
                    return;
                }
                let is_video =
                    offer.call_type == CallType::Video && ctx.control.capabilities.direct_video;
                let camera_enabled = is_video && !camera_off && !ctx.current_video_source.is_off();
                let staged = StagedAction::Answer {
                    call_id: Some(offer.call_id.clone()),
                    muted,
                    camera_off: !camera_enabled,
                };
                if let Err(err) = submit_ready_or_action(
                    ctx.control,
                    ctx.pending,
                    ctx.ready_scope.as_ref(),
                    *ctx.desired_ready,
                    CallReadyScope::Offer {
                        call_id: offer.call_id,
                        expected_revision: offer.revision,
                    },
                    ctx.receive_codec,
                    staged,
                    before_seq,
                    reply,
                )
                .await
                {
                    ctx.bus
                        .emit(json!({"type": "session.error", "message": err.to_string()}));
                }
            } else if let Some(pending_offer) = ctx
                .control
                .pending_offer
                .as_ref()
                .filter(|o| call_id.as_deref().is_none_or(|id| id == o.call_id))
                .cloned()
            {
                if let Err(err) = apply_media_overrides(
                    &request,
                    ctx.audio_io,
                    ctx.current_video_source,
                    ctx.audio_format,
                )
                .await
                {
                    let _ = reply.send(Err(err));
                    return;
                }
                // Promote the pending offer via Ready(Offer) and answer once CallEvent::Offered arrives.
                let scope = CallReadyScope::Offer {
                    call_id: pending_offer.call_id.clone(),
                    expected_revision: pending_offer.revision,
                };
                *ctx.waiting_offer = Some(WaitingPendingOffer {
                    call_id: pending_offer.call_id,
                    muted,
                    camera_off,
                    event_seq_before: before_seq,
                    reply,
                    deadline: Instant::now() + Duration::from_secs(30),
                });
                if let Err(err) = submit_ready(
                    ctx.control,
                    ctx.pending,
                    true,
                    scope,
                    ctx.receive_codec,
                    false,
                    true,
                    None,
                    before_seq,
                    None,
                )
                .await
                {
                    if let Some(staged) = ctx.waiting_offer.take() {
                        let _ = staged.reply.send(Err(err));
                    }
                }
            } else {
                let _ = reply.send(Err(anyhow!(
                    "offer_expired: no matching current incoming call offer"
                )));
            }
        }
        "reject" => {
            let call_id = request.get("callId").and_then(Value::as_str);
            let Some(offer) = ctx
                .control
                .offer
                .as_ref()
                .filter(|o| call_id.is_none_or(|id| id == o.call_id))
                .cloned()
            else {
                let _ = reply.send(Err(anyhow!(
                    "offer_expired: no matching current incoming call offer to reject"
                )));
                return;
            };
            let rid = request_id();
            match send_json(
                &mut ctx.control.socket,
                &CallCommand::Reject {
                    request_id: rid.clone(),
                    call_id: offer.call_id,
                    expected_revision: offer.revision,
                },
            )
            .await
            {
                Ok(()) => {
                    ctx.pending.insert(
                        rid,
                        PendingRequest {
                            kind: PendingKind::Reject,
                            event_seq_before: before_seq,
                            reply: Some(reply),
                            sent_at: Instant::now(),
                        },
                    );
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "hangup" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Ok(json!({
                    "callId": Value::Null,
                    "hadCall": false,
                    "ended": true,
                    "eventSeq": before_seq,
                })));
                return;
            };
            call.video_capture.take();
            call.stopping = true;
            let view = ctx
                .control
                .call
                .as_ref()
                .filter(|v| v.call_id == call.call_id)
                .cloned();
            match view {
                Some(view) if view.state == CallState::Ended => {
                    let rec = stop_inspector_recording(&mut call.inspector).await;
                    if let Some(r) = &rec {
                        *ctx.last_recording = Some(r.clone());
                    }
                    let cid = call.call_id.clone();
                    *ctx.active_call = None;
                    let _ = reply.send(Ok(json!({
                        "callId": cid,
                        "hadCall": true,
                        "ended": true,
                        "eventSeq": before_seq,
                        "recording": rec,
                    })));
                }
                Some(view) if view.state == CallState::Ending => {
                    call.hangup_started_at.get_or_insert_with(Instant::now);
                    call.end_waiters.push(reply);
                }
                Some(view) => {
                    let rid = request_id();
                    let cmd = if view.direction == CallDirection::Outgoing
                        && matches!(view.state, CallState::Dialing | CallState::Ringing)
                    {
                        CallCommand::Cancel {
                            request_id: rid.clone(),
                            call_id: call.call_id.clone(),
                            expected_revision: view.revision,
                        }
                    } else {
                        CallCommand::Hangup {
                            request_id: rid.clone(),
                            call_id: call.call_id.clone(),
                            expected_revision: view.revision,
                        }
                    };
                    match send_json(&mut ctx.control.socket, &cmd).await {
                        Ok(()) => {
                            call.hangup_started_at = Some(Instant::now());
                            call.end_waiters.push(reply);
                            ctx.pending.insert(
                                rid,
                                PendingRequest {
                                    kind: PendingKind::Hangup,
                                    event_seq_before: before_seq,
                                    reply: None,
                                    sent_at: Instant::now(),
                                },
                            );
                        }
                        Err(err) => {
                            let _ = reply.send(Err(err));
                        }
                    }
                }
                None => {
                    // Claim accepted but CallEvent::State hasn't arrived yet; defer Cancel/Hangup
                    // until the first CallEvent::State delivers the authoritative state and revision.
                    call.pending_hangup = true;
                    call.hangup_started_at = Some(Instant::now());
                    call.end_waiters.push(reply);
                }
            }
        }
        "conference.start" => {
            let group = match request
                .get("group")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(g) => g.to_owned(),
                None => {
                    let _ = reply.send(Err(anyhow!("group is required")));
                    return;
                }
            };
            let invitees: Vec<String> = request
                .get("invitees")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if invitees.is_empty() {
                let _ = reply.send(Err(anyhow!("at least one invitee is required")));
                return;
            }
            let is_video = request
                .get("video")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let muted = request
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let camera_off = request
                .get("cameraOff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !ctx.control.capabilities.conference_create
                || !ctx.control.capabilities.conference_audio
            {
                let _ = reply.send(Err(anyhow!(
                    "conference creation/audio unavailable on this daemon"
                )));
                return;
            }
            let call_type = if is_video {
                if !ctx.control.capabilities.conference_video {
                    let _ = reply.send(Err(anyhow!("conference video unavailable on this daemon")));
                    return;
                }
                CallType::Video
            } else {
                CallType::Voice
            };
            if ctx.control.availability.busy || ctx.active_call.is_some() {
                let _ = reply.send(Err(anyhow!(
                    "busy: the account already has an ongoing or unresolved call"
                )));
                return;
            }
            if let Err(err) = apply_media_overrides(
                &request,
                ctx.audio_io,
                ctx.current_video_source,
                ctx.audio_format,
            )
            .await
            {
                let _ = reply.send(Err(err));
                return;
            }
            let camera_enabled =
                call_type == CallType::Video && !camera_off && !ctx.current_video_source.is_off();
            let staged = StagedAction::ConferenceStart {
                group,
                invitees,
                call_type,
                muted,
                camera_enabled,
            };
            if let Err(err) = submit_ready_or_action(
                ctx.control,
                ctx.pending,
                ctx.ready_scope.as_ref(),
                *ctx.desired_ready,
                CallReadyScope::Outgoing,
                ctx.receive_codec,
                staged,
                before_seq,
                reply,
            )
            .await
            {
                ctx.bus
                    .emit(json!({"type": "session.error", "message": err.to_string()}));
            }
        }
        "conference.join" => {
            let group = match request
                .get("group")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                Some(g) => g,
                None => {
                    let _ = reply.send(Err(anyhow!("group is required")));
                    return;
                }
            };
            let invitation_id = request.get("invitationId").and_then(Value::as_str);
            let is_video = request
                .get("video")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let muted = request
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let camera_off = request
                .get("cameraOff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !ctx.control.capabilities.conference_join
                || !ctx.control.capabilities.conference_audio
            {
                let _ = reply.send(Err(anyhow!(
                    "conference joining/audio unavailable on this daemon"
                )));
                return;
            }
            if is_video && !ctx.control.capabilities.conference_video {
                let _ = reply.send(Err(anyhow!("conference video unavailable on this daemon")));
                return;
            }
            if ctx.control.availability.busy || ctx.active_call.is_some() {
                let _ = reply.send(Err(anyhow!(
                    "busy: the account already has an ongoing or unresolved call"
                )));
                return;
            }
            let matching: Vec<&CallConferenceInvitation> = ctx
                .control
                .conference_invitations
                .iter()
                .filter(|inv| {
                    inv.group_id == group && invitation_id.is_none_or(|id| inv.invitation_id == id)
                })
                .collect();
            let invitation = match matching.as_slice() {
                [] => {
                    let _ = reply.send(Err(anyhow!(
                        "no current conference invitation for group {group:?}"
                    )));
                    return;
                }
                [one] => (*one).clone(),
                _ => {
                    let _ = reply.send(Err(anyhow!(
                        "more than one conference invitation for group {group:?}; pass --invitation-id"
                    )));
                    return;
                }
            };
            if let Err(err) = apply_media_overrides(
                &request,
                ctx.audio_io,
                ctx.current_video_source,
                ctx.audio_format,
            )
            .await
            {
                let _ = reply.send(Err(err));
                return;
            }
            let camera_enabled = is_video && !camera_off && !ctx.current_video_source.is_off();
            let staged = StagedAction::ConferenceJoin {
                invitation,
                muted,
                camera_enabled,
            };
            if let Err(err) = submit_ready_or_action(
                ctx.control,
                ctx.pending,
                ctx.ready_scope.as_ref(),
                *ctx.desired_ready,
                CallReadyScope::Outgoing,
                ctx.receive_codec,
                staged,
                before_seq,
                reply,
            )
            .await
            {
                ctx.bus
                    .emit(json!({"type": "session.error", "message": err.to_string()}));
            }
        }
        "conference.invite" => {
            let Some(call) = ctx.active_call.as_ref() else {
                let _ = reply.send(Err(anyhow!("no active owned call")));
                return;
            };
            if !ctx.control.capabilities.conference_invite {
                let _ = reply.send(Err(anyhow!(
                    "conference invitations unavailable on this daemon"
                )));
                return;
            }
            let contacts: Vec<String> = request
                .get("contacts")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if contacts.is_empty() {
                let _ = reply.send(Err(anyhow!("at least one contact is required")));
                return;
            }
            let rid = request_id();
            match send_json(
                &mut ctx.control.socket,
                &CallCommand::Invite {
                    request_id: rid.clone(),
                    call_id: call.call_id.clone(),
                    contact_ids: contacts,
                },
            )
            .await
            {
                Ok(()) => {
                    ctx.pending.insert(
                        rid,
                        PendingRequest {
                            kind: PendingKind::Invite,
                            event_seq_before: before_seq,
                            reply: Some(reply),
                            sent_at: Instant::now(),
                        },
                    );
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "mute" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Err(anyhow!("no active owned call to mute/unmute")));
                return;
            };
            let new_muted = request
                .get("muted")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let old_muted = call.muted;
            call.muted = new_muted;
            ctx.audio_io.discard_capture();
            let rid = request_id();
            match send_json(
                &mut ctx.control.socket,
                &CallCommand::Mute {
                    request_id: rid.clone(),
                    call_id: call.call_id.clone(),
                    muted: new_muted,
                },
            )
            .await
            {
                Ok(()) => {
                    ctx.pending.insert(
                        rid,
                        PendingRequest {
                            kind: PendingKind::Mute {
                                old_muted,
                                new_muted,
                            },
                            event_seq_before: before_seq,
                            reply: Some(reply),
                            sent_at: Instant::now(),
                        },
                    );
                }
                Err(err) => {
                    call.muted = old_muted;
                    let _ = reply.send(Err(err));
                }
            }
        }
        "camera" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Err(anyhow!("no active owned call")));
                return;
            };
            if !call.video_allowed {
                let _ = reply.send(Err(anyhow!(
                    "camera is unavailable in this voice-only call"
                )));
                return;
            }
            let enabled = request
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if enabled && ctx.current_video_source.is_off() {
                let _ = reply.send(Err(anyhow!(
                    "video source is set to off; switch it with `wx calls session video <SOURCE>` first"
                )));
                return;
            }
            let old_enabled = call.camera_enabled;
            call.camera_enabled = enabled;
            if !enabled {
                call.video_capture.take();
            } else if let Err(err) =
                start_session_video_capture(call, ctx.current_video_source, ctx.control, ctx.bus)
                    .await
            {
                call.camera_enabled = old_enabled;
                let _ = reply.send(Err(err));
                return;
            }
            let rid = request_id();
            match send_json(
                &mut ctx.control.socket,
                &CallCommand::Camera {
                    request_id: rid.clone(),
                    call_id: call.call_id.clone(),
                    enabled,
                },
            )
            .await
            {
                Ok(()) => {
                    ctx.pending.insert(
                        rid,
                        PendingRequest {
                            kind: PendingKind::Camera {
                                old_enabled,
                                new_enabled: enabled,
                            },
                            event_seq_before: before_seq,
                            reply: Some(reply),
                            sent_at: Instant::now(),
                        },
                    );
                }
                Err(err) => {
                    call.camera_enabled = old_enabled;
                    if !old_enabled {
                        call.video_capture.take();
                    }
                    let _ = reply.send(Err(err));
                }
            }
        }
        "clear_audio" => {
            let Some(call) = ctx.active_call.as_ref() else {
                let _ = reply.send(Err(anyhow!("no active owned call")));
                return;
            };
            let rid = request_id();
            match send_json(
                &mut ctx.control.socket,
                &CallCommand::ClearAudio {
                    request_id: rid.clone(),
                    call_id: call.call_id.clone(),
                },
            )
            .await
            {
                Ok(()) => {
                    ctx.pending.insert(
                        rid,
                        PendingRequest {
                            kind: PendingKind::ClearAudio,
                            event_seq_before: before_seq,
                            reply: Some(reply),
                            sent_at: Instant::now(),
                        },
                    );
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "audio" => {
            let raw = match request.get("source").and_then(Value::as_str) {
                Some(s) => s,
                None => {
                    let _ = reply.send(Err(anyhow!("audio source is required")));
                    return;
                }
            };
            match AudioSourceSpec::parse(raw) {
                Ok(spec) => match ctx.audio_io.set_input(spec, ctx.audio_format).await {
                    Ok(()) => {
                        let label = ctx.audio_io.input_spec().label();
                        let _ = reply.send(Ok(json!({
                            "audioSource": label,
                            "eventSeq": before_seq,
                        })));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(err));
                    }
                },
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "video" => {
            let raw = match request.get("source").and_then(Value::as_str) {
                Some(s) => s,
                None => {
                    let _ = reply.send(Err(anyhow!("video source is required")));
                    return;
                }
            };
            match CameraSource::parse(raw) {
                Ok(spec) => {
                    if let Some(call) = ctx.active_call.as_mut() {
                        if spec.is_off() {
                            *ctx.current_video_source = spec;
                            call.video_capture.take();
                            if call.camera_enabled && call.video_allowed {
                                let old_enabled = call.camera_enabled;
                                call.camera_enabled = false;
                                let rid = request_id();
                                if send_json(
                                    &mut ctx.control.socket,
                                    &CallCommand::Camera {
                                        request_id: rid.clone(),
                                        call_id: call.call_id.clone(),
                                        enabled: false,
                                    },
                                )
                                .await
                                .is_ok()
                                {
                                    ctx.pending.insert(
                                        rid,
                                        PendingRequest {
                                            kind: PendingKind::Camera {
                                                old_enabled,
                                                new_enabled: false,
                                            },
                                            event_seq_before: before_seq,
                                            reply: None,
                                            sent_at: Instant::now(),
                                        },
                                    );
                                }
                            }
                        } else if call.camera_enabled && call.video_allowed {
                            if let Some(format) = call
                                .media
                                .as_ref()
                                .and_then(|m| m.video_send_track.as_ref())
                                .and_then(CallTrack::video_format)
                            {
                                let src = spec.clone();
                                let started = match tokio::task::spawn_blocking(move || {
                                    LocalVideoCapture::start(&src, format)
                                })
                                .await
                                {
                                    Ok(Ok(cap)) => cap,
                                    Ok(Err(err)) => {
                                        let _ = reply.send(Err(err));
                                        return;
                                    }
                                    Err(err) => {
                                        let _ = reply
                                            .send(Err(anyhow!("camera worker panicked: {err}")));
                                        return;
                                    }
                                };
                                // Replace capture and source only after the new source starts cleanly.
                                call.video_capture = Some(started);
                                *ctx.current_video_source = spec;
                                ctx.bus.emit(json!({
                                    "type": "media.video_capture_started",
                                    "callId": call.call_id,
                                    "source": ctx.current_video_source.label(),
                                    "codec": format.codec,
                                }));
                            } else {
                                *ctx.current_video_source = spec;
                            }
                        } else {
                            *ctx.current_video_source = spec;
                        }
                    } else {
                        *ctx.current_video_source = spec;
                    }
                    let _ = reply.send(Ok(json!({
                        "videoSource": ctx.current_video_source.label(),
                        "eventSeq": before_seq,
                    })));
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "keyframe" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Err(anyhow!("no active owned call")));
                return;
            };
            let Some(media) = call.media.as_mut() else {
                let _ = reply.send(Err(anyhow!("media socket is not attached yet")));
                return;
            };
            let track_ids: Vec<u32> = match request.get("track").and_then(Value::as_u64) {
                Some(id) => vec![id as u32],
                None => {
                    let ids = call.inspector.video_receive_track_ids();
                    if ids.is_empty() {
                        media.receive_video.tracks().keys().copied().collect()
                    } else {
                        ids
                    }
                }
            };
            if track_ids.is_empty() {
                let _ = reply.send(Err(anyhow!("no active remote video tracks")));
                return;
            }
            for id in &track_ids {
                if let Err(err) = media.request_keyframe(*id).await {
                    let _ = reply.send(Err(err));
                    return;
                }
            }
            let _ = reply.send(Ok(json!({
                "callId": call.call_id,
                "tracks": track_ids,
                "eventSeq": before_seq,
            })));
        }
        "snapshot" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Err(anyhow!("no active owned call")));
                return;
            };
            let track = request
                .get("track")
                .and_then(Value::as_u64)
                .map(|v| v as u32);
            let out_arg = request
                .get("out")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let (out_path, is_file) = match out_arg {
                Some(p) => {
                    let looks_like_file =
                        p.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("jpg")
                                || ext.eq_ignore_ascii_case("jpeg")
                                || ext.eq_ignore_ascii_case("png")
                        });
                    (p, looks_like_file)
                }
                None => (ctx.session_dir.join("snapshots"), false),
            };
            match call.inspector.save_snapshots(track, &out_path, is_file) {
                Ok(snaps) => {
                    for snap in &snaps {
                        ctx.bus.emit(json!({
                            "type": "media.snapshot",
                            "callId": call.call_id,
                            "snapshot": snap,
                        }));
                    }
                    let _ = reply.send(Ok(json!({
                        "callId": call.call_id,
                        "snapshots": snaps,
                        "eventSeq": before_seq,
                    })));
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "record.start" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Err(anyhow!("no active owned call to record")));
                return;
            };
            let dir = request
                .get("dir")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
                    ctx.session_dir.join("recordings").join(format!(
                        "{stamp}-{}",
                        wechat_call_client::inspect::sanitize_filename_part(&call.call_id)
                    ))
                });
            match call.inspector.start_recording(dir) {
                Ok(report) => {
                    ctx.bus.emit(json!({
                        "type": "recording.started",
                        "callId": call.call_id,
                        "recording": report,
                    }));
                    let _ = reply.send(Ok(json!({
                        "callId": call.call_id,
                        "recording": report,
                        "eventSeq": before_seq,
                    })));
                }
                Err(err) => {
                    let _ = reply.send(Err(err));
                }
            }
        }
        "record.stop" => {
            let Some(call) = ctx.active_call.as_mut() else {
                let _ = reply.send(Ok(json!({
                    "recording": ctx.last_recording,
                    "eventSeq": before_seq,
                })));
                return;
            };
            let report = stop_inspector_recording(&mut call.inspector).await;
            if let Some(r) = &report {
                *ctx.last_recording = Some(r.clone());
                ctx.bus.emit(json!({
                    "type": "recording.stopped",
                    "callId": call.call_id,
                    "recording": r,
                }));
            }
            let _ = reply.send(Ok(json!({
                "callId": call.call_id,
                "recording": report,
                "eventSeq": before_seq,
            })));
        }
        other => {
            let _ = reply.send(Err(failure(
                2,
                "invalid_arguments",
                &format!("unknown session operation {other:?}"),
            )));
        }
    }
}

async fn apply_media_overrides(
    request: &Value,
    audio_io: &mut CallAudioIo,
    video_source: &mut CameraSource,
    format: AudioFormat,
) -> Result<()> {
    if let Some(raw_audio) = request.get("audio").and_then(Value::as_str) {
        let spec = AudioSourceSpec::parse(raw_audio)?;
        audio_io.set_input(spec, format).await?;
    }
    if let Some(raw_video) = request.get("videoSource").and_then(Value::as_str) {
        *video_source = CameraSource::parse(raw_video)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn submit_ready(
    control: &mut Control,
    pending: &mut HashMap<String, PendingRequest>,
    enable: bool,
    scope: CallReadyScope,
    receive_codec: CallVideoCodec,
    initial: bool,
    for_waiting_offer: bool,
    followup: Option<StagedAction>,
    event_seq_before: u64,
    reply: Option<oneshot::Sender<Result<Value>>>,
) -> Result<()> {
    let video_caps = if enable {
        let daemon_video =
            control.capabilities.direct_video || control.capabilities.conference_video;
        tokio::task::spawn_blocking(move || {
            client_video_capabilities_with_receive(daemon_video, None, Some(vec![receive_codec]))
        })
        .await
        .context("video capability probe failed")?
    } else {
        None
    };
    let rid = request_id();
    let format = control.format;
    let res = send_json(
        &mut control.socket,
        &CallCommand::Ready {
            request_id: rid.clone(),
            ready: enable,
            format,
            scope: Some(scope.clone()),
            video: video_caps,
        },
    )
    .await;
    match res {
        Ok(()) => {
            pending.insert(
                rid,
                PendingRequest {
                    kind: PendingKind::Ready {
                        scope: enable.then_some(scope),
                        initial,
                        for_waiting_offer,
                        followup: followup.map(Box::new),
                    },
                    event_seq_before,
                    reply,
                    sent_at: Instant::now(),
                },
            );
            Ok(())
        }
        Err(err) => {
            if let Some(r) = reply {
                let _ = r.send(Err(anyhow!("{err:#}")));
            }
            Err(err)
        }
    }
}

fn ready_scope_covers(current: Option<&CallReadyScope>, needed: &CallReadyScope) -> bool {
    match current {
        Some(CallReadyScope::All) => true,
        Some(existing) => existing == needed,
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn submit_ready_or_action(
    control: &mut Control,
    pending: &mut HashMap<String, PendingRequest>,
    ready_scope: Option<&CallReadyScope>,
    desired_ready: bool,
    scope: CallReadyScope,
    receive_codec: CallVideoCodec,
    action: StagedAction,
    event_seq_before: u64,
    reply: oneshot::Sender<Result<Value>>,
) -> Result<()> {
    if ready_scope_covers(ready_scope, &scope) {
        return submit_staged_action(control, pending, action, event_seq_before, Some(reply)).await;
    }
    let effective_scope = if desired_ready {
        CallReadyScope::All
    } else {
        scope
    };
    submit_ready(
        control,
        pending,
        true,
        effective_scope,
        receive_codec,
        false,
        false,
        Some(action),
        event_seq_before,
        Some(reply),
    )
    .await
}

async fn submit_staged_action(
    control: &mut Control,
    pending: &mut HashMap<String, PendingRequest>,
    action: StagedAction,
    event_seq_before: u64,
    reply: Option<oneshot::Sender<Result<Value>>>,
) -> Result<()> {
    let rid = request_id();
    let (cmd, kind) = match action {
        StagedAction::Dial {
            contact,
            call_type,
            muted,
            camera_enabled,
        } => (
            CallCommand::Dial {
                request_id: rid.clone(),
                target: CallTarget::Direct {
                    contact_id: contact,
                },
                call_type,
                camera_enabled: (call_type == CallType::Video).then_some(camera_enabled),
                muted: muted.then_some(true),
            },
            PendingKind::Dial {
                is_conference: false,
                video_allowed: call_type == CallType::Video && control.capabilities.direct_video,
                muted,
                camera_enabled,
            },
        ),
        StagedAction::ConferenceStart {
            group,
            invitees,
            call_type,
            muted,
            camera_enabled,
        } => (
            CallCommand::Dial {
                request_id: rid.clone(),
                target: CallTarget::Conference {
                    group_id: group,
                    invitees,
                },
                call_type,
                camera_enabled: (call_type == CallType::Video).then_some(camera_enabled),
                muted: muted.then_some(true),
            },
            PendingKind::Dial {
                is_conference: true,
                video_allowed: call_type == CallType::Video
                    && control.capabilities.conference_video,
                muted,
                camera_enabled,
            },
        ),
        StagedAction::ConferenceJoin {
            invitation,
            muted,
            camera_enabled,
        } => (
            CallCommand::Join {
                request_id: rid.clone(),
                invitation_id: invitation.invitation_id,
                invitation_revision: invitation.revision,
                camera_enabled: camera_enabled.then_some(true),
                muted: muted.then_some(true),
            },
            PendingKind::Dial {
                is_conference: true,
                video_allowed: control.capabilities.conference_video,
                muted,
                camera_enabled,
            },
        ),
        StagedAction::Answer {
            call_id,
            muted,
            camera_off,
        } => {
            let Some(offer) = control
                .offer
                .as_ref()
                .filter(|o| call_id.as_deref().is_none_or(|id| id == o.call_id))
                .cloned()
            else {
                let err = anyhow!("offer_expired: no matching current incoming call offer");
                if let Some(r) = reply {
                    let _ = r.send(Err(anyhow!("{err:#}")));
                }
                return Err(err);
            };
            let is_video = offer.call_type == CallType::Video && control.capabilities.direct_video;
            let camera_enabled = is_video && !camera_off;
            (
                CallCommand::Answer {
                    request_id: rid.clone(),
                    call_id: offer.call_id,
                    expected_revision: offer.revision,
                    camera_enabled: (offer.call_type == CallType::Video).then_some(camera_enabled),
                    muted: muted.then_some(true),
                },
                PendingKind::Answer {
                    video_allowed: is_video,
                    muted,
                    camera_enabled,
                },
            )
        }
    };
    match send_json(&mut control.socket, &cmd).await {
        Ok(()) => {
            pending.insert(
                rid,
                PendingRequest {
                    kind,
                    event_seq_before,
                    reply,
                    sent_at: Instant::now(),
                },
            );
            Ok(())
        }
        Err(err) => {
            if let Some(r) = reply {
                let _ = r.send(Err(anyhow!("{err:#}")));
            }
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_control_event(
    event: &CallEvent,
    control: &mut Control,
    active_call: &mut Option<ActiveSessionCall>,
    audio_io: &mut CallAudioIo,
    current_video_source: &CameraSource,
    pending: &mut HashMap<String, PendingRequest>,
    waiting_offer: &mut Option<WaitingPendingOffer>,
    ready_scope: &mut Option<CallReadyScope>,
    startup_ready_error: &mut Option<String>,
    desired_ready: bool,
    receive_codec: CallVideoCodec,
    options: &SessionRunOptions,
    session_dir: &Path,
    bus: &Arc<SharedEventBus>,
    last_media_report: &mut Option<MediaInspectionReport>,
    last_recording: &mut Option<RecordingReport>,
) {
    match event {
        CallEvent::CommandResult {
            request_id: rid,
            status,
            call_id,
            owner_token,
            code,
        } => {
            let Some(mut req) = pending.remove(rid) else {
                return;
            };
            if *status == CallCommandStatus::Error {
                let code_str = serde_json::to_value(code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "command_rejected".into());
                match req.kind {
                    PendingKind::Ready {
                        initial,
                        for_waiting_offer,
                        ..
                    } => {
                        if initial {
                            *startup_ready_error = Some(code_str.clone());
                            bus.emit(json!({
                                "type": "session.error",
                                "message": format!("initial call.ready rejected: {code_str}"),
                            }));
                        }
                        if for_waiting_offer {
                            if let Some(staged) = waiting_offer.take() {
                                let _ = staged.reply.send(Err(anyhow!(
                                    "call readiness rejected while preparing offer: {code_str}"
                                )));
                            }
                        }
                    }
                    PendingKind::Mute { old_muted, .. } => {
                        if let Some(call) = active_call.as_mut() {
                            call.muted = old_muted;
                            audio_io.discard_capture();
                        }
                    }
                    PendingKind::Camera { old_enabled, .. } => {
                        if let Some(call) = active_call.as_mut() {
                            call.camera_enabled = old_enabled;
                            if !old_enabled {
                                call.video_capture.take();
                            } else {
                                let _ = start_session_video_capture(
                                    call,
                                    current_video_source,
                                    control,
                                    bus,
                                )
                                .await;
                            }
                        }
                    }
                    PendingKind::Hangup => {
                        if let Some(call) = active_call.as_mut() {
                            for waiter in call.end_waiters.drain(..) {
                                let _ = waiter.send(Err(anyhow!("hangup rejected: {code_str}")));
                            }
                        }
                    }
                    _ => {}
                }
                if let Some(reply) = req.reply.take() {
                    let _ = reply.send(Err(anyhow!("call command rejected: {code_str}")));
                }
                return;
            }

            // Accepted!
            match req.kind {
                PendingKind::Ready {
                    scope: next_scope,
                    initial,
                    followup,
                    ..
                } => {
                    *ready_scope = next_scope;
                    if initial {
                        *startup_ready_error = None;
                    }
                    if let Some(action) = followup {
                        let _ = submit_staged_action(
                            control,
                            pending,
                            *action,
                            req.event_seq_before,
                            req.reply.take(),
                        )
                        .await;
                    } else if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "ready": matches!(ready_scope, Some(CallReadyScope::All)),
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Dial {
                    is_conference,
                    video_allowed,
                    muted,
                    camera_enabled,
                } => {
                    let is_conf = is_conference;
                    let (Some(cid), Some(token)) = (call_id.clone(), owner_token.clone()) else {
                        if let Some(reply) = req.reply.take() {
                            let _ = reply.send(Err(anyhow!(
                                "call claim accepted without callId/ownerToken"
                            )));
                        }
                        return;
                    };
                    bus.publish_media(None);
                    let mut inspector = CallMediaInspector::new(control.format.sample_rate);
                    if let Some(rec_root) = &options.record_dir {
                        let rec_dir = rec_root.join(format!(
                            "{}-{}",
                            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                            wechat_call_client::inspect::sanitize_filename_part(&cid)
                        ));
                        match inspector.start_recording(rec_dir) {
                            Ok(report) => {
                                bus.emit(json!({
                                    "type": "recording.started",
                                    "callId": cid,
                                    "recording": report,
                                }));
                            }
                            Err(err) => {
                                bus.emit(json!({
                                    "type": "session.error",
                                    "message": format!("failed to start recording: {err}"),
                                }));
                            }
                        }
                    }
                    let _ = session_dir;
                    *active_call = Some(ActiveSessionCall {
                        call_id: cid.clone(),
                        owner_token: token,
                        is_conference: is_conf,
                        video_allowed,
                        muted,
                        camera_enabled: video_allowed && camera_enabled,
                        media: None,
                        video_capture: None,
                        media_closed_at: None,
                        media_closed_err: None,
                        media_attach_failed: false,
                        inspector,
                        last_snapshot_at: Instant::now(),
                        stopping: false,
                        pending_hangup: false,
                        hangup_started_at: None,
                        end_waiters: Vec::new(),
                    });
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": cid,
                            "isConference": is_conf,
                            "videoAllowed": video_allowed,
                            "muted": muted,
                            "cameraEnabled": video_allowed && camera_enabled,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Answer {
                    video_allowed,
                    muted,
                    camera_enabled,
                } => {
                    let (Some(cid), Some(token)) = (call_id.clone(), owner_token.clone()) else {
                        if let Some(reply) = req.reply.take() {
                            let _ = reply.send(Err(anyhow!(
                                "call claim accepted without callId/ownerToken"
                            )));
                        }
                        return;
                    };
                    bus.publish_media(None);
                    let mut inspector = CallMediaInspector::new(control.format.sample_rate);
                    if let Some(rec_root) = &options.record_dir {
                        let rec_dir = rec_root.join(format!(
                            "{}-{}",
                            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
                            wechat_call_client::inspect::sanitize_filename_part(&cid)
                        ));
                        match inspector.start_recording(rec_dir) {
                            Ok(report) => {
                                bus.emit(json!({
                                    "type": "recording.started",
                                    "callId": cid,
                                    "recording": report,
                                }));
                            }
                            Err(err) => {
                                bus.emit(json!({
                                    "type": "session.error",
                                    "message": format!("failed to start recording: {err}"),
                                }));
                            }
                        }
                    }
                    *active_call = Some(ActiveSessionCall {
                        call_id: cid.clone(),
                        owner_token: token,
                        is_conference: false,
                        video_allowed,
                        muted,
                        camera_enabled: video_allowed && camera_enabled,
                        media: None,
                        video_capture: None,
                        media_closed_at: None,
                        media_closed_err: None,
                        media_attach_failed: false,
                        inspector,
                        last_snapshot_at: Instant::now(),
                        stopping: false,
                        pending_hangup: false,
                        hangup_started_at: None,
                        end_waiters: Vec::new(),
                    });
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": cid,
                            "isConference": false,
                            "videoAllowed": video_allowed,
                            "muted": muted,
                            "cameraEnabled": video_allowed && camera_enabled,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Reject => {
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "rejected": true,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Hangup => {
                    // `end_waiters` will be resolved once CallState::Ended / !busy arrives.
                }
                PendingKind::Mute { new_muted, .. } => {
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": active_call.as_ref().map(|c| c.call_id.as_str()),
                            "muted": new_muted,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Camera { new_enabled, .. } => {
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": active_call.as_ref().map(|c| c.call_id.as_str()),
                            "cameraEnabled": new_enabled,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::Invite => {
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": active_call.as_ref().map(|c| c.call_id.as_str()),
                            "submitted": true,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
                PendingKind::ClearAudio => {
                    if let Some(reply) = req.reply.take() {
                        let _ = reply.send(Ok(json!({
                            "callId": active_call.as_ref().map(|c| c.call_id.as_str()),
                            "cleared": true,
                            "eventSeq": req.event_seq_before,
                        })));
                    }
                }
            }
        }
        CallEvent::State { call: view } => {
            let mut deferred_hangup_cmd: Option<CallCommand> = None;
            if let Some(call) = active_call.as_mut().filter(|c| c.call_id == view.call_id) {
                call.inspector.apply_roster(view);
                if let Some(media) = call.media.as_mut() {
                    let _ = media.receive.roster(view);
                    media.receive_video.roster(view);
                }
                match view.state {
                    CallState::Ended | CallState::Uncertain => {
                        finalize_active_call(
                            active_call,
                            bus,
                            last_media_report,
                            last_recording,
                            view.state == CallState::Ended,
                        )
                        .await;
                    }
                    CallState::Ending => {
                        call.pending_hangup = false;
                        call.video_capture.take();
                        if call.media_closed_at.is_some() {
                            call.media_closed_at = Some(Instant::now());
                        }
                    }
                    _ => {
                        if call.pending_hangup {
                            call.pending_hangup = false;
                            let rid = request_id();
                            deferred_hangup_cmd = Some(
                                if view.direction == CallDirection::Outgoing
                                    && matches!(view.state, CallState::Dialing | CallState::Ringing)
                                {
                                    CallCommand::Cancel {
                                        request_id: rid,
                                        call_id: call.call_id.clone(),
                                        expected_revision: view.revision,
                                    }
                                } else {
                                    CallCommand::Hangup {
                                        request_id: rid,
                                        call_id: call.call_id.clone(),
                                        expected_revision: view.revision,
                                    }
                                },
                            );
                        }
                    }
                }
            }
            if let Some(cmd) = deferred_hangup_cmd {
                let rid = cmd.request_id().to_owned();
                let before_seq = bus.current_seq();
                match send_json(&mut control.socket, &cmd).await {
                    Ok(()) => {
                        pending.insert(
                            rid,
                            PendingRequest {
                                kind: PendingKind::Hangup,
                                event_seq_before: before_seq,
                                reply: None,
                                sent_at: Instant::now(),
                            },
                        );
                    }
                    Err(err) => {
                        if let Some(call) = active_call.as_mut() {
                            for waiter in call.end_waiters.drain(..) {
                                let _ = waiter.send(Err(anyhow!("{err:#}")));
                            }
                        }
                    }
                }
            }
        }
        CallEvent::Availability { availability } if !availability.busy => {
            if active_call.is_some() {
                finalize_active_call(active_call, bus, last_media_report, last_recording, true)
                    .await;
            }
            bus.publish_media(None);
            if desired_ready
                && !matches!(ready_scope, Some(CallReadyScope::All))
                && !pending
                    .values()
                    .any(|p| matches!(p.kind, PendingKind::Ready { .. }))
            {
                // Restore full readiness scope once idle.
                let before = bus.current_seq();
                let _ = submit_ready(
                    control,
                    pending,
                    true,
                    CallReadyScope::All,
                    receive_codec,
                    false,
                    false,
                    None,
                    before,
                    None,
                )
                .await;
            }
        }
        CallEvent::TrackAdded { call_id: id, track }
        | CallEvent::TrackUpdated { call_id: id, track } => {
            if let Some(call) = active_call.as_mut().filter(|c| &c.call_id == id) {
                if track.direction() == CallTrackDirection::Receive {
                    call.inspector.upsert_receive_track(track);
                }
                if let Some(media) = call.media.as_mut() {
                    media.receive_video.event(event, &call.call_id);
                    if let Some(view) = control.call.as_ref().filter(|v| v.call_id == call.call_id)
                    {
                        media.receive_video.roster(view);
                        call.inspector.apply_roster(view);
                    }
                    let _ = media.receive.event(event, &call.call_id);
                }
            }
        }
        CallEvent::TrackRemoved {
            call_id: id,
            track_id,
            ..
        } => {
            if let Some(call) = active_call.as_mut().filter(|c| &c.call_id == id) {
                call.inspector.mark_receive_track_removed(*track_id);
                if let Some(media) = call.media.as_mut() {
                    media.receive_video.event(event, &call.call_id);
                    if media.receive.event(event, &call.call_id).unwrap_or(false) {
                        let _ = audio_io.clear_playback();
                    }
                }
            }
        }
        CallEvent::ParticipantUpdated { call_id: id, .. } => {
            if let Some(call) = active_call.as_mut().filter(|c| &c.call_id == id) {
                if let Some(view) = control.call.as_ref().filter(|v| v.call_id == call.call_id) {
                    call.inspector.apply_roster(view);
                    if let Some(media) = call.media.as_mut() {
                        let _ = media.receive.roster(view);
                        media.receive_video.roster(view);
                    }
                }
            }
        }
        _ => {}
    }
}

async fn finalize_active_call(
    active_call: &mut Option<ActiveSessionCall>,
    bus: &Arc<SharedEventBus>,
    last_media_report: &mut Option<MediaInspectionReport>,
    last_recording: &mut Option<RecordingReport>,
    ended: bool,
) {
    let Some(mut call) = active_call.take() else {
        return;
    };
    call.video_capture.take();
    let rec = stop_inspector_recording(&mut call.inspector).await;
    if let Some(r) = &rec {
        *last_recording = Some(r.clone());
        bus.emit(json!({
            "type": "recording.stopped",
            "callId": call.call_id,
            "recording": r,
        }));
    }
    let final_media = call.inspector.report(Instant::now());
    *last_media_report = Some(final_media);
    bus.publish_media(None);
    for waiter in call.end_waiters.drain(..) {
        let _ = waiter.send(Ok(json!({
            "callId": call.call_id,
            "hadCall": true,
            "ended": ended,
            "eventSeq": bus.current_seq(),
            "recording": rec,
        })));
    }
    drop(call.media);
}

// ---------------------------------------------------------------------------
// IPC server connection handler & concurrent `wait` / `events.follow`
// ---------------------------------------------------------------------------

async fn handle_ipc_stream<R, W>(
    reader: R,
    mut writer: W,
    bus: Arc<SharedEventBus>,
    cmd_tx: mpsc::Sender<EngineCommand>,
    expected_secret: Option<String>,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let bounded_reader = reader.take((MAX_IPC_LINE_BYTES + 1) as u64);
    let mut lines = BufReader::new(bounded_reader).lines();
    let line = match timeout(Duration::from_secs(5), lines.next_line()).await {
        Ok(Ok(Some(line))) => line,
        _ => return,
    };
    if line.len() > MAX_IPC_LINE_BYTES {
        let msg = ipc_err(None, "invalid_arguments", "IPC request exceeds size limit");
        let _ = writer
            .write_all(format!("{}\n", serde_json::to_string(&msg).unwrap_or_default()).as_bytes())
            .await;
        return;
    }
    let req: Value = match serde_json::from_str(&line) {
        Ok(v) => v,
        Err(err) => {
            let msg = ipc_err(None, "invalid_arguments", format!("invalid JSON: {err}"));
            let _ = writer
                .write_all(
                    format!("{}\n", serde_json::to_string(&msg).unwrap_or_default()).as_bytes(),
                )
                .await;
            return;
        }
    };
    if let Some(secret) = expected_secret.as_deref() {
        let provided = req.get("secret").and_then(Value::as_str).unwrap_or("");
        if provided.len() != secret.len()
            || provided
                .bytes()
                .zip(secret.bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                != 0
        {
            let msg = ipc_err(None, "unauthorized", "invalid IPC secret");
            let _ = writer
                .write_all(
                    format!("{}\n", serde_json::to_string(&msg).unwrap_or_default()).as_bytes(),
                )
                .await;
            return;
        }
    }
    let req_id = req.get("id").cloned();
    let op = req.get("op").and_then(Value::as_str).unwrap_or("");
    if op == "events.follow" {
        let after = req.get("after").and_then(Value::as_u64).unwrap_or(0);
        let mut rx = bus.subscribe();
        let mut last_sent = after;
        for ev in bus.events_after(after, EVENT_BUFFER_CAP) {
            if let Some(s) = ev["seq"].as_u64() {
                last_sent = last_sent.max(s);
            }
            let text = format!("{}\n", serde_json::to_string(&ev).unwrap_or_default());
            if writer.write_all(text.as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = writer.flush().await;
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let seq = ev["seq"].as_u64().unwrap_or(0);
                    if seq <= last_sent {
                        continue;
                    }
                    last_sent = seq;
                    let stopped = ev["type"].as_str() == Some("session.stopped");
                    let text = format!("{}\n", serde_json::to_string(&ev).unwrap_or_default());
                    if writer.write_all(text.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = writer.flush().await;
                    if stopped {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    for ev in bus.events_after(last_sent, EVENT_BUFFER_CAP) {
                        let seq = ev["seq"].as_u64().unwrap_or(0);
                        if seq <= last_sent {
                            continue;
                        }
                        last_sent = seq;
                        let stopped = ev["type"].as_str() == Some("session.stopped");
                        let text = format!("{}\n", serde_json::to_string(&ev).unwrap_or_default());
                        if writer.write_all(text.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = writer.flush().await;
                        if stopped {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    let mut stop_flushed_tx: Option<oneshot::Sender<()>> = None;
    let result = if op == "events" {
        let after = req.get("after").and_then(Value::as_u64).unwrap_or(0);
        let limit = req
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .min(EVENT_BUFFER_CAP as u64) as usize;
        Ok(json!({
            "eventSeq": bus.current_seq(),
            "events": bus.events_after(after, limit),
        }))
    } else if op == "wait" {
        run_wait_request(&bus, &req).await
    } else {
        let (tx, rx) = oneshot::channel();
        let flushed = if op == "stop" {
            let (ftx, frx) = oneshot::channel();
            stop_flushed_tx = Some(ftx);
            Some(frx)
        } else {
            None
        };
        if cmd_tx
            .send(EngineCommand {
                request: req,
                reply: tx,
                flushed,
            })
            .await
            .is_err()
        {
            Err(anyhow!("call session stopped"))
        } else {
            rx.await
                .unwrap_or_else(|_| Err(anyhow!("call session stopped")))
        }
    };

    let envelope = match result {
        Ok(val) => ipc_ok(req_id.as_ref(), val),
        Err(err) => {
            let (_, code) = crate::outcome::classify(&err);
            ipc_err(req_id.as_ref(), &code, err.to_string())
        }
    };
    let _ = writer
        .write_all(format!("{}\n", serde_json::to_string(&envelope).unwrap_or_default()).as_bytes())
        .await;
    let _ = writer.flush().await;
    if let Some(ftx) = stop_flushed_tx {
        let _ = ftx.send(());
    }
}

fn matches_media_entry(args: &SessionWaitArgs, entry: &(String, MediaInspectionReport)) -> bool {
    args.call_id.as_deref().is_none_or(|id| id == entry.0) && args.matches_media(&entry.1)
}

async fn run_wait_request(bus: &Arc<SharedEventBus>, request: &Value) -> Result<Value> {
    let args: SessionWaitArgs =
        serde_json::from_value(request.clone()).context("invalid wait filter arguments")?;
    let max_wait = Duration::from_secs(args.timeout.clamp(1, 3600));
    let mut rx = bus.subscribe();
    let mut media_rx = bus.subscribe_media();

    let wait_media_only = args.has_media_predicate() && !args.has_event_predicate();
    let mut matched_event: Option<Value> = None;
    let mut last_seen = args.after;

    // Fast path: check already-buffered events after `args.after`.
    if !wait_media_only {
        for ev in bus.events_after(args.after, EVENT_BUFFER_CAP) {
            if let Some(seq) = ev["seq"].as_u64() {
                last_seen = last_seen.max(seq);
            }
            if args.matches_event(&ev) {
                if !args.has_media_predicate() {
                    return Ok(json!({
                        "matched": true,
                        "eventSeq": ev["seq"],
                        "event": ev,
                    }));
                }
                matched_event = Some(ev);
                if let Some(entry) = media_rx.borrow().clone() {
                    if matches_media_entry(&args, &entry) {
                        return Ok(json!({
                            "matched": true,
                            "eventSeq": bus.current_seq(),
                            "event": matched_event,
                            "media": entry.1,
                        }));
                    }
                }
                break;
            }
        }
    } else if let Some(entry) = media_rx.borrow().clone() {
        if matches_media_entry(&args, &entry) {
            return Ok(json!({
                "matched": true,
                "eventSeq": bus.current_seq(),
                "media": entry.1,
            }));
        }
    }

    let deadline = tokio::time::Instant::now() + max_wait;

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(failure(
                3,
                "wait_timeout",
                &format!(
                    "timed out after {}s waiting for matching call event (lastSeq={})",
                    args.timeout,
                    bus.current_seq()
                ),
            ));
        }
        let remaining = deadline - now;
        tokio::select! {
            _ = tokio::time::sleep(remaining) => {
                return Err(failure(
                    3,
                    "wait_timeout",
                    &format!(
                        "timed out after {}s waiting for matching call event (lastSeq={})",
                        args.timeout,
                        bus.current_seq()
                    ),
                ));
            }
            ev = rx.recv() => {
                match ev {
                    Ok(ev) => {
                        let seq = ev["seq"].as_u64().unwrap_or(0);
                        if seq <= last_seen {
                            continue;
                        }
                        last_seen = seq;
                        if ev["type"].as_str() == Some("session.stopped") {
                            if !wait_media_only && seq > args.after && args.matches_event(&ev) {
                                return Ok(json!({
                                    "matched": true,
                                    "eventSeq": seq,
                                    "event": ev,
                                }));
                            }
                            return Err(failure(
                                3,
                                "session_stopped",
                                "call session stopped before a matching event arrived",
                            ));
                        }
                        if !wait_media_only && matched_event.is_none() && seq > args.after && args.matches_event(&ev) {
                            if !args.has_media_predicate() {
                                return Ok(json!({
                                    "matched": true,
                                    "eventSeq": seq,
                                    "event": ev,
                                }));
                            }
                            matched_event = Some(ev);
                            if let Some(entry) = media_rx.borrow().clone() {
                                if matches_media_entry(&args, &entry) {
                                    return Ok(json!({
                                        "matched": true,
                                        "eventSeq": bus.current_seq(),
                                        "event": matched_event,
                                        "media": entry.1,
                                    }));
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Re-scan the ring buffer so lagged subscribers do not miss matching events.
                        for ev in bus.events_after(last_seen, EVENT_BUFFER_CAP) {
                            let seq = ev["seq"].as_u64().unwrap_or(0);
                            if seq <= last_seen {
                                continue;
                            }
                            last_seen = seq;
                            if ev["type"].as_str() == Some("session.stopped") {
                                if !wait_media_only && args.matches_event(&ev) {
                                    return Ok(json!({
                                        "matched": true,
                                        "eventSeq": seq,
                                        "event": ev,
                                    }));
                                }
                                return Err(failure(
                                    3,
                                    "session_stopped",
                                    "call session stopped before a matching event arrived",
                                ));
                            }
                            if !wait_media_only && matched_event.is_none() && args.matches_event(&ev) {
                                if !args.has_media_predicate() {
                                    return Ok(json!({
                                        "matched": true,
                                        "eventSeq": seq,
                                        "event": ev,
                                    }));
                                }
                                matched_event = Some(ev);
                                if let Some(entry) = media_rx.borrow().clone() {
                                    if matches_media_entry(&args, &entry) {
                                        return Ok(json!({
                                            "matched": true,
                                            "eventSeq": bus.current_seq(),
                                            "event": matched_event,
                                            "media": entry.1,
                                        }));
                                    }
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(failure(3, "session_stopped", "call session closed"));
                    }
                }
            }
            changed = media_rx.changed(), if args.has_media_predicate() => {
                if changed.is_err() {
                    return Err(failure(3, "session_stopped", "call session closed"));
                }
                if let Some(entry) = media_rx.borrow().clone() {
                    if matches_media_entry(&args, &entry) && (wait_media_only || matched_event.is_some()) {
                        return Ok(json!({
                            "matched": true,
                            "eventSeq": bus.current_seq(),
                            "event": matched_event,
                            "media": entry.1,
                        }));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unit & mock-server integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{
            ws::{Message as AxumMsg, WebSocket},
            State, WebSocketUpgrade,
        },
        response::IntoResponse,
        routing::get,
        Router,
    };
    use wechat_core::{
        call_types::{
            CallCapabilities, CallControl, CallKind, CallMembership, CallParticipant,
            CallParticipantStatus, CallVideoFormat, CallVideoMode, CallView, CallsAvailability,
            CALL_PROTOCOL_VERSION,
        },
        calls::pcm_envelope::{encode_pcm_chunk, encode_video, MediaHeader, FLAG_KEYFRAME},
    };

    #[test]
    fn wait_matcher_filters_events_and_media_predicates() {
        let peer_ev = json!({
            "seq": 5,
            "type": "call.peer_status",
            "callId": "call-1",
            "participantId": "wxid_bob",
            "status": { "code": 3, "kind": "acknowledged" }
        });
        let args = SessionWaitArgs {
            peer_statuses: vec!["acknowledged".into()],
            participant: Some("wxid_bob".into()),
            ..Default::default()
        };
        assert!(args.matches_event(&peer_ev));
        let wrong = SessionWaitArgs {
            peer_statuses: vec!["joined".into()],
            ..Default::default()
        };
        assert!(!wrong.matches_event(&peer_ev));

        let ctrl_ev = json!({
            "seq": 6,
            "type": "call.control",
            "callId": "call-1",
            "participantId": "wxid_bob",
            "control": { "kind": "microphone", "muted": true }
        });
        let mic_wait = SessionWaitArgs {
            control_kind: Some("microphone".into()),
            control_value: Some(true),
            ..Default::default()
        };
        assert!(mic_wait.matches_event(&ctrl_ev));
        let unmute_wait = SessionWaitArgs {
            control_kind: Some("microphone".into()),
            control_value: Some(false),
            ..Default::default()
        };
        assert!(!unmute_wait.matches_event(&ctrl_ev));

        let part_ev = json!({
            "seq": 7,
            "type": "participant.updated",
            "callId": "conf-1",
            "revision": 2,
            "participant": {
                "participantId": "p-bob",
                "contactId": "wxid_bob",
                "membership": "joined",
                "microphoneEnabled": false,
                "cameraEnabled": true,
                "status": { "code": 1, "kind": "joined" }
            }
        });
        let part_wait = SessionWaitArgs {
            participant: Some("wxid_bob".into()),
            membership: Some("joined".into()),
            mic: Some(false),
            camera: Some(true),
            ..Default::default()
        };
        assert!(part_wait.matches_event(&part_ev));
    }

    #[derive(Clone, Default)]
    struct MockDaemonState {
        commands: Arc<Mutex<Vec<Value>>>,
    }

    fn test_jpeg(r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbImage::from_fn(32, 24, |_, _| image::Rgb([r, g, b]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    async fn mock_control_ws(
        ws: WebSocketUpgrade,
        State(state): State<MockDaemonState>,
    ) -> impl IntoResponse {
        ws.on_upgrade(move |socket| handle_mock_control(socket, state))
    }

    async fn handle_mock_control(mut socket: WebSocket, state: MockDaemonState) {
        let _ = socket.recv().await; // call.hello
        let hello = CallEvent::Hello {
            protocol_version: CALL_PROTOCOL_VERSION,
            client_id: "mock-client-1".into(),
            capabilities: CallCapabilities {
                direct_audio: true,
                direct_video: true,
                conference_audio: true,
                conference_video: true,
                conference_create: true,
                conference_join: true,
                conference_invite: true,
            },
            availability: CallsAvailability {
                busy: false,
                owned_by_this_client: false,
            },
            current_offer: None,
            current_pending_offer: None,
            current_call: None,
            audio_format: CallAudioFormat::default(),
            video_capabilities: Some(wechat_core::call_types::CallVideoCapabilities {
                send_codecs: vec![CallVideoCodec::Jpeg],
                receive_codecs: vec![CallVideoCodec::Jpeg],
                max_width: Some(320),
                max_height: Some(240),
                max_fps: Some(10),
            }),
            conference_invitations: vec![],
        };
        let _ = socket
            .send(AxumMsg::Text(serde_json::to_string(&hello).unwrap().into()))
            .await;

        let mut revision = 1u32;
        while let Some(Ok(AxumMsg::Text(text))) = socket.recv().await {
            let cmd: Value = serde_json::from_str(&text).unwrap();
            state.commands.lock().unwrap().push(cmd.clone());
            let rid = cmd["requestId"].as_str().unwrap_or("").to_owned();
            match cmd["type"].as_str().unwrap_or("") {
                "call.ready" => {
                    let res = CallEvent::CommandResult {
                        request_id: rid,
                        status: CallCommandStatus::Accepted,
                        call_id: None,
                        owner_token: None,
                        code: None,
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&res).unwrap().into()))
                        .await;
                    let avail = CallEvent::Availability {
                        availability: CallsAvailability {
                            busy: false,
                            owned_by_this_client: false,
                        },
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&avail).unwrap().into()))
                        .await;
                }
                "call.dial" => {
                    let call_type = if cmd["callType"] == "video" {
                        CallType::Video
                    } else {
                        CallType::Voice
                    };
                    let res = CallEvent::CommandResult {
                        request_id: rid,
                        status: CallCommandStatus::Accepted,
                        call_id: Some("call-mock-1".into()),
                        owner_token: Some("secret-owner-tok".into()),
                        code: None,
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&res).unwrap().into()))
                        .await;
                    let ack = CallEvent::PeerStatus {
                        call_id: "call-mock-1".into(),
                        participant_id: "wxid_bob".into(),
                        status: CallParticipantStatus::from_code(3),
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&ack).unwrap().into()))
                        .await;
                    revision += 1;
                    let connected = CallEvent::State {
                        call: CallView {
                            call_id: "call-mock-1".into(),
                            revision,
                            kind: CallKind::Direct,
                            call_type,
                            direction: CallDirection::Outgoing,
                            state: CallState::Connected,
                            group_id: None,
                            participants: vec![CallParticipant {
                                participant_id: "wxid_bob".into(),
                                contact_id: "wxid_bob".into(),
                                membership: CallMembership::Joined,
                                microphone_enabled: Some(true),
                                camera_enabled: Some(call_type == CallType::Video),
                                status: Some(CallParticipantStatus::from_code(1)),
                            }],
                            owned_by_this_client: true,
                            controls: vec![],
                        },
                    };
                    let _ = socket
                        .send(AxumMsg::Text(
                            serde_json::to_string(&connected).unwrap().into(),
                        ))
                        .await;
                    let joined = CallEvent::PeerStatus {
                        call_id: "call-mock-1".into(),
                        participant_id: "wxid_bob".into(),
                        status: CallParticipantStatus::from_code(1),
                    };
                    let _ = socket
                        .send(AxumMsg::Text(
                            serde_json::to_string(&joined).unwrap().into(),
                        ))
                        .await;
                }
                "call.mute" => {
                    let muted = cmd["muted"].as_bool().unwrap_or(false);
                    let res = CallEvent::CommandResult {
                        request_id: rid,
                        status: CallCommandStatus::Accepted,
                        call_id: Some("call-mock-1".into()),
                        owner_token: None,
                        code: None,
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&res).unwrap().into()))
                        .await;
                    // Simulate peer also toggling microphone back so we test call.control delivery.
                    let ctrl = CallEvent::Control {
                        call_id: "call-mock-1".into(),
                        participant_id: "wxid_bob".into(),
                        control: CallControl::Microphone { muted },
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&ctrl).unwrap().into()))
                        .await;
                }
                "call.hangup" | "call.cancel" => {
                    let res = CallEvent::CommandResult {
                        request_id: rid,
                        status: CallCommandStatus::Accepted,
                        call_id: Some("call-mock-1".into()),
                        owner_token: None,
                        code: None,
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&res).unwrap().into()))
                        .await;
                    revision += 1;
                    let ended = CallEvent::State {
                        call: CallView {
                            call_id: "call-mock-1".into(),
                            revision,
                            kind: CallKind::Direct,
                            call_type: CallType::Video,
                            direction: CallDirection::Outgoing,
                            state: CallState::Ended,
                            group_id: None,
                            participants: vec![],
                            owned_by_this_client: false,
                            controls: vec![],
                        },
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&ended).unwrap().into()))
                        .await;
                    let avail = CallEvent::Availability {
                        availability: CallsAvailability {
                            busy: false,
                            owned_by_this_client: false,
                        },
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&avail).unwrap().into()))
                        .await;
                }
                _ => {
                    let res = CallEvent::CommandResult {
                        request_id: rid,
                        status: CallCommandStatus::Accepted,
                        call_id: None,
                        owner_token: None,
                        code: None,
                    };
                    let _ = socket
                        .send(AxumMsg::Text(serde_json::to_string(&res).unwrap().into()))
                        .await;
                }
            }
        }
    }

    async fn mock_media_ws(ws: WebSocketUpgrade) -> impl IntoResponse {
        ws.on_upgrade(handle_mock_media)
    }

    async fn handle_mock_media(mut socket: WebSocket) {
        let _ = socket.recv().await; // media.attach
        let ready = CallEvent::MediaReady {
            protocol_version: CALL_PROTOCOL_VERSION,
            send_tracks: vec![
                CallTrack::Audio {
                    id: 1,
                    generation: 1,
                    direction: CallTrackDirection::Send,
                    format: CallAudioFormat::default(),
                    participant_id: None,
                },
                CallTrack::Video {
                    id: 65_537,
                    generation: 1,
                    direction: CallTrackDirection::Send,
                    format: CallVideoFormat {
                        codec: CallVideoCodec::Jpeg,
                        mode: CallVideoMode::Transcoded,
                        max_width: 320,
                        max_height: 240,
                        max_fps: 10,
                        bitrate_kbps: None,
                    },
                    participant_id: None,
                    active: true,
                },
            ],
            receive_tracks: vec![
                CallTrack::Audio {
                    id: 2,
                    generation: 1,
                    direction: CallTrackDirection::Receive,
                    format: CallAudioFormat::default(),
                    participant_id: Some("wxid_bob".into()),
                },
                CallTrack::Video {
                    id: 65_538,
                    generation: 1,
                    direction: CallTrackDirection::Receive,
                    format: CallVideoFormat {
                        codec: CallVideoCodec::Jpeg,
                        mode: CallVideoMode::Transcoded,
                        max_width: 320,
                        max_height: 240,
                        max_fps: 10,
                        bitrate_kbps: None,
                    },
                    participant_id: Some("wxid_bob".into()),
                    active: true,
                },
            ],
        };
        let _ = socket
            .send(AxumMsg::Text(serde_json::to_string(&ready).unwrap().into()))
            .await;

        // Echo incoming TX PCM back on RX audio track 2, and inject a green JPEG on RX video track 65538.
        let green = test_jpeg(20, 210, 30);
        let mut video_seq = 0u64;
        let mut rx_audio_seq = 0u64;
        while let Some(Ok(msg)) = socket.recv().await {
            if let AxumMsg::Binary(bytes) = msg {
                if let Ok(wire) = WireMediaFrame::decode(&bytes) {
                    if wire.kind == MediaPayloadKind::PcmS16le {
                        let echoed = encode_pcm_chunk(
                            MediaHeader {
                                track_id: 2,
                                generation: 1,
                                sequence: rx_audio_seq,
                                timestamp_micros: rx_audio_seq * 20_000,
                            },
                            wire.payload(),
                        )
                        .unwrap();
                        rx_audio_seq += 1;
                        let _ = socket.send(AxumMsg::Binary(echoed.into())).await;
                        if rx_audio_seq % 3 == 1 {
                            let vframe = encode_video(
                                MediaPayloadKind::Jpeg,
                                FLAG_KEYFRAME,
                                MediaHeader {
                                    track_id: 65_538,
                                    generation: 1,
                                    sequence: video_seq,
                                    timestamp_micros: video_seq * 100_000,
                                },
                                &green,
                            )
                            .unwrap();
                            video_seq += 1;
                            let _ = socket.send(AxumMsg::Binary(vframe.into())).await;
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn session_end_to_end_ipc_dial_tone_fft_video_snapshot_record_and_hangup() {
        let state = MockDaemonState::default();
        let router = Router::new()
            .route(
                "/v1/accounts/test-account/calls/socket",
                get(mock_control_ws),
            )
            .route(
                "/v1/accounts/test-account/calls/{call_id}/media",
                get(mock_media_ws),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        let temp = tempfile::tempdir().unwrap();
        let session_dir = temp.path().join("sess");
        let rec_dir = temp.path().join("rec");
        let snap_path = temp.path().join("frame.jpg");

        let remote = super::super::tests::remote(addr);
        let opts = SessionRunOptions {
            name: "agent-a".into(),
            dir: Some(session_dir.clone()),
            ready: true,
            audio: "tone:440:0.3".into(),
            output: "null".into(),
            video: "off".into(),
            receive_video_codec: "jpeg".into(),
            record_dir: Some(rec_dir.clone()),
            snapshot_interval: 0,
            stats_interval: 150,
            idle_timeout: 60,
        };

        let runner = tokio::spawn(async move {
            run_foreground(&remote, opts, false, false, None)
                .await
                .unwrap();
        });

        // Wait until session IPC accepts `status`.
        let mut ready_status = None;
        for _ in 0..40 {
            if let Ok(st) = ipc_call_dir(
                &session_dir,
                json!({ "op": "status" }),
                Duration::from_millis(400),
            )
            .await
            {
                if st["session"]["ready"] == true {
                    ready_status = Some(st);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let st = ready_status.expect("session should become ready");
        assert_eq!(st["session"]["name"], "agent-a");
        assert_eq!(st["session"]["audioSource"], "tone:440:0.3");

        // Dial a video call (camera off because video source is off, still receives video).
        let dial_res = ipc_call_dir(
            &session_dir,
            json!({
                "op": "dial",
                "contact": "wxid_bob",
                "video": true,
                "cameraOff": true
            }),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(dial_res["callId"], "call-mock-1");
        let after = dial_res["eventSeq"].as_u64().unwrap();

        // Wait for peer_status: acknowledged, then state: connected, then FFT ~440 Hz + video frames.
        let ack_wait = ipc_call_dir(
            &session_dir,
            json!({
                "op": "wait",
                "after": after,
                "timeout": 5,
                "peerStatuses": ["acknowledged"]
            }),
            Duration::from_secs(6),
        )
        .await
        .unwrap();
        assert_eq!(ack_wait["event"]["type"], "call.peer_status");
        assert_eq!(ack_wait["event"]["status"]["kind"], "acknowledged");

        let media_wait = ipc_call_dir(
            &session_dir,
            json!({
                "op": "wait",
                "after": after,
                "timeout": 5,
                "audioFrames": 10,
                "dominantHz": 440.0,
                "hzTolerance": 25.0,
                "videoFrames": 2
            }),
            Duration::from_secs(6),
        )
        .await
        .unwrap();
        assert_eq!(media_wait["matched"], true);

        // Save snapshot and verify green pixels & file created.
        let snap_res = ipc_call_dir(
            &session_dir,
            json!({ "op": "snapshot", "out": snap_path.display().to_string() }),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(snap_path.is_file());
        let mean_rgb = snap_res["snapshots"][0]["meanRgb"].as_array().unwrap();
        assert!(mean_rgb[1].as_f64().unwrap() > 180.0);
        assert!(mean_rgb[0].as_f64().unwrap() < 60.0);

        // Mute and wait for peer call.control echo.
        let mute_res = ipc_call_dir(
            &session_dir,
            json!({ "op": "mute", "muted": true }),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(mute_res["muted"], true);
        let ctrl_wait = ipc_call_dir(
            &session_dir,
            json!({
                "op": "wait",
                "after": mute_res["eventSeq"],
                "timeout": 5,
                "controlKind": "microphone",
                "controlValue": true
            }),
            Duration::from_secs(6),
        )
        .await
        .unwrap();
        assert_eq!(ctrl_wait["event"]["control"]["muted"], true);

        // Hangup cleanly and confirm recording manifest & WAVs were written.
        let hangup_res = ipc_call_dir(
            &session_dir,
            json!({ "op": "hangup" }),
            Duration::from_secs(8),
        )
        .await
        .unwrap();
        assert_eq!(hangup_res["ended"], true);
        let manifest_path = hangup_res["recording"]["manifest"].as_str().unwrap();
        assert!(Path::new(manifest_path).is_file());
        let rx_wav = hangup_res["recording"]["audioRxMixed"].as_str().unwrap();
        assert!(fs::metadata(rx_wav).unwrap().len() > 44);

        // Verify no secret ownerToken ever leaked into events.jsonl.
        let events_log = fs::read_to_string(session_dir.join("events.jsonl")).unwrap();
        assert!(!events_log.contains("secret-owner-tok"));
        assert!(!events_log.contains("ownerToken"));

        // Stop session cleanly.
        let stop_res = ipc_call_dir(
            &session_dir,
            json!({ "op": "stop", "hangup": false }),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(stop_res["state"], "stopped");

        runner.await.unwrap();
        let ready_count = state
            .commands
            .lock()
            .unwrap()
            .iter()
            .filter(|cmd| cmd["type"] == "call.ready")
            .count();
        assert_eq!(
            ready_count, 1,
            "calls.availability must not trigger repeated call.ready commands"
        );
        server.abort();
    }

    #[test]
    fn cli_parses_negative_rms_protocol_enums_and_run_only_exit_with_parent() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(subcommand)]
            cmd: SessionCommand,
        }

        let parsed = Wrapper::try_parse_from([
            "session",
            "wait",
            "--min-rms-db",
            "-35.5",
            "--state",
            "offered",
            "--peer-status",
            "invited",
            "--peer-status",
            "cancelled",
            "--participant-status",
            "undefined",
            "--membership",
            "unknown",
        ])
        .expect("valid wait args should parse");
        match parsed.cmd {
            SessionCommand::Wait(args) => {
                assert_eq!(args.min_rms_db, Some(-35.5));
                assert_eq!(args.states, vec!["offered"]);
                assert_eq!(args.peer_statuses, vec!["invited", "cancelled"]);
                assert_eq!(args.participant_status.as_deref(), Some("undefined"));
                assert_eq!(args.membership.as_deref(), Some("unknown"));
            }
            _ => panic!("expected wait command"),
        }

        // `--exit-with-parent` is accepted on foreground `run` and rejected on detached `start`.
        assert!(Wrapper::try_parse_from(["session", "run", "--exit-with-parent"]).is_ok());
        assert!(Wrapper::try_parse_from(["session", "start", "--exit-with-parent"]).is_err());
    }

    #[tokio::test]
    async fn wait_preserves_buffered_event_for_later_media_and_enforces_call_id() {
        let bus = SharedEventBus::new(None, None).unwrap();
        let ev_seq = bus.emit(json!({
            "type": "call.state",
            "call": { "callId": "call-2", "state": "connected" }
        }));
        assert_eq!(ev_seq, 1);

        // Publish stale/other-call media first; a combined wait for call-2 must not match it,
        // and must preserve the already-buffered `call.state` until call-2's media arrives.
        let mut wrong_inspector = CallMediaInspector::new(16_000);
        let now = Instant::now();
        let tone: Vec<i16> = (0..320)
            .map(|i| ((std::f64::consts::TAU * 440.0 * i as f64 / 16_000.0).sin() * 10000.0) as i16)
            .collect();
        for _ in 0..12 {
            wrong_inspector.observe_receive_audio(2, None, &tone, now);
        }
        let report = wrong_inspector.report(now);
        bus.publish_media(Some(("call-1".into(), report.clone())));

        let bus_c = bus.clone();
        let wait_task = tokio::spawn(async move {
            run_wait_request(
                &bus_c,
                &json!({
                    "after": 0,
                    "timeout": 3,
                    "callId": "call-2",
                    "states": ["connected"],
                    "audioFrames": 10
                }),
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!wait_task.is_finished(), "must not match call-1 media");
        bus.publish_media(Some(("call-2".into(), report)));

        let matched = wait_task.await.unwrap().unwrap();
        assert_eq!(matched["matched"], true);
        assert_eq!(matched["event"]["call"]["callId"], "call-2");
        assert!(matched.get("media").is_some());
    }

    #[tokio::test]
    async fn ipc_stream_rejects_oversized_line_without_unbounded_buffering() {
        let bus = SharedEventBus::new(None, None).unwrap();
        let (cmd_tx, _cmd_rx) = mpsc::channel(4);
        let oversized = format!("{}\n", "x".repeat(MAX_IPC_LINE_BYTES + 256));
        let (mut client_sock, server_sock) = tokio::io::duplex(4096);
        let handler = tokio::spawn(async move {
            let (r, w) = tokio::io::split(server_sock);
            handle_ipc_stream(r, w, bus, cmd_tx, None).await;
        });
        let write_task = tokio::spawn(async move {
            let _ = client_sock.write_all(oversized.as_bytes()).await;
            let mut reply = String::new();
            let _ = client_sock.read_to_string(&mut reply).await;
            reply
        });
        handler.await.unwrap();
        let reply = write_task.await.unwrap();
        assert!(reply.contains("exceeds size limit"), "got reply: {reply}");
    }
}
