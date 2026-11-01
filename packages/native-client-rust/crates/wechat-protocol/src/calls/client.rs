//! Stateful personal-voice signaling over the maintained native host transport.
//! Account ownership, journal persistence, logout fences, cancellation and timeout
//! supervision belong to the caller. These methods never retry an RPC.

use super::{
    app_frame::{self, AppFrameParameters},
    app_session::{AppAuthority, AppSession, PendingAppAuth},
    host_policy::CallHostRoute,
    personal::{self, VoiceInputs, VoiceRoom},
    relay::SelectedRelay,
    signaling::{CallOperation, CommonRequest, CommonResponse},
};
use crate::{
    protobuf::{self as p, Field, Value},
    wire, AccountState, NativeClient,
};
use anyhow::{bail, ensure, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

/// A host RPC boundary for signaling. Implementations must authenticate transport
/// replies, update accepted host cookies and perform exactly one submission.
/// The default native implementation also enforces active per-CGI route policy.
pub trait PersonalTransport {
    fn call_rpc(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
    ) -> Result<Vec<u8>>;
}
/// A local transport stopped before handing business request bytes to its
/// submission layer. An already-recorded prepared intent is preserved. Only
/// this proven pre-submission case may use this marker;
/// timeouts, server replies and other ambiguous outcomes must use other errors.
#[derive(Debug)]
pub struct CallRpcNotSubmitted;
impl std::fmt::Display for CallRpcNotSubmitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Call control request was not submitted")
    }
}
impl std::error::Error for CallRpcNotSubmitted {}

impl PersonalTransport for NativeClient {
    fn call_rpc(
        &self,
        state: &mut AccountState,
        route: CallHostRoute,
        request: &[u8],
    ) -> Result<Vec<u8>> {
        self.experimental_call_rpc(state, route, request)
    }
}
/// The room/control exchange is understood, but its selected media profile is
/// outside this implementation. This is distinct from an ambiguous RPC outcome;
/// the owner can end the known call once using its current control phase.
#[derive(Debug)]
pub struct UnsupportedCallMedia;
impl std::fmt::Display for UnsupportedCallMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Selected call media is unsupported")
    }
}
impl std::error::Error for UnsupportedCallMedia {}

mod incoming;
mod termination;
pub use super::personal::RemoteExitReason;
pub use incoming::{IncomingPhase, NativeIncomingSession};
pub use termination::{LocalStopReason, RemoteTermination};

type AvSnapshot = BTreeMap<u16, (u32, bool)>;

/// Metadata and advertised voice support supplied by the embedding client.
/// Request buffers are not returned codec/format/channel selections. This is
/// independent client metadata, never a copied official profile/session.
#[derive(Clone)]
pub struct VoiceParameters {
    pub sdk: Vec<u8>,
    pub group: Vec<u8>,
    pub device: Vec<u8>,
    pub engine_capability: Vec<u8>,
    pub channel_capability: Vec<u8>,
    pub network_type: u32,
    pub sim_type: u32,
    pub video_length: u32,
    pub video_ratio: u32,
    pub nics: Vec<Vec<u8>>,
    pub cloud_device_id: Vec<u8>,
    pub app_config_version: u32,
}
impl VoiceParameters {
    pub fn inputs(&self) -> VoiceInputs<'_> {
        VoiceInputs {
            sdk: &self.sdk,
            group: &self.group,
            device: &self.device,
            engine_capability: &self.engine_capability,
            channel_capability: &self.channel_capability,
            network_type: self.network_type,
            sim_type: self.sim_type,
            video_length: self.video_length,
            video_ratio: self.video_ratio,
            nics: &self.nics,
        }
    }
    fn sdk_version(&self) -> Result<u32> {
        u32::try_from(number(&fields(&self.sdk)?, 1, None)?).map_err(Into::into)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskIds {
    pub sdk: u32,
    pub host: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonalPhase {
    Idle,
    Inviting,
    Ringing,
    Joined,
    Ending,
    TerminalPending,
    Uncertain,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncProgress {
    Waiting,
    MediaReady,
    Terminal(RemoteTermination),
}

#[derive(Clone)]
struct Member {
    member: u16,
    status: u32,
    openid: Vec<u8>,
}
enum ControlOrigin {
    Outgoing { invite_id: u64 },
    Incoming,
}
struct PersonalControl {
    identity: VoiceRoom,
    peer: String,
    origin: ControlOrigin,
    peer_joined: Option<bool>,
    terminal: Option<RemoteTermination>,
    invitation_pending: bool,
    pending_recipients: Vec<String>,
}

#[derive(Clone)]
struct ActiveRoom {
    identity: VoiceRoom,
    peer: String,
    live_id: u64,
    cookies: Vec<u8>,
    strategy: Zeroizing<Vec<u8>>,
    members: BTreeMap<String, Member>,
    original_members: BTreeMap<String, Member>,
    member_sequence: u32,
    cap_sequence: u32,
    engine: Vec<u8>,
    channel: Vec<u8>,
    av_sequence: u32,
    av: AvSnapshot,
    media_taken: bool,
}

pub struct NegotiatedMedia {
    pub relay: SelectedRelay,
    pub peer_member: u16,
}

/// One generic app session with at most one personal call. It is not Clone and
/// never auto-renews authentication, redials, fabricates a callee Join for caller
/// context, or retries an uncertain action. Callers persist state.cookie changes
/// even when subsequent app parsing fails.
pub struct NativePersonalSession {
    app: AppSession,
    authority: AppAuthority,
    own_username: String,
    parameters: VoiceParameters,
    phase: PersonalPhase,
    active: Option<ActiveRoom>,
    control: Option<PersonalControl>,
    host_tasks: BTreeSet<u32>,
    sdk_tasks: BTreeSet<u32>,
}
impl NativePersonalSession {
    /// Perform exactly one generic7994 request after the host has journaled its
    /// original intent. `device32` is fresh own app identity from a secure RNG.
    pub fn authenticate(
        client: &impl PersonalTransport,
        state: &mut AccountState,
        authority: AppAuthority,
        host_task: u32,
        device32: [u8; 32],
        parameters: VoiceParameters,
    ) -> Result<Self> {
        ensure!(host_task != 0, "Missing app auth task");
        parameters.inputs().validate()?;
        ensure!(
            number(&fields(&parameters.group)?, 5, None)? == 1,
            "Selected caller sync requires group mode1"
        );
        let pending = PendingAppAuth::new(authority, device32)?;
        let request = common(state, host_task, &pending.request_body()?)?;
        let reply = client.call_rpc(state, CallHostRoute::AppAuth, &request)?;
        let app = pending.complete(&reply, authority)?;
        Ok(Self {
            app,
            authority,
            own_username: state.own_username.clone(),
            parameters,
            phase: PersonalPhase::Idle,
            active: None,
            control: None,
            host_tasks: BTreeSet::from([host_task]),
            sdk_tasks: BTreeSet::new(),
        })
    }
    pub fn phase(&self) -> PersonalPhase {
        self.phase
    }
    /// Allocated control identity survives a later media-admission failure. It
    /// does not by itself authorize Cancel or Exit; the current phase still must
    /// prove pending versus joined membership.
    pub fn has_allocated_control(&self) -> bool {
        self.control.is_some()
    }
    pub fn media_ready(&self, state: &AccountState, current: AppAuthority) -> Result<bool> {
        self.check(state, current)?;
        if self.phase != PersonalPhase::Joined {
            return Ok(false);
        }
        match self.active.as_ref() {
            Some(active) => ready(active, &self.own_username),
            None => Ok(false),
        }
    }
    fn check(&self, state: &AccountState, current: AppAuthority) -> Result<()> {
        self.app.validate_authority(current)?;
        ensure!(
            current == self.authority && state.own_username == self.own_username,
            "Personal call account changed"
        );
        Ok(())
    }
    fn reserve(&mut self, tasks: TaskIds) -> Result<()> {
        ensure!(
            tasks.host != 0
                && tasks.sdk != 0
                && self.host_tasks.len() < 4096
                && self.sdk_tasks.len() < 4096,
            "Invalid or exhausted call task IDs"
        );
        ensure!(
            !self.host_tasks.contains(&tasks.host) && !self.sdk_tasks.contains(&tasks.sdk),
            "Call task already spent"
        );
        self.host_tasks.insert(tasks.host);
        self.sdk_tasks.insert(tasks.sdk);
        Ok(())
    }
    fn host(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        route: CallHostRoute,
        body: &[u8],
        host_task: u32,
    ) -> Result<Vec<u8>> {
        let request = common(state, host_task, body)?;
        let raw = client.call_rpc(state, route, &request)?;
        let reply = CommonResponse::decode(&raw)?;
        ensure!(
            reply.return_code()? == 0,
            "Personal call host operation rejected"
        );
        reply
            .body
            .ok_or_else(|| anyhow::anyhow!("Missing personal call host reply"))
    }
    pub fn invite(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        peer: &str,
        invite_id: u64,
        tasks: TaskIds,
    ) -> Result<()> {
        self.check(state, current)?;
        ensure!(
            self.phase == PersonalPhase::Idle && peer != self.own_username,
            "Personal session busy or invalid peer"
        );
        self.reserve(tasks)?;
        self.phase = PersonalPhase::Inviting;
        let result = self.invite_once(client, state, peer, invite_id, tasks);
        let understood_media_error = result
            .as_ref()
            .is_err_and(|error| error.is::<UnsupportedCallMedia>())
            && self
                .control
                .as_ref()
                .is_some_and(|control| control.peer_joined.is_some())
            && matches!(self.phase, PersonalPhase::Ringing | PersonalPhase::Joined);
        if result.is_err() && !understood_media_error {
            self.phase = PersonalPhase::Uncertain;
        }
        result
    }
    fn invite_once(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        peer: &str,
        invite_id: u64,
        tasks: TaskIds,
    ) -> Result<()> {
        let request =
            personal::invite(&self.parameters.inputs(), peer, invite_id, tasks.sdk, None)?;
        let body = self.host(
            client,
            state,
            request.operation.into(),
            request.body(),
            tasks.host,
        )?;
        let wrapper = personal::InviteResponse::decode(&body)?;
        ensure!(
            wrapper.custom_auth.as_ref().is_none_or(|v| v.is_empty()),
            "Additional custom call auth unsupported"
        );
        ensure!(
            wrapper.invite_results.len() <= 1
                && wrapper.invite_results.iter().all(|(name, _)| name == peer),
            "Personal invitation result identity mismatch"
        );
        ensure!(
            wrapper.unknown_fields.is_empty(),
            "Unknown personal allocation fields"
        );
        let own_member = u16::try_from(wrapper.own_member)?;
        self.control = Some(PersonalControl {
            identity: VoiceRoom {
                room: wrapper.room,
                room_key: wrapper.room_key,
                own_member: wrapper.own_member,
            },
            peer: peer.to_owned(),
            origin: ControlOrigin::Outgoing { invite_id },
            peer_joined: None,
            terminal: None,
            invitation_pending: false,
            pending_recipients: vec![],
        });
        // The personal host wrapper's per-target status is not the overall
        // allocation outcome. The source reduces the initial full roster before
        // AV/codec setup, including busy/rejected peers and our own removal.
        let initial = fields(&wrapper.client_response)?;
        only(&initial, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10])?;
        let raw_members = data(&initial, 2)?;
        let list = fields(raw_members)?;
        only(&list, &[1, 2, 3])?;
        p::optional_data(&list, 3)?;
        let member_sequence = u32::try_from(number(&list, 1, Some(0))?)?;
        ensure!(
            member_sequence != 0,
            "Missing initial current member sequence"
        );
        let members = parse_roster_entries(repeated(&list, 2, 16)?, &self.own_username, peer)?;
        ensure!(
            members
                .get(&self.own_username)
                .is_none_or(|own| own.member == own_member)
                && members
                    .get(peer)
                    .is_none_or(|peer| peer.member != own_member),
            "Initial personal member identity mismatch"
        );
        let peer_joined = members.get(peer).is_some_and(|peer| peer.status == 1);
        let pending = members
            .get(peer)
            .is_some_and(|peer| matches!(peer.status, 2 | 3));
        let control = self.control.as_mut().expect("allocated control");
        control.peer_joined = Some(peer_joined);
        control.invitation_pending = pending;
        if pending {
            control.pending_recipients = vec![peer.to_owned()];
        }
        let terminal = termination::classify(
            termination::Origin::Caller,
            1,
            members.get(&self.own_username).map(|own| own.status),
            members.get(peer).map(|peer| peer.status),
            pending,
        )?;
        if let Some(action @ RemoteTermination::LocalStop(_)) = terminal {
            control.terminal = Some(action);
            self.phase = PersonalPhase::Finished;
            return Ok(());
        }
        let live_id = number(&initial, 5, None)?;
        ensure!(live_id != 0, "Missing initial live identity");
        self.parameters.group = replace_group(&self.parameters.group, live_id, &wrapper.group_id)?;
        if let Some(action) = terminal {
            self.control.as_mut().expect("allocated control").terminal = Some(action);
            self.phase = PersonalPhase::TerminalPending;
            return Ok(());
        }
        ensure!(
            members.len() == 2 && members[&self.own_username].status == 1,
            "Incomplete initial personal roster"
        );
        self.phase = if peer_joined {
            PersonalPhase::Joined
        } else {
            PersonalPhase::Ringing
        };
        let client_response = personal::JoinClientResponse::decode(&wrapper.client_response)?;
        let strategy = fields(&client_response.strategy)?;
        ensure!(
            number(&strategy, 14, None)? & (1 << 36) == 0,
            "Alternate caller sync unsupported"
        );
        let (cap_sequence, engine, channel) = cap(data(&strategy, 13)?)?;
        let (av_sequence, av) = av(
            client_response
                .av_status
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("Missing initial AV snapshot"))?,
            own_member,
            members[peer].member,
        )?;
        let active = ActiveRoom {
            identity: VoiceRoom {
                room: wrapper.room,
                room_key: wrapper.room_key,
                own_member: wrapper.own_member,
            },
            peer: peer.to_owned(),
            live_id: client_response.live_id,
            cookies: client_response.cookies,
            strategy: Zeroizing::new(client_response.strategy),
            original_members: members.clone(),
            members,
            member_sequence,
            cap_sequence,
            engine,
            channel,
            av_sequence,
            av,
            media_taken: false,
        };
        self.active = Some(active);
        let _ = ready(
            self.active.as_ref().expect("accepted room"),
            &self.own_username,
        )?;
        Ok(())
    }
    /// One caller LiveSync with source-selected timeout. Install all supported
    /// effects atomically, then cookies. Unknown or post-media reconfiguration
    /// stops with the old cursor retained and no automatic next poll.
    pub fn poll(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<SyncProgress> {
        self.check(state, current)?;
        ensure!(
            matches!(self.phase, PersonalPhase::Ringing | PersonalPhase::Joined),
            "Personal caller sync unavailable"
        );
        self.reserve(tasks)?;
        let result = self.poll_once(client, state, tasks);
        if result.as_ref().is_err_and(|error| {
            !error.is::<CallRpcNotSubmitted>() && !error.is::<UnsupportedCallMedia>()
        }) {
            self.phase = PersonalPhase::Uncertain;
        }
        result
    }
    fn poll_once(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        tasks: TaskIds,
    ) -> Result<SyncProgress> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing personal room"))?;
        let strategy = fields(&active.strategy)?;
        let policies = strategy
            .iter()
            .filter(|v| v.number == 11)
            .map(|v| match v.value {
                Value::Varint(n) => u32::try_from(n).map_err(Into::into),
                _ => bail!("Invalid call policy wire type"),
            })
            .collect::<Result<Vec<u32>>>()?;
        let timeout = if policies.len() < 83 {
            6
        } else if (3..=50).contains(&policies[82]) {
            policies[82]
        } else {
            10
        };
        let request = personal::live_sync(active.live_id, &active.cookies, timeout, tasks.sdk)?;
        let body = self.host(
            client,
            state,
            CallHostRoute::LiveSync,
            request.body(),
            tasks.host,
        )?;
        let response = personal::LiveSyncResponse::decode(&body)?;
        ensure!(
            response.unknown_fields.is_empty(),
            "Unknown caller sync response field"
        );
        let control = self
            .control
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing personal control"))?;
        let origin = match control.origin {
            ControlOrigin::Outgoing { .. } => termination::Origin::Caller,
            ControlOrigin::Incoming => termination::Origin::Callee,
        };
        let local_status = if matches!(control.origin, ControlOrigin::Incoming)
            && self.phase != PersonalPhase::Joined
        {
            3
        } else {
            1
        };
        let original_invite =
            matches!(control.origin,ControlOrigin::Outgoing{invite_id}if invite_id!=0);
        let mut invitation_pending = control.invitation_pending;
        let mut pending_recipients = control.pending_recipients.clone();
        let mut terminal = None;
        let mut next = self.active.as_ref().expect("active room").clone();
        for item in response.items {
            ensure!(
                item.kind == 1 && item.unknown_fields.is_empty(),
                "Unsupported caller sync item"
            );
            let message = fields(&item.body)?;
            only(&message, &[1, 2])?;
            ensure!(
                number(&message, 1, None)? == 10002,
                "Unsupported caller sync command"
            );
            let notify = fields(data(&message, 2)?)?;
            only(&notify, &[1, 2, 3, 4, 5])?;
            ensure!(
                number(&notify, 3, None)? == next.identity.room
                    && matches_room_key(&notify, 4, next.identity.room_key)?,
                "Caller notification room mismatch"
            );
            for raw in repeated(&notify, 5, 64)? {
                let item = fields(raw)?;
                only(&item, &[1, 2])?;
                let kind = number(&item, 1, None)?;
                let bytes = data(&item, 2)?;
                match kind {
                    10 => {
                        let update = fields(bytes)?;
                        only(&update, &[1, 2, 3, 4])?;
                        ensure!(
                            matches_room_key(&update, 1, next.identity.room)?
                                && matches_room_key(&update, 2, next.identity.room_key)?,
                            "Caller roster room mismatch"
                        );
                        let sequence = u32::try_from(number(&update, 3, Some(0))?)?;
                        if sequence <= next.member_sequence {
                            continue;
                        }
                        let members = parse_roster_entries(
                            repeated(&update, 4, 16)?,
                            &self.own_username,
                            &next.peer,
                        )?;
                        ensure!(
                            members.iter().all(|(name, m)| next
                                .original_members
                                .get(name)
                                .is_some_and(
                                    |old| old.member == m.member && old.openid == m.openid
                                )),
                            "Caller member identity changed"
                        );
                        if members
                            .get(&next.peer)
                            .is_some_and(|member| member.status == 1)
                        {
                            invitation_pending = false;
                            pending_recipients.clear();
                        }
                        let action = termination::classify(
                            origin,
                            local_status,
                            members.get(&self.own_username).map(|m| m.status),
                            members.get(&next.peer).map(|m| m.status),
                            original_invite && invitation_pending && !pending_recipients.is_empty(),
                        )?;
                        // A later authoritative own-removal proof suppresses
                        // an unsubmitted teardown; no later row revives us.
                        if !matches!(terminal, Some(RemoteTermination::LocalStop(_)))
                            && (terminal.is_none()
                                || matches!(action, Some(RemoteTermination::LocalStop(_))))
                        {
                            terminal = action;
                        }
                        next.members = members;
                        next.member_sequence = sequence;
                    }
                    11 => {
                        let (sequence, engine, channel) = cap(bytes)?;
                        if sequence <= next.cap_sequence {
                            continue;
                        }
                        ensure!(
                            !next.media_taken || terminal.is_some(),
                            "Post-start call capability reconfiguration unsupported"
                        );
                        next.cap_sequence = sequence;
                        next.engine = engine;
                        next.channel = channel;
                    }
                    12 => {
                        let (sequence, av) = av(
                            bytes,
                            next.identity.own_member as u16,
                            next.original_members[&next.peer].member,
                        )?;
                        if sequence <= next.av_sequence {
                            continue;
                        }
                        ensure!(
                            !next.media_taken || terminal.is_some(),
                            "Post-start call AV reconfiguration unsupported"
                        );
                        next.av_sequence = sequence;
                        next.av = av;
                    }
                    _ => bail!("Unsupported caller notification kind"),
                }
            }
        }
        if let Some(action) = terminal {
            next.cookies = response.cookies.unwrap_or_default();
            let control = self.control.as_mut().expect("personal control");
            control.peer_joined = Some(
                next.members
                    .get(&next.peer)
                    .is_some_and(|member| member.status == 1),
            );
            control.invitation_pending = invitation_pending;
            control.pending_recipients = pending_recipients;
            control.terminal = Some(action);
            self.phase = if matches!(action, RemoteTermination::LocalStop(_)) {
                PersonalPhase::Finished
            } else {
                PersonalPhase::TerminalPending
            };
            self.active = Some(next);
            return Ok(SyncProgress::Terminal(action));
        }
        let media_result = match if local_status == 3 {
            Ok(false)
        } else {
            ready(&next, &self.own_username)
        } {
            Err(error) if !error.is::<UnsupportedCallMedia>() => return Err(error),
            result => result,
        };
        // A supported full batch can publish its control state. Unsupported
        // media retains the previous cursor and permits no further media work.
        if media_result.is_ok() {
            next.cookies = response.cookies.unwrap_or_default();
        }
        let joined = next
            .members
            .get(&next.peer)
            .is_some_and(|member| member.status == 1);
        self.control
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing allocated control"))?
            .peer_joined = Some(joined);
        let control = self.control.as_mut().expect("personal control");
        control.invitation_pending = invitation_pending;
        control.pending_recipients = pending_recipients;
        self.phase = if (joined || !invitation_pending) && local_status == 1 {
            PersonalPhase::Joined
        } else {
            PersonalPhase::Ringing
        };
        self.active = Some(next);
        Ok(if media_result? {
            SyncProgress::MediaReady
        } else {
            SyncProgress::Waiting
        })
    }
    /// Move one admitted media context out only after current roster/capability/AV
    /// state agrees. This never constructs a synthetic callee Join response.
    pub fn take_media(
        &mut self,
        state: &AccountState,
        current: AppAuthority,
        connection_index: usize,
        udp_index: usize,
    ) -> Result<NegotiatedMedia> {
        self.check(state, current)?;
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Missing personal room"))?;
        ensure!(
            self.phase == PersonalPhase::Joined
                && !active.media_taken
                && ready(active, &self.own_username)?,
            "Personal media not ready or already taken"
        );
        let relay = SelectedRelay::from_strategy(
            active.identity.room,
            active.identity.room_key,
            active.identity.own_member as u16,
            &active.strategy,
            connection_index,
            udp_index,
        )?;
        active.media_taken = true;
        Ok(NegotiatedMedia {
            relay,
            peer_member: active.members[&active.peer].member,
        })
    }
    /// Cancel the original unanswered personal invitation once. This selected
    /// client owns one original pending peer; no unrelated recipients are added.
    pub fn cancel(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
    ) -> Result<()> {
        self.check(state, current)?;
        ensure!(
            self.phase == PersonalPhase::Ringing,
            "Personal invitation no longer cancellable"
        );
        self.reserve(tasks)?;
        self.phase = PersonalPhase::Ending;
        let result = (|| {
            let control = self
                .control
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing allocated control"))?;
            let room = control.identity;
            let ControlOrigin::Outgoing { invite_id } = control.origin else {
                bail!("Incoming calls cannot use caller Cancel")
            };
            ensure!(
                control.peer_joined == Some(false)
                    && control.invitation_pending
                    && !control.pending_recipients.is_empty(),
                "Pending invitation provenance missing or already joined"
            );
            let request = personal::cancel(
                &self.parameters.inputs(),
                room.room,
                room.room_key,
                invite_id,
                &control.pending_recipients,
                personal::CancelReason::Local,
                tasks.sdk,
            )?;
            let body = self.host(
                client,
                state,
                CallHostRoute::Cancel,
                request.body(),
                tasks.host,
            )?;
            let outer = fields(&body)?;
            let cancelled = fields(data(&outer, 1)?)?;
            ensure!(
                number(&cancelled, 1, None)? == room.room
                    && number(&cancelled, 2, None)? == room.room_key,
                "Personal cancellation room mismatch"
            );
            Ok(())
        })();
        self.phase = if result.is_ok() {
            PersonalPhase::Finished
        } else {
            PersonalPhase::Uncertain
        };
        result
    }
    /// Complete one source-classified remote termination. This is separate from
    /// local Hangup: the peer may already be absent or nonjoined. The embedding
    /// owner must stop media before calling. Failed replies remain uncertain.
    pub fn finish_remote(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
        talk_success: bool,
    ) -> Result<()> {
        self.check(state, current)?;
        ensure!(
            self.phase == PersonalPhase::TerminalPending,
            "No pending remote teardown"
        );
        self.reserve(tasks)?;
        self.phase = PersonalPhase::Ending;
        let result = (|| {
            let control = self
                .control
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Missing remote termination control"))?;
            let action = control
                .terminal
                .take()
                .ok_or_else(|| anyhow::anyhow!("Remote termination already spent"))?;
            let identity = control.identity;
            match action {
                RemoteTermination::Exit(reason) => {
                    let request = personal::exit_remote(
                        &self.parameters.inputs(),
                        identity,
                        reason,
                        talk_success,
                        tasks.sdk,
                    )?;
                    let parameters = AppFrameParameters {
                        sdk_version: self.parameters.sdk_version()?,
                        cloud_device_id: &self.parameters.cloud_device_id,
                        host_client_version: state.device.client_version as u32,
                        app_config_version: self.parameters.app_config_version,
                    };
                    let body = app_frame::prepare_transfer(
                        &self.app,
                        current,
                        CallOperation::Exit,
                        request.body.as_deref().expect("prepared remote Exit"),
                        &parameters,
                    )?;
                    let reply =
                        self.host(client, state, CallHostRoute::Transfer, &body, tasks.host)?;
                    let exit = fields(&app_frame::decode_transfer_reply(
                        &self.app,
                        current,
                        CallOperation::Exit,
                        &reply,
                    )?)?;
                    ensure!(
                        number(&exit, 1, None)? == identity.room
                            && number(&exit, 2, None)? == identity.room_key
                            && number(&exit, 3, None)? == u64::from(identity.own_member),
                        "Remote Exit identity mismatch"
                    );
                }
                RemoteTermination::NoResponseCancel => {
                    let ControlOrigin::Outgoing { invite_id } = control.origin else {
                        bail!("Callee cannot cancel an original invitation")
                    };
                    ensure!(
                        control.invitation_pending
                            && !control.pending_recipients.is_empty()
                            && control
                                .pending_recipients
                                .iter()
                                .all(|recipient| recipient == &control.peer),
                        "Missing current pending-recipient provenance"
                    );
                    control.invitation_pending = false;
                    let recipients = std::mem::take(&mut control.pending_recipients);
                    let request = personal::cancel(
                        &self.parameters.inputs(),
                        identity.room,
                        identity.room_key,
                        invite_id,
                        &recipients,
                        personal::CancelReason::NoResponse,
                        tasks.sdk,
                    )?;
                    let body = self.host(
                        client,
                        state,
                        CallHostRoute::Cancel,
                        request.body(),
                        tasks.host,
                    )?;
                    let outer = fields(&body)?;
                    let cancelled = fields(data(&outer, 1)?)?;
                    ensure!(
                        number(&cancelled, 1, None)? == identity.room
                            && number(&cancelled, 2, None)? == identity.room_key,
                        "Remote cancellation room mismatch"
                    );
                }
                RemoteTermination::LocalStop(_) => {
                    bail!("Local-only stop must not submit a teardown")
                }
            }
            Ok(())
        })();
        self.phase = if result.is_ok() {
            PersonalPhase::Finished
        } else {
            PersonalPhase::Uncertain
        };
        result
    }

    /// Submit joined Exit once. `talk_success` is the independently established
    /// source predicate, not inferred here from a phone timer. Reliable peer
    /// notice and canonical history remain distinct from accepted server Exit.
    pub fn exit(
        &mut self,
        client: &impl PersonalTransport,
        state: &mut AccountState,
        current: AppAuthority,
        tasks: TaskIds,
        talk_success: bool,
    ) -> Result<()> {
        self.check(state, current)?;
        ensure!(
            self.phase == PersonalPhase::Joined,
            "Personal call not joined"
        );
        self.reserve(tasks)?;
        self.phase = PersonalPhase::Ending;
        let result = (|| {
            let control = self
                .control
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Missing allocated control"))?;
            ensure!(
                matches!(control.origin, ControlOrigin::Incoming) || !control.invitation_pending,
                "Joined control state unknown"
            );
            let identity = control.identity;
            let request = personal::exit(
                &self.parameters.inputs(),
                identity,
                1,
                talk_success,
                tasks.sdk,
            )?;
            let parameters = AppFrameParameters {
                sdk_version: self.parameters.sdk_version()?,
                cloud_device_id: &self.parameters.cloud_device_id,
                host_client_version: state.device.client_version as u32,
                app_config_version: self.parameters.app_config_version,
            };
            let body = app_frame::prepare_transfer(
                &self.app,
                current,
                CallOperation::Exit,
                request.body.as_deref().expect("prepared Exit body"),
                &parameters,
            )?;
            let delegate = self.host(client, state, CallHostRoute::Transfer, &body, tasks.host)?;
            let exit = fields(&app_frame::decode_transfer_reply(
                &self.app,
                current,
                CallOperation::Exit,
                &delegate,
            )?)?;
            ensure!(
                number(&exit, 1, None)? == identity.room
                    && number(&exit, 2, None)? == identity.room_key
                    && number(&exit, 3, None)? == u64::from(identity.own_member),
                "Personal Exit identity mismatch"
            );
            Ok(())
        })();
        self.phase = if result.is_ok() {
            PersonalPhase::Finished
        } else {
            PersonalPhase::Uncertain
        };
        result
    }
}

fn common(state: &AccountState, host_task: u32, body: &[u8]) -> Result<Vec<u8>> {
    CommonRequest {
        base_request: Some(wire::base_request(&state.device, state.uin, host_task)?),
        body: Some(body.to_vec()),
        unknown_fields: vec![],
    }
    .encode()
}
fn fields(raw: &[u8]) -> Result<Vec<Field>> {
    ensure!(raw.len() <= 64 * 1024, "Personal signaling body too large");
    super::signaling::decode_fields(raw)
}
fn data(values: &[Field], n: u32) -> Result<&[u8]> {
    p::optional_data(values, n)?.ok_or_else(|| anyhow::anyhow!("Missing personal bytes field {n}"))
}
fn number(values: &[Field], n: u32, default: Option<u64>) -> Result<u64> {
    match p::optional(values, n)? {
        Some(Value::Varint(v)) => Ok(*v),
        None => default.ok_or_else(|| anyhow::anyhow!("Missing personal integer {n}")),
        _ => bail!("Invalid personal integer {n}"),
    }
}
fn repeated(values: &[Field], n: u32, limit: usize) -> Result<Vec<&[u8]>> {
    let mut out = Vec::new();
    for f in values.iter().filter(|v| v.number == n) {
        ensure!(out.len() < limit, "Too many personal entries");
        if let Value::Bytes(raw) = &f.value {
            out.push(raw.as_slice());
        } else {
            bail!("Invalid personal entry wire type");
        }
    }
    Ok(out)
}
fn only(values: &[Field], allowed: &[u32]) -> Result<()> {
    ensure!(
        values.iter().all(|f| allowed.contains(&f.number)),
        "Unknown personal signaling field"
    );
    Ok(())
}
fn matches_room_key(values: &[Field], n: u32, expected: u64) -> Result<bool> {
    let value = number(values, n, Some(0))?;
    Ok(value == 0 || value == expected)
}
fn roster(raw: &[u8], own: &str, peer: &str, own_member: u16) -> Result<BTreeMap<String, Member>> {
    let values = fields(raw)?;
    roster_entries(repeated(&values, 2, 16)?, own, peer, own_member)
}
fn roster_entries(
    entries: Vec<&[u8]>,
    own: &str,
    peer: &str,
    own_member: u16,
) -> Result<BTreeMap<String, Member>> {
    let members = parse_roster_entries(entries, own, peer)?;
    ensure!(
        members.len() == 2
            && members
                .get(own)
                .is_some_and(|m| m.member == own_member && m.status == 1)
            && members
                .get(peer)
                .is_some_and(|m| (1..=3).contains(&m.status)),
        "Incomplete or unsupported personal roster"
    );
    Ok(members)
}
fn parse_roster_entries(
    entries: Vec<&[u8]>,
    own: &str,
    peer: &str,
) -> Result<BTreeMap<String, Member>> {
    ensure!(entries.len() <= 2, "Unsupported direct roster size");
    let mut members = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for raw in entries {
        let values = fields(raw)?;
        only(&values, &[1, 2, 3, 4, 5, 6, 7, 8, 9])?;
        for tag in [4, 5, 9] {
            if let Some(Value::Varint(value)) = p::optional(&values, tag)? {
                u32::try_from(*value)?;
            } else {
                ensure!(
                    p::optional(&values, tag)?.is_none(),
                    "Invalid member scalar wire type"
                );
            }
        }
        for tag in [6, 8] {
            if let Some(value) = p::optional_data(&values, tag)? {
                std::str::from_utf8(value)?;
            }
        }
        let name = std::str::from_utf8(data(&values, 7)?)?;
        ensure!(
            (name == own || name == peer) && !members.contains_key(name),
            "Unexpected personal roster identity"
        );
        let member = u16::try_from(number(&values, 2, None)?)?;
        let status = u32::try_from(number(&values, 3, None)?)?;
        let openid = data(&values, 1)?.to_vec();
        ensure!(
            !openid.is_empty() && ids.insert(member),
            "Incomplete or duplicate personal member"
        );
        members.insert(
            name.to_owned(),
            Member {
                member,
                status,
                openid,
            },
        );
    }
    Ok(members)
}
fn cap(raw: &[u8]) -> Result<(u32, Vec<u8>, Vec<u8>)> {
    let values = fields(raw)?;
    only(&values, &[1, 2])?;
    let sequence = u32::try_from(number(&values, 1, Some(0))?)?;
    ensure!(sequence != 0, "Missing current call capability sequence");
    let content = fields(data(&values, 2)?)?;
    only(&content, &[1, 2])?;
    let engine = data(&content, 1)?.to_vec();
    let channel = data(&content, 2)?.to_vec();
    ensure!(
        !engine.is_empty() && !channel.is_empty(),
        "Partial call capabilities unsupported"
    );
    profile_shape(&engine, &channel)?;
    Ok((sequence, engine, channel))
}
fn av(raw: &[u8], own: u16, peer: u16) -> Result<(u32, AvSnapshot)> {
    let values = fields(raw)?;
    only(&values, &[1, 3, 4])?;
    let sequence = u32::try_from(number(&values, 3, Some(0))?)?;
    let mut members = BTreeMap::new();
    for raw in repeated(&values, 4, 2)? {
        let values = fields(raw)?;
        only(&values, &[1, 2, 3])?;
        let member = u16::try_from(number(&values, 1, None)?)?;
        ensure!(
            [own, peer].contains(&member) && !members.contains_key(&member),
            "Unexpected AV member"
        );
        members.insert(
            member,
            (
                u32::try_from(number(&values, 2, None)?)?,
                p::optional_data(&values, 3)?.is_some(),
            ),
        );
    }
    Ok((sequence, members))
}
fn profile_shape(engine: &[u8], channel: &[u8]) -> Result<[u64; 6]> {
    let ef = fields(engine)?;
    let cf = fields(channel)?;
    let mut seen = BTreeSet::new();
    for field in &ef {
        ensure!(
            (1..=42).contains(&field.number) && field.number != 41 && seen.insert(field.number),
            "Unsupported engine capability shape"
        );
        let Value::Varint(value) = field.value else {
            bail!("Invalid engine capability wire type")
        };
        if [1, 2, 3, 4, 25, 26, 27].contains(&field.number) {
            i32::try_from(value as i64)?;
        } else {
            u32::try_from(value)?;
        }
    }
    only(&cf, &[1, 2])?;
    Ok([
        number(&ef, 5, Some(0))?,
        number(&ef, 6, Some(0))?,
        number(&ef, 7, Some(0))?,
        number(&ef, 39, Some(0))?,
        u64::from(u32::try_from(number(&cf, 1, Some(0))?)?),
        u64::from(u32::try_from(number(&cf, 2, Some(0))?)?),
    ])
}
fn profile(engine: &[u8], channel: &[u8]) -> Result<()> {
    if profile_shape(engine, channel)? != [17, 0x206, 0x800000, 0, 2, 3] {
        return Err(UnsupportedCallMedia.into());
    }
    Ok(())
}
fn ready(room: &ActiveRoom, own: &str) -> Result<bool> {
    // Current capability bytes are validated even while a peer/AV stream is
    // pending. Codec0 is then unselected data, never decoder permission.
    profile_shape(&room.engine, &room.channel)?;
    let own_id = room
        .members
        .get(own)
        .context("Missing current own member")?
        .member;
    let peer = room
        .members
        .get(&room.peer)
        .context("Missing current peer member")?;
    if peer.status != 1
        || room.av.len() != 2
        || ![own_id, peer.member]
            .iter()
            .all(|id| room.av.get(id) == Some(&(1, false)))
    {
        return Ok(false);
    }
    profile(&room.engine, &room.channel)?;
    Ok(true)
}
fn replace_group(raw: &[u8], live_id: u64, group: &str) -> Result<Vec<u8>> {
    let values = fields(raw)?;
    let mut encoded = Vec::new();
    for field in values {
        let value = match field.number {
            2 => p::uint(2, live_id),
            4 => p::bytes(4, group.as_bytes()),
            _ => match field.value {
                Value::Varint(v) => p::uint(field.number, v),
                Value::Bytes(v) => p::bytes(field.number, &v),
                Value::Fixed64(_) | Value::Fixed32(_) => bail!("Unsupported group field wire type"),
            },
        };
        encoded.push(value);
    }
    p::message(&encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct FakeTransport(
        pub(super) std::cell::RefCell<std::collections::VecDeque<(CallHostRoute, Vec<u8>)>>,
    );
    impl PersonalTransport for FakeTransport {
        fn call_rpc(
            &self,
            state: &mut AccountState,
            route: CallHostRoute,
            request: &[u8],
        ) -> Result<Vec<u8>> {
            let (expected, reply) = self
                .0
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("Unexpected fake call RPC"))?;
            ensure!(route == expected, "Unexpected fake call route");
            let request = CommonRequest::decode(request)?;
            ensure!(
                request.base_request.is_some() && request.body.is_some(),
                "Missing host common wrapper"
            );
            source_request(route, &request)?;
            state.cookie = vec![route.command() as u8];
            Ok(reply)
        }
    }
    /// Independent selected request contract at the actual session/transport
    /// boundary. Literal field numbers and command/path values are not obtained
    /// by serializing another production request or reading its enum accessors.
    fn source_request(route: CallHostRoute, request: &CommonRequest) -> Result<()> {
        let base = fields(
            request
                .base_request
                .as_deref()
                .context("Missing host base")?,
        )?;
        assert_eq!(data(&base, 1)?, &[0]);
        assert_eq!(number(&base, 2, None)?, 1);
        assert!(number(&base, 6, None)? > 0);
        let body = fields(request.body.as_deref().context("Missing host body")?)?;
        match route {
            CallHostRoute::AppAuth => {
                let device = data(&fields(data(&body, 1)?)?, 1)?.to_vec();
                assert_eq!(device.len(), 32);
                let raw = request.body.as_deref().unwrap();
                assert_eq!(&raw[..4], &[10, 34, 10, 32]);
                assert_eq!(raw.len(), 36);
            }
            CallHostRoute::Invite => {
                assert_eq!(data(&body, 1)?, b"peer");
                assert_eq!(number(&body, 2, None)?, 2);
                assert!(number(&body, 5, None)? > 0);
                assert!(p::optional(&body, 6)?.is_none());
                assert_eq!(data(&body, 7)?, b"voip-2p");
                let client = fields(data(&body, 4)?)?;
                assert_eq!(number(&client, 1, None)?, 0x050000cb);
                assert_eq!(data(&client, 2)?, b"test");
                assert_eq!(number(&client, 3, None)?, 0);
                assert_eq!(number(&client, 4, None)?, 1);
                let caps = fields(data(&client, 5)?)?;
                assert_eq!(number(&fields(data(&caps, 1)?)?, 5, None)?, 0x40000);
                assert_eq!(data(&caps, 2)?.len(), 12);
            }
            CallHostRoute::LiveSync => {
                assert_eq!(number(&body, 1, None)?, 99);
                data(&body, 2)?;
                assert_eq!(data(&body, 4)?, b"voip-2p");
                assert_eq!(number(&body, 5, None)?, 6);
            }
            CallHostRoute::Cancel => {
                let inner = fields(data(&body, 1)?)?;
                assert_eq!(number(&inner, 3, None)?, 7);
                assert_eq!(number(&inner, 4, None)?, 9);
                assert_eq!(number(&inner, 5, None)?, number(&body, 2, None)?);
                assert!([1, 2].contains(&number(&inner, 6, None)?));
                assert_eq!(number(&body, 3, None)?, 7);
                assert_eq!(data(&body, 4)?, b"peer");
                assert_eq!(data(&body, 5)?, b"voip-2p");
            }
            CallHostRoute::RingingAck => {
                let inner = fields(data(&body, 1)?)?;
                assert_eq!(number(&inner, 4, None)?, 1);
                assert_eq!(number(&inner, 5, None)?, 7);
                assert_eq!(number(&inner, 6, None)?, 9);
                assert_eq!(number(&inner, 7, None)?, 0);
                assert_eq!(number(&inner, 13, None)?, 1);
                let caps = fields(data(&inner, 12)?)?;
                assert_eq!(number(&fields(data(&caps, 1)?)?, 5, None)?, 0x40000);
                assert_eq!(data(&caps, 2)?.len(), 12);
                assert_eq!(data(&body, 3)?, b"voip-2p");
            }
            CallHostRoute::Transfer => {
                assert_eq!(number(&body, 1, None)?, 1901);
                let command = number(&body, 2, None)? as u32;
                let path: &[u8] = match command {
                    3361 => b"/cloudim/cloud-bin/cloudvoipmtjoin",
                    3095 => b"/cloudim/cloud-bin/cloudvoipmtreject",
                    3380 => b"/cloudim/cloud-bin/cloudvoipmtexit",
                    _ => bail!("Unexpected synthetic Cloud command"),
                };
                assert_eq!(data(&body, 3)?, b"wechat");
                assert_eq!(data(&body, 4)?, path);
                let frame = app_frame::TaskFrame::decode(data(&body, 5)?, command)?;
                assert_eq!(frame.integers[&2], frame.integers[&22]);
                assert!(frame.integers[&2] > 0);
                assert_eq!(frame.strings[&24], b"wechat");
                let clear = app_frame::lz4_decode(&frame.payload, 64 * 1024)?;
                let cloud = fields(&clear)?;
                let app_base = fields(data(&cloud, 1)?)?;
                assert_eq!(number(&app_base, 1, None)?, frame.integers[&22]);
                assert_eq!(data(&app_base, 2)?, b"Ldeviceid");
                let inner = fields(data(&cloud, 2)?)?;
                if command == 3361 {
                    assert_eq!(number(&inner, 5, None)?, 2);
                    assert_eq!(number(&inner, 6, None)?, 1);
                    assert_eq!(number(&inner, 8, None)?, 0);
                    assert_eq!(number(&inner, 11, None)?, 7);
                    assert_eq!(number(&inner, 12, None)?, 9);
                } else {
                    assert_eq!(number(&inner, 3, None)?, 7);
                    assert_eq!(number(&inner, 4, None)?, 9);
                    assert_eq!(number(&inner, 5, None)?, 0);
                    let reason = number(&inner, 6, None)?;
                    if command == 3095 {
                        assert_eq!(reason, 1);
                    } else {
                        assert!([1, 5, 6, 7].contains(&reason));
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn host_reply(body: &[u8]) -> Vec<u8> {
        p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, body)]).unwrap()
    }
    pub(super) fn state() -> AccountState {
        AccountState {
            schema: 1,
            device: crate::DeviceProfile {
                device_id: vec![b'x'; 16],
                device_type: "Linux".to_owned(),
                client_version: -227271398,
            },
            uin: 1,
            own_username: "own".to_owned(),
            cookie: vec![],
            session_key: vec![1; 16],
            request_key: vec![],
            response_key: vec![],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: crate::AuthSnapshot::default(),
            long_host: "localhost".to_owned(),
            short_host: "localhost".to_owned(),
        }
    }
    pub(super) fn authority() -> AppAuthority {
        AppAuthority {
            account_generation: 1,
            logout_epoch: 0,
            credential_digest: [4; 32],
        }
    }

    pub(super) fn capabilities_for_codec(codec: u64) -> (Vec<u8>, Vec<u8>) {
        (
            p::message(&[p::uint(5, codec), p::uint(6, 0x206), p::uint(7, 0x800000)]).unwrap(),
            p::message(&[p::uint(1, 2), p::uint(2, 3)]).unwrap(),
        )
    }
    pub(super) fn parameters() -> VoiceParameters {
        let offer =
            super::super::voice_offer::VoiceOffer::from_owned_random(0x01000000, [5; 32]).unwrap();
        VoiceParameters {
            sdk: p::message(&[p::uint(1, 0x050000cb), p::bytes(2, b"test")]).unwrap(),
            group: p::message(&[
                p::bytes(1, b"wechat"),
                p::uint(2, 0),
                p::bytes(3, b"voip-2p"),
                p::bytes(4, &[]),
                p::uint(5, 1),
                p::bytes(6, &[]),
                p::uint(7, 0),
            ])
            .unwrap(),
            device: vec![],
            engine_capability: offer.engine_advertisement().to_vec(),
            channel_capability: offer.advertised_channel_identity().to_vec(),
            network_type: 0,
            sim_type: 0,
            video_length: 0,
            video_ratio: 0,
            nics: vec![],
            cloud_device_id: b"Ldeviceid".to_vec(),
            app_config_version: 1,
        }
    }
    pub(super) fn member(name: &str, id: u16, status: u32) -> Vec<u8> {
        p::message(&[
            p::bytes(1, name.as_bytes()),
            p::uint(2, u64::from(id)),
            p::uint(3, u64::from(status)),
            p::bytes(7, name.as_bytes()),
        ])
        .unwrap()
    }
    pub(super) fn av_status(sequence: u32, peer_status: u32) -> Vec<u8> {
        let own = p::message(&[p::uint(1, 0), p::uint(2, 1)]).unwrap();
        let peer = p::message(&[p::uint(1, 1), p::uint(2, u64::from(peer_status))]).unwrap();
        p::message(&[
            p::uint(3, u64::from(sequence)),
            p::bytes(4, &own),
            p::bytes(4, &peer),
        ])
        .unwrap()
    }
    pub(super) fn auth_reply() -> Vec<u8> {
        auth_reply_uin(17)
    }
    fn auth_reply_uin(uin: u64) -> Vec<u8> {
        let auth = p::message(&[p::bytes(9, b"server"), p::uint(11, 1)]).unwrap();
        let unify = p::message(&[p::bytes(2, &auth), p::bytes(3, &p::uint(1, uin))]).unwrap();
        host_reply(&p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &unify)]).unwrap())
    }
    fn invite_reply() -> Vec<u8> {
        invite_reply_profile(17, 2)
    }
    fn invite_reply_profile(codec: u64, peer_status: u32) -> Vec<u8> {
        invite_reply_full(codec, peer_status, 0)
    }
    fn invite_reply_full(codec: u64, peer_status: u32, peer_av: u32) -> Vec<u8> {
        let (engine, channel) = capabilities_for_codec(codec);
        // Scalar0 may be absent in a current, unselected capability message.
        let engine = if codec == 0 {
            p::message(&[p::uint(6, 0x206), p::uint(7, 0x800000)]).unwrap()
        } else {
            engine
        };
        let cap = p::message(&[
            p::uint(1, 1),
            p::bytes(
                2,
                &p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap(),
            ),
        ])
        .unwrap();
        let address = p::message(&[p::bytes(1, b"127.0.0.1"), p::uint(2, 6000)]).unwrap();
        let connection = p::message(&[p::uint(1, 1), p::bytes(4, &address)]).unwrap();
        let strategy = p::message(&[
            p::bytes(10, &[7; 32]),
            p::bytes(13, &cap),
            p::uint(14, 0),
            p::bytes(15, &connection),
        ])
        .unwrap();
        let roster = p::message(&[
            p::uint(1, 1),
            p::bytes(2, &member("own", 0, 1)),
            p::bytes(2, &member("peer", 1, peer_status)),
        ])
        .unwrap();
        let client = p::message(&[
            p::bytes(1, &strategy),
            p::bytes(2, &roster),
            p::bytes(3, &av_status(1, peer_av)),
            p::uint(5, 99),
            p::bytes(6, b"old"),
        ])
        .unwrap();
        host_reply(
            &p::message(&[
                p::uint(1, 7),
                p::uint(2, 9),
                p::uint(3, 0),
                p::bytes(4, b"group"),
                p::bytes(5, &client),
            ])
            .unwrap(),
        )
    }
    fn with_result(reply: &[u8], recipient: &str, code: u64) -> Vec<u8> {
        let common = fields(reply).unwrap();
        let mut body = data(&common, 2).unwrap().to_vec();
        body.extend(p::bytes(
            8,
            &p::message(&[p::bytes(1, recipient.as_bytes()), p::uint(2, code)]).unwrap(),
        ));
        host_reply(&body)
    }
    fn initial_roster_reply(sequence: u32, own: Option<u32>, peer: Option<u32>) -> Vec<u8> {
        let mut list = vec![p::uint(1, u64::from(sequence))];
        if let Some(status) = own {
            list.push(p::bytes(2, &member("own", 0, status)));
        }
        if let Some(status) = peer {
            list.push(p::bytes(2, &member("peer", 1, status)));
        }
        // No AV/codec data is needed for the source's earlier terminal reducer.
        let client = p::message(&[
            p::bytes(2, &p::message(&list).unwrap()),
            p::uint(5, 99),
            p::bytes(6, b"initial"),
        ])
        .unwrap();
        with_result(
            &host_reply(
                &p::message(&[
                    p::uint(1, 7),
                    p::uint(2, 9),
                    p::uint(3, 0),
                    p::bytes(4, b"group"),
                    p::bytes(5, &client),
                ])
                .unwrap(),
            ),
            "peer",
            23,
        )
    }
    fn capability_sync(engine: &[u8], channel: &[u8]) -> Vec<u8> {
        let content = p::message(&[p::bytes(1, engine), p::bytes(2, channel)]).unwrap();
        let cap = p::message(&[p::uint(1, 2), p::bytes(2, &content)]).unwrap();
        let notify = p::message(&[
            p::uint(3, 7),
            p::uint(4, 9),
            p::bytes(
                5,
                &p::message(&[p::uint(1, 11), p::bytes(2, &cap)]).unwrap(),
            ),
        ])
        .unwrap();
        let message = p::message(&[p::uint(1, 10002), p::bytes(2, &notify)]).unwrap();
        host_reply(
            &p::message(&[
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &message)]).unwrap(),
                ),
                p::bytes(2, b"bad-cursor"),
            ])
            .unwrap(),
        )
    }
    fn joined_sync(extra_unknown: bool) -> Vec<u8> {
        joined_sync_profile(extra_unknown, None)
    }
    fn joined_sync_profile(extra_unknown: bool, codec: Option<u64>) -> Vec<u8> {
        let members = p::message(&[
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, 2),
            p::bytes(4, &member("own", 0, 1)),
            p::bytes(4, &member("peer", 1, 1)),
        ])
        .unwrap();
        let mut notify = vec![
            p::uint(3, 7),
            p::uint(4, 9),
            p::bytes(
                5,
                &p::message(&[p::uint(1, 10), p::bytes(2, &members)]).unwrap(),
            ),
            p::bytes(
                5,
                &p::message(&[p::uint(1, 12), p::bytes(2, &av_status(2, 1))]).unwrap(),
            ),
        ];
        if let Some(codec) = codec {
            let (engine, channel) = capabilities_for_codec(codec);
            let content = p::message(&[p::bytes(1, &engine), p::bytes(2, &channel)]).unwrap();
            let cap = p::message(&[p::uint(1, 2), p::bytes(2, &content)]).unwrap();
            notify.push(p::bytes(
                5,
                &p::message(&[p::uint(1, 11), p::bytes(2, &cap)]).unwrap(),
            ));
        }
        if extra_unknown {
            notify.push(p::bytes(
                5,
                &p::message(&[p::uint(1, 999), p::bytes(2, &[])]).unwrap(),
            ));
        }
        let message = p::message(&[
            p::uint(1, 10002),
            p::bytes(2, &p::message(&notify).unwrap()),
        ])
        .unwrap();
        host_reply(
            &p::message(&[
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &message)]).unwrap(),
                ),
                p::bytes(2, b"new"),
            ])
            .unwrap(),
        )
    }
    pub(super) fn exit_reply() -> Vec<u8> {
        exit_reply_uin(17)
    }
    fn exit_reply_uin(uin: u64) -> Vec<u8> {
        let exit = p::message(&[p::uint(1, 7), p::uint(2, 9), p::uint(3, 0)]).unwrap();
        let cloud =
            p::message(&[p::bytes(1, &[8, 0]), p::uint(2, uin), p::bytes(3, &exit)]).unwrap();
        let frame = app_frame::TaskFrame {
            command: 3380,
            integers: BTreeMap::from([
                (4, 0),
                (7, 0),
                (8, 0),
                (21, 0),
                (22, uin),
                (23, 0),
                (25, 0),
                (26, 0),
            ]),
            strings: BTreeMap::new(),
            payload: cloud,
        }
        .encode()
        .unwrap();
        host_reply(&p::message(&[p::uint(1, 0), p::bytes(2, &frame)]).unwrap())
    }
    #[test]
    fn fake_caller_auth_invite_full_sync_media_and_exit() {
        let transport = RecordingTransport {
            inner: FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, auth_reply_uin(u64::MAX)),
                (CallHostRoute::Invite, invite_reply()),
                (CallHostRoute::LiveSync, joined_sync(false)),
                (CallHostRoute::Transfer, exit_reply_uin(u64::MAX)),
            ]))),
            requests: Default::default(),
        };
        let mut state = state();
        let mut session = NativePersonalSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                &transport,
                &mut state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        assert_eq!(session.phase(), PersonalPhase::Ringing);
        assert_eq!(
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 }
                )
                .unwrap(),
            SyncProgress::MediaReady
        );
        let media = session.take_media(&state, authority(), 0, 0).unwrap();
        assert_eq!(media.peer_member, 1);
        assert_eq!(media.relay.room_id, 7);
        assert!(session.take_media(&state, authority(), 0, 0).is_err());
        session
            .exit(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 },
                false,
            )
            .unwrap();
        assert_eq!(session.phase(), PersonalPhase::Finished);
        assert!(transport.inner.0.borrow().is_empty());
        let requests = transport.requests.borrow();
        let host_tasks = requests
            .iter()
            .map(|(_, raw)| {
                let common = CommonRequest::decode(raw).unwrap();
                number(
                    &fields(common.base_request.as_deref().unwrap()).unwrap(),
                    6,
                    None,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(host_tasks, [1, 2, 3, 4]);
        let common = CommonRequest::decode(&requests.last().unwrap().1).unwrap();
        let delegate = fields(common.body.as_deref().unwrap()).unwrap();
        let frame = app_frame::TaskFrame::decode(data(&delegate, 5).unwrap(), 3380).unwrap();
        assert_eq!(frame.integers[&2], u64::MAX);
        assert_eq!(frame.integers[&22], u64::MAX);
        let common = fields(&session.app.common_session(authority()).unwrap()).unwrap();
        assert_eq!(number(&common, 4, None).unwrap(), u64::MAX);
        assert_eq!(data(&common, 5).unwrap(), b"server");
        assert_eq!(number(&common, 6, None).unwrap(), 1);
        assert!(p::optional(&common, 1).unwrap().is_none());
    }
    #[test]
    fn later_unknown_sync_item_prevents_cookie_and_partial_member_install() {
        let transport = FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::Invite, invite_reply()),
            (CallHostRoute::LiveSync, joined_sync(true)),
        ])));
        let mut state = state();
        let mut session = NativePersonalSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                &transport,
                &mut state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        assert!(session
            .poll(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 2, host: 3 }
            )
            .is_err());
        assert_eq!(session.phase(), PersonalPhase::Uncertain);
        let active = session.active.as_ref().unwrap();
        assert_eq!(active.cookies, b"old");
        assert_eq!(active.members["peer"].status, 2);
        assert!(session
            .poll(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 }
            )
            .is_err());
    }

    #[test]
    fn only_proven_unsent_poll_retains_known_room_for_exit() {
        struct StopTransport<'a> {
            inner: &'a FakeTransport,
            unsent: bool,
        }
        impl PersonalTransport for StopTransport<'_> {
            fn call_rpc(
                &self,
                state: &mut AccountState,
                route: CallHostRoute,
                request: &[u8],
            ) -> Result<Vec<u8>> {
                if route == CallHostRoute::LiveSync {
                    if self.unsent {
                        return Err(CallRpcNotSubmitted.into());
                    }
                    bail!("Ambiguous transport failure")
                }
                self.inner.call_rpc(state, route, request)
            }
        }
        for unsent in [true, false] {
            let transport =
                FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                    (CallHostRoute::AppAuth, auth_reply()),
                    (CallHostRoute::Invite, invite_reply()),
                    (CallHostRoute::LiveSync, joined_sync(false)),
                    (CallHostRoute::Transfer, exit_reply()),
                ])));
            let mut state = state();
            let mut session = NativePersonalSession::authenticate(
                &transport,
                &mut state,
                authority(),
                1,
                [9; 32],
                parameters(),
            )
            .unwrap();
            session
                .invite(
                    &transport,
                    &mut state,
                    authority(),
                    "peer",
                    123,
                    TaskIds { sdk: 1, host: 2 },
                )
                .unwrap();
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 },
                )
                .unwrap();
            let cookie = state.cookie.clone();
            let stop = StopTransport {
                inner: &transport,
                unsent,
            };
            assert!(session
                .poll(&stop, &mut state, authority(), TaskIds { sdk: 3, host: 4 })
                .is_err());
            assert_eq!(state.cookie, cookie);
            if unsent {
                assert_eq!(session.phase(), PersonalPhase::Joined);
                session
                    .exit(
                        &transport,
                        &mut state,
                        authority(),
                        TaskIds { sdk: 4, host: 5 },
                        false,
                    )
                    .unwrap();
                assert_eq!(session.phase(), PersonalPhase::Finished);
            } else {
                assert_eq!(session.phase(), PersonalPhase::Uncertain);
            }
        }
    }

    #[test]
    fn pending_codec_zero_continues_sync_until_both_av_and_selected_codec_are_ready() {
        let transport = FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
            (CallHostRoute::AppAuth, auth_reply()),
            (
                CallHostRoute::Invite,
                with_result(&invite_reply_profile(0, 2), "peer", 23),
            ),
            (
                CallHostRoute::LiveSync,
                host_reply(&p::bytes(2, b"waiting")),
            ),
            (
                CallHostRoute::LiveSync,
                joined_sync_profile(false, Some(17)),
            ),
        ])));
        let mut state = state();
        let mut session = NativePersonalSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                &transport,
                &mut state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        assert_eq!(session.phase(), PersonalPhase::Ringing);
        assert!(!session.media_ready(&state, authority()).unwrap());
        assert!(session.take_media(&state, authority(), 0, 0).is_err());
        assert!(p::optional(
            &fields(&session.active.as_ref().unwrap().engine).unwrap(),
            5
        )
        .unwrap()
        .is_none());
        assert_eq!(
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 }
                )
                .unwrap(),
            SyncProgress::Waiting
        );
        assert_eq!(session.active.as_ref().unwrap().cookies, b"waiting");
        assert_eq!(
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 3, host: 4 }
                )
                .unwrap(),
            SyncProgress::MediaReady
        );
        assert!(session.media_ready(&state, authority()).unwrap());
        assert_eq!(
            session
                .take_media(&state, authority(), 0, 0)
                .unwrap()
                .peer_member,
            1
        );
    }
    #[test]
    fn pending_capability_shapes_are_checked_before_cursor_installation() {
        let (engine, channel) = capabilities_for_codec(0);
        for (bad_engine, bad_channel) in [
            ([engine.clone(), p::uint(99, 0)].concat(), channel.clone()),
            ([engine.clone(), p::uint(6, 0)].concat(), channel.clone()),
            (p::bytes(6, b"wrong-wire"), channel.clone()),
            (
                engine.clone(),
                p::message(&[p::uint(1, u64::MAX), p::uint(2, 3)]).unwrap(),
            ),
        ] {
            let transport =
                FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                    (CallHostRoute::AppAuth, auth_reply()),
                    (CallHostRoute::Invite, invite_reply_profile(0, 2)),
                    (
                        CallHostRoute::LiveSync,
                        capability_sync(&bad_engine, &bad_channel),
                    ),
                ])));
            let mut state = state();
            let mut session = NativePersonalSession::authenticate(
                &transport,
                &mut state,
                authority(),
                1,
                [9; 32],
                parameters(),
            )
            .unwrap();
            session
                .invite(
                    &transport,
                    &mut state,
                    authority(),
                    "peer",
                    123,
                    TaskIds { sdk: 1, host: 2 },
                )
                .unwrap();
            assert!(session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 }
                )
                .is_err());
            assert_eq!(session.phase(), PersonalPhase::Uncertain);
            let active = session.active.as_ref().unwrap();
            assert_eq!(active.cookies, b"old");
            assert_eq!(active.cap_sequence, 1);
            assert_eq!(active.members["peer"].status, 2);
            assert!(session.take_media(&state, authority(), 0, 0).is_err());
        }
    }
    #[test]
    fn initial_full_roster_terminates_before_profile_and_recipient_result_is_not_overall_error() {
        for (own, peer, action) in [
            (
                Some(1),
                Some(6),
                RemoteTermination::Exit(personal::RemoteExitReason::PeerBusy),
            ),
            (
                Some(1),
                Some(7),
                RemoteTermination::Exit(personal::RemoteExitReason::PeerRejected),
            ),
            (
                Some(1),
                None,
                RemoteTermination::Exit(personal::RemoteExitReason::LastPeerLeft),
            ),
            (
                None,
                Some(1),
                RemoteTermination::LocalStop(LocalStopReason::SelfRemoved),
            ),
            (
                Some(2),
                Some(1),
                RemoteTermination::LocalStop(LocalStopReason::SelfRemoved),
            ),
        ] {
            let local = matches!(action, RemoteTermination::LocalStop(_));
            let mut replies = std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, auth_reply()),
                (CallHostRoute::Invite, initial_roster_reply(1, own, peer)),
            ]);
            if !local {
                replies.push_back((CallHostRoute::Transfer, exit_reply()));
            }
            let transport = RecordingTransport {
                inner: FakeTransport(std::cell::RefCell::new(replies)),
                requests: Default::default(),
            };
            let mut state = state();
            let mut session = NativePersonalSession::authenticate(
                &transport,
                &mut state,
                authority(),
                1,
                [9; 32],
                parameters(),
            )
            .unwrap();
            session
                .invite(
                    &transport,
                    &mut state,
                    authority(),
                    "peer",
                    123,
                    TaskIds { sdk: 1, host: 2 },
                )
                .unwrap();
            assert_eq!(session.control.as_ref().unwrap().terminal, Some(action));
            assert!(session.active.is_none());
            assert!(!session.media_ready(&state, authority()).unwrap());
            if local {
                assert_eq!(session.phase(), PersonalPhase::Finished);
            } else {
                assert_eq!(session.phase(), PersonalPhase::TerminalPending);
                session
                    .finish_remote(
                        &transport,
                        &mut state,
                        authority(),
                        TaskIds { sdk: 2, host: 3 },
                        false,
                    )
                    .unwrap();
                let expected = match action {
                    RemoteTermination::Exit(reason) => reason.wire(),
                    _ => unreachable!(),
                };
                assert_eq!(
                    recorded_exit_reason(&transport.requests.borrow().last().unwrap().1),
                    expected
                );
            }
            assert_eq!(session.phase(), PersonalPhase::Finished);
            assert!(transport.inner.0.borrow().is_empty());
        }
    }
    #[test]
    fn initial_zero_sequence_or_unknown_membership_is_not_terminal_proof() {
        for (sequence, own, peer) in [
            (0, None, Some(1)),
            (1, Some(1), Some(99)),
            (1, Some(1), Some(8)),
        ] {
            let transport =
                FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                    (CallHostRoute::AppAuth, auth_reply()),
                    (
                        CallHostRoute::Invite,
                        initial_roster_reply(sequence, own, peer),
                    ),
                ])));
            let mut state = state();
            let mut session = NativePersonalSession::authenticate(
                &transport,
                &mut state,
                authority(),
                1,
                [9; 32],
                parameters(),
            )
            .unwrap();
            assert!(session
                .invite(
                    &transport,
                    &mut state,
                    authority(),
                    "peer",
                    123,
                    TaskIds { sdk: 1, host: 2 }
                )
                .is_err());
            assert_eq!(session.phase(), PersonalPhase::Uncertain);
            assert!(session.control.as_ref().unwrap().terminal.is_none());
            assert!(session
                .finish_remote(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 },
                    false
                )
                .is_err());
            assert!(transport.0.borrow().is_empty());
        }
    }
    #[test]
    fn pending_unselected_control_is_not_media_admission_and_cancels_once() {
        let cancel = host_reply(
            &p::message(&[p::bytes(
                1,
                &p::message(&[p::uint(1, 7), p::uint(2, 9)]).unwrap(),
            )])
            .unwrap(),
        );
        let transport = FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::Invite, invite_reply_profile(0, 2)),
            (CallHostRoute::Cancel, cancel),
        ])));
        let mut state = state();
        let mut session = NativePersonalSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                &transport,
                &mut state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        assert!(!session.media_ready(&state, authority()).unwrap());
        assert!(session.has_allocated_control());
        assert_eq!(session.phase(), PersonalPhase::Ringing);
        assert!(session.take_media(&state, authority(), 0, 0).is_err());
        struct CheckCancel<'a>(&'a FakeTransport);
        impl PersonalTransport for CheckCancel<'_> {
            fn call_rpc(
                &self,
                state: &mut AccountState,
                route: CallHostRoute,
                request: &[u8],
            ) -> Result<Vec<u8>> {
                assert_eq!(route, CallHostRoute::Cancel);
                let common = CommonRequest::decode(request)?;
                let outer = fields(common.body.as_deref().unwrap())?;
                let inner = fields(data(&outer, 1)?)?;
                assert_eq!(number(&inner, 3, None)?, 7);
                assert_eq!(number(&inner, 4, None)?, 9);
                assert_eq!(number(&inner, 5, None)?, 123);
                assert_eq!(data(&outer, 4)?, b"peer");
                self.0.call_rpc(state, route, request)
            }
        }
        session
            .cancel(
                &CheckCancel(&transport),
                &mut state,
                authority(),
                TaskIds { sdk: 2, host: 3 },
            )
            .unwrap();
        assert_eq!(session.phase(), PersonalPhase::Finished);
        assert!(transport.0.borrow().is_empty());
        assert!(session
            .cancel(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 }
            )
            .is_err());
    }
    #[test]
    fn missing_cleanup_reply_stays_uncertain_and_is_not_retried() {
        let transport = FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::Invite, invite_reply_profile(0, 2)),
        ])));
        let mut state = state();
        let mut session = NativePersonalSession::authenticate(
            &transport,
            &mut state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                &transport,
                &mut state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        assert!(session
            .cancel(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 2, host: 3 }
            )
            .is_err());
        assert_eq!(session.phase(), PersonalPhase::Uncertain);
        assert!(session
            .cancel(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 }
            )
            .is_err());
        assert!(transport.0.borrow().is_empty());
    }
    pub(super) fn roster_sync(
        sequence: u32,
        own: Option<u32>,
        peer: Option<u32>,
        unknown_after: bool,
    ) -> Vec<u8> {
        let mut roster = vec![
            p::uint(1, 7),
            p::uint(2, 9),
            p::uint(3, u64::from(sequence)),
        ];
        if let Some(status) = own {
            roster.push(p::bytes(4, &member("own", 0, status)));
        }
        if let Some(status) = peer {
            roster.push(p::bytes(4, &member("peer", 1, status)));
        }
        let mut notify = vec![
            p::uint(3, 7),
            p::uint(4, 9),
            p::bytes(
                5,
                &p::message(&[p::uint(1, 10), p::bytes(2, &p::message(&roster).unwrap())]).unwrap(),
            ),
        ];
        if unknown_after {
            notify.push(p::bytes(
                5,
                &p::message(&[p::uint(1, 999), p::bytes(2, &[])]).unwrap(),
            ));
        }
        let message = p::message(&[
            p::uint(1, 10002),
            p::bytes(2, &p::message(&notify).unwrap()),
        ])
        .unwrap();
        host_reply(
            &p::message(&[
                p::bytes(
                    1,
                    &p::message(&[p::uint(1, 1), p::bytes(2, &message)]).unwrap(),
                ),
                p::bytes(2, b"terminal-cursor"),
            ])
            .unwrap(),
        )
    }
    pub(super) struct RecordingTransport {
        pub(super) inner: FakeTransport,
        pub(super) requests: std::cell::RefCell<Vec<(CallHostRoute, Vec<u8>)>>,
    }
    impl PersonalTransport for RecordingTransport {
        fn call_rpc(
            &self,
            state: &mut AccountState,
            route: CallHostRoute,
            request: &[u8],
        ) -> Result<Vec<u8>> {
            self.requests.borrow_mut().push((route, request.to_vec()));
            self.inner.call_rpc(state, route, request)
        }
    }
    fn caller_with(
        transport: &impl PersonalTransport,
        state: &mut AccountState,
    ) -> NativePersonalSession {
        let mut session = NativePersonalSession::authenticate(
            transport,
            state,
            authority(),
            1,
            [9; 32],
            parameters(),
        )
        .unwrap();
        session
            .invite(
                transport,
                state,
                authority(),
                "peer",
                123,
                TaskIds { sdk: 1, host: 2 },
            )
            .unwrap();
        session
    }
    pub(super) fn recorded_exit_reason(request: &[u8]) -> u64 {
        let common = CommonRequest::decode(request).unwrap();
        let delegate = fields(common.body.as_deref().unwrap()).unwrap();
        assert_eq!(number(&delegate, 2, None).unwrap(), 3380);
        let frame = app_frame::TaskFrame::decode(data(&delegate, 5).unwrap(), 3380).unwrap();
        let cloud = fields(&app_frame::lz4_decode(&frame.payload, 64 * 1024).unwrap()).unwrap();
        number(&fields(data(&cloud, 2).unwrap()).unwrap(), 6, None).unwrap()
    }
    #[test]
    fn caller_remote_last_peer_uses_one_exit_with_distinct_reason_and_no_local_cancel() {
        for (peer, wire) in [(4, 5), (6, 7), (7, 6)] {
            let transport = RecordingTransport {
                inner: FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                    (CallHostRoute::AppAuth, auth_reply()),
                    (CallHostRoute::Invite, invite_reply()),
                    (
                        CallHostRoute::LiveSync,
                        roster_sync(2, Some(1), Some(peer), false),
                    ),
                    (CallHostRoute::Transfer, exit_reply()),
                ]))),
                requests: Default::default(),
            };
            let mut state = state();
            let mut session = caller_with(&transport, &mut state);
            assert!(matches!(
                session
                    .poll(
                        &transport,
                        &mut state,
                        authority(),
                        TaskIds { sdk: 2, host: 3 }
                    )
                    .unwrap(),
                SyncProgress::Terminal(RemoteTermination::Exit(_))
            ));
            assert_eq!(session.phase(), PersonalPhase::TerminalPending);
            assert!(session.take_media(&state, authority(), 0, 0).is_err());
            session
                .finish_remote(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 3, host: 4 },
                    false,
                )
                .unwrap();
            let requests = transport.requests.borrow();
            let last = requests.last().unwrap();
            assert_eq!(last.0, CallHostRoute::Transfer);
            assert_eq!(recorded_exit_reason(&last.1), wire);
            assert_eq!(session.phase(), PersonalPhase::Finished);
            assert!(session
                .finish_remote(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 4, host: 5 },
                    false
                )
                .is_err());
            assert!(transport.inner.0.borrow().is_empty());
        }
    }
    #[test]
    fn no_response_cancel_uses_original_id_and_direct_invite_timeout_wire_reason() {
        let cancel = host_reply(&p::bytes(
            1,
            &p::message(&[p::uint(1, 7), p::uint(2, 9)]).unwrap(),
        ));
        let transport = RecordingTransport {
            inner: FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
                (CallHostRoute::AppAuth, auth_reply()),
                (CallHostRoute::Invite, invite_reply()),
                (
                    CallHostRoute::LiveSync,
                    roster_sync(2, Some(1), Some(8), false),
                ),
                (CallHostRoute::Cancel, cancel),
            ]))),
            requests: Default::default(),
        };
        let mut state = state();
        let mut session = caller_with(&transport, &mut state);
        assert_eq!(
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 }
                )
                .unwrap(),
            SyncProgress::Terminal(RemoteTermination::NoResponseCancel)
        );
        session
            .finish_remote(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 },
                false,
            )
            .unwrap();
        let requests = transport.requests.borrow();
        let last = requests.last().unwrap();
        assert_eq!(last.0, CallHostRoute::Cancel);
        let common = CommonRequest::decode(&last.1).unwrap();
        let outer = fields(common.body.as_deref().unwrap()).unwrap();
        assert_eq!(number(&outer, 2, None).unwrap(), 123);
        assert_eq!(repeated(&outer, 4, 16).unwrap(), vec![b"peer".as_slice()]);

        let inner = fields(data(&outer, 1).unwrap()).unwrap();
        assert_eq!(number(&inner, 5, None).unwrap(), 123);
        assert_eq!(number(&inner, 6, None).unwrap(), 2);
    }
    #[test]
    fn previously_joined_peer_permanently_disarms_original_pending_cancel() {
        let transport = FakeTransport(std::cell::RefCell::new(std::collections::VecDeque::from([
            (CallHostRoute::AppAuth, auth_reply()),
            (CallHostRoute::Invite, invite_reply()),
            (CallHostRoute::LiveSync, joined_sync(false)),
            (
                CallHostRoute::LiveSync,
                roster_sync(3, Some(1), Some(8), false),
            ),
        ])));
        let mut state = state();
        let mut session = caller_with(&transport, &mut state);
        assert_eq!(
            session
                .poll(
                    &transport,
                    &mut state,
                    authority(),
                    TaskIds { sdk: 2, host: 3 }
                )
                .unwrap(),
            SyncProgress::MediaReady
        );
        assert!(!session.control.as_ref().unwrap().invitation_pending);
        assert!(session
            .control
            .as_ref()
            .unwrap()
            .pending_recipients
            .is_empty());
        assert!(session
            .poll(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 3, host: 4 }
            )
            .is_err());
        assert_eq!(session.phase(), PersonalPhase::Uncertain);
        assert_eq!(session.active.as_ref().unwrap().cookies, b"new");
        assert!(session
            .finish_remote(
                &transport,
                &mut state,
                authority(),
                TaskIds { sdk: 4, host: 5 },
                false
            )
            .is_err());
        assert!(transport.0.borrow().is_empty());
    }
}
