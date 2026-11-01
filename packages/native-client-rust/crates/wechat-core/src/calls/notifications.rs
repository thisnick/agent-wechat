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
}
impl PendingEntry {
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
}
impl Registry {
    pub fn prune_dedup(&mut self) {
        let now = Instant::now();
        self.rooms.retain(|_, expiry| *expiry > now);
        self.receipts.retain(|_, expiry| *expiry > now);
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
                    || invite.caller_member_id.is_none()
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
