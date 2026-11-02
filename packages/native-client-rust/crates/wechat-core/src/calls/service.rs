//! Account-bound call service shared by HTTP/WebSocket clients and native actors.
//! The backend owns remote operations and authentication; ownership is reserved
//! before dispatch and stale callbacks cannot affect a later call.
use super::ownership::{
    self, AccountAuthority, CallIdentity, Decision, Operation, Ownership, Phase, Ticket,
};
use crate::call_types::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex, Weak,
    },
    time::Instant,
};
use tokio::sync::mpsc;
use wechat_calls::{
    audio::AudioFormat,
    frame::{self, MediaFrame, MediaHeader, MediaStream},
};
#[path = "service/ingress.rs"]
mod ingress;

type BackendFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[derive(Clone, Copy, Debug)]
pub struct CallServiceError(pub CallErrorCode);
impl CallServiceError {
    pub fn code(&self) -> &'static str {
        match self.0 {
            CallErrorCode::InvalidRequest => "invalid_request",
            CallErrorCode::Busy => "busy",
            CallErrorCode::AlreadyClaimed => "already_claimed",
            CallErrorCode::OfferExpired => "offer_expired",
            CallErrorCode::NotOwner => "not_owner",
            CallErrorCode::Unsupported => "unsupported",
            CallErrorCode::MediaNotReady => "media_not_ready",
            CallErrorCode::OutcomeUnknown => "outcome_unknown",
            CallErrorCode::Capacity => "capacity",
            CallErrorCode::Stale => "stale",
            CallErrorCode::Forbidden => "forbidden",
            CallErrorCode::Closed => "closed",
        }
    }
}
impl std::fmt::Display for CallServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for CallServiceError {}
impl From<ownership::Error> for CallServiceError {
    fn from(e: ownership::Error) -> Self {
        Self(match e {
            ownership::Error::Busy => CallErrorCode::Busy,
            ownership::Error::NotReady => CallErrorCode::MediaNotReady,
            ownership::Error::NotOwner => CallErrorCode::NotOwner,
            ownership::Error::Capacity => CallErrorCode::Capacity,
            ownership::Error::Stale => CallErrorCode::Stale,
            ownership::Error::NotConnected => CallErrorCode::Closed,
            _ => CallErrorCode::InvalidRequest,
        })
    }
}
type Result<T> = std::result::Result<T, CallServiceError>;
fn error(code: CallErrorCode) -> CallServiceError {
    CallServiceError(code)
}

pub struct ControlConnection {
    pub client_id: String,
    pub events: mpsc::Receiver<CallEvent>,
}
pub struct MediaConnection {
    pub id: String,
    pub frames: mpsc::Receiver<Vec<u8>>,
    pub ready: CallEvent,
}
#[derive(Clone, Copy)]
pub enum BackendOutcome {
    Admitted,
    Connected,
    Ended,
    Uncertain,
}
#[derive(Clone)]
pub struct BackendRequest {
    pub ticket: Ticket,
    pub peer_id: String,
    pub call_type: CallType,
}
/// Owned audio admitted for one call generation. The backend must fence delayed
/// submissions against its latest generation and accepted account authority.
pub struct BackendAudioRequest {
    pub identity: CallIdentity,
    pub format: CallAudioFormat,
    pub generation: u32,
    pub sequence: u64,
    pub timestamp_micros: u64,
    pub samples: Vec<i16>,
    lease: Arc<AtomicU32>,
}
impl BackendAudioRequest {
    /// Recheck immediately before packet submission; an async queue must retain
    /// this permit rather than just copying its original generation number.
    pub fn is_current(&self) -> bool {
        self.generation != 0 && self.lease.load(Ordering::Acquire) == self.generation
    }
}
/// Implementations must persist each remote mutation before submission and check
/// current account/logout generations. No backend is implicitly enabled by a socket.
pub trait CallBackend: Send + Sync {
    fn authority(&self, account: &str) -> Option<AccountAuthority>;
    fn capabilities(&self, account: &str) -> CallCapabilities;
    /// This only admits an actor's supported identity/scope; it performs no I/O.
    fn incoming_supported(&self, _authority: &AccountAuthority, _caller: &str) -> bool {
        false
    }
    /// Called outside the sync staging/account-lock path, once per reservation.
    /// Install the exact actor/mailbox and mark request.registration before the first preparation await so a
    /// concurrent Cancel can be admitted before Auth completes and ACK begins.
    /// Returning Acked also requires that actor to handle Answer/Reject.
    fn prepare_incoming(
        &self,
        _request: super::notifications::IncomingRequest,
    ) -> BackendFuture<Result<super::notifications::IncomingPreparation>> {
        Box::pin(async { Ok(super::notifications::IncomingPreparation::Ignored) })
    }
    /// Classify an owned cancellation against the actor's serialized source
    /// phase. Delivery may run while preparation awaits Auth; it must not wait
    /// on a broker-held account lock or invent a remote refusal/Exit.
    fn cancel_incoming(
        &self,
        _identity: CallIdentity,
        _notification: Arc<super::notifications::IncomingNotification>,
    ) -> BackendFuture<Result<super::notifications::IncomingCancellation>> {
        Box::pin(async { Ok(super::notifications::IncomingCancellation::Uncertain) })
    }
    fn submit(
        &self,
        request: BackendRequest,
        sink: BackendSink,
    ) -> BackendFuture<Result<BackendOutcome>>;
    /// Check identity, accepted account authority and generation again at the
    /// actual packet submission point, including work delayed in an async queue.
    fn send_audio(&self, request: BackendAudioRequest) -> BackendFuture<Result<()>>;
    /// Install a newer send generation and discard older queued/in-flight audio.
    fn clear_audio(&self, _identity: CallIdentity, _generation: u32) -> BackendFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn stop(&self, identity: CallIdentity) -> BackendFuture<Result<BackendOutcome>>;
}
pub struct DisabledBackend;
impl CallBackend for DisabledBackend {
    fn authority(&self, _: &str) -> Option<AccountAuthority> {
        None
    }
    fn capabilities(&self, _: &str) -> CallCapabilities {
        CallCapabilities::default()
    }
    fn submit(&self, _: BackendRequest, _: BackendSink) -> BackendFuture<Result<BackendOutcome>> {
        Box::pin(async { Err(error(CallErrorCode::Unsupported)) })
    }
    fn send_audio(&self, _: BackendAudioRequest) -> BackendFuture<Result<()>> {
        Box::pin(async { Err(error(CallErrorCode::Unsupported)) })
    }
    fn stop(&self, _: CallIdentity) -> BackendFuture<Result<BackendOutcome>> {
        Box::pin(async { Ok(BackendOutcome::Uncertain) })
    }
}
#[derive(Clone)]
pub struct BackendSink {
    service: Weak<CallService>,
    ticket: Ticket,
}
impl BackendSink {
    /// Admission for delayed nonterminal work, including initial preparation.
    /// This reads broker state; do not call it inside a UDP submission gate.
    pub fn is_current(&self) -> bool {
        let Some(service) = self.service.upgrade() else {
            return false;
        };
        let Ok(s) = service.lock() else {
            return false;
        };
        service.ticket_current(&s, &self.ticket)
    }
    /// A pre-media Answer can lose its original controller while Join is in
    /// flight. This identifies that narrow cleanup case; generic uncertainty
    /// or a stale ticket does not grant another remote operation.
    #[cfg(feature = "native-calls")]
    pub(crate) fn lost_incoming_answer_owner(&self) -> bool {
        let Some(service) = self.service.upgrade() else {
            return false;
        };
        let Ok(s) = service.lock() else {
            return false;
        };
        if self.ticket.operation() != Operation::Answer
            || !s.ownership.pending_ticket_matches(&self.ticket)
        {
            return false;
        }
        let Some(call) = s.call.as_ref().filter(|call| {
            call.identity == self.ticket.identity()
                && &call.authority == self.ticket.authority()
                && call.direction == CallDirection::Incoming
                && call.state == CallState::Connecting
                && !call.revoked
                && !call.cancellation_requested
        }) else {
            return false;
        };
        let Some(owner) = call.owner.as_ref() else {
            return false;
        };
        let disconnected = match s.clients.get(owner) {
            Some(peer) => peer.authority == call.authority && peer.events.is_closed(),
            None => true,
        };
        disconnected
            && service
                .backend
                .authority(&call.authority.account_id)
                .as_ref()
                == Some(&call.authority)
    }
    /// Admission for the current owner's exact accepted terminal action.
    /// Owner-loss cleanup uses its separately verified native context instead.
    pub fn is_terminal_current(&self) -> bool {
        let Some(service) = self.service.upgrade() else {
            return false;
        };
        let Ok(s) = service.lock() else {
            return false;
        };
        if !matches!(
            self.ticket.operation(),
            Operation::Reject | Operation::Cancel | Operation::Hangup
        ) || !s.ownership.pending_ticket_matches(&self.ticket)
        {
            return false;
        }
        let Some(call) = s.call.as_ref().filter(|call| {
            call.identity == self.ticket.identity()
                && &call.authority == self.ticket.authority()
                && call.state == CallState::Ending
                && !call.revoked
                && !call.cancellation_requested
        }) else {
            return false;
        };
        let Some(owner) = call.owner.as_ref().and_then(|owner| s.clients.get(owner)) else {
            return false;
        };
        owner.control
            && !owner.events.is_closed()
            && owner.authority == call.authority
            && service
                .backend
                .authority(&call.authority.account_id)
                .as_ref()
                == Some(&call.authority)
    }
    pub async fn connected(&self) -> Result<()> {
        self.service
            .upgrade()
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .connected(&self.ticket)
    }
    pub async fn uncertain(&self) -> Result<()> {
        self.service
            .upgrade()
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .uncertain(&self.ticket)
    }
    /// Publish autonomous teardown before late owner PCM reaches a stopped worker.
    pub async fn ending(&self) -> Result<()> {
        self.service
            .upgrade()
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .ending(&self.ticket)
    }
    pub async fn ended(&self) -> Result<()> {
        self.service
            .upgrade()
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .terminal(self.ticket.authority(), &self.ticket.identity())
    }
    pub async fn audio(&self, samples: &[i16]) -> Result<()> {
        self.service
            .upgrade()
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .received_audio(&self.ticket.identity(), samples)
    }
}
struct Peer {
    account: String,
    authority: AccountAuthority,
    principal: String,
    control: bool,
    incoming_ready: Option<CallReadyScope>,
    events: mpsc::Sender<CallEvent>,
}
struct LiveCall {
    identity: CallIdentity,
    authority: AccountAuthority,
    peer: String,
    call_type: CallType,
    direction: CallDirection,
    owner: Option<String>,
    token: Option<String>,
    state: CallState,
    revision: u32,
    send_generation: u32,
    receive_generation: u32,
    audio_lease: Arc<AtomicU32>,
    offer_until: Option<u64>,
    stop_requested: bool,
    revoked: bool,
    sequence: u64,
    sample_clock: u64,
    muted: bool,
    cancellation_requested: bool,
}
struct MediaPeer {
    client: String,
    identity: CallIdentity,
    stream: MediaStream,
    frames: mpsc::Sender<Vec<u8>>,
}
#[derive(Default)]
struct State {
    ownership: Ownership,
    clients: BTreeMap<String, Peer>,
    call: Option<LiveCall>,
    media: BTreeMap<String, MediaPeer>,
    receipts: BTreeMap<(String, String), CommandReceipt>,
    fences: Vec<(CallIdentity, u32)>,
    expired_offers: BTreeMap<(String, String), u64>,
    recovered_unknown: BTreeSet<(String, String)>,
    incoming: super::notifications::Registry,
}
struct CommandReceipt {
    fingerprint: [u8; 32],
    identity: Option<CallIdentity>,
    claims_owner: bool,
    expires: u64,
}
#[derive(Default)]
struct Prepared {
    submit: Option<BackendRequest>,
    clear: Option<(CallIdentity, u32)>,
    wake_incoming: bool,
}
pub struct CallService {
    state: Mutex<State>,
    backend: Arc<dyn CallBackend>,
    started: Instant,
    maintenance_started: AtomicBool,
    incoming_preparations: mpsc::Sender<super::notifications::IncomingRequest>,
    incoming_cancellations: mpsc::Sender<ingress::IncomingCancel>,
    incoming_receiver: Mutex<Option<ingress::IncomingReceivers>>,
}
impl Default for CallService {
    fn default() -> Self {
        Self::new(Arc::new(DisabledBackend))
    }
}
impl CallService {
    pub fn new(backend: Arc<dyn CallBackend>) -> Self {
        let (incoming_preparations, preparations) = mpsc::channel(1);
        let (incoming_cancellations, cancellations) = mpsc::channel(8);
        Self {
            state: Mutex::new(State::default()),
            backend,
            started: Instant::now(),
            maintenance_started: AtomicBool::new(false),
            incoming_preparations,
            incoming_cancellations,
            incoming_receiver: Mutex::new(Some(ingress::IncomingReceivers {
                preparations,
                cancellations,
            })),
        }
    }
    /// Install an unresolved operational journal record before serving clients.
    /// No owner, direction, room context or terminal result is fabricated. The
    /// global barrier remains until a separately verified recovery resolves it.
    pub fn recover_unknown(&self, account_id: &str, public_call_id: &str) -> Result<()> {
        if [account_id, public_call_id].into_iter().any(|value| {
            value.is_empty() || value.len() > 200 || value.chars().any(char::is_control)
        }) {
            return Err(error(CallErrorCode::InvalidRequest));
        }
        let mut s = self.lock()?;
        if s.call.is_some() {
            return Err(error(CallErrorCode::Busy));
        }
        let key = (account_id.to_owned(), public_call_id.to_owned());
        if s.recovered_unknown.contains(&key) {
            return Ok(());
        }
        if s.recovered_unknown.len() >= 256 {
            return Err(error(CallErrorCode::Capacity));
        }
        s.recovered_unknown.insert(key);
        self.broadcast(&mut s);
        Ok(())
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| error(CallErrorCode::Closed))
    }
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn start_maintenance(self: &Arc<Self>) {
        if self.maintenance_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                service.maintenance().await;
                service.wake_incoming();
                let active = service
                    .lock()
                    .map(|s| {
                        let active = !s.clients.is_empty() || s.call.is_some();
                        if !active {
                            service.maintenance_started.store(false, Ordering::Release);
                        }
                        active
                    })
                    .unwrap_or(false);
                if !active {
                    break;
                }
                // Do not retain a strong service reference across the next sleep.
                drop(service);
            }
        });
    }
    fn drop_client(&self, s: &mut State, id: &str) {
        if s.clients.remove(id).is_none() {
            return;
        }
        s.ownership.disconnect(id, self.now());
        s.media.retain(|_, m| m.client != id);
        if let Some(c) = s.call.as_mut().filter(|c| c.owner.as_deref() == Some(id)) {
            c.audio_lease.store(0, Ordering::Release);
            if let Some(generation) = c.send_generation.checked_add(1) {
                c.send_generation = generation;
                s.fences.push((c.identity.clone(), generation));
            }
        }
    }
    async fn maintenance(&self) {
        let (jobs, fences) = match self.lock() {
            Ok(mut s) => {
                let now = self.now();
                s.receipts.retain(|_, receipt| now < receipt.expires);
                s.expired_offers.retain(|_, expires| now < *expires);
                let mut changed = false;
                changed |= self.prune_incoming(&mut s);
                let closed: Vec<_> = s
                    .clients
                    .iter()
                    .filter(|(_, p)| p.events.is_closed())
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in closed {
                    self.drop_client(&mut s, &id);
                    changed = true;
                }
                let stale: Vec<_> = s
                    .clients
                    .iter()
                    .filter(|(_, p)| {
                        self.backend.authority(&p.account).as_ref() != Some(&p.authority)
                            && p.authority.generation != "unavailable"
                    })
                    .map(|(id, p)| (id.clone(), p.authority.clone()))
                    .collect();
                for (id, authority) in stale {
                    if let Some(peer) = s.clients.get(&id) {
                        let _ = peer.events.try_send(CallEvent::Error {
                            code: CallErrorCode::Stale,
                        });
                    }
                    s.ownership.invalidate(&authority);
                    self.drop_client(&mut s, &id);
                    changed = true;
                }
                let expired_offer = s.call.as_ref().is_some_and(|c| {
                    c.owner.is_none() && c.offer_until.is_some_and(|until| now >= until)
                });
                let expired_owner = s.ownership.expire(now).map(|(_, identity)| identity);
                if expired_offer {
                    if let Some(c) = &s.call {
                        c.audio_lease.store(0, Ordering::Release);
                        let key = (
                            c.authority.account_id.clone(),
                            c.identity.call_id().to_owned(),
                        );
                        if s.expired_offers.len() >= 256 {
                            if let Some(old) = s
                                .expired_offers
                                .iter()
                                .min_by_key(|(_, expires)| **expires)
                                .map(|(key, _)| key.clone())
                            {
                                s.expired_offers.remove(&old);
                            }
                        }
                        s.expired_offers.insert(key, now.saturating_add(300_000));
                    }
                    s.call = None;
                    s.media.clear();
                    changed = true;
                }
                let mut jobs = Vec::new();
                let mut stop_media = false;
                if let Some(c) = s.call.as_mut() {
                    let stale = self.backend.authority(&c.authority.account_id).as_ref()
                        != Some(&c.authority);
                    if (stale && !c.revoked) || expired_owner.as_ref() == Some(&c.identity) {
                        c.revoked |= stale;
                        c.audio_lease.store(0, Ordering::Release);
                        c.state = CallState::Uncertain;
                        c.revision = c.revision.saturating_add(1);
                        stop_media = true;
                        changed = true;
                    }
                    if c.state == CallState::Uncertain && !c.stop_requested {
                        c.stop_requested = true;
                        jobs.push((c.authority.clone(), c.identity.clone()));
                    }
                }
                if stop_media {
                    s.media.clear();
                }
                if changed {
                    self.broadcast(&mut s);
                }
                let fences = std::mem::take(&mut s.fences);
                (jobs, fences)
            }
            Err(_) => return,
        };
        for (identity, generation) in fences {
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    self.backend.clear_audio(identity.clone(), generation)
                )
                .await,
                Ok(Ok(()))
            ) {
                self.mark_uncertain(&identity);
            }
        }
        for (authority, identity) in jobs {
            if matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    self.backend.stop(identity.clone())
                )
                .await,
                Ok(Ok(BackendOutcome::Ended))
            ) {
                let _ = self.terminal(&authority, &identity);
            }
        }
    }
    fn mark_uncertain(&self, identity: &CallIdentity) {
        if let Ok(mut s) = self.lock() {
            let authority = match s.call.as_ref().filter(|c| &c.identity == identity) {
                Some(c) => c.authority.clone(),
                None => return,
            };
            let _ = s.ownership.uncertain(&authority, identity);
            if let Some(c) = s.call.as_mut() {
                c.state = CallState::Uncertain;
                c.revision = c.revision.saturating_add(1);
                c.audio_lease.store(0, Ordering::Release);
            }
            s.media.clear();
            self.broadcast(&mut s);
        }
    }
    fn uncertain(&self, ticket: &Ticket) -> Result<()> {
        {
            let s = self.lock()?;
            if !s.call.as_ref().is_some_and(|c| {
                c.identity == ticket.identity() && &c.authority == ticket.authority()
            }) {
                return Err(error(CallErrorCode::Stale));
            }
        }
        self.mark_uncertain(&ticket.identity());
        Ok(())
    }
    fn authority(&self, account: &str) -> AccountAuthority {
        self.backend
            .authority(account)
            .unwrap_or_else(|| AccountAuthority {
                account_id: account.into(),
                generation: "unavailable".into(),
            })
    }
    fn format() -> CallAudioFormat {
        CallAudioFormat::default()
    }
    fn media_format() -> AudioFormat {
        AudioFormat::new(16000, 1, 20).expect("fixed supported format")
    }
    fn snapshot(&self, s: &State, account: &str, client: Option<&str>) -> CallSnapshot {
        let mut snap = CallSnapshot {
            capabilities: self.backend.capabilities(account),
            availability: CallsAvailability {
                busy: s.call.is_some() || !s.recovered_unknown.is_empty(),
                owned_by_this_client: false,
            },
            current_pending_offer: self.pending_incoming(s, account),
            ..CallSnapshot::default()
        };
        if let Some(call) = s
            .call
            .as_ref()
            .filter(|c| c.authority.account_id == account)
        {
            let owner = client.is_some_and(|id| call.owner.as_deref() == Some(id));
            snap.availability.owned_by_this_client = owner;
            if call.direction == CallDirection::Incoming
                && call.state == CallState::Offered
                && call.owner.is_none()
            {
                snap.current_offer = Some(Self::offer_view(call));
            }
            if call.owner.is_some()
                || (call.direction == CallDirection::Incoming
                    && call.owner.is_none()
                    && call.state == CallState::Ended)
            {
                snap.current_call = Some(Self::call_view(call, owner));
            }
        }
        snap
    }
    fn offer_view(c: &LiveCall) -> CallOffer {
        CallOffer {
            call_id: c.identity.call_id().into(),
            revision: c.revision,
            kind: CallKind::Direct,
            call_type: c.call_type,
            caller_id: c.peer.clone(),
        }
    }
    fn call_view(c: &LiveCall, owned: bool) -> CallView {
        CallView {
            call_id: c.identity.call_id().into(),
            revision: c.revision,
            kind: CallKind::Direct,
            call_type: c.call_type,
            direction: c.direction,
            state: c.state,
            owned_by_this_client: owned,
        }
    }
    pub async fn current(&self, account: &str, _principal: &str) -> Result<CallSnapshot> {
        self.maintenance().await;
        let s = self.lock()?;
        Ok(self.snapshot(&s, account, None))
    }
    pub async fn connect(
        self: &Arc<Self>,
        account: &str,
        principal: &str,
        can_control: bool,
    ) -> Result<ControlConnection> {
        self.maintenance().await;
        let authority = self.authority(account);
        let client_id = uuid::Uuid::new_v4().to_string();
        let (tx, events) = mpsc::channel(32);
        let mut s = self.lock()?;
        s.ownership
            .connect(&client_id, authority.clone(), principal)?;
        let snap = self.snapshot(&s, account, Some(&client_id));
        tx.try_send(CallEvent::Hello {
            protocol_version: 1,
            client_id: client_id.clone(),
            capabilities: snap.capabilities,
            availability: snap.availability,
            current_offer: snap.current_offer,
            current_pending_offer: snap.current_pending_offer,
            current_call: snap.current_call,
            audio_format: snap.audio_format,
        })
        .map_err(|_| error(CallErrorCode::Closed))?;
        if !s.recovered_unknown.is_empty() {
            tx.try_send(CallEvent::Error {
                code: CallErrorCode::OutcomeUnknown,
            })
            .map_err(|_| error(CallErrorCode::Closed))?;
        }
        s.clients.insert(
            client_id.clone(),
            Peer {
                account: account.into(),
                authority,
                principal: principal.into(),
                control: can_control,
                incoming_ready: None,
                events: tx,
            },
        );
        self.start_maintenance();
        Ok(ControlConnection { client_id, events })
    }
    fn emit(s: &mut State, client: &str, event: CallEvent) -> Result<()> {
        s.clients
            .get(client)
            .ok_or_else(|| error(CallErrorCode::Closed))?
            .events
            .try_send(event)
            .map_err(|_| error(CallErrorCode::Capacity))
    }
    fn broadcast(&self, s: &mut State) {
        let mut failed = Vec::new();
        for (id, peer) in &s.clients {
            let snap = self.snapshot(s, &peer.account, Some(id));
            let mut events = vec![CallEvent::Availability {
                availability: snap.availability,
            }];
            if let Some(offer) = snap.current_offer {
                events.push(CallEvent::Offered { offer });
            }
            events.push(CallEvent::Pending {
                offer: snap.current_pending_offer,
            });
            if let Some(call) = snap.current_call {
                events.push(CallEvent::State { call });
            }
            if events.into_iter().any(|e| peer.events.try_send(e).is_err()) {
                failed.push(id.clone());
            }
        }
        for id in failed {
            self.drop_client(s, &id);
        }
    }
    /// Trusted account actors only; incoming offers never auto-answer.
    pub async fn offer(
        self: &Arc<Self>,
        authority: AccountAuthority,
        call_id: String,
        caller: String,
        ttl: std::time::Duration,
    ) -> Result<CallIdentity> {
        self.maintenance().await;
        if self.backend.authority(&authority.account_id).as_ref() != Some(&authority) {
            return Err(error(CallErrorCode::Stale));
        }
        let mut s = self.lock()?;
        if !s.recovered_unknown.is_empty() {
            return Err(error(CallErrorCode::Busy));
        }
        let now = self.now();
        let identity = s.ownership.offer(
            authority.clone(),
            &call_id,
            now.saturating_add(
                ttl.as_millis()
                    .try_into()
                    .map_err(|_| error(CallErrorCode::Capacity))?,
            ),
            now,
        )?;
        s.expired_offers
            .remove(&(authority.account_id.clone(), call_id.clone()));
        s.call = Some(LiveCall {
            identity: identity.clone(),
            authority,
            peer: caller,
            call_type: CallType::Voice,
            direction: CallDirection::Incoming,
            owner: None,
            token: None,
            state: CallState::Offered,
            revision: 1,
            send_generation: 1,
            receive_generation: 1,
            audio_lease: Arc::new(AtomicU32::new(1)),
            offer_until: Some(
                now.saturating_add(
                    ttl.as_millis()
                        .try_into()
                        .map_err(|_| error(CallErrorCode::Capacity))?,
                ),
            ),
            stop_requested: false,
            revoked: false,
            sequence: 0,
            sample_clock: 0,
            muted: false,
            cancellation_requested: false,
        });
        self.broadcast(&mut s);
        self.start_maintenance();
        Ok(identity)
    }
    fn valid_peer(&self, s: &State, id: &str) -> Result<()> {
        let p = s
            .clients
            .get(id)
            .ok_or_else(|| error(CallErrorCode::Closed))?;
        if p.events.is_closed() {
            return Err(error(CallErrorCode::Closed));
        }
        if self.authority(&p.account) != p.authority {
            return Err(error(CallErrorCode::Stale));
        }
        Ok(())
    }
    fn owner(s: &State, client: &str, call_id: &str) -> Result<()> {
        let c = s.call.as_ref().ok_or_else(|| error(CallErrorCode::Stale))?;
        if c.identity.call_id() != call_id || c.owner.as_deref() != Some(client) {
            return Err(error(CallErrorCode::NotOwner));
        }
        Ok(())
    }
    fn ticket_current(&self, s: &State, ticket: &Ticket) -> bool {
        let Some(call) = s.call.as_ref().filter(|call| {
            call.identity == ticket.identity()
                && &call.authority == ticket.authority()
                && !call.revoked
                && !call.cancellation_requested
                && matches!(
                    call.state,
                    CallState::Dialing
                        | CallState::Ringing
                        | CallState::Connecting
                        | CallState::Connected
                )
        }) else {
            return false;
        };
        let Some(owner) = call.owner.as_ref().and_then(|owner| s.clients.get(owner)) else {
            return false;
        };
        owner.control
            && !owner.events.is_closed()
            && owner.authority == call.authority
            && self.backend.authority(&call.authority.account_id).as_ref() == Some(&call.authority)
    }
    fn cancel_pending_start(
        &self,
        s: &mut State,
        client: &str,
        id: &str,
        revision: u32,
        operation: Operation,
    ) -> Result<bool> {
        let Some(call) = s.call.as_ref().filter(|call| call.identity.call_id() == id) else {
            return Ok(false);
        };
        let pending = s.ownership.pending_start(&call.authority, &call.identity);
        if !matches!(
            (operation, pending),
            (Operation::Cancel, Some(Operation::Dial))
                | (Operation::Hangup, Some(Operation::Answer))
        ) {
            return Ok(false);
        }
        Self::owner(s, client, id)?;
        let call = s.call.as_mut().expect("pending start");
        if call.revision != revision {
            return Err(error(CallErrorCode::Stale));
        }
        if !call.cancellation_requested {
            call.cancellation_requested = true;
            call.state = CallState::Ending;
            call.revision = call
                .revision
                .checked_add(1)
                .ok_or_else(|| error(CallErrorCode::Capacity))?;
            call.audio_lease.store(0, Ordering::Release);
        }
        // Keep the original pending ticket and its possible remote result.
        // The actor must prove no mutation or perform accepted-context cleanup.
        Ok(true)
    }
    fn interrupt_audio(
        &self,
        s: &mut State,
        client: &str,
        call_id: &str,
    ) -> Result<(CallIdentity, u32)> {
        let call = s.call.as_mut().expect("owner call");
        let generation = call
            .send_generation
            .checked_add(1)
            .ok_or_else(|| error(CallErrorCode::Capacity))?;
        call.send_generation = generation;
        call.audio_lease.store(generation, Ordering::Release);
        let identity = call.identity.clone();
        for media in s
            .media
            .values_mut()
            .filter(|media| media.identity == identity)
        {
            media
                .stream
                .advance_generation(generation)
                .map_err(|_| error(CallErrorCode::Stale))?;
        }
        let event = CallEvent::TrackUpdated {
            call_id: call_id.into(),
            track: CallAudioTrack {
                id: 1,
                generation,
                direction: CallTrackDirection::Send,
                format: Self::format(),
            },
        };
        if Self::emit(s, client, event).is_err() {
            self.drop_client(s, client);
        }
        Ok((identity, generation))
    }
    pub async fn command(self: &Arc<Self>, client: &str, command: CallCommand) -> Result<()> {
        let request_id = command.request_id().to_owned();
        if request_id.is_empty()
            || request_id.len() > 200
            || request_id.chars().any(char::is_control)
        {
            return Err(error(CallErrorCode::InvalidRequest));
        }
        self.maintenance().await;
        let result = self.prepare(client, command);
        let dispatch = match result {
            Ok(value) => value,
            Err(e) => {
                let mut s = self.lock()?;
                Self::emit(
                    &mut s,
                    client,
                    CallEvent::CommandResult {
                        request_id,
                        status: CallCommandStatus::Error,
                        call_id: None,
                        owner_token: None,
                        code: Some(e.0),
                    },
                )?;
                return Ok(());
            }
        };
        if dispatch.wake_incoming {
            self.wake_incoming();
        }
        if let Some(request) = dispatch.submit {
            let service = self.clone();
            let backend = self.backend.clone();
            let sink = BackendSink {
                service: Arc::downgrade(self),
                ticket: request.ticket.clone(),
            };
            tokio::spawn(async move {
                let ticket = request.ticket.clone();
                let outcome = backend
                    .submit(request, sink)
                    .await
                    .unwrap_or(BackendOutcome::Uncertain);
                let _ = service.outcome(&ticket, outcome);
            });
        }
        if let Some((identity, generation)) = dispatch.clear {
            if !matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    self.backend.clear_audio(identity.clone(), generation)
                )
                .await,
                Ok(Ok(()))
            ) {
                self.mark_uncertain(&identity);
                return Err(error(CallErrorCode::OutcomeUnknown));
            }
        }
        Ok(())
    }
    fn prepare(&self, client: &str, command: CallCommand) -> Result<Prepared> {
        let request_id = command.request_id().to_owned();
        let mut s = self.lock()?;
        self.valid_peer(&s, client)?;
        if !s.clients.get(client).is_some_and(|p| p.control) {
            return Err(error(CallErrorCode::Forbidden));
        }
        use sha2::{Digest, Sha256};
        let fingerprint: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&command).map_err(|_| error(CallErrorCode::InvalidRequest))?,
        )
        .into();
        let key = (client.to_owned(), request_id.clone());
        let now = self.now();
        s.receipts.retain(|_, r| now < r.expires);
        if let Some(prior) = s.receipts.get(&key) {
            if prior.fingerprint != fingerprint {
                return Err(error(CallErrorCode::InvalidRequest));
            }
            let owner_token = if prior.claims_owner {
                s.call
                    .as_ref()
                    .filter(|c| {
                        Some(&c.identity) == prior.identity.as_ref()
                            && c.owner.as_deref() == Some(client)
                            && !c.revoked
                    })
                    .and_then(|c| c.token.clone())
            } else {
                None
            };
            let event = CallEvent::CommandResult {
                request_id,
                status: CallCommandStatus::Replayed,
                call_id: prior
                    .identity
                    .as_ref()
                    .map(|identity| identity.call_id().to_owned()),
                owner_token,
                code: None,
            };
            if Self::emit(&mut s, client, event).is_err() {
                self.drop_client(&mut s, client);
            }
            return Ok(Prepared::default());
        }
        if s.receipts.len() >= 256 {
            return Err(error(CallErrorCode::Capacity));
        }
        let claims_owner = matches!(
            command,
            CallCommand::Dial { .. } | CallCommand::Answer { .. }
        );
        if !s.recovered_unknown.is_empty()
            && matches!(
                command,
                CallCommand::Dial { .. } | CallCommand::Answer { .. }
            )
        {
            return Err(error(CallErrorCode::Busy));
        }
        let mut prepared = Prepared::default();
        let mut result_identity = None;
        let decision = match command {
            CallCommand::Ready {
                ready,
                format,
                scope,
                ..
            } => {
                if format != Self::format() {
                    return Err(error(CallErrorCode::Unsupported));
                }
                if ready {
                    self.validate_ready_scope(&s, client, scope.as_ref())?;
                }
                s.ownership.ready(client, ready)?;
                s.clients
                    .get_mut(client)
                    .expect("valid peer")
                    .incoming_ready = ready.then(|| scope.unwrap_or(CallReadyScope::All));
                prepared.wake_incoming = ready;
                None
            }
            CallCommand::Dial {
                target, call_type, ..
            } => {
                let p = &s.clients[client];
                if !self.backend.capabilities(&p.account).direct_audio
                    || call_type != CallType::Voice
                {
                    return Err(error(CallErrorCode::Unsupported));
                }
                let CallTarget::Direct { contact_id } = target else {
                    return Err(error(CallErrorCode::Unsupported));
                };
                if contact_id.is_empty()
                    || contact_id.len() > 200
                    || contact_id.chars().any(char::is_control)
                {
                    return Err(error(CallErrorCode::InvalidRequest));
                }
                // Reuse the original generated ID for an idempotent retry.
                let id = s
                    .call
                    .as_ref()
                    .filter(|c| c.owner.as_deref() == Some(client))
                    .map(|c| c.identity.call_id().to_owned())
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let d = s
                    .ownership
                    .dial(client, &request_id, &id, &contact_id, self.now())?;
                if let Decision::Submit(ticket) = &d {
                    s.call = Some(LiveCall {
                        identity: ticket.identity(),
                        authority: ticket.authority().clone(),
                        peer: contact_id,
                        call_type,
                        direction: CallDirection::Outgoing,
                        owner: Some(client.into()),
                        token: Some(format!(
                            "{}{}",
                            uuid::Uuid::new_v4().simple(),
                            uuid::Uuid::new_v4().simple()
                        )),
                        state: CallState::Dialing,
                        revision: 1,
                        send_generation: 1,
                        receive_generation: 1,
                        audio_lease: Arc::new(AtomicU32::new(1)),
                        offer_until: None,
                        stop_requested: false,
                        revoked: false,
                        sequence: 0,
                        sample_clock: 0,
                        muted: false,
                        cancellation_requested: false,
                    });
                }
                Some(d)
            }
            CallCommand::Answer {
                call_id,
                expected_revision,
                ..
            } => {
                self.require_incoming_offer(&s, &call_id)?;
                if s.expired_offers
                    .contains_key(&(s.clients[client].account.clone(), call_id.clone()))
                {
                    return Err(error(CallErrorCode::OfferExpired));
                }
                if s.call.as_ref().is_some_and(|c| {
                    c.identity.call_id() == call_id
                        && c.authority == s.clients[client].authority
                        && c.owner.as_deref().is_some_and(|owner| owner != client)
                }) {
                    return Err(error(CallErrorCode::AlreadyClaimed));
                }
                Some(s.ownership.act(
                    client,
                    &request_id,
                    &call_id,
                    u64::from(expected_revision),
                    Operation::Answer,
                    self.now(),
                )?)
            }
            CallCommand::Reject {
                call_id,
                expected_revision,
                ..
            } => {
                self.require_incoming_offer(&s, &call_id)?;
                Some(s.ownership.act(
                    client,
                    &request_id,
                    &call_id,
                    u64::from(expected_revision),
                    Operation::Reject,
                    self.now(),
                )?)
            }
            CallCommand::Cancel {
                call_id,
                expected_revision,
                ..
            } => {
                if self.cancel_pending_start(
                    &mut s,
                    client,
                    &call_id,
                    expected_revision,
                    Operation::Cancel,
                )? {
                    result_identity = s.call.as_ref().map(|call| call.identity.clone());
                    None
                } else {
                    Some(s.ownership.act(
                        client,
                        &request_id,
                        &call_id,
                        u64::from(expected_revision),
                        Operation::Cancel,
                        self.now(),
                    )?)
                }
            }
            CallCommand::Hangup {
                call_id,
                expected_revision,
                ..
            } => {
                if self.cancel_pending_start(
                    &mut s,
                    client,
                    &call_id,
                    expected_revision,
                    Operation::Hangup,
                )? {
                    result_identity = s.call.as_ref().map(|call| call.identity.clone());
                    None
                } else {
                    Some(s.ownership.act(
                        client,
                        &request_id,
                        &call_id,
                        u64::from(expected_revision),
                        Operation::Hangup,
                        self.now(),
                    )?)
                }
            }
            CallCommand::Mute { call_id, muted, .. } => {
                Self::owner(&s, client, &call_id)?;
                if s.call.as_ref().expect("owner call").muted != muted {
                    let (identity, generation) = self.interrupt_audio(&mut s, client, &call_id)?;
                    s.call.as_mut().expect("owner call").muted = muted;
                    prepared.clear = Some((identity.clone(), generation));
                    result_identity = Some(identity);
                }
                None
            }
            CallCommand::ClearAudio { call_id, .. } => {
                Self::owner(&s, client, &call_id)?;
                let (identity, generation) = self.interrupt_audio(&mut s, client, &call_id)?;
                prepared.clear = Some((identity.clone(), generation));
                result_identity = Some(identity);
                None
            }
        };
        let mut status = CallCommandStatus::Accepted;
        if let Some(d) = decision {
            let ticket = match d {
                Decision::Submit(t) => {
                    let c = s.call.as_mut().ok_or_else(|| error(CallErrorCode::Stale))?;
                    c.owner = Some(client.into());
                    if c.token.is_none() {
                        c.token = Some(format!(
                            "{}{}",
                            uuid::Uuid::new_v4().simple(),
                            uuid::Uuid::new_v4().simple()
                        ));
                    }
                    c.state = match t.operation() {
                        Operation::Dial => CallState::Dialing,
                        Operation::Answer => CallState::Connecting,
                        _ => CallState::Ending,
                    };
                    if matches!(
                        t.operation(),
                        Operation::Cancel | Operation::Reject | Operation::Hangup
                    ) {
                        c.audio_lease.store(0, Ordering::Release);
                    }
                    c.offer_until = None;
                    prepared.submit = Some(BackendRequest {
                        ticket: t.clone(),
                        peer_id: c.peer.clone(),
                        call_type: c.call_type,
                    });
                    t
                }
                Decision::Replay(t) => {
                    status = CallCommandStatus::Replayed;
                    t
                }
            };
            result_identity = Some(ticket.identity());
            if s.call
                .as_ref()
                .is_some_and(|c| c.identity == ticket.identity())
            {
                if let Some(view) = s.ownership.snapshot(client)?.current {
                    let revision =
                        u32::try_from(view.revision).map_err(|_| error(CallErrorCode::Capacity))?;
                    s.call.as_mut().expect("call").revision = revision;
                }
            }
        }
        if result_identity.is_none() {
            result_identity = s
                .call
                .as_ref()
                .filter(|c| c.owner.as_deref() == Some(client))
                .map(|c| c.identity.clone());
        }
        let call_id = result_identity
            .as_ref()
            .map(|identity| identity.call_id().to_owned());
        let owner_token = if claims_owner {
            s.call
                .as_ref()
                .filter(|c| {
                    Some(&c.identity) == result_identity.as_ref()
                        && c.owner.as_deref() == Some(client)
                })
                .and_then(|c| c.token.clone())
        } else {
            None
        };
        s.receipts.insert(
            key,
            CommandReceipt {
                fingerprint,
                identity: result_identity.clone(),
                claims_owner,
                expires: now.saturating_add(300_000),
            },
        );
        let event = CallEvent::CommandResult {
            request_id,
            status,
            call_id,
            owner_token,
            code: None,
        };
        if Self::emit(&mut s, client, event).is_err() {
            self.drop_client(&mut s, client);
        }
        self.broadcast(&mut s);
        Ok(prepared)
    }

    fn outcome(&self, ticket: &Ticket, outcome: BackendOutcome) -> Result<()> {
        let mut s = self.lock()?;
        let outcome =
            if !matches!(outcome, BackendOutcome::Ended) && !self.ticket_current(&s, ticket) {
                BackendOutcome::Uncertain
            } else {
                outcome
            };
        let phase = match outcome {
            BackendOutcome::Admitted => Some(if ticket.operation() == Operation::Dial {
                Phase::Dialing
            } else {
                Phase::Answering
            }),
            BackendOutcome::Connected => Some(Phase::Connected),
            BackendOutcome::Ended => None,
            BackendOutcome::Uncertain => Some(Phase::Uncertain),
        };
        if self
            .backend
            .authority(&ticket.authority().account_id)
            .as_ref()
            != Some(ticket.authority())
        {
            return Err(error(CallErrorCode::Stale));
        }
        s.ownership.result(ticket, phase)?;
        if let Some(c) = s.call.as_mut().filter(|c| c.identity == ticket.identity()) {
            if matches!(outcome, BackendOutcome::Ended | BackendOutcome::Uncertain) {
                c.audio_lease.store(0, Ordering::Release);
            }
            c.state = match outcome {
                BackendOutcome::Admitted => {
                    if ticket.operation() == Operation::Dial {
                        CallState::Ringing
                    } else {
                        CallState::Connecting
                    }
                }
                BackendOutcome::Connected => CallState::Connected,
                BackendOutcome::Ended => CallState::Ended,
                BackendOutcome::Uncertain => CallState::Uncertain,
            };
            c.revision = c
                .revision
                .checked_add(1)
                .ok_or_else(|| error(CallErrorCode::Capacity))?;
        }
        self.broadcast(&mut s);
        if matches!(outcome, BackendOutcome::Ended) {
            s.call = None;
            s.media.clear();
            self.broadcast(&mut s);
        }
        Ok(())
    }
    fn connected(&self, ticket: &Ticket) -> Result<()> {
        let mut s = self.lock()?;
        if !self.ticket_current(&s, ticket) {
            return Err(error(CallErrorCode::Stale));
        }
        if self
            .backend
            .authority(&ticket.authority().account_id)
            .as_ref()
            != Some(ticket.authority())
        {
            return Err(error(CallErrorCode::Stale));
        }
        s.ownership.connected(ticket)?;
        let c = s
            .call
            .as_mut()
            .filter(|c| c.identity == ticket.identity())
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        c.state = CallState::Connected;
        c.revision = c
            .revision
            .checked_add(1)
            .ok_or_else(|| error(CallErrorCode::Capacity))?;
        self.broadcast(&mut s);
        Ok(())
    }
    fn ending(&self, ticket: &Ticket) -> Result<()> {
        let mut s = self.lock()?;
        if self
            .backend
            .authority(&ticket.authority().account_id)
            .as_ref()
            != Some(ticket.authority())
        {
            return Err(error(CallErrorCode::Stale));
        }
        let c = s
            .call
            .as_mut()
            .filter(|c| {
                c.identity == ticket.identity()
                    && &c.authority == ticket.authority()
                    && !c.revoked
                    && !matches!(c.state, CallState::Uncertain | CallState::Ended)
            })
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        if c.state != CallState::Ending {
            c.state = CallState::Ending;
            c.revision = c.revision.saturating_add(1);
            c.audio_lease.store(0, Ordering::Release);
            // Keep transports until confirmed termination. Late PCM is consumed
            // without dispatch, just as for an owner-requested Hangup.
            self.broadcast(&mut s);
        }
        Ok(())
    }
    fn terminal(&self, authority: &AccountAuthority, identity: &CallIdentity) -> Result<()> {
        let mut s = self.lock()?;
        self.terminal_locked(&mut s, authority, identity)
    }
    fn terminal_locked(
        &self,
        s: &mut State,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<()> {
        s.ownership.terminal(authority, identity)?;
        if let Some(c) = s.call.as_ref().filter(|c| &c.identity == identity) {
            c.audio_lease.store(0, Ordering::Release);
        }
        if let Some(c) = s.call.as_mut().filter(|c| &c.identity == identity) {
            c.state = CallState::Ended;
            c.revision = c.revision.saturating_add(1);
        }
        self.broadcast(s);
        s.call = None;
        s.incoming.entries.remove(identity.call_id());
        s.media.clear();
        self.broadcast(s);
        Ok(())
    }
    pub async fn disconnect(self: &Arc<Self>, client: &str) {
        if let Ok(mut s) = self.lock() {
            self.drop_client(&mut s, client);
        }
        self.maintenance().await;
        self.start_maintenance();
    }
    /// Recheck before forwarding queued control data to a socket.
    pub async fn validate_control(&self, client: &str) -> Result<()> {
        self.maintenance().await;
        let s = self.lock()?;
        self.valid_peer(&s, client)
    }
    fn media_transport<'a>(&self, state: &'a State, id: &str) -> Result<&'a LiveCall> {
        let media = state
            .media
            .get(id)
            .ok_or_else(|| error(CallErrorCode::Closed))?;
        self.valid_peer(state, &media.client)?;
        let peer = &state.clients[&media.client];
        let call = state
            .call
            .as_ref()
            .filter(|call| {
                call.identity == media.identity
                    && call.authority == peer.authority
                    && matches!(call.state, CallState::Connected | CallState::Ending)
                    && !call.revoked
                    && call.owner.as_deref() == Some(&media.client)
            })
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        let lease = call.audio_lease.load(Ordering::Acquire);
        if (call.state == CallState::Connected && lease != call.send_generation)
            || (call.state == CallState::Ending && lease != 0)
        {
            return Err(error(CallErrorCode::Stale));
        }
        Ok(call)
    }
    /// Keep an authenticated attachment until terminal state, even after audio
    /// has stopped. Transport liveness does not authorize frame delivery.
    pub async fn validate_media(&self, id: &str) -> Result<()> {
        self.maintenance().await;
        let s = self.lock()?;
        self.media_transport(&s, id)?;
        Ok(())
    }
    /// Recheck before forwarding queued RX. Frames left in the API receiver
    /// after Ending are discarded while the control owner awaits confirmation.
    pub async fn media_delivery_allowed(&self, id: &str) -> Result<bool> {
        self.maintenance().await;
        let s = self.lock()?;
        Ok(self.media_transport(&s, id)?.state == CallState::Connected)
    }
    pub async fn attach_media(
        &self,
        account: &str,
        principal: &str,
        call_id: &str,
        client: &str,
        owner_token: &str,
    ) -> Result<MediaConnection> {
        self.maintenance().await;
        let mut s = self.lock()?;
        self.valid_peer(&s, client)?;
        Self::owner(&s, client, call_id)?;
        let peer = &s.clients[client];
        if peer.account != account || peer.principal != principal {
            return Err(error(CallErrorCode::Forbidden));
        }
        let c = s.call.as_ref().expect("owner call");
        if c.state != CallState::Connected {
            return Err(error(CallErrorCode::MediaNotReady));
        }
        if owner_token.len() != 64
            || !c
                .token
                .as_ref()
                .is_some_and(|t| openssl::memcmp::eq(t.as_bytes(), owner_token.as_bytes()))
        {
            return Err(error(CallErrorCode::NotOwner));
        }
        if !s.media.is_empty() {
            return Err(error(CallErrorCode::Busy));
        }
        let identity = c.identity.clone();
        let generation = c.send_generation;
        let receive_generation = c.receive_generation;
        let (tx, frames) = mpsc::channel(16);
        let id = uuid::Uuid::new_v4().to_string();
        s.media.insert(
            id.clone(),
            MediaPeer {
                client: client.into(),
                identity,
                stream: MediaStream::new(1, generation)
                    .map_err(|_| error(CallErrorCode::InvalidRequest))?,
                frames: tx,
            },
        );
        Ok(MediaConnection {
            id,
            frames,
            ready: CallEvent::MediaReady {
                send_track: CallAudioTrack {
                    id: 1,
                    generation,
                    direction: CallTrackDirection::Send,
                    format: Self::format(),
                },
                receive_track: CallAudioTrack {
                    id: 2,
                    generation: receive_generation,
                    direction: CallTrackDirection::Receive,
                    format: Self::format(),
                },
            },
        })
    }
    pub async fn media_frame(&self, id: &str, bytes: &[u8]) -> Result<()> {
        self.maintenance().await;
        let request = {
            let mut s = self.lock()?;
            let c = self.media_transport(&s, id)?;
            let identity = c.identity.clone();
            let lease = c.audio_lease.clone();
            let muted = c.muted;
            let ending = c.state == CallState::Ending;
            let current_generation = c.send_generation;
            let m = s.media.get_mut(id).expect("media");
            if m.identity != identity {
                return Err(error(CallErrorCode::Stale));
            }
            let frame = MediaFrame::decode(bytes, Self::media_format())
                .map_err(|_| error(CallErrorCode::InvalidRequest))?;
            if frame.header.track_id == 1 && frame.header.generation < current_generation {
                // A previous generation may already be in flight on this other
                // socket when clear_audio fences it. Drop it without revoking
                // the still-current owner or advancing the new stream guard.
                return Ok(());
            }
            m.stream
                .admit(&frame)
                .map_err(|_| error(CallErrorCode::Stale))?;
            if ending {
                // This valid PCM may have been written on the other socket
                // before the owner received Ending. It has no audio effect.
                return Ok(());
            }
            BackendAudioRequest {
                identity,
                format: Self::format(),
                generation: frame.header.generation,
                sequence: frame.header.sequence,
                timestamp_micros: frame.header.timestamp_micros,
                lease,
                samples: if muted {
                    vec![0; Self::media_format().samples_per_frame()]
                } else {
                    frame.samples().collect()
                },
            }
        };
        let generation = request.generation;
        let identity = request.identity.clone();
        let result = self.backend.send_audio(request).await;
        if result.is_err() {
            let s = self.lock()?;
            if let Ok(call) = self.media_transport(&s, id) {
                if call.identity == identity
                    && (call.state == CallState::Ending
                        || (matches!(result, Err(CallServiceError(CallErrorCode::Stale)))
                            && call.send_generation > generation))
                {
                    return Ok(());
                }
            }
        }
        result
    }
    pub async fn detach_media(&self, id: &str) {
        if let Ok(mut s) = self.lock() {
            if let Some(client) = s.media.get(id).map(|m| m.client.clone()) {
                self.drop_client(&mut s, &client);
            }
        }
        self.maintenance().await;
    }
    fn received_audio(&self, identity: &CallIdentity, samples: &[i16]) -> Result<()> {
        let mut s = self.lock()?;
        let allowed = s.call.as_ref().is_some_and(|c| {
            &c.identity == identity
                && c.state == CallState::Connected
                && !c.revoked
                && self.backend.authority(&c.authority.account_id).as_ref() == Some(&c.authority)
                && c.owner.as_ref().is_some_and(|owner| {
                    s.clients
                        .get(owner)
                        .is_some_and(|peer| !peer.events.is_closed())
                })
        });
        if !allowed {
            return Err(error(CallErrorCode::Stale));
        }
        let c = s
            .call
            .as_mut()
            .filter(|c| &c.identity == identity && c.state == CallState::Connected)
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        let header = MediaHeader {
            track_id: 2,
            generation: c.receive_generation,
            sequence: c.sequence,
            timestamp_micros: c
                .sample_clock
                .checked_mul(1_000_000)
                .ok_or_else(|| error(CallErrorCode::Capacity))?
                / 16000,
        };
        let bytes = frame::encode_pcm(header, Self::media_format(), samples)
            .map_err(|_| error(CallErrorCode::InvalidRequest))?;
        c.sequence = c
            .sequence
            .checked_add(1)
            .ok_or_else(|| error(CallErrorCode::Capacity))?;
        c.sample_clock = c
            .sample_clock
            .checked_add(samples.len() as u64)
            .ok_or_else(|| error(CallErrorCode::Capacity))?;
        let mut failed = Vec::new();
        for (id, m) in &s.media {
            if &m.identity == identity && m.frames.try_send(bytes.clone()).is_err() {
                failed.push(id.clone());
            }
        }
        for id in failed {
            if let Some(client) = s.media.get(&id).map(|m| m.client.clone()) {
                self.drop_client(&mut s, &client);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "service/tests.rs"]
mod tests;
