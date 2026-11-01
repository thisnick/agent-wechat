use super::super::notifications::{
    IncomingGuard, IncomingNotification, IncomingPreparation, IncomingRequest, PendingStage,
    Registry,
};
use super::*;
use std::time::Duration;
use wechat_protocol::calls::incoming::NotificationPayload;

pub(super) enum IncomingJob {
    Prepare(IncomingRequest),
    Cancel {
        identity: CallIdentity,
        note: Arc<IncomingNotification>,
    },
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
        let Some(room) = Registry::room(&note) else {
            return;
        };
        let Ok(mut s) = self.lock() else {
            return;
        };
        let found = s
            .incoming
            .entries
            .iter()
            .find(|(_, entry)| {
                Registry::room(&entry.note).as_deref() == Some(room.as_str())
                    && entry.note.header_from == note.header_from
                    && entry.note.own_username == note.own_username
            })
            .map(|(id, entry)| (id.clone(), entry.identity.clone()));
        let Some((id, identity)) = found else {
            return;
        };
        let Some(identity) = identity else {
            s.incoming.entries.remove(&id);
            self.broadcast(&mut s);
            return;
        };
        if let Some(entry) = s.incoming.entries.get_mut(&id) {
            entry.cancelled = true;
        }
        let _ = s.ownership.uncertain(note.lease.authority(), &identity);
        if let Some(call) = s.call.as_mut().filter(|call| call.identity == identity) {
            call.state = CallState::Uncertain;
            call.revision = call.revision.saturating_add(1);
            call.audio_lease.store(0, Ordering::Release);
            // The notification's role/reason belongs to the native consumer;
            // generic maintenance must not invent an additional refusal here.
            call.stop_requested = true;
        }
        s.media.clear();
        self.broadcast(&mut s);
        drop(s);
        let _ = self.incoming_jobs.try_send(IncomingJob::Cancel {
            identity,
            note: Arc::new(note),
        });
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
        entry.stage = PendingStage::Preparing;
        entry.metadata.phase = CallPendingPhase::Preparing;
        entry.metadata.revision = 2;
        entry.identity = Some(identity.clone());
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
        };
        if self
            .incoming_jobs
            .try_send(IncomingJob::Prepare(request))
            .is_err()
        {
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
            while let Some(job) = receiver.recv().await {
                let Some(service) = weak.upgrade() else {
                    break;
                };
                match job {
                    IncomingJob::Prepare(request) => {
                        let identity = request.identity.clone();
                        if let Some(core) = request.notification.core.upgrade() {
                            let Ok(guard) = core.lock_account(&request.authority.account_id).await
                            else {
                                service.finish_preparation(&identity, IncomingPreparation::Ignored);
                                continue;
                            };
                            drop(guard);
                        }
                        let outcome = if !request.guard.is_current() {
                            IncomingPreparation::Ignored
                        } else {
                            let remaining =
                                request.expires.saturating_duration_since(Instant::now());
                            match tokio::time::timeout(
                                remaining,
                                service.backend.prepare_incoming(request),
                            )
                            .await
                            {
                                Ok(Ok(outcome)) => outcome,
                                _ => IncomingPreparation::Uncertain,
                            }
                        };
                        service.finish_preparation(&identity, outcome);
                    }
                    IncomingJob::Cancel { identity, note } => {
                        let authority = note.lease.authority().clone();
                        if let Some(core) = note.core.upgrade() {
                            let Ok(guard) = core.lock_account(&authority.account_id).await else {
                                continue;
                            };
                            drop(guard);
                        }
                        if !note.lease.current() {
                            continue;
                        }
                        if matches!(
                            tokio::time::timeout(
                                Duration::from_secs(5),
                                service.backend.cancel_incoming(identity.clone(), note)
                            )
                            .await,
                            Ok(Ok(BackendOutcome::Ended))
                        ) {
                            let _ = service.terminal(&authority, &identity);
                        }
                    }
                }
            }
        });
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
            drop(s);
            let _ = self.verified_incoming_terminal(&authority, identity);
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
            }
        }
        self.broadcast(&mut s);
    }
    /// Trusted native actors only: room, member, origin and terminal meaning
    /// must already be verified. A timeout is not terminal evidence.
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
