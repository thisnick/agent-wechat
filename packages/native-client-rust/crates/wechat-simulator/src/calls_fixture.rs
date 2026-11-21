//! Synthetic cross-language fixture for the real authenticated call API/broker.
//! No native upstream, profile login, account workers or host audio is started.
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    future::Future,
    io::Write,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    sync::{oneshot, watch},
};
use wechat_core::{
    call_types::{
        CallAudioFormat, CallCapabilities, CallConferenceInvitation, CallErrorCode,
        CallInvitationResult, CallInvitationStatus, CallMembership, CallParticipant, CallTrack,
        CallTrackDirection, CallType, CallVideoCodec, CallVideoFormat, CallVideoMode,
        VIDEO_RECEIVE_TRACK_BASE,
    },
    calls::{
        ownership::{AccountAuthority, CallIdentity, Operation},
        pcm_envelope::{FLAG_CODEC_CONFIG, FLAG_KEYFRAME},
        service::{
            BackendAudioRequest, BackendInviteRequest, BackendOutcome, BackendRequest, BackendSink,
            BackendVideoRequest, CallBackend, CallServiceError,
        },
    },
    upstream::{Upstream, UpstreamKind},
    Core, Store,
};
use wechat_simulator::workspace;

const ACCOUNT: &str = "calls-fixture";
const GENERATION: &str = "synthetic-fixture-generation";
const MAX_LINE: usize = 512 * 1024;
type BackendResult<T> = std::result::Result<T, CallServiceError>;
type BackendFuture<T> = Pin<Box<dyn Future<Output = BackendResult<T>> + Send>>;

// The trait's default native() fails closed. Even unrelated authenticated API
// routes cannot construct a native transport from this synthetic Store.
struct NoNetworkUpstream;
impl Upstream for NoNetworkUpstream {
    fn kind(&self) -> UpstreamKind {
        UpstreamKind::Simulator
    }
}

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Stats {
    dial: u64,
    answer: u64,
    reject: u64,
    cancel: u64,
    hangup: u64,
    audio_frames: u64,
    audio_samples: u64,
    nonzero_frames: u64,
    last_tx_generation: u32,
    last_tx_peak: u32,
    clear_requested: u64,
    clear_completed: u64,
    stop: u64,
    active: bool,
    invitations: u64,
    video_frames: u64,
    video_bytes: u64,
    video_keyframes: u64,
    last_video_generation: u32,
    camera_enabled: bool,
    muted: bool,
    camera_toggles: u64,
    mute_toggles: u64,
    keyframe_requests: u64,
    video_clear_requested: u64,
}
struct Active {
    identity: CallIdentity,
    sink: BackendSink,
    conference: bool,
}
struct BackendState {
    conference: bool,
    conference_group_id: String,
    conference_caller_id: String,
    video: bool,
    echo_audio: bool,
    echo_video: bool,
    stats: Stats,
    active: Option<Active>,
}
struct Backend {
    state: Arc<Mutex<BackendState>>,
    hold_clear: watch::Sender<bool>,
    hold_end: watch::Sender<bool>,
}
impl Backend {
    fn new() -> Self {
        let (hold_clear, _) = watch::channel(false);
        let (hold_end, _) = watch::channel(false);
        Self {
            state: Arc::new(Mutex::new(BackendState {
                conference: false,
                conference_group_id: "synthetic-group@chatroom".into(),
                conference_caller_id: "synthetic-caller".into(),
                video: false,
                echo_audio: true,
                echo_video: false,
                stats: Stats::default(),
                active: None,
            })),
            hold_clear,
            hold_end,
        }
    }
    fn active_sink(&self) -> BackendResult<BackendSink> {
        self.state
            .lock()
            .map_err(|_| CallServiceError(CallErrorCode::Closed))?
            .active
            .as_ref()
            .map(|active| active.sink.clone())
            .ok_or(CallServiceError(CallErrorCode::MediaNotReady))
    }
    fn stats(&self) -> BackendResult<Stats> {
        let state = self
            .state
            .lock()
            .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
        let mut stats = state.stats.clone();
        stats.active = state.active.is_some();
        Ok(stats)
    }
    async fn end(&self) -> BackendResult<()> {
        let (identity, sink) = {
            let state = self
                .state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            let active = state
                .active
                .as_ref()
                .ok_or(CallServiceError(CallErrorCode::MediaNotReady))?;
            (active.identity.clone(), active.sink.clone())
        };
        sink.ended().await?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.identity == identity)
        {
            state.active = None;
        }
        Ok(())
    }
}
impl CallBackend for Backend {
    fn authority(&self, account: &str) -> Option<AccountAuthority> {
        (account == ACCOUNT).then(|| AccountAuthority {
            account_id: ACCOUNT.into(),
            generation: GENERATION.into(),
        })
    }
    fn capabilities(&self, account: &str) -> CallCapabilities {
        let (conference, video) = self
            .state
            .lock()
            .map(|s| (s.conference, s.video))
            .unwrap_or((false, false));
        CallCapabilities {
            direct_audio: account == ACCOUNT,
            direct_video: account == ACCOUNT && video,
            conference_create: account == ACCOUNT && conference,
            conference_join: account == ACCOUNT && conference,
            conference_invite: account == ACCOUNT && conference,
            conference_audio: account == ACCOUNT && conference,
            conference_video: account == ACCOUNT && conference && video,
        }
    }
    fn conference_invitations(
        &self,
        authority: &AccountAuthority,
    ) -> Vec<CallConferenceInvitation> {
        if authority.account_id == ACCOUNT {
            if let Ok(state) = self.state.lock() {
                if state.conference {
                    return vec![CallConferenceInvitation {
                        invitation_id: "synthetic-invitation".into(),
                        revision: 1,
                        group_id: state.conference_group_id.clone(),
                        caller_id: state.conference_caller_id.clone(),
                    }];
                }
            }
        }
        vec![]
    }
    fn invite(&self, request: BackendInviteRequest) -> BackendFuture<Vec<CallInvitationResult>> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            ensure_fixture_owner(&state, &request.identity)?;
            state.stats.invitations += request.contacts.len() as u64;
            Ok(request
                .contacts
                .into_iter()
                .map(|contact_id| CallInvitationResult {
                    contact_id,
                    status: CallInvitationStatus::Submitted,
                    service_code: Some(0),
                    verification_required: false,
                })
                .collect())
        })
    }
    fn submit(&self, request: BackendRequest, sink: BackendSink) -> BackendFuture<BackendOutcome> {
        let state = self.state.clone();
        let mut held_end = self.hold_end.subscribe();
        Box::pin(async move {
            let starting = matches!(
                request.ticket.operation(),
                Operation::Dial | Operation::Answer
            );
            if starting && !sink.is_current() {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            {
                let mut state = state
                    .lock()
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
                let counter = match request.ticket.operation() {
                    Operation::Dial => &mut state.stats.dial,
                    Operation::Answer => &mut state.stats.answer,
                    Operation::Reject => &mut state.stats.reject,
                    Operation::Cancel => &mut state.stats.cancel,
                    Operation::Hangup => &mut state.stats.hangup,
                };
                *counter = counter.saturating_add(1);
                if starting {
                    state.stats.camera_enabled = request.camera_enabled;
                    state.stats.muted = request.muted;
                    state.active = Some(Active {
                        identity: request.ticket.identity(),
                        conference: request.peer_id.ends_with("@chatroom"),
                        sink,
                    });
                    return Ok(BackendOutcome::Connected);
                }
            }
            while *held_end.borrow_and_update() {
                held_end
                    .changed()
                    .await
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            }
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.identity == request.ticket.identity())
            {
                state.active = None;
            }
            Ok(BackendOutcome::Ended)
        })
    }
    fn send_audio(&self, request: BackendAudioRequest) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            if !request.is_current() {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            let (sink, echo_audio) = {
                let mut state = state
                    .lock()
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
                let active = state
                    .active
                    .as_ref()
                    .filter(|active| active.identity == request.identity)
                    .ok_or(CallServiceError(CallErrorCode::Stale))?;
                let sink = active.sink.clone();
                let echo_audio = state.echo_audio && !active.conference;
                state.stats.audio_frames = state.stats.audio_frames.saturating_add(1);
                state.stats.audio_samples = state
                    .stats
                    .audio_samples
                    .saturating_add(request.samples.len() as u64);
                if request.samples.iter().any(|sample| *sample != 0) {
                    state.stats.nonzero_frames = state.stats.nonzero_frames.saturating_add(1);
                }
                state.stats.last_tx_generation = request.generation;
                state.stats.last_tx_peak = request
                    .samples
                    .iter()
                    .map(|sample| u32::from(sample.unsigned_abs()))
                    .max()
                    .unwrap_or(0);
                (sink, echo_audio)
            };
            if echo_audio {
                sink.audio(&request.samples).await
            } else {
                Ok(())
            }
        })
    }
    fn clear_audio(&self, _: CallIdentity, _: u32) -> BackendFuture<()> {
        let state = self.state.clone();
        let mut held = self.hold_clear.subscribe();
        Box::pin(async move {
            {
                let mut state = state
                    .lock()
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
                state.stats.clear_requested = state.stats.clear_requested.saturating_add(1);
            }
            while *held.borrow_and_update() {
                held.changed()
                    .await
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            }
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            state.stats.clear_completed = state.stats.clear_completed.saturating_add(1);
            Ok(())
        })
    }
    fn set_muted(&self, identity: CallIdentity, muted: bool) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            if !state
                .active
                .as_ref()
                .is_some_and(|active| active.identity == identity)
            {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            state.stats.muted = muted;
            state.stats.mute_toggles = state.stats.mute_toggles.saturating_add(1);
            Ok(())
        })
    }
    fn set_camera(&self, identity: CallIdentity, enabled: bool) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            if !state
                .active
                .as_ref()
                .is_some_and(|active| active.identity == identity)
            {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            state.stats.camera_enabled = enabled;
            state.stats.camera_toggles = state.stats.camera_toggles.saturating_add(1);
            Ok(())
        })
    }
    fn send_video(&self, request: BackendVideoRequest) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            if !request.is_current() {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            let (sink, echo, sequence) = {
                let mut state = state
                    .lock()
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
                let active = state
                    .active
                    .as_ref()
                    .filter(|active| active.identity == request.identity)
                    .ok_or(CallServiceError(CallErrorCode::Stale))?;
                let sink = active.sink.clone();
                let echo = state.echo_video && !active.conference;
                let sequence = state.stats.video_frames;
                state.stats.video_frames = state.stats.video_frames.saturating_add(1);
                state.stats.video_bytes = state
                    .stats
                    .video_bytes
                    .saturating_add(request.payload.len() as u64);
                if (request.flags & FLAG_KEYFRAME) != 0 {
                    state.stats.video_keyframes = state.stats.video_keyframes.saturating_add(1);
                }
                state.stats.last_video_generation = request.generation;
                (sink, echo, sequence)
            };
            if echo {
                let recv_codec = sink
                    .negotiated_video_formats()
                    .ok()
                    .and_then(|(_, recv)| recv.map(|f| f.codec))
                    .unwrap_or(request.codec);
                if recv_codec == request.codec {
                    let _ = sink
                        .video_frame(
                            VIDEO_RECEIVE_TRACK_BASE,
                            1,
                            request.codec,
                            request.flags & (FLAG_KEYFRAME | FLAG_CODEC_CONFIG),
                            sequence,
                            request.timestamp_micros,
                            &request.payload,
                        )
                        .await;
                }
            }
            Ok(())
        })
    }
    fn clear_video(&self, _: CallIdentity, _: u32) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            state.stats.video_clear_requested = state.stats.video_clear_requested.saturating_add(1);
            Ok(())
        })
    }
    fn request_video_keyframe(&self, identity: CallIdentity, _: u32) -> BackendFuture<()> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            if !state
                .active
                .as_ref()
                .is_some_and(|active| active.identity == identity)
            {
                return Err(CallServiceError(CallErrorCode::Stale));
            }
            state.stats.keyframe_requests = state.stats.keyframe_requests.saturating_add(1);
            Ok(())
        })
    }
    fn stop(&self, identity: CallIdentity) -> BackendFuture<BackendOutcome> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut state = state
                .lock()
                .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
            state.stats.stop = state.stats.stop.saturating_add(1);
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.identity == identity)
            {
                state.active = None;
            }
            Ok(BackendOutcome::Ended)
        })
    }
}

fn ensure_fixture_owner(state: &BackendState, identity: &CallIdentity) -> BackendResult<()> {
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.conference && active.identity == *identity)
    {
        Ok(())
    } else {
        Err(CallServiceError(CallErrorCode::NotOwner))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConferenceSource {
    track_id: u32,
    generation: u32,
    contact_id: String,
    microphone_enabled: Option<bool>,
    #[serde(default)]
    camera_enabled: Option<bool>,
    #[serde(default)]
    video_track_id: Option<u32>,
    #[serde(default)]
    video_generation: Option<u32>,
}

#[derive(Deserialize)]
struct Request {
    id: String,
    #[serde(flatten)]
    command: Command,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    ConfigureConference {
        enabled: bool,
        #[serde(rename = "groupId", default)]
        group_id: Option<String>,
        #[serde(rename = "callerId", default)]
        caller_id: Option<String>,
    },
    ConfigureAudio {
        echo: bool,
    },
    ConfigureVideo {
        enabled: bool,
        #[serde(default)]
        echo: bool,
    },
    ConferenceSources {
        sources: Vec<ConferenceSource>,
    },
    ConferenceAudio {
        #[serde(rename = "trackId")]
        track_id: u32,
        generation: u32,
        sequence: u64,
        #[serde(rename = "sampleOffset")]
        sample_offset: u64,
        #[serde(default)]
        value: i16,
        #[serde(rename = "pcmBase64", default)]
        pcm_base64: Option<String>,
    },
    Offer {
        #[serde(rename = "callerId")]
        caller_id: String,
        #[serde(rename = "callType", default)]
        call_type: Option<CallType>,
    },
    InjectAudio {
        #[serde(default)]
        value: i16,
        #[serde(default)]
        frames: u8,
        #[serde(rename = "pcmBase64", default)]
        pcm_base64: Option<String>,
    },
    InjectVideo {
        #[serde(rename = "trackId", default)]
        track_id: Option<u32>,
        #[serde(default)]
        generation: Option<u32>,
        codec: CallVideoCodec,
        #[serde(default)]
        flags: Option<u16>,
        #[serde(default)]
        sequence: Option<u64>,
        #[serde(rename = "timestampMicros", default)]
        timestamp_micros: Option<u64>,
        #[serde(rename = "payloadBase64")]
        payload_base64: String,
    },
    PeerMicrophone {
        muted: bool,
    },
    PeerCamera {
        enabled: bool,
    },
    PeerKeyframeRequested,
    BeginEnd,
    End,
    Stats,
    HoldClear {
        enabled: bool,
    },
    ReleaseClear,
    HoldEnd {
        enabled: bool,
    },
    ReleaseEnd,
    Shutdown,
}

fn emit(value: &Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

// Bound allocation before copying a chunk, including a line without a newline.
async fn input_line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8(line)?))
            };
        }
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        ensure!(line.len() + length <= MAX_LINE, "fixture_input_too_large");
        let complete = available[length - 1] == b'\n';
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        if complete {
            return Ok(Some(String::from_utf8(line)?));
        }
    }
}

async fn control(
    core: &Arc<Core>,
    backend: &Backend,
    command: Command,
) -> Result<Value, &'static str> {
    match command {
        Command::ConfigureConference {
            enabled,
            group_id,
            caller_id,
        } => {
            let mut state = backend.state.lock().map_err(|_| "fixture_closed")?;
            if state.active.is_some() {
                return Err("fixture_busy");
            }
            state.conference = enabled;
            if let Some(group_id) = group_id {
                state.conference_group_id = group_id;
            }
            if let Some(caller_id) = caller_id {
                state.conference_caller_id = caller_id;
            }
            Ok(json!({"enabled":enabled}))
        }
        Command::ConfigureAudio { echo } => {
            let mut state = backend.state.lock().map_err(|_| "fixture_closed")?;
            state.echo_audio = echo;
            Ok(json!({"echo":echo}))
        }
        Command::ConfigureVideo { enabled, echo } => {
            let mut state = backend.state.lock().map_err(|_| "fixture_closed")?;
            if state.active.is_some() {
                return Err("fixture_busy");
            }
            state.video = enabled;
            state.echo_video = echo;
            Ok(json!({"enabled":enabled,"echo":echo}))
        }
        Command::ConferenceSources { sources } => {
            if sources.len() > 32
                || sources.iter().any(|s| {
                    s.track_id < 3
                        || s.generation == 0
                        || s.contact_id.len() > 100
                        || s.contact_id.chars().any(char::is_control)
                })
            {
                return Err("invalid_sources");
            }
            let sink = backend.active_sink().map_err(|e| e.code())?;
            sink.participants(
                sources
                    .iter()
                    .map(|source| CallParticipant {
                        participant_id: format!("participant-{}", source.track_id),
                        contact_id: source.contact_id.clone(),
                        membership: CallMembership::Joined,
                        status: None,
                        microphone_enabled: source.microphone_enabled,
                        camera_enabled: source.camera_enabled,
                    })
                    .collect(),
            )
            .await
            .map_err(|e| e.code())?;
            sink.receive_tracks(
                sources
                    .iter()
                    .map(|source| CallTrack::Audio {
                        id: source.track_id,
                        generation: source.generation,
                        direction: CallTrackDirection::Receive,
                        format: CallAudioFormat::default(),
                        participant_id: Some(format!("participant-{}", source.track_id)),
                    })
                    .collect(),
            )
            .await
            .map_err(|e| e.code())?;
            let recv_video_format = sink
                .negotiated_video_formats()
                .ok()
                .and_then(|(_, recv)| recv)
                .unwrap_or(CallVideoFormat {
                    codec: CallVideoCodec::H265,
                    mode: CallVideoMode::Passthrough,
                    max_width: 640,
                    max_height: 480,
                    max_fps: 15,
                    bitrate_kbps: None,
                });
            let video_tracks: Vec<CallTrack> = sources
                .iter()
                .filter(|source| source.camera_enabled == Some(true))
                .map(|source| CallTrack::Video {
                    id: source
                        .video_track_id
                        .unwrap_or(VIDEO_RECEIVE_TRACK_BASE + source.track_id),
                    generation: source.video_generation.unwrap_or(source.generation),
                    direction: CallTrackDirection::Receive,
                    format: recv_video_format,
                    participant_id: Some(format!("participant-{}", source.track_id)),
                    active: true,
                })
                .collect();
            sink.receive_video_tracks(video_tracks)
                .await
                .map_err(|e| e.code())?;
            Ok(json!({"sources":sources.len()}))
        }
        Command::ConferenceAudio {
            track_id,
            generation,
            sequence,
            sample_offset,
            value,
            pcm_base64,
        } => {
            let micros = sample_offset
                .checked_mul(1_000_000)
                .ok_or("clock_overflow")?
                / 16000;
            let samples = match pcm_base64 {
                Some(encoded) => {
                    let raw = BASE64.decode(encoded).map_err(|_| "invalid_payload")?;
                    if raw.len() != 640 {
                        return Err("invalid_payload");
                    }
                    raw.chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect::<Vec<_>>()
                }
                None => vec![value; 320],
            };
            backend
                .active_sink()
                .map_err(|e| e.code())?
                .track_audio(
                    wechat_core::calls::pcm_envelope::MediaHeader {
                        track_id,
                        generation,
                        sequence,
                        timestamp_micros: micros,
                    },
                    &samples,
                )
                .await
                .map_err(|e| e.code())?;
            Ok(json!({"delivered":true}))
        }
        Command::Offer {
            caller_id,
            call_type,
        } => {
            if caller_id.is_empty()
                || caller_id.len() > 200
                || caller_id.chars().any(char::is_control)
            {
                return Err("invalid_caller");
            }
            let authority = backend.authority(ACCOUNT).ok_or("fixture_unavailable")?;
            let identity = core
                .call_service(ACCOUNT)
                .map_err(|_| "fixture_unavailable")?
                .offer_with_type(
                    authority,
                    uuid::Uuid::new_v4().to_string(),
                    caller_id,
                    call_type.unwrap_or(CallType::Voice),
                    Duration::from_secs(60),
                )
                .await
                .map_err(|error| error.code())?;
            Ok(json!({"callId":identity.call_id(),"revision":1}))
        }
        Command::InjectAudio {
            value,
            frames,
            pcm_base64,
        } => {
            let sink = backend.active_sink().map_err(|error| error.code())?;
            if let Some(encoded) = pcm_base64 {
                let raw = BASE64.decode(encoded).map_err(|_| "invalid_payload")?;
                if raw.is_empty() || raw.len() % 640 != 0 || raw.len() > 4 * 640 {
                    return Err("invalid_payload");
                }
                let count = raw.len() / 640;
                for chunk in raw.chunks_exact(640) {
                    let samples: Vec<i16> = chunk
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    sink.audio(&samples).await.map_err(|error| error.code())?;
                }
                return Ok(json!({"frames":count}));
            }
            if !(1..=4).contains(&frames) {
                return Err("invalid_frame_count");
            }
            for _ in 0..frames {
                sink.audio(&[value; 320])
                    .await
                    .map_err(|error| error.code())?;
            }
            Ok(json!({"frames":frames}))
        }
        Command::InjectVideo {
            track_id,
            generation,
            codec,
            flags,
            sequence,
            timestamp_micros,
            payload_base64,
        } => {
            let payload = BASE64
                .decode(payload_base64)
                .map_err(|_| "invalid_payload")?;
            if payload.is_empty() || payload.len() > 512 * 1024 {
                return Err("invalid_payload");
            }
            let sink = backend.active_sink().map_err(|error| error.code())?;
            sink.video_frame(
                track_id.unwrap_or(VIDEO_RECEIVE_TRACK_BASE),
                generation.unwrap_or(1),
                codec,
                flags.unwrap_or(FLAG_KEYFRAME),
                sequence.unwrap_or(0),
                timestamp_micros.unwrap_or(0),
                &payload,
            )
            .await
            .map_err(|error| error.code())?;
            Ok(json!({"bytes":payload.len()}))
        }
        Command::PeerMicrophone { muted } => {
            backend
                .active_sink()
                .map_err(|error| error.code())?
                .peer_microphone(muted)
                .await
                .map_err(|error| error.code())?;
            Ok(json!({"muted":muted}))
        }
        Command::PeerCamera { enabled } => {
            backend
                .active_sink()
                .map_err(|error| error.code())?
                .peer_camera(enabled)
                .await
                .map_err(|error| error.code())?;
            Ok(json!({"enabled":enabled}))
        }
        Command::PeerKeyframeRequested => {
            backend
                .active_sink()
                .map_err(|error| error.code())?
                .peer_keyframe_requested()
                .await
                .map_err(|error| error.code())?;
            Ok(json!({"requested":true}))
        }
        Command::BeginEnd => {
            backend
                .active_sink()
                .map_err(|error| error.code())?
                .ending()
                .await
                .map_err(|error| error.code())?;
            Ok(json!({}))
        }
        Command::End => {
            backend.end().await.map_err(|error| error.code())?;
            Ok(json!({}))
        }
        Command::Stats => Ok(json!({"stats":backend.stats().map_err(|error|error.code())?})),
        Command::HoldClear { enabled } => {
            backend.hold_clear.send_replace(enabled);
            Ok(json!({"held":enabled}))
        }
        Command::ReleaseClear => {
            backend.hold_clear.send_replace(false);
            Ok(json!({"held":false}))
        }
        Command::HoldEnd { enabled } => {
            backend.hold_end.send_replace(enabled);
            Ok(json!({"held":enabled}))
        }
        Command::ReleaseEnd => {
            backend.hold_end.send_replace(false);
            Ok(json!({"held":false}))
        }
        Command::Shutdown => Ok(json!({"shutdown":true})),
    }
}

async fn run(root: &Path) -> Result<()> {
    let root = workspace::prepare(root)?;
    let store = Arc::new(Store::open(&workspace::hub_directory(&root)?)?);
    store.create_account(ACCOUNT)?;
    // Model a previous process disappearing after journaling an invitation.
    // The NoNetworkUpstream below makes any attempted replay fail closed.
    let prior_call = "synthetic-restart-orphan";
    store.begin_call(ACCOUNT, prior_call, "previous-process")?;
    let vault = wechat_store::vault::Vault::open(&store.root.join("storage-key"), true)?;
    let request = b"synthetic retained invitation; never submit";
    let sealed = vault.seal(
        ACCOUNT,
        &format!("call-request:{prior_call}:invite"),
        request,
    )?;
    {
        use sha2::{Digest, Sha256};
        store.begin_call_action(
            ACCOUNT,
            prior_call,
            "invite",
            10342,
            &format!("{:x}", Sha256::digest(request)),
            &sealed,
        )?;
    }
    let token_file = root.join("api-token");
    let token = wechat_api::provision_token(&token_file, true)?;
    let calls_token_file = root.join("calls-token");
    let calls_token = wechat_api::provision_token(&calls_token_file, true)?;
    let non_calls_token_file = root.join("non-calls-token");
    let non_calls_token = wechat_api::provision_token(&non_calls_token_file, true)?;
    {
        use sha2::{Digest, Sha256};
        for (name, token, scope) in [
            ("call fixture participant", &calls_token, "calls"),
            ("call fixture non-participant", &non_calls_token, "read"),
        ] {
            store.issue_token(
                name,
                &format!("{:x}", Sha256::digest(token)),
                &[ACCOUNT.into()],
                &[scope.into()],
                None,
            )?;
        }
    }
    let core = Core::with_upstream(store, None, Arc::new(NoNetworkUpstream))?;
    let recovered = core
        .store
        .call_session_status(ACCOUNT, prior_call)?
        .context("restart session")?;
    ensure!(
        recovered.local_closed
            && !recovered.remote_terminal_confirmed
            && recovered.local_end_reason.as_deref() == Some("server_restart"),
        "restart recovery mismatch"
    );
    let retained = core.store.call_action_records(ACCOUNT, prior_call)?;
    ensure!(
        retained.len() == 1
            && retained[0].outcome == "needs_review"
            && retained[0].encrypted_request.as_deref() == Some(sealed.as_slice()),
        "restart receipt changed"
    );
    let backend = Arc::new(Backend::new());
    Core::configure_calls(&core, backend.clone())?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (shutdown, stopped) = oneshot::channel();
    let router = wechat_api::router(core.clone(), &token);
    let mut server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    emit(
        &json!({"event":"ready","baseUrl":format!("http://{address}"),"accountId":ACCOUNT,
        "tokenFile":token_file,"callsTokenFile":calls_token_file,"nonCallsTokenFile":non_calls_token_file,
        "restartRecovery":{"localClosed":true,"remoteConfirmed":false,"requestOutcome":"needs_review"}}),
    )?;
    let mut input = BufReader::new(tokio::io::stdin());
    while let Some(line) = input_line(&mut input).await? {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request)
                if !request.id.is_empty()
                    && request.id.len() <= 100
                    && !request.id.chars().any(char::is_control) =>
            {
                request
            }
            _ => {
                emit(&json!({"id":null,"ok":false,"code":"invalid_request"}))?;
                continue;
            }
        };
        let stopping = matches!(request.command, Command::Shutdown);
        let response = match control(&core, &backend, request.command).await {
            Ok(mut fields) => {
                fields["id"] = request.id.into();
                fields["ok"] = true.into();
                fields
            }
            Err(code) => json!({"id":request.id,"ok":false,"code":code}),
        };
        emit(&response)?;
        if stopping {
            break;
        }
    }
    backend.hold_clear.send_replace(false);
    backend.hold_end.send_replace(false);
    core.shutdown_gracefully().await;
    let _ = shutdown.send(());
    if tokio::time::timeout(Duration::from_secs(2), &mut server)
        .await
        .is_err()
    {
        server.abort();
        let _ = server.await;
    }
    Ok(())
}

fn main() -> Result<()> {
    ensure!(
        std::env::args_os().len() == 1,
        "fixture_accepts_no_arguments"
    );
    let temporary = wechat_platform::private_tempdir("wechat-calls-fixture-")?;
    let root = workspace::prepare(temporary.path())?;
    let runtime_directory = temporary.path().join("runtime");
    wechat_platform::private_dir(&runtime_directory)?;
    // This standalone process configures its own lock directory before creating
    // runtime threads. It never uses the user's existing profile lock directory.
    std::env::set_var("XDG_RUNTIME_DIR", runtime_directory);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(&root)).context("call fixture failed");
    // stdin uses a blocking reader. Bound shutdown even if its pipe is abandoned.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stdio_records_are_bounded_before_copying_and_preserve_following_lines() {
        let data = b"{\"id\":\"one\",\"op\":\"stats\"}\n{\"id\":\"two\",\"op\":\"shutdown\"}\n";
        let mut reader = BufReader::with_capacity(7, data.as_slice());
        let first: Request =
            serde_json::from_str(&input_line(&mut reader).await.unwrap().unwrap()).unwrap();
        let next: Request =
            serde_json::from_str(&input_line(&mut reader).await.unwrap().unwrap()).unwrap();
        assert_eq!(first.id, "one");
        assert!(matches!(first.command, Command::Stats));
        assert_eq!(next.id, "two");
        assert!(matches!(next.command, Command::Shutdown));
        assert!(input_line(&mut reader).await.unwrap().is_none());
        let oversized = vec![b'x'; MAX_LINE + 1];
        let mut reader = BufReader::new(oversized.as_slice());
        assert!(input_line(&mut reader).await.is_err());
    }
}
