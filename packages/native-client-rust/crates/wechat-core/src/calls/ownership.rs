//! Daemon-wide call ownership. All operations run under one caller-held lock.
//!
//! This policy layer performs no network I/O. A reservation authorizes one
//! account actor operation only after that actor persists the intent and checks
//! its current credential/logout generation. Uncertain operations retain the slot.
use std::collections::BTreeMap;

const MAX_CLIENTS: usize = 64;
const MAX_REQUESTS: usize = 256;
const REQUEST_TTL_MS: u64 = 300_000;
const OWNER_GRACE_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountAuthority {
    pub account_id: String,
    /// A nonsecret accepted-credential/logout generation supplied by the actor.
    pub generation: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Ringing,
    Dialing,
    Answering,
    Connected,
    Ending,
    Uncertain,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Dial,
    Answer,
    Reject,
    Cancel,
    Hangup,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallView {
    pub call_id: String,
    pub revision: u64,
    pub phase: Phase,
    pub owned: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Global availability, including calls on another account.
    pub busy: bool,
    /// Only the incoming offer or the owner's call is visible in detail.
    pub current: Option<CallView>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Capacity,
    Busy,
    NotConnected,
    NotReady,
    Stale,
    NotOwner,
    Conflict,
    InvalidTransition,
}
/// Opaque identity for one local call attempt. A reused public call ID cannot
/// make a delayed actor event apply to a newer attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallIdentity {
    attempt: u64,
    call_id: String,
}
impl CallIdentity {
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ticket {
    attempt: u64,
    authority: AccountAuthority,
    call_id: String,
    revision: u64,
    operation: Operation,
}
impl Ticket {
    pub fn identity(&self) -> CallIdentity {
        CallIdentity {
            attempt: self.attempt,
            call_id: self.call_id.clone(),
        }
    }
    pub fn authority(&self) -> &AccountAuthority {
        &self.authority
    }
    pub fn call_id(&self) -> &str {
        &self.call_id
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// A unique operation. Persist its account intent before submitting it.
    Submit(Ticket),
    /// Same request on the same identity: observe its result, never submit again.
    Replay(Ticket),
}
#[derive(Clone)]
struct Client {
    logical_id: u64,
    authority: AccountAuthority,
    principal: String,
    ready: bool,
}
struct Session {
    attempt: u64,
    authority: AccountAuthority,
    id: String,
    revision: u64,
    phase: Phase,
    owner: Option<String>,
    owner_principal: Option<String>,
    owner_logical_id: Option<u64>,
    resume_until: Option<u64>,
    offer_until: Option<u64>,
    pending: Option<Ticket>,
    invalidated: bool,
}
#[derive(Clone, PartialEq, Eq)]
struct Intent {
    operation: Operation,
    call_id: String,
    expected_revision: Option<u64>,
    parameters: String,
}
struct Receipt {
    intent: Intent,
    ticket: Ticket,
    expires: u64,
}
/// No durable settled history is stored here. The account actor owns pending
/// intents and their uncertain network outcomes; official profiles own history.
#[derive(Default)]
pub struct Ownership {
    next_attempt: u64,
    next_client: u64,
    clients: BTreeMap<String, Client>,
    session: Option<Session>,
    requests: BTreeMap<(u64, String), Receipt>,
}
fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && !s.chars().any(char::is_control)
}
impl Ownership {
    /// IDs are server-generated connection identities, never user-chosen claims.
    /// Snapshot and event subscription must be installed under the same lock.
    pub fn connect(
        &mut self,
        client_id: &str,
        authority: AccountAuthority,
        principal: &str,
    ) -> Result<Snapshot, Error> {
        if !valid_id(client_id)
            || !valid_id(&authority.account_id)
            || !valid_id(&authority.generation)
            || !valid_id(principal)
            || self.clients.contains_key(client_id)
        {
            return Err(Error::Invalid);
        }
        if self.clients.len() >= MAX_CLIENTS {
            return Err(Error::Capacity);
        }
        let logical_id = self.next_client.checked_add(1).ok_or(Error::Capacity)?;
        self.next_client = logical_id;
        self.clients.insert(
            client_id.into(),
            Client {
                logical_id,
                authority,
                principal: principal.into(),
                ready: false,
            },
        );
        self.snapshot(client_id)
    }
    pub fn ready(&mut self, client_id: &str, ready: bool) -> Result<(), Error> {
        self.clients
            .get_mut(client_id)
            .ok_or(Error::NotConnected)?
            .ready = ready;
        Ok(())
    }
    pub fn snapshot(&self, client_id: &str) -> Result<Snapshot, Error> {
        let client = self.clients.get(client_id).ok_or(Error::NotConnected)?;
        let current = self.session.as_ref().and_then(|s| {
            let owned = s.owner.as_deref() == Some(client_id)
                && s.owner_logical_id == Some(client.logical_id)
                && s.owner_principal.as_deref() == Some(client.principal.as_str());
            (s.authority == client.authority
                && (owned || (s.phase == Phase::Ringing && s.owner.is_none())))
            .then(|| CallView {
                call_id: s.id.clone(),
                revision: s.revision,
                phase: s.phase,
                owned,
            })
        });
        Ok(Snapshot {
            busy: self.session.is_some(),
            current,
        })
    }
    /// Only the account actor may introduce an independently current offer.
    /// No client readiness is needed to retain it for late attachment.
    pub fn offer(
        &mut self,
        authority: AccountAuthority,
        call_id: &str,
        expires: u64,
        now: u64,
    ) -> Result<CallIdentity, Error> {
        if !valid_id(call_id)
            || !valid_id(&authority.account_id)
            || !valid_id(&authority.generation)
            || expires <= now
        {
            return Err(Error::Invalid);
        }
        if self.session.is_some() {
            return Err(Error::Busy);
        }
        let attempt = self.next_attempt.checked_add(1).ok_or(Error::Capacity)?;
        self.next_attempt = attempt;
        self.session = Some(Session {
            attempt,
            authority,
            id: call_id.into(),
            revision: 1,
            phase: Phase::Ringing,
            owner: None,
            owner_principal: None,
            owner_logical_id: None,
            resume_until: None,
            offer_until: Some(expires),
            pending: None,
            invalidated: false,
        });
        Ok(CallIdentity {
            attempt,
            call_id: call_id.into(),
        })
    }
    pub(super) fn offer_revision(
        &mut self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
        revision: u64,
    ) -> Result<(), Error> {
        let session = self.session.as_mut().ok_or(Error::Stale)?;
        if &session.authority != authority
            || session.attempt != identity.attempt
            || session.id != identity.call_id
            || session.invalidated
            || session.owner.is_some()
            || session.phase != Phase::Ringing
            || revision <= session.revision
        {
            return Err(Error::Stale);
        }
        session.revision = revision;
        Ok(())
    }
    pub(super) fn pending_start(
        &self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Option<Operation> {
        let session = self.session.as_ref()?;
        if &session.authority != authority
            || session.attempt != identity.attempt
            || session.id != identity.call_id
            || session.invalidated
        {
            return None;
        }
        session
            .pending
            .as_ref()
            .map(|ticket| ticket.operation)
            .filter(|operation| matches!(operation, Operation::Dial | Operation::Answer))
    }
    /// Reserve the global slot for the initiating client before Invite.
    pub fn dial(
        &mut self,
        client_id: &str,
        request_id: &str,
        new_call_id: &str,
        parameters: &str,
        now: u64,
    ) -> Result<Decision, Error> {
        self.command(
            client_id,
            request_id,
            Intent {
                operation: Operation::Dial,
                call_id: new_call_id.into(),
                expected_revision: None,
                parameters: parameters.into(),
            },
            now,
        )
    }
    /// Parameters is a canonical nonsecret digest over all resolved arguments.
    pub fn act(
        &mut self,
        client_id: &str,
        request_id: &str,
        call_id: &str,
        revision: u64,
        operation: Operation,
        now: u64,
    ) -> Result<Decision, Error> {
        if operation == Operation::Dial {
            return Err(Error::Invalid);
        }
        self.command(
            client_id,
            request_id,
            Intent {
                operation,
                call_id: call_id.into(),
                expected_revision: Some(revision),
                parameters: String::new(),
            },
            now,
        )
    }
    fn command(
        &mut self,
        client_id: &str,
        request_id: &str,
        intent: Intent,
        now: u64,
    ) -> Result<Decision, Error> {
        if !valid_id(request_id) || !valid_id(&intent.call_id) || intent.parameters.len() > 200 {
            return Err(Error::Invalid);
        }
        let client = self
            .clients
            .get(client_id)
            .ok_or(Error::NotConnected)?
            .clone();
        self.requests.retain(|_, r| {
            now < r.expires
                || self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.attempt == r.ticket.attempt)
        });
        let key = (client.logical_id, request_id.to_owned());
        if let Some(prior) = self.requests.get(&key) {
            return if prior.intent == intent && prior.ticket.authority == client.authority {
                Ok(Decision::Replay(prior.ticket.clone()))
            } else {
                Err(Error::Conflict)
            };
        }
        if self.requests.len() >= MAX_REQUESTS {
            return Err(Error::Capacity);
        }
        if intent.operation == Operation::Dial {
            if !client.ready {
                return Err(Error::NotReady);
            }
            if self.session.is_some() {
                return Err(Error::Busy);
            }
            let attempt = self.next_attempt.checked_add(1).ok_or(Error::Capacity)?;
            self.next_attempt = attempt;
            self.session = Some(Session {
                attempt,
                authority: client.authority.clone(),
                id: intent.call_id.clone(),
                revision: 1,
                phase: Phase::Dialing,
                owner: Some(client_id.into()),
                owner_principal: Some(client.principal.clone()),
                owner_logical_id: Some(client.logical_id),
                resume_until: None,
                offer_until: None,
                pending: None,
                invalidated: false,
            });
        } else {
            let s = self.session.as_mut().ok_or(Error::Stale)?;
            if s.invalidated
                || s.authority != client.authority
                || s.id != intent.call_id
                || intent.expected_revision != Some(s.revision)
                || s.offer_until.is_some_and(|until| now >= until)
            {
                return Err(Error::Stale);
            }
            if let Some(owner) = &s.owner {
                if owner != client_id
                    || s.owner_logical_id != Some(client.logical_id)
                    || s.owner_principal.as_deref() != Some(client.principal.as_str())
                {
                    return Err(Error::NotOwner);
                }
            }
            // Dispatch only after the previous operation has an admitted or
            // terminal result. In particular, an early Cancel must not replace
            // an unresolved Invite and lose its possible accepted outcome.
            if s.pending.is_some() {
                return Err(Error::InvalidTransition);
            }
            let next_phase = match intent.operation {
                Operation::Answer if s.phase == Phase::Ringing && s.owner.is_none() => {
                    if !client.ready {
                        return Err(Error::NotReady);
                    }
                    Phase::Answering
                }
                Operation::Reject if s.phase == Phase::Ringing && s.owner.is_none() => {
                    Phase::Ending
                }
                Operation::Cancel if s.phase == Phase::Dialing => Phase::Ending,
                Operation::Hangup if matches!(s.phase, Phase::Answering | Phase::Connected) => {
                    Phase::Ending
                }
                _ => return Err(Error::InvalidTransition),
            };
            let revision = s.revision.checked_add(1).ok_or(Error::Capacity)?;
            s.owner = Some(client_id.into());
            s.owner_principal = Some(client.principal.clone());
            s.owner_logical_id = Some(client.logical_id);
            s.phase = next_phase;
            s.revision = revision;
            s.offer_until = None;
        }
        let s = self.session.as_ref().expect("reserved session");
        let ticket = Ticket {
            attempt: s.attempt,
            authority: s.authority.clone(),
            call_id: s.id.clone(),
            revision: s.revision,
            operation: intent.operation,
        };
        self.session.as_mut().expect("reserved session").pending = Some(ticket.clone());
        self.requests.insert(
            key,
            Receipt {
                intent,
                ticket: ticket.clone(),
                expires: now.saturating_add(REQUEST_TTL_MS),
            },
        );
        Ok(Decision::Submit(ticket))
    }
    /// An account actor reports a correlated result. Late results cannot clear
    /// or connect another call. Unknown outcomes keep the global slot occupied.
    pub fn result(&mut self, ticket: &Ticket, phase: Option<Phase>) -> Result<(), Error> {
        let s = self.session.as_mut().ok_or(Error::Stale)?;
        if s.attempt != ticket.attempt
            || s.authority != ticket.authority
            || s.id != ticket.call_id
            || s.pending.as_ref() != Some(ticket)
            || s.invalidated
        {
            return Err(Error::Stale);
        }
        let next = match (ticket.operation, phase) {
            (_, None) => None,
            (_, Some(Phase::Uncertain)) => Some(Phase::Uncertain),
            (Operation::Dial, Some(Phase::Dialing)) => Some(Phase::Dialing),
            (Operation::Answer, Some(Phase::Answering)) => Some(Phase::Answering),
            (Operation::Dial | Operation::Answer, Some(Phase::Connected)) => Some(Phase::Connected),
            _ => return Err(Error::InvalidTransition),
        };
        if let Some(phase) = next {
            s.revision = s.revision.checked_add(1).ok_or(Error::Capacity)?;
            s.phase = phase;
            s.pending = None;
        } else {
            self.session = None;
        }
        Ok(())
    }
    /// Invite/Join admission and peer connection are different facts. The actor
    /// may report admission with `result(ticket, Some(Dialing/Answering))`, then
    /// call this only on verified peer/media readiness. The original ticket's
    /// attempt identity fences reused IDs and a newer operation such as Cancel.
    pub fn connected(&mut self, ticket: &Ticket) -> Result<(), Error> {
        let s = self.session.as_mut().ok_or(Error::Stale)?;
        if s.attempt != ticket.attempt
            || s.authority != ticket.authority
            || s.id != ticket.call_id
            || s.invalidated
            || s.pending.is_some()
            || !matches!(ticket.operation, Operation::Dial | Operation::Answer)
            || !matches!(s.phase, Phase::Dialing | Phase::Answering)
        {
            return Err(Error::Stale);
        }
        let revision = s.revision.checked_add(1).ok_or(Error::Capacity)?;
        s.revision = revision;
        s.phase = Phase::Connected;
        Ok(())
    }
    /// Stop local work without claiming that the remote call has ended.
    pub fn uncertain(
        &mut self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<(), Error> {
        let s = self.session.as_mut().ok_or(Error::Stale)?;
        if &s.authority != authority || s.attempt != identity.attempt || s.id != identity.call_id {
            return Err(Error::Stale);
        }
        let revision = s.revision.checked_add(1).ok_or(Error::Capacity)?;
        s.revision = revision;
        s.phase = Phase::Uncertain;
        s.pending = None;
        s.resume_until = None;
        Ok(())
    }
    /// Match the one pending business action without changing its ownership.
    pub fn pending_ticket_matches(&self, ticket: &Ticket) -> bool {
        self.session.as_ref().is_some_and(|session| {
            !session.invalidated
                && session.attempt == ticket.attempt
                && session.authority == ticket.authority
                && session.id == ticket.call_id
                && session.pending.as_ref() == Some(ticket)
        })
    }
    /// Verified terminal signaling from the accepted generation, including a
    /// pickup by another official device. The source adapter establishes meaning.
    pub fn terminal(
        &mut self,
        authority: &AccountAuthority,
        identity: &CallIdentity,
    ) -> Result<(), Error> {
        if !self.session.as_ref().is_some_and(|s| {
            &s.authority == authority && s.id == identity.call_id && s.attempt == identity.attempt
        }) {
            return Err(Error::Stale);
        }
        self.session = None;
        Ok(())
    }
    pub fn disconnect(&mut self, client_id: &str, now: u64) {
        // Several transport error paths can close one connection. Only the
        // first close starts the grace period; old callbacks cannot extend it.
        let Some(client) = self.clients.remove(client_id) else {
            return;
        };
        if let Some(s) = self.session.as_mut().filter(|s| {
            !s.invalidated
                && s.owner.as_deref() == Some(client_id)
                && s.owner_logical_id == Some(client.logical_id)
                && s.owner_principal.as_deref() == Some(client.principal.as_str())
        }) {
            s.resume_until = Some(now.saturating_add(OWNER_GRACE_MS));
        }
    }
    /// Resume needs both the authenticated principal and a secret lease checked
    /// by the transport. The caller supplies the old server-issued connection ID
    /// only after checking that lease; knowledge of a public call ID is insufficient.
    pub fn resume(&mut self, old_client: &str, new_client: &str, now: u64) -> Result<(), Error> {
        let client = self
            .clients
            .get(new_client)
            .ok_or(Error::NotConnected)?
            .clone();
        if !client.ready {
            return Err(Error::NotReady);
        }
        let s = self.session.as_mut().ok_or(Error::Stale)?;
        if s.invalidated
            || s.owner.as_deref() != Some(old_client)
            || s.authority != client.authority
            || s.owner_principal.as_deref() != Some(client.principal.as_str())
            || s.resume_until.is_none_or(|until| now >= until)
        {
            return Err(Error::NotOwner);
        }
        let revision = s.revision.checked_add(1).ok_or(Error::Capacity)?;
        let logical_id = s.owner_logical_id.ok_or(Error::Stale)?;
        s.owner = Some(new_client.into());
        s.resume_until = None;
        s.revision = revision;
        self.clients
            .get_mut(new_client)
            .expect("checked client")
            .logical_id = logical_id;
        Ok(())
    }
    /// Stop local media immediately on disconnect. At grace expiry, the actor
    /// receives the call identity once to terminate/reconcile, never transfer it.
    pub fn expire(&mut self, now: u64) -> Option<(AccountAuthority, CallIdentity)> {
        let s = self.session.as_mut()?;
        if s.phase == Phase::Ringing && s.offer_until.is_some_and(|until| now >= until) {
            self.session = None;
            return None;
        }
        if s.resume_until.is_some_and(|until| now >= until) {
            s.resume_until = None;
            // Any late accepted Invite/Join result must not revive a call whose
            // owner has gone. The actor reconciles/terminates it by identity.
            s.pending = None;
            s.phase = Phase::Uncertain;
            s.revision = s.revision.saturating_add(1);
            return Some((
                s.authority.clone(),
                CallIdentity {
                    attempt: s.attempt,
                    call_id: s.id.clone(),
                },
            ));
        }
        None
    }
    /// Credential replacement/logout fences all sockets and stops affected work.
    /// The account actor still reconciles any pending remote operation separately.
    pub fn invalidate(&mut self, authority: &AccountAuthority) -> Option<String> {
        self.clients.retain(|_, c| &c.authority != authority);
        if self
            .session
            .as_ref()
            .is_some_and(|s| &s.authority == authority)
        {
            let s = self.session.as_mut().expect("matched session");
            if s.invalidated {
                return None;
            }
            s.invalidated = true;
            s.phase = Phase::Uncertain;
            s.revision = s.revision.saturating_add(1);
            s.resume_until = None;
            return Some(s.id.clone());
        }
        None
    }
}

#[cfg(test)]
#[path = "ownership/tests.rs"]
mod tests;
