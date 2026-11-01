//! Owned, fresh incoming controls. These never discharge profile projection.
use super::{
    authority::CallAuthorityLease,
    incoming::DecodedCallControl,
    ownership::{AccountAuthority, CallIdentity},
};
use crate::{
    call_types::{CallKind, CallMedia, CallPendingOffer, CallPendingPhase},
    Core,
};
use std::{collections::BTreeMap, sync::Weak, time::Duration};
use std::{sync::Arc, time::Instant};
use tokio::sync::watch;
use wechat_protocol::{
    calls::incoming::NotificationPayload,
    wire::{decode_add_message, SyncBatch},
};

/// One authenticated live-sync control, copied only after encrypted staging.
/// Room/application data stays inside the account actor and is never an API event.
pub struct IncomingNotification {
    pub lease: CallAuthorityLease,
    pub receipt_id: String,
    pub source_created_at: u64,
    pub received_at_unix_ms: u64,
    pub own_username: String,
    pub caller_username: String,
    pub original_message: Arc<[u8]>,
    pub control: DecodedCallControl,
    pub(crate) core: Weak<Core>,
    pub(crate) header_from: String,
}

/// Recheck before every preparation mutation. Expiry is a local admission bound,
/// never proof that a remote operation or room has ended.
#[derive(Clone)]
pub struct IncomingGuard {
    pub(crate) lease: CallAuthorityLease,
    pub(crate) current: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl IncomingGuard {
    pub fn is_current(&self) -> bool {
        self.lease.current() && (self.current)()
    }
}

/// The broker has reserved the daemon-wide slot, but no actionable offer exists
/// yet. Native actors own generic Auth7994, ringing ACK8069 and their receipts.
pub struct IncomingRequest {
    pub identity: CallIdentity,
    pub authority: AccountAuthority,
    pub expires: Instant,
    pub notification: Arc<IncomingNotification>,
    pub guard: IncomingGuard,
    pub registration: IncomingRegistration,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegistrationState {
    Pending,
    Registered,
    Closed,
}

/// One request's actor-mailbox installation signal. Registration alone grants
/// no mutation permission: the actor must still check its authority/guard.
#[derive(Clone)]
pub struct IncomingRegistration {
    state: watch::Sender<RegistrationState>,
}
impl IncomingRegistration {
    pub(crate) fn new() -> Self {
        let (state, _) = watch::channel(RegistrationState::Pending);
        Self { state }
    }
    /// Call only after the exact actor/mailbox is installed, before any await,
    /// profile proof or Auth. A retired request cannot be registered again.
    pub fn mark_registered(&self) -> bool {
        let mut registered = false;
        self.state.send_if_modified(|state| match state {
            RegistrationState::Pending => {
                *state = RegistrationState::Registered;
                registered = true;
                true
            }
            RegistrationState::Registered => {
                registered = true;
                false
            }
            RegistrationState::Closed => false,
        });
        registered
    }
    pub(crate) fn is_registered(&self) -> bool {
        *self.state.borrow() == RegistrationState::Registered
    }
    pub(crate) fn close(&self) {
        self.state.send_replace(RegistrationState::Closed);
    }
    pub(crate) fn close_unregistered(&self) {
        self.state.send_if_modified(|state| {
            if *state == RegistrationState::Pending {
                *state = RegistrationState::Closed;
                true
            } else {
                false
            }
        });
    }
    pub(crate) async fn wait_registered(&self) -> bool {
        let mut state = self.state.subscribe();
        loop {
            match *state.borrow_and_update() {
                RegistrationState::Registered => return true,
                RegistrationState::Closed => return false,
                RegistrationState::Pending => {}
            }
            if state.changed().await.is_err() {
                return false;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingPreparation {
    /// No ringing ACK was submitted and no uncertain remote mutation remains.
    Ignored,
    /// The exact ringing ACK was verified and the actor can handle Answer/Reject.
    Acked,
    /// A source-verified terminal result for this exact room/attempt, including
    /// a terminal member update contained in the successful ringing ACK.
    Ended,
    /// Submission or cleanup is unresolved; the slot must remain occupied.
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingCancellation {
    /// This notification does not apply to the actor's current source phase.
    Ignored,
    /// The actor verified local terminal state for this exact incoming attempt.
    Ended,
    /// Classification or submission remains unresolved; retain the reservation.
    Uncertain,
}

// These are local admission/retention limits, not a claim about a remote room's
// lifetime. An expired potentially submitted preparation remains uncertain.
pub(super) const ADMISSION_SECONDS: u64 = 30;
pub(super) const MAX_PENDING: usize = 8;
const DEDUP_SECONDS: u64 = 300;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingStage {
    Waiting,
    Preparing,
    Offered,
}
pub(super) struct PendingEntry {
    pub metadata: CallPendingOffer,
    pub note: Arc<IncomingNotification>,
    pub expires: Instant,
    pub stage: PendingStage,
    pub cancelled: bool,
    pub identity: Option<CallIdentity>,
    pub admitted_member: Option<u32>,
    pub member_revision: u64,
    pub registration: Option<IncomingRegistration>,
}
impl Drop for PendingEntry {
    fn drop(&mut self) {
        if let Some(registration) = &self.registration {
            registration.close();
        }
    }
}
impl PendingEntry {
    pub fn matches_cancel(&self, note: &IncomingNotification) -> bool {
        if !self.matches_cancel_source(note) {
            return false;
        }
        let (NotificationPayload::Invite(invite), NotificationPayload::Cancel(cancel)) =
            (&self.note.control.payload, &note.control.payload)
        else {
            return false;
        };
        self.admitted_member.or(invite.member_id) == cancel.member_id
    }
    pub fn matches_cancel_source(&self, note: &IncomingNotification) -> bool {
        let (NotificationPayload::Invite(invite), NotificationPayload::Cancel(cancel)) =
            (&self.note.control.payload, &note.control.payload)
        else {
            return false;
        };
        if cancel.member_id.is_none() {
            return false;
        }
        self.note.lease.current()
            && note.lease.current()
            && self.note.lease.authority() == note.lease.authority()
            && invite.room_id.is_some()
            && invite.room_id == cancel.room_id
            && invite.room_key.is_some()
            && invite.room_key == cancel.room_key
            && self.note.header_from == note.header_from
            && self.note.own_username == note.own_username
            && invite.callee_username.as_deref() == Some(note.own_username.as_str())
    }
    pub fn view(&self, account: &str) -> Option<CallPendingOffer> {
        (self.note.lease.authority().account_id == account
            && !self.cancelled
            && self.stage != PendingStage::Offered
            && Instant::now() < self.expires)
            .then(|| self.metadata.clone())
    }
}
#[derive(Default)]
pub(super) struct Registry {
    pub entries: BTreeMap<String, PendingEntry>,
    pub rooms: BTreeMap<String, Instant>,
    pub receipts: BTreeMap<String, Instant>,
    pub cancellations: BTreeMap<(String, u64, u32, Option<u32>), Instant>,
}
impl Registry {
    pub fn prune_dedup(&mut self) {
        let now = Instant::now();
        self.rooms.retain(|_, expiry| *expiry > now);
        self.receipts.retain(|_, expiry| *expiry > now);
        self.cancellations.retain(|_, expiry| *expiry > now);
    }
    pub fn room(note: &IncomingNotification) -> Option<String> {
        let (room, key) = match &note.control.payload {
            NotificationPayload::Invite(invite) => (invite.room_id?, invite.room_key?),
            NotificationPayload::Cancel(cancel) => (cancel.room_id?, cancel.room_key?),
        };
        Some(format!(
            "{}:{}:{room}:{key}",
            note.lease.authority().account_id,
            note.lease.authority().generation
        ))
    }
    pub fn insert(&mut self, note: IncomingNotification, remaining: Duration) -> Option<String> {
        self.prune_dedup();
        let room = Self::room(&note)?;
        let receipt = format!(
            "{}:{}:{}",
            note.lease.authority().account_id,
            note.lease.authority().generation,
            note.receipt_id
        );
        if self.rooms.contains_key(&room)
            || self
                .entries
                .values()
                .any(|entry| Self::room(&entry.note).as_deref() == Some(room.as_str()))
            || self.receipts.contains_key(&receipt)
            || self.entries.len() >= MAX_PENDING
            || self.receipts.len() >= 256
            || self.rooms.len() >= 256
        {
            return None;
        }
        let now = Instant::now();
        self.rooms
            .insert(room, now + Duration::from_secs(DEDUP_SECONDS));
        self.receipts
            .insert(receipt, now + Duration::from_secs(DEDUP_SECONDS));
        let id = uuid::Uuid::new_v4().to_string();
        self.entries.insert(
            id.clone(),
            PendingEntry {
                metadata: CallPendingOffer {
                    call_id: id.clone(),
                    revision: 1,
                    kind: CallKind::Direct,
                    caller_id: note.caller_username.clone(),
                    offered_media: CallMedia::default(),
                    phase: CallPendingPhase::WaitingForReady,
                },
                note: Arc::new(note),
                expires: now + remaining,
                stage: PendingStage::Waiting,
                cancelled: false,
                identity: None,
                admitted_member: None,
                member_revision: 0,
                registration: None,
            },
        );
        Some(id)
    }
}

/// Inspect only this newly staged live batch. Recovery/catchup never calls this
/// hook, and decoding does not mark a command projected or move its cursor.
pub(crate) fn after_sync_staged(
    core: &Arc<Core>,
    account: &str,
    batch_id: &str,
    batch: &SyncBatch,
    origin: &str,
) {
    if origin != "live" {
        return;
    }
    let Ok(lease) = core.call_authority(account) else {
        return;
    };
    let Ok(state) = core.account_state(account) else {
        return;
    };
    let received = chrono::Utc::now().timestamp_millis();
    let Ok(received) = u64::try_from(received) else {
        return;
    };
    let own = state.own_username.clone();
    // No private credential state crosses into the queue.
    drop(state);
    for (index, command) in batch
        .commands
        .iter()
        .enumerate()
        .filter(|(_, command)| command.id == 5)
    {
        let Ok(message) = decode_add_message(&command.payload, &own) else {
            continue;
        };
        if message.outgoing || message.to != own || message.message_type != 10002 {
            continue;
        }
        let source = u64::from(message.created_at);
        let now = received / 1000;
        if source > now.saturating_add(5) || now.saturating_sub(source) > ADMISSION_SECONDS {
            continue;
        }
        let Ok(Some(control)) = super::incoming::decode_system_control(&message) else {
            continue;
        };
        let caller = match &control.payload {
            NotificationPayload::Invite(invite) => {
                if invite.room_type != Some(2)
                    || invite.app_id.as_deref() != Some("wechat")
                    || invite.sub_app_id.as_deref() != Some("voip-2p")
                    || invite.callee_username.as_deref() != Some(own.as_str())
                    || invite
                        .wechat_group_id
                        .as_deref()
                        .is_some_and(|group| !group.is_empty())
                    || invite.room_id.is_none_or(|id| id == 0)
                    || invite.room_key.is_none_or(|key| key == 0)
                    || invite.member_id.is_none()
                    || !invite.caller_openid.as_deref().is_some_and(valid_identity)
                    || !invite.callee_openid.as_deref().is_some_and(valid_identity)
                {
                    continue;
                }
                let Some(caller) = invite
                    .caller_username
                    .as_deref()
                    .filter(|caller| valid_identity(caller) && *caller != own)
                else {
                    continue;
                };
                caller.to_owned()
            }
            NotificationPayload::Cancel(_) => String::new(),
        };
        let remaining =
            Duration::from_secs(ADMISSION_SECONDS.saturating_sub(now.saturating_sub(source)));
        if remaining.is_zero() {
            continue;
        }
        let note = IncomingNotification {
            lease: lease.clone(),
            receipt_id: format!("{batch_id}:{index}"),
            source_created_at: source,
            received_at_unix_ms: received,
            own_username: own.clone(),
            caller_username: caller,
            original_message: Arc::from(command.payload.clone()),
            control,
            core: Arc::downgrade(core),
            header_from: message.from,
        };
        core.call_service().ingest_incoming(note, remaining);
    }
}
fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.chars().any(char::is_control)
        && !value.ends_with("@chatroom")
}

#[cfg(test)]
#[path = "notifications/tests.rs"]
mod tests;
