use super::super::notifications::{
    IncomingCancellation, IncomingGuard, IncomingNotification, IncomingPreparation,
    IncomingRegistration, IncomingRequest, PendingStage,
};
use super::*;
use std::time::Duration;
use wechat_protocol::calls::incoming::NotificationPayload;

pub(super) struct IncomingCancel {
    identity: CallIdentity,
    note: Arc<IncomingNotification>,
    registration: IncomingRegistration,
    expires: Instant,
}
pub(super) struct IncomingReceivers {
    pub preparations: mpsc::Receiver<IncomingRequest>,
    pub cancellations: mpsc::Receiver<IncomingCancel>,
}
type IncomingFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
async fn poll_incoming(future: &mut Option<IncomingFuture>) {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}
impl CallService {
    pub(super) fn pending_incoming(&self, s: &State, account: &str) -> Option<CallPendingOffer> {
        s.incoming
            .entries
            .values()
            .filter_map(|entry| entry.view(account))
            .min_by_key(|offer| s.incoming.entries[&offer.call_id].note.received_at_unix_ms)
    }
    pub(super) fn validate_ready_scope(
        &self,
        s: &State,
        client: &str,
        scope: Option<&CallReadyScope>,
    ) -> Result<()> {
        let Some(CallReadyScope::Offer {
            call_id,
            expected_revision,
        }) = scope
        else {
            return Ok(());
        };
        let authority = &s.clients[client].authority;
        let candidate = s.incoming.entries.get(call_id).is_some_and(|entry| {
            entry.note.lease.authority() == authority
                && !entry.cancelled
                && Instant::now() < entry.expires
                && entry.metadata.revision == *expected_revision
        });
        let offered = s.call.as_ref().is_some_and(|call| {
            call.authority == *authority
                && call.identity.call_id() == call_id
                && call.state == CallState::Offered
                && call.revision == *expected_revision
        });
        if candidate || offered {
            Ok(())
        } else {
            Err(error(CallErrorCode::OfferExpired))
        }
    }
    fn ready_for(s: &State, id: &str, note: &IncomingNotification) -> bool {
        s.clients.values().any(|peer| {
            peer.control
                && !peer.events.is_closed()
                && peer.authority == *note.lease.authority()
                && match &peer.incoming_ready {
                    Some(CallReadyScope::All) => true,
                    Some(CallReadyScope::Offer { call_id, .. }) => call_id == id,
                    _ => false,
                }
        })
    }
    pub(super) fn require_incoming_offer(&self, s: &State, id: &str) -> Result<()> {
        if let Some(entry) = s.incoming.entries.get(id) {
            if entry.cancelled || Instant::now() >= entry.expires {
                return Err(error(CallErrorCode::OfferExpired));
            }
            if entry.stage != PendingStage::Offered {
                return Err(error(CallErrorCode::MediaNotReady));
            }
        }
        Ok(())
    }
    pub(super) fn prune_incoming(&self, s: &mut State) -> bool {
        s.incoming.prune_dedup();
        let now = Instant::now();
        let expired: Vec<_> = s
            .incoming
            .entries
            .iter()
            .filter(|(id, entry)| {
                let owned = s.call.as_ref().is_some_and(|call| {
                    call.identity.call_id() == id.as_str() && call.owner.is_some()
                });
                (!owned && now >= entry.expires) || !entry.note.lease.current()
            })
            .map(|(id, entry)| {
                (
                    id.clone(),
                    entry.identity.clone(),
                    entry.note.lease.authority().clone(),
                )
            })
            .collect();
        let mut changed = false;
        for (id, identity, authority) in expired {
            if let Some(identity) = identity {
                if let Some(entry) = s.incoming.entries.get_mut(&id) {
                    entry.cancelled = true;
                }
                if let Some(call) = s
                    .call
                    .as_mut()
                    .filter(|call| call.identity == identity && call.state != CallState::Uncertain)
                {
                    let _ = s.ownership.uncertain(&authority, &identity);
                    call.state = CallState::Uncertain;
                    call.revision = call.revision.saturating_add(1);
                    call.audio_lease.store(0, Ordering::Release);
                    call.stop_requested = true;
                    changed = true;
                }
            } else {
                s.incoming.entries.remove(&id);
                changed = true;
            }
        }
        changed
    }
    pub(crate) fn ingest_incoming(
        self: &Arc<Self>,
        note: IncomingNotification,
        remaining: Duration,
    ) {
        if !note.lease.current() || remaining.is_zero() {
            return;
        }
        let is_cancel = matches!(&note.control.payload, NotificationPayload::Cancel(_));
        if is_cancel {
            self.ingest_cancel(note);
            return;
        }
        if !self
            .backend
            .incoming_supported(note.lease.authority(), &note.caller_username)
        {
            return;
        }
        if let Ok(mut s) = self.lock() {
            self.prune_incoming(&mut s);
            if s.incoming.insert(note, remaining).is_none() {
                return;
            }
            self.broadcast(&mut s);
        }
        self.wake_incoming();
    }
    fn ingest_cancel(self: &Arc<Self>, note: IncomingNotification) {
        let NotificationPayload::Cancel(cancel) = &note.control.payload else {
            return;
        };
        let Some(member) = cancel.member_id else {
            return;
        };
        if !note.lease.current()
            || self
                .backend
                .authority(&note.lease.authority().account_id)
                .as_ref()
                != Some(note.lease.authority())
        {
            return;
        }
        let Ok(mut s) = self.lock() else {
            return;
        };
        s.incoming.prune_dedup();
        let found = s
            .incoming
            .entries
            .iter()
            .find(|(_, entry)| {
                if entry.matches_cancel(&note) {
                    return true;
                }
                // ACK/Join can already have returned while its admitted member
                // is still being interpreted. This delivers owned bytes only;
                // the serialized native actor must verify its current member.
                entry.matches_cancel_source(&note)
                    && entry.identity.as_ref().is_some_and(|identity| {
                        s.call.as_ref().is_some_and(|call| {
                            call.identity == *identity
                                && call.direction == CallDirection::Incoming
                                && matches!(
                                    call.state,
                                    CallState::Ringing | CallState::Offered | CallState::Connecting
                                )
                        })
                    })
            })
            .map(|(id, entry)| {
                (
                    id.clone(),
                    entry.identity.clone(),
                    entry.registration.clone(),
                    entry.expires,
                    entry.member_revision,
                )
            });
        let Some((id, identity, registration, expires, member_revision)) = found else {
            return;
        };
        let Some(identity) = identity else {
            s.incoming.entries.remove(&id);
            self.broadcast(&mut s);
            return;
        };
        let Some(registration) = registration else {
            return;
        };
        let key = (id, member_revision, member, cancel.reason);
        if s.incoming.cancellations.contains_key(&key) {
            return;
        }
        if s.incoming.cancellations.len() >= 256 {
            drop(s);
            let _ = self.incoming_uncertain(note.lease.authority(), &identity);
            return;
        }
        s.incoming
            .cancellations
            .insert(key, Instant::now() + Duration::from_secs(300));
        drop(s);
        let authority = note.lease.authority().clone();
        if self
            .incoming_cancellations
            .try_send(IncomingCancel {
                identity: identity.clone(),
                note: Arc::new(note),
                registration,
                expires,
            })
            .is_err()
        {
            let _ = self.incoming_uncertain(&authority, &identity);
            return;
        }
        // Only the source actor can determine whether this is a pending callee,
        // an already-joined member, or an unresolved phase. No early revocation.
        self.start_incoming_worker();
    }
    pub(super) fn wake_incoming(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let Ok(mut s) = self.lock() else {
            return;
        };
        self.prune_incoming(&mut s);
        if s.call.is_some() || !s.recovered_unknown.is_empty() {
            return;
        }
        let candidate = s
            .incoming
            .entries
            .iter()
            .filter(|(id, entry)| {
                entry.stage == PendingStage::Waiting
                    && !entry.cancelled
                    && entry.note.lease.current()
                    && Instant::now() < entry.expires
                    && Self::ready_for(&s, id, &entry.note)
            })
            .min_by_key(|(_, entry)| entry.note.received_at_unix_ms)
            .map(|(id, _)| id.clone());
        let Some(id) = candidate else {
            return;
        };
        let entry = &s.incoming.entries[&id];
        let note = entry.note.clone();
        let expires = entry.expires;
        if self
            .backend
            .authority(&note.lease.authority().account_id)
            .as_ref()
            != Some(note.lease.authority())
        {
            return;
        }
        // Keep the policy reservation until explicit terminal evidence. A local
        // notification deadline must not free an ACK that may already be submitted.
        let Ok(identity) =
            s.ownership
                .offer(note.lease.authority().clone(), &id, u64::MAX, self.now())
        else {
            return;
        };
        let entry = s.incoming.entries.get_mut(&id).expect("selected candidate");
        let registration = IncomingRegistration::new();
        entry.stage = PendingStage::Preparing;
        entry.metadata.phase = CallPendingPhase::Preparing;
        entry.metadata.revision = 2;
        entry.identity = Some(identity.clone());
        entry.registration = Some(registration.clone());
        s.call = Some(LiveCall {
            identity: identity.clone(),
            authority: note.lease.authority().clone(),
            peer: note.caller_username.clone(),
            direction: CallDirection::Incoming,
            owner: None,
            token: None,
            state: CallState::Ringing,
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
        self.broadcast(&mut s);
        drop(s);
        let weak = Arc::downgrade(self);
        let guard_id = id.clone();
        let guard_identity = identity.clone();
        let guard = IncomingGuard {
            lease: note.lease.clone(),
            current: Arc::new(move || {
                let Some(service) = weak.upgrade() else {
                    return false;
                };
                let Ok(s) = service.lock() else {
                    return false;
                };
                let Some(entry) = s.incoming.entries.get(&guard_id) else {
                    return false;
                };
                if entry.cancelled || entry.identity.as_ref() != Some(&guard_identity) {
                    return false;
                }
                let Some(call) = s.call.as_ref().filter(|call| {
                    call.identity == guard_identity
                        && call.authority == *entry.note.lease.authority()
                        && !call.revoked
                        && !call.cancellation_requested
                        && matches!(
                            call.state,
                            CallState::Ringing
                                | CallState::Offered
                                | CallState::Connecting
                                | CallState::Connected
                        )
                }) else {
                    return false;
                };
                if let Some(owner) = &call.owner {
                    return s.clients.get(owner).is_some_and(|peer| {
                        peer.control && !peer.events.is_closed() && peer.authority == call.authority
                    });
                }
                Instant::now() < entry.expires && Self::ready_for(&s, &guard_id, &entry.note)
            }),
        };
        let request = IncomingRequest {
            identity: identity.clone(),
            authority: note.lease.authority().clone(),
            expires,
            notification: note,
            guard,
            registration,
        };
        if self.incoming_preparations.try_send(request).is_err() {
            self.finish_preparation(&identity, IncomingPreparation::Ignored);
            return;
        }
        self.start_incoming_worker();
        self.start_maintenance();
    }
    fn start_incoming_worker(self: &Arc<Self>) {
        let Ok(mut receiver) = self.incoming_receiver.lock() else {
            return;
        };
        let Some(mut receiver) = receiver.take() else {
            return;
        };
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut preparing: Option<IncomingFuture> = None;
            let mut cancelling: Option<IncomingFuture> = None;
            loop {
                tokio::select! {
                    // Registration is an explicit signal, never inferred from
                    // constructing or polling a preparation future.
                    biased;
                    _ = poll_incoming(&mut preparing), if preparing.is_some() => { preparing = None; }
                    _ = poll_incoming(&mut cancelling), if cancelling.is_some() => { cancelling = None; }
                    request = receiver.preparations.recv(), if preparing.is_none() => {
                        let Some(request) = request else { break; };
                        let Some(service) = weak.upgrade() else { break; };
                        preparing = Some(Box::pin(service.prepare_incoming_one(request)));
                    }
                    request = receiver.cancellations.recv(), if cancelling.is_none() => {
                        let Some(request) = request else { break; };
                        let Some(service) = weak.upgrade() else { break; };
                        cancelling = Some(Box::pin(service.cancel_incoming_one(request)));
                    }
                }
            }
        });
    }
    async fn prepare_incoming_one(self: Arc<Self>, request: IncomingRequest) {
        let identity = request.identity.clone();
        let registration = request.registration.clone();
        if let Some(core) = request.notification.core.upgrade() {
            let Ok(guard) = core.lock_account(&request.authority.account_id).await else {
                self.finish_preparation(&identity, IncomingPreparation::Ignored);
                registration.close_unregistered();
                return;
            };
            drop(guard);
        }
        let mut outcome = if !request.guard.is_current() {
            IncomingPreparation::Ignored
        } else {
            let remaining = request.expires.saturating_duration_since(Instant::now());
            match tokio::time::timeout(remaining, self.backend.prepare_incoming(request)).await {
                Ok(Ok(outcome)) => outcome,
                _ => IncomingPreparation::Uncertain,
            }
        };
        if outcome == IncomingPreparation::Acked && !registration.is_registered() {
            outcome = IncomingPreparation::Uncertain;
        }
        self.finish_preparation(&identity, outcome);
        registration.close_unregistered();
    }
    async fn cancel_incoming_one(self: Arc<Self>, request: IncomingCancel) {
        let authority = request.note.lease.authority().clone();
        if !request.registration.is_registered() {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(request.expires),
                request.registration.wait_registered(),
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => return,
                Err(_) => {
                    let _ = self.incoming_uncertain(&authority, &request.identity);
                    return;
                }
            }
        }
        if !request.note.lease.current() {
            return;
        }
        {
            let Ok(s) = self.lock() else {
                return;
            };
            if !s
                .incoming
                .entries
                .get(request.identity.call_id())
                .is_some_and(|entry| {
                    entry.identity.as_ref() == Some(&request.identity)
                        && entry.note.lease.authority() == &authority
                })
            {
                return;
            }
        }
        // This local actor classification must not wait on the account lock
        // held by an Auth RPC. The staged notification already owns its bytes
        // and accepted-generation binding; the actor owns mutation admission.
        let outcome = match tokio::time::timeout(
            Duration::from_secs(5),
            self.backend
                .cancel_incoming(request.identity.clone(), request.note.clone()),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            _ => IncomingCancellation::Uncertain,
        };
        let Ok(mut s) = self.lock() else {
            return;
        };
        if self.backend.authority(&authority.account_id).as_ref() != Some(&authority)
            || !s
                .incoming
                .entries
                .get(request.identity.call_id())
                .is_some_and(|entry| {
                    entry.identity.as_ref() == Some(&request.identity)
                        && entry.matches_cancel_source(&request.note)
                })
        {
            return;
        }
        match outcome {
            IncomingCancellation::Ignored => {}
            IncomingCancellation::Ended => {
                if s.incoming
                    .entries
                    .get(request.identity.call_id())
                    .is_some_and(|entry| entry.matches_cancel(&request.note))
                {
                    let _ = self.terminal_locked(&mut s, &authority, &request.identity);
                }
            }
            IncomingCancellation::Uncertain => {
                let _ = self.incoming_uncertain_locked(&mut s, &authority, &request.identity);
            }
        }
    }
    fn finish_preparation(&self, identity: &CallIdentity, outcome: IncomingPreparation) {
        let Ok(mut s) = self.lock() else {
            return;
        };
        let id = identity.call_id();
        let Some(entry) = s.incoming.entries.get(id) else {
            return;
        };
        if entry.identity.as_ref() != Some(identity) {
            return;
        }
        let authority = entry.note.lease.authority().clone();
        let current = !entry.cancelled
            && entry.note.lease.current()
            && Instant::now() < entry.expires
            && Self::ready_for(&s, id, &entry.note)
            && self.backend.authority(&authority.account_id).as_ref() == Some(&authority);
        if outcome == IncomingPreparation::Ended {
            let _ = self.terminal_locked(&mut s, &authority, identity);
            return;
        }
        if entry.cancelled || entry.stage != PendingStage::Preparing {
            return;
        }
        if outcome == IncomingPreparation::Ignored {
            let _ = s.ownership.terminal(&authority, identity);
            s.incoming.entries.remove(id);
            s.call = None;
            s.media.clear();
        } else if outcome == IncomingPreparation::Acked && current {
            if s.ownership.offer_revision(&authority, identity, 3).is_err() {
                return;
            }
            let entry = s.incoming.entries.get_mut(id).expect("current candidate");
            entry.stage = PendingStage::Offered;
            entry.metadata.revision = 3;
            if let Some(call) = s.call.as_mut().filter(|call| call.identity == *identity) {
                call.state = CallState::Offered;
                call.revision = 3;
            }
        } else {
            let _ = s.ownership.uncertain(&authority, identity);
            if let Some(entry) = s.incoming.entries.get_mut(id) {
                entry.cancelled = true;
            }
            if let Some(call) = s.call.as_mut().filter(|call| call.identity == *identity) {
                call.state = CallState::Uncertain;
                call.revision = call.revision.saturating_add(1);
                call.audio_lease.store(0, Ordering::Release);
                call.stop_requested = true;
            }
        }
        self.broadcast(&mut s);
    }
    /// Record only the source-verified current own member after authenticated
    /// ACK/Join admission. Original invitation and receipt bytes remain intact.
    #[cfg(feature = "native-calls")]
    pub(crate) fn set_incoming_member(
        &self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
        room_id: u64,
        room_key: u64,
        member: u32,
    ) -> Result<()> {
        let mut s = self.lock()?;
        let entry = s
            .incoming
            .entries
            .get(identity.call_id())
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        let NotificationPayload::Invite(invite) = &entry.note.control.payload else {
            return Err(error(CallErrorCode::Stale));
        };
        if entry.identity.as_ref() != Some(identity)
            || entry.note.lease.authority() != authority
            || !entry.note.lease.current()
            || entry.cancelled
            || invite.room_id != Some(room_id)
            || invite.room_key != Some(room_key)
            || self.backend.authority(&authority.account_id).as_ref() != Some(authority)
        {
            return Err(error(CallErrorCode::Stale));
        }
        let call = s
            .call
            .as_ref()
            .filter(|call| {
                call.direction == CallDirection::Incoming
                    && &call.identity == identity
                    && &call.authority == authority
                    && !call.revoked
                    && !call.cancellation_requested
                    && matches!(
                        call.state,
                        CallState::Ringing
                            | CallState::Offered
                            | CallState::Connecting
                            | CallState::Connected
                    )
            })
            .ok_or_else(|| error(CallErrorCode::Stale))?;
        let admitted = if let Some(owner) = &call.owner {
            s.clients.get(owner).is_some_and(|peer| {
                peer.control && !peer.events.is_closed() && peer.authority == *authority
            })
        } else {
            Instant::now() < entry.expires && Self::ready_for(&s, identity.call_id(), &entry.note)
        };
        if !admitted {
            return Err(error(CallErrorCode::Stale));
        }
        let entry = s
            .incoming
            .entries
            .get_mut(identity.call_id())
            .expect("verified incoming binding");
        if entry.admitted_member != Some(member) {
            entry.member_revision = entry
                .member_revision
                .checked_add(1)
                .ok_or_else(|| error(CallErrorCode::Capacity))?;
            entry.admitted_member = Some(member);
        }
        Ok(())
    }
    /// Trusted incoming actor only. An unresolved source phase fences audio and
    /// future admission, without inventing another remote control operation.
    pub(crate) fn incoming_uncertain(
        &self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<()> {
        let mut s = self.lock()?;
        self.incoming_uncertain_locked(&mut s, authority, identity)
    }
    fn incoming_uncertain_locked(
        &self,
        s: &mut State,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<()> {
        if !s.call.as_ref().is_some_and(|call| {
            call.direction == CallDirection::Incoming
                && &call.identity == identity
                && &call.authority == authority
        }) {
            return Err(error(CallErrorCode::Stale));
        }
        let changed = s
            .call
            .as_ref()
            .is_some_and(|call| call.state != CallState::Uncertain);
        if changed {
            s.ownership.uncertain(authority, identity)?;
        }
        if let Some(entry) = s.incoming.entries.get_mut(identity.call_id()) {
            entry.cancelled = true;
        }
        let call = s.call.as_mut().expect("verified incoming attempt");
        call.state = CallState::Uncertain;
        if changed {
            call.revision = call.revision.saturating_add(1);
        }
        call.audio_lease.store(0, Ordering::Release);
        call.stop_requested = true;
        s.media.clear();
        if changed {
            self.broadcast(s);
        }
        Ok(())
    }
    /// Trusted native actors only: room, member, origin and terminal meaning
    /// must already be verified. A timeout is not terminal evidence.
    #[cfg(any(feature = "native-calls", test))]
    pub(crate) fn verified_incoming_terminal(
        &self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<()> {
        let s = self.lock()?;
        if !s.call.as_ref().is_some_and(|call| {
            call.direction == CallDirection::Incoming
                && &call.identity == identity
                && &call.authority == authority
        }) {
            return Err(error(CallErrorCode::Stale));
        }
        drop(s);
        self.terminal(authority, identity)
    }
}
