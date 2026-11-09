//! Synthetic cross-language fixture for the real authenticated call API/broker.
//! No native upstream, profile login, account workers or host audio is started.
use anyhow::{ensure, Context, Result};
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
        CallAudioFormat, CallAudioTrack, CallCapabilities, CallConferenceInvitation, CallErrorCode,
        CallInvitationResult, CallInvitationStatus, CallMembership, CallParticipant,
        CallTrackDirection,
    },
    calls::{
        ownership::{AccountAuthority, CallIdentity, Operation},
        service::{
            BackendAudioRequest, BackendInviteRequest, BackendOutcome, BackendRequest, BackendSink,
            CallBackend, CallServiceError,
        },
    },
    upstream::{Upstream, UpstreamKind},
    Core, Store,
};
use wechat_simulator::workspace;

const ACCOUNT: &str = "calls-fixture";
const GENERATION: &str = "synthetic-fixture-generation";
const MAX_LINE: usize = 16 * 1024;
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
}
struct Active {
    identity: CallIdentity,
    sink: BackendSink,
    conference: bool,
}
struct BackendState {
    conference: bool,
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
        CallCapabilities {
            direct_audio: account == ACCOUNT,
            conference_create: account == ACCOUNT && self.state.lock().is_ok_and(|s| s.conference),
            conference_join: account == ACCOUNT && self.state.lock().is_ok_and(|s| s.conference),
            conference_invite: account == ACCOUNT && self.state.lock().is_ok_and(|s| s.conference),
            conference_audio: account == ACCOUNT && self.state.lock().is_ok_and(|s| s.conference),
            ..Default::default()
        }
    }
    fn conference_invitations(
        &self,
        authority: &AccountAuthority,
    ) -> Vec<CallConferenceInvitation> {
        if authority.account_id == ACCOUNT && self.state.lock().is_ok_and(|s| s.conference) {
            vec![CallConferenceInvitation {
                invitation_id: "synthetic-invitation".into(),
                revision: 1,
                group_id: "synthetic-group@chatroom".into(),
                caller_id: "synthetic-caller".into(),
            }]
        } else {
            vec![]
        }
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
            let (sink, conference) = {
                let mut state = state
                    .lock()
                    .map_err(|_| CallServiceError(CallErrorCode::Closed))?;
                let active = state
                    .active
                    .as_ref()
                    .filter(|active| active.identity == request.identity)
                    .ok_or(CallServiceError(CallErrorCode::Stale))?;
                let sink = active.sink.clone();
                let conference = active.conference;
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
                (sink, conference)
            };
            if conference {
                Ok(())
            } else {
                sink.audio(&request.samples).await
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
        value: i16,
    },
    Offer {
        #[serde(rename = "callerId")]
        caller_id: String,
    },
    InjectAudio {
        value: i16,
        frames: u8,
    },
    PeerMicrophone {
        muted: bool,
    },
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
        Command::ConfigureConference { enabled } => {
            let mut state = backend.state.lock().map_err(|_| "fixture_closed")?;
            if state.active.is_some() {
                return Err("fixture_busy");
            }
            state.conference = enabled;
            Ok(json!({"enabled":enabled}))
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
                    })
                    .collect(),
            )
            .await
            .map_err(|e| e.code())?;
            sink.receive_tracks(
                sources
                    .iter()
                    .map(|source| CallAudioTrack {
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
            Ok(json!({"sources":sources.len()}))
        }
        Command::ConferenceAudio {
            track_id,
            generation,
            sequence,
            sample_offset,
            value,
        } => {
            let micros = sample_offset
                .checked_mul(1_000_000)
                .ok_or("clock_overflow")?
                / 16000;
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
                    &[value; 320],
                )
                .await
                .map_err(|e| e.code())?;
            Ok(json!({"delivered":true}))
        }
        Command::Offer { caller_id } => {
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
                .offer(
                    authority,
                    uuid::Uuid::new_v4().to_string(),
                    caller_id,
                    Duration::from_secs(60),
                )
                .await
                .map_err(|error| error.code())?;
            Ok(json!({"callId":identity.call_id(),"revision":1}))
        }
        Command::InjectAudio { value, frames } => {
            if !(1..=4).contains(&frames) {
                return Err("invalid_frame_count");
            }
            let sink = backend.active_sink().map_err(|error| error.code())?;
            for _ in 0..frames {
                sink.audio(&[value; 320])
                    .await
                    .map_err(|error| error.code())?;
            }
            Ok(json!({"frames":frames}))
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
