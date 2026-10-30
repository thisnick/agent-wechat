//! Offline call coordination. These states and signals are local concepts, not
//! server enum values. No protocol parser, transport or device adapter is wired.
//!
//! The caller must verify account/profile identity and hold the account mutation
//! guard around decisions. Raw notifications must already be recoverably staged.
//! Consuming one here never completes an official-file projection obligation.

use std::collections::{BTreeMap, BTreeSet};

const MAX_RECEIPTS: usize = 512;
const MAX_OPERATIONS: usize = 128;
const MAX_SEQUENCE_DOMAINS: usize = 8;

/// Current authenticated account authority, obtained after existing Core checks.
/// `authorization` is a credential-generation digest, never a raw session key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Authority {
    pub account: String,
    pub owner: String,
    pub authorization: [u8; 32],
    pub logout_epoch: Option<Vec<u8>>,
}

/// Adapter-supplied identity derived only from independently established fields.
/// No particular remote field layout or room-ID width is assumed here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteIdentity(pub Vec<u8>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Audio,
    Video,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureConsent {
    pub microphone: bool,
    pub camera: bool,
}

impl CaptureConsent {
    fn for_mode(self, mode: Mode) -> Self {
        Self {
            microphone: self.microphone,
            camera: self.camera && mode == Mode::Video,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Offering,
    Ringing,
    Joining,
    Negotiating,
    Connected,
    Ending,
    Uncertain,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallToken {
    authority: Authority,
    attempt: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// New receipt AND a separately verified, still-current invitation.
    VerifiedCurrent,
    Unverified,
    Replay,
}

pub struct Incoming {
    pub authority: Authority,
    pub ingress: u64,
    pub receipt: [u8; 32],
    pub identity: RemoteIdentity,
    pub freshness: Freshness,
    pub mode: Mode,
    /// Set only if the invitation shares a proved notification ordering domain.
    pub sequence: Option<Sequence>,
}

/// A sequence in an adapter-proven domain. Not AddMessage.server_sequence, SSE
/// sequence, wall-clock time or a media packet sequence. Wrap is not inferred.
#[derive(Clone, Copy)]
pub struct Sequence {
    pub domain: [u8; 32],
    pub value: u64,
}

#[derive(Clone, Copy)]
pub enum Signal {
    Established,
    HandledOnOtherDevice,
    Finished,
    Uninterpreted,
}

pub struct Notice {
    pub authority: Authority,
    pub ingress: u64,
    pub token: CallToken,
    pub identity: RemoteIdentity,
    pub receipt: [u8; 32],
    pub sequence: Sequence,
    pub signal: Signal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndIntent {
    Decline,
    Cancel,
    Hangup,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Offer { mode: Mode, consent: CaptureConsent },
    Accept { consent: CaptureConsent },
    End(EndIntent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub action: Action,
    /// Account-scoped digest of all resolved action parameters, including peers.
    pub parameters: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationToken {
    call: CallToken,
    key: String,
}

/// A unique permission to dispatch a persisted intent. Deliberately not Clone.
/// This is not a network request or proof of server acceptance.
pub struct Submission {
    operation: OperationToken,
}

impl Submission {
    pub fn operation(&self) -> &OperationToken {
        &self.operation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OperationState {
    Prepared,
    Submitting,
    Confirmed,
    Rejected,
    NeedsReview,
    ReconciledTerminal,
}

struct Operation {
    token: OperationToken,
    intent: Intent,
    state: OperationState,
}

pub enum SubmissionOutcome {
    Confirmed(Option<RemoteIdentity>),
    Rejected,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub ring: Option<bool>,
    pub stop_media: bool,
    pub release_devices: bool,
    pub capture: Option<CaptureConsent>,
}

impl Effects {
    fn stop() -> Self {
        Self {
            ring: Some(false),
            stop_media: true,
            release_devices: true,
            capture: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    Stale,
    Duplicate,
    Unverified,
    Busy,
    NeedsReconciliation,
    InvalidTransition,
    IdempotencyConflict,
    Capacity,
    MissingIdentity,
}

struct Session {
    token: CallToken,
    remote: Option<RemoteIdentity>,
    mode: Mode,
    phase: Phase,
    consent: CaptureConsent,
    capture: CaptureConsent,
    sequences: BTreeMap<[u8; 32], u64>,
}

/// One account's active coordination and pending action receipts, never a
/// canonical history. A real adapter must durably retain unresolved operations.
/// Bounded receipt storage fails closed instead of evicting replay protection.
pub struct Controller {
    authority: Authority,
    ingress: u64,
    enabled: bool,
    next_attempt: u64,
    session: Option<Session>,
    operations: BTreeMap<String, Operation>,
    receipts: BTreeSet<[u8; 32]>,
    needs_reconciliation: bool,
}

impl Controller {
    pub fn new(authority: Authority, ingress: u64, unresolved_after_restart: bool) -> Self {
        Self {
            authority,
            ingress,
            enabled: true,
            next_attempt: 0,
            session: None,
            operations: BTreeMap::new(),
            receipts: BTreeSet::new(),
            needs_reconciliation: unresolved_after_restart,
        }
    }

    pub fn phase(&self) -> Option<Phase> {
        self.session.as_ref().map(|session| session.phase)
    }

    pub fn needs_reconciliation(&self) -> bool {
        self.needs_reconciliation
    }

    /// Socket replacement fences delivery only; it does not prove media ended.
    pub fn replace_ingress(&mut self, ingress: u64) {
        self.ingress = ingress;
    }

    fn check_token(&self, token: &CallToken) -> Result<(), ControlError> {
        if !self.enabled
            || token.authority != self.authority
            || self.session.as_ref().map(|s| &s.token) != Some(token)
        {
            return Err(ControlError::Stale);
        }
        Ok(())
    }

    fn can_start(&self) -> Result<(), ControlError> {
        if !self.enabled {
            return Err(ControlError::Stale);
        }
        if self.needs_reconciliation {
            return Err(ControlError::NeedsReconciliation);
        }
        if self.phase().is_some_and(|phase| phase != Phase::Finished) {
            return Err(ControlError::Busy);
        }
        Ok(())
    }

    fn begin(
        &mut self,
        remote: Option<RemoteIdentity>,
        mode: Mode,
        phase: Phase,
    ) -> Result<CallToken, ControlError> {
        self.next_attempt = self
            .next_attempt
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        let token = CallToken {
            authority: self.authority.clone(),
            attempt: self.next_attempt,
        };
        self.session = Some(Session {
            token: token.clone(),
            remote,
            mode,
            phase,
            consent: CaptureConsent::default(),
            capture: CaptureConsent::default(),
            sequences: BTreeMap::new(),
        });
        Ok(token)
    }

    fn admit_receipt(&mut self, receipt: [u8; 32]) -> Result<(), ControlError> {
        if self.receipts.contains(&receipt) {
            return Err(ControlError::Duplicate);
        }
        if self.receipts.len() == MAX_RECEIPTS {
            return Err(ControlError::Capacity);
        }
        self.receipts.insert(receipt);
        Ok(())
    }

    pub fn incoming(&mut self, incoming: Incoming) -> Result<(CallToken, Effects), ControlError> {
        if !self.enabled || incoming.authority != self.authority || incoming.ingress != self.ingress
        {
            return Err(ControlError::Stale);
        }
        if incoming.freshness != Freshness::VerifiedCurrent {
            return Err(ControlError::Unverified);
        }
        if incoming.identity.0.is_empty() {
            return Err(ControlError::MissingIdentity);
        }
        self.admit_receipt(incoming.receipt)?;
        self.can_start()?;
        // A different receipt for the same finished remote call cannot revive it.
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.remote.as_ref() == Some(&incoming.identity))
        {
            return Err(ControlError::Duplicate);
        }
        let token = self.begin(Some(incoming.identity), incoming.mode, Phase::Ringing)?;
        if let Some(sequence) = incoming.sequence {
            self.session
                .as_mut()
                .expect("new session")
                .sequences
                .insert(sequence.domain, sequence.value);
        }
        Ok((
            token,
            Effects {
                ring: Some(true),
                ..Effects::default()
            },
        ))
    }

    /// Prepare a caller intent. Repeated matching keys return the old token;
    /// they never create another call or reopen local capture. A completed old
    /// attempt returns its original token, not a token for the current call;
    /// `persisted` rejects that token. Invalidated account authority is rejected
    /// even for a previously admitted key.
    pub fn prepare(
        &mut self,
        key: &str,
        call: Option<&CallToken>,
        intent: Intent,
    ) -> Result<(OperationToken, Effects), ControlError> {
        if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
            return Err(ControlError::InvalidTransition);
        }
        if !self.enabled {
            return Err(ControlError::Stale);
        }
        if let Some(prior) = self.operations.get(key) {
            if prior.token.call.authority != self.authority {
                return Err(ControlError::Stale);
            }
            if prior.intent != intent || call.is_some_and(|call| &prior.token.call != call) {
                return Err(ControlError::IdempotencyConflict);
            }
            return Ok((prior.token.clone(), Effects::default()));
        }
        if self.operations.len() == MAX_OPERATIONS {
            return Err(ControlError::Capacity);
        }
        let effects;
        let token = match &intent.action {
            Action::Offer { mode, consent } => {
                if call.is_some() {
                    return Err(ControlError::InvalidTransition);
                }
                self.can_start()?;
                let token = self.begin(None, *mode, Phase::Offering)?;
                self.session.as_mut().expect("new session").consent = consent.for_mode(*mode);
                effects = Effects::default();
                token
            }
            Action::Accept { consent } => {
                let token = call.ok_or(ControlError::InvalidTransition)?;
                self.check_token(token)?;
                let session = self.session.as_mut().expect("checked session");
                if session.phase != Phase::Ringing {
                    return Err(ControlError::InvalidTransition);
                }
                session.phase = Phase::Joining;
                session.consent = consent.for_mode(session.mode);
                effects = Effects {
                    ring: Some(false),
                    ..Effects::default()
                };
                token.clone()
            }
            Action::End(kind) => {
                let token = call.ok_or(ControlError::InvalidTransition)?;
                self.check_token(token)?;
                let session = self.session.as_mut().expect("checked session");
                let valid = match kind {
                    EndIntent::Decline => session.phase == Phase::Ringing,
                    EndIntent::Cancel => session.phase == Phase::Offering,
                    EndIntent::Hangup => matches!(
                        session.phase,
                        Phase::Joining | Phase::Negotiating | Phase::Connected
                    ),
                };
                if !valid {
                    return Err(ControlError::InvalidTransition);
                }
                session.phase = Phase::Ending;
                session.capture = CaptureConsent::default();
                effects = Effects::stop();
                token.clone()
            }
        };
        let token = OperationToken {
            call: token,
            key: key.into(),
        };
        self.operations.insert(
            key.into(),
            Operation {
                token: token.clone(),
                intent,
                state: OperationState::Prepared,
            },
        );
        Ok((token, effects))
    }

    /// Caller has persisted the exact intent under its account guard. Obtain at
    /// most one submission permit, then release the writer before network work.
    pub fn persisted(&mut self, token: &OperationToken) -> Result<Submission, ControlError> {
        self.check_token(&token.call)?;
        let operation = self
            .operations
            .get_mut(&token.key)
            .ok_or(ControlError::Stale)?;
        if operation.token != *token {
            return Err(ControlError::Stale);
        }
        if operation.state != OperationState::Prepared {
            return Err(ControlError::Duplicate);
        }
        let required_phase = match operation.intent.action {
            Action::Offer { .. } => Phase::Offering,
            Action::Accept { .. } => Phase::Joining,
            Action::End(_) => Phase::Ending,
        };
        if self.session.as_ref().expect("checked session").phase != required_phase {
            return Err(ControlError::Stale);
        }
        operation.state = OperationState::Submitting;
        Ok(Submission {
            operation: token.clone(),
        })
    }

    pub fn submission_result(
        &mut self,
        submission: Submission,
        outcome: SubmissionOutcome,
    ) -> Result<Effects, ControlError> {
        let token = submission.operation;
        self.check_token(&token.call)?;
        let operation = self
            .operations
            .get_mut(&token.key)
            .ok_or(ControlError::Stale)?;
        if operation.token != token || operation.state != OperationState::Submitting {
            return Err(ControlError::Stale);
        }
        let session = self.session.as_mut().expect("checked session");
        if matches!(session.phase, Phase::Finished | Phase::Uncertain) {
            return Err(ControlError::Stale);
        }
        match outcome {
            SubmissionOutcome::Confirmed(remote) => {
                if matches!(operation.intent.action, Action::Offer { .. }) {
                    let Some(remote) = remote.filter(|identity| !identity.0.is_empty()) else {
                        operation.state = OperationState::NeedsReview;
                        session.phase = Phase::Uncertain;
                        session.capture = CaptureConsent::default();
                        self.needs_reconciliation = true;
                        return Err(ControlError::MissingIdentity);
                    };
                    if session
                        .remote
                        .as_ref()
                        .is_some_and(|prior| prior != &remote)
                    {
                        return Err(ControlError::Stale);
                    }
                    session.remote = Some(remote);
                }
                operation.state = OperationState::Confirmed;
                if matches!(operation.intent.action, Action::End(_)) {
                    return Ok(self.finish(true));
                }
                // RPC success alone establishes neither peer pickup nor media.
                Ok(Effects::default())
            }
            SubmissionOutcome::Rejected | SubmissionOutcome::Unknown => {
                operation.state = match outcome {
                    SubmissionOutcome::Rejected => OperationState::Rejected,
                    _ => OperationState::NeedsReview,
                };
                session.phase = Phase::Uncertain;
                session.capture = CaptureConsent::default();
                // A rejected end does not establish that the remote room ended.
                self.needs_reconciliation = true;
                Ok(Effects::stop())
            }
        }
    }

    pub fn notice(&mut self, notice: Notice) -> Result<Effects, ControlError> {
        self.check_token(&notice.token)?;
        if notice.authority != self.authority || notice.ingress != self.ingress {
            return Err(ControlError::Stale);
        }
        if matches!(notice.signal, Signal::Uninterpreted) {
            return Err(ControlError::Unverified);
        }
        let session = self.session.as_ref().expect("checked session");
        if session.phase == Phase::Finished || session.remote.as_ref() != Some(&notice.identity) {
            return Err(ControlError::Stale);
        }
        if session
            .sequences
            .get(&notice.sequence.domain)
            .is_some_and(|value| notice.sequence.value <= *value)
        {
            return Err(ControlError::Stale);
        }
        if !session.sequences.contains_key(&notice.sequence.domain)
            && session.sequences.len() == MAX_SEQUENCE_DOMAINS
        {
            return Err(ControlError::Capacity);
        }
        if matches!(notice.signal, Signal::Established)
            && !matches!(session.phase, Phase::Offering | Phase::Joining)
        {
            return Err(ControlError::InvalidTransition);
        }
        self.admit_receipt(notice.receipt)?;
        let session = self.session.as_mut().expect("checked session");
        session
            .sequences
            .insert(notice.sequence.domain, notice.sequence.value);
        match notice.signal {
            Signal::Established => {
                session.phase = Phase::Negotiating;
                Ok(Effects {
                    ring: Some(false),
                    ..Effects::default()
                })
            }
            Signal::HandledOnOtherDevice | Signal::Finished => Ok(self.finish(true)),
            Signal::Uninterpreted => unreachable!("uninterpreted signal rejected above"),
        }
    }

    /// Adapter has separately verified negotiated codecs, media authentication
    /// and transport readiness. Signaling acknowledgment is insufficient.
    pub fn media_ready(&mut self, token: &CallToken) -> Result<Effects, ControlError> {
        self.check_token(token)?;
        let session = self.session.as_mut().expect("checked session");
        if session.phase != Phase::Negotiating {
            return Err(ControlError::InvalidTransition);
        }
        session.phase = Phase::Connected;
        session.capture = session.consent.for_mode(session.mode);
        Ok(Effects {
            capture: Some(session.capture),
            ..Effects::default()
        })
    }

    fn finish(&mut self, remote_terminal: bool) -> Effects {
        if let Some(session) = &mut self.session {
            session.phase = Phase::Finished;
            session.capture = CaptureConsent::default();
            if remote_terminal {
                for operation in self
                    .operations
                    .values_mut()
                    .filter(|operation| operation.token.call == session.token)
                {
                    if matches!(
                        operation.state,
                        OperationState::Prepared
                            | OperationState::Submitting
                            | OperationState::NeedsReview
                            | OperationState::Rejected
                    ) {
                        operation.state = OperationState::ReconciledTerminal;
                    }
                }
                self.needs_reconciliation = false;
            }
        }
        Effects::stop()
    }

    pub fn deadline(&mut self, token: &CallToken) -> Result<Effects, ControlError> {
        self.check_token(token)?;
        if self.phase() == Some(Phase::Finished) {
            return Err(ControlError::Stale);
        }
        self.needs_reconciliation = true;
        let session = self.session.as_mut().expect("checked session");
        session.phase = Phase::Uncertain;
        session.capture = CaptureConsent::default();
        Ok(Effects::stop())
    }

    /// Local account invalidation is immediate and independent of any remote
    /// cleanup. The caller must apply these effects even if persistence fails.
    pub fn invalidate(&mut self) -> Effects {
        if self.phase().is_some_and(|phase| phase != Phase::Finished) {
            self.needs_reconciliation = true;
        }
        self.enabled = false;
        for operation in self.operations.values_mut() {
            if operation.state == OperationState::Submitting {
                operation.state = OperationState::NeedsReview;
            }
        }
        self.finish(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> Authority {
        Authority {
            account: "synthetic-account".into(),
            owner: "owner-one".into(),
            authorization: [1; 32],
            logout_epoch: Some(vec![2]),
        }
    }
    fn incoming(
        controller: &mut Controller,
        id: u8,
        freshness: Freshness,
    ) -> Result<(CallToken, Effects), ControlError> {
        controller.incoming(Incoming {
            authority: authority(),
            ingress: 1,
            receipt: [id; 32],
            identity: RemoteIdentity(vec![id]),
            freshness,
            mode: Mode::Video,
            sequence: None,
        })
    }
    fn intent(action: Action) -> Intent {
        Intent {
            action,
            parameters: [3; 32],
        }
    }
    fn accept(controller: &mut Controller, call: &CallToken, camera: bool) -> Submission {
        let (operation, _) = controller
            .prepare(
                "accept",
                Some(call),
                intent(Action::Accept {
                    consent: CaptureConsent {
                        microphone: true,
                        camera,
                    },
                }),
            )
            .unwrap();
        controller.persisted(&operation).unwrap()
    }
    fn notice(
        call: &CallToken,
        identity: u8,
        receipt: u8,
        sequence: u64,
        signal: Signal,
    ) -> Notice {
        Notice {
            authority: authority(),
            ingress: 1,
            token: call.clone(),
            identity: RemoteIdentity(vec![identity]),
            receipt: [receipt; 32],
            sequence: Sequence {
                domain: [4; 32],
                value: sequence,
            },
            signal,
        }
    }
    fn ringing() -> (Controller, CallToken) {
        let mut controller = Controller::new(authority(), 1, false);
        let (call, effects) = incoming(&mut controller, 10, Freshness::VerifiedCurrent).unwrap();
        assert_eq!(effects.ring, Some(true));
        assert_eq!(
            controller.session.as_ref().unwrap().capture,
            CaptureConsent::default()
        );
        (controller, call)
    }

    #[test]
    fn replay_unknown_freshness_and_duplicate_receipts_never_ring_again() {
        let mut controller = Controller::new(authority(), 1, false);
        for freshness in [Freshness::Unverified, Freshness::Replay] {
            assert!(matches!(
                incoming(&mut controller, 10, freshness),
                Err(ControlError::Unverified)
            ));
            assert!(controller.phase().is_none());
        }
        let (call, _) = incoming(&mut controller, 10, Freshness::VerifiedCurrent).unwrap();
        assert!(matches!(
            incoming(&mut controller, 10, Freshness::VerifiedCurrent),
            Err(ControlError::Duplicate)
        ));
        controller
            .notice(notice(&call, 10, 11, 1, Signal::Finished))
            .unwrap();
        let old_call_new_receipt = Incoming {
            authority: authority(),
            ingress: 1,
            receipt: [12; 32],
            identity: RemoteIdentity(vec![10]),
            freshness: Freshness::VerifiedCurrent,
            mode: Mode::Audio,
            sequence: None,
        };
        assert!(matches!(
            controller.incoming(old_call_new_receipt),
            Err(ControlError::Duplicate)
        ));
    }

    #[test]
    fn persist_before_one_submission_and_compare_the_full_intent_on_key_replay() {
        let mut controller = Controller::new(authority(), 1, false);
        let original = intent(Action::Offer {
            mode: Mode::Audio,
            consent: CaptureConsent {
                microphone: true,
                camera: true,
            },
        });
        let (operation, _) = controller.prepare("offer", None, original.clone()).unwrap();
        assert_eq!(
            controller.operations["offer"].state,
            OperationState::Prepared
        );
        let (same, effects) = controller.prepare("offer", None, original.clone()).unwrap();
        assert_eq!(operation, same);
        assert_eq!(effects, Effects::default());
        let different = Intent {
            parameters: [9; 32],
            ..original
        };
        assert!(matches!(
            controller.prepare("offer", None, different),
            Err(ControlError::IdempotencyConflict)
        ));
        let submission = controller.persisted(&operation).unwrap();
        assert_eq!(submission.operation(), &operation);
        assert!(matches!(
            controller.persisted(&operation),
            Err(ControlError::Duplicate)
        ));
        controller
            .submission_result(
                submission,
                SubmissionOutcome::Confirmed(Some(RemoteIdentity(vec![10]))),
            )
            .unwrap();
        assert_eq!(controller.phase(), Some(Phase::Offering));
        assert!(matches!(
            controller.media_ready(&operation.call),
            Err(ControlError::InvalidTransition)
        ));
    }

    #[test]
    fn acceptance_and_signaling_are_not_media_and_camera_requires_its_own_consent() {
        for camera in [false, true] {
            let (mut controller, call) = ringing();
            let submission = accept(&mut controller, &call, camera);
            controller
                .submission_result(submission, SubmissionOutcome::Confirmed(None))
                .unwrap();
            assert_eq!(controller.phase(), Some(Phase::Joining));
            assert!(controller.media_ready(&call).is_err());
            controller
                .notice(notice(&call, 10, 11, 1, Signal::Established))
                .unwrap();
            assert_eq!(
                controller.session.as_ref().unwrap().capture,
                CaptureConsent::default()
            );
            let effects = controller.media_ready(&call).unwrap();
            assert_eq!(
                effects.capture,
                Some(CaptureConsent {
                    microphone: true,
                    camera
                })
            );
        }
    }

    #[test]
    fn replayed_key_never_rebinds_to_new_authority_or_a_later_call() {
        let mut controller = Controller::new(authority(), 1, false);
        let offer = intent(Action::Offer {
            mode: Mode::Audio,
            consent: CaptureConsent::default(),
        });
        let (old, _) = controller.prepare("old", None, offer.clone()).unwrap();
        let submission = controller.persisted(&old).unwrap();
        controller
            .submission_result(
                submission,
                SubmissionOutcome::Confirmed(Some(RemoteIdentity(vec![10]))),
            )
            .unwrap();
        controller
            .notice(notice(&old.call, 10, 11, 1, Signal::Finished))
            .unwrap();
        let (current, _) = controller.prepare("new", None, offer.clone()).unwrap();
        let (replayed, effects) = controller.prepare("old", None, offer.clone()).unwrap();
        assert_eq!(replayed, old);
        assert_ne!(replayed.call, current.call);
        assert_eq!(effects, Effects::default());
        assert!(matches!(
            controller.persisted(&replayed),
            Err(ControlError::Stale)
        ));
        controller.invalidate();
        assert!(matches!(
            controller.prepare("old", None, offer.clone()),
            Err(ControlError::Stale)
        ));
        // Model a replacement credential generation without rebinding its old
        // operation record; no public mutation method bypasses invalidation.
        controller.enabled = true;
        controller.authority.authorization = [9; 32];
        assert!(matches!(
            controller.prepare("old", None, offer),
            Err(ControlError::Stale)
        ));
    }

    #[test]
    fn positive_offer_response_without_proved_identity_remains_uncertain() {
        let mut controller = Controller::new(authority(), 1, false);
        let (operation, _) = controller
            .prepare(
                "offer",
                None,
                intent(Action::Offer {
                    mode: Mode::Audio,
                    consent: CaptureConsent::default(),
                }),
            )
            .unwrap();
        let submission = controller.persisted(&operation).unwrap();
        assert!(matches!(
            controller.submission_result(submission, SubmissionOutcome::Confirmed(None)),
            Err(ControlError::MissingIdentity)
        ));
        assert_eq!(controller.phase(), Some(Phase::Uncertain));
        assert!(controller.needs_reconciliation());
        assert_eq!(
            controller.operations["offer"].state,
            OperationState::NeedsReview
        );
    }

    #[test]
    fn another_device_answer_races_with_local_accept_without_account_logout_or_revival() {
        for remote_first in [false, true] {
            let (mut controller, call) = ringing();
            let submission = accept(&mut controller, &call, true);
            if !remote_first {
                controller
                    .notice(notice(&call, 10, 11, 1, Signal::Established))
                    .unwrap();
            }
            let effects = controller
                .notice(notice(&call, 10, 12, 2, Signal::HandledOnOtherDevice))
                .unwrap();
            assert_eq!(effects, Effects::stop());
            assert_eq!(controller.phase(), Some(Phase::Finished));
            assert!(controller.enabled);
            assert_eq!(controller.authority, authority());
            assert!(!controller.needs_reconciliation());
            assert!(controller
                .submission_result(submission, SubmissionOutcome::Confirmed(None))
                .is_err());
            assert!(controller.media_ready(&call).is_err());
            assert!(controller
                .notice(notice(&call, 10, 13, 3, Signal::Established))
                .is_err());
        }
    }

    #[test]
    fn unknown_submission_blocks_new_keys_until_current_room_is_known_terminal() {
        let (mut controller, call) = ringing();
        let submission = accept(&mut controller, &call, true);
        controller
            .submission_result(submission, SubmissionOutcome::Unknown)
            .unwrap();
        assert_eq!(controller.phase(), Some(Phase::Uncertain));
        assert!(controller.needs_reconciliation());
        let offer = intent(Action::Offer {
            mode: Mode::Audio,
            consent: CaptureConsent::default(),
        });
        assert!(matches!(
            controller.prepare("different-key", None, offer.clone()),
            Err(ControlError::NeedsReconciliation)
        ));
        controller
            .notice(notice(&call, 10, 11, 1, Signal::Finished))
            .unwrap();
        assert!(controller.prepare("new-user-intent", None, offer).is_ok());
    }

    #[test]
    fn local_end_stops_devices_before_remote_response_and_rejection_does_not_claim_room_ended() {
        let (mut controller, call) = ringing();
        let submission = accept(&mut controller, &call, true);
        controller
            .submission_result(submission, SubmissionOutcome::Confirmed(None))
            .unwrap();
        controller
            .notice(notice(&call, 10, 11, 1, Signal::Established))
            .unwrap();
        controller.media_ready(&call).unwrap();
        let (operation, effects) = controller
            .prepare(
                "hangup",
                Some(&call),
                intent(Action::End(EndIntent::Hangup)),
            )
            .unwrap();
        assert_eq!(effects, Effects::stop());
        assert_eq!(
            controller.session.as_ref().unwrap().capture,
            CaptureConsent::default()
        );
        let submission = controller.persisted(&operation).unwrap();
        controller
            .submission_result(submission, SubmissionOutcome::Rejected)
            .unwrap();
        assert_eq!(controller.phase(), Some(Phase::Uncertain));
        assert!(controller.needs_reconciliation());
    }

    #[test]
    fn logout_revokes_queued_submission_and_late_media_for_one_account_only() {
        let (mut one, call) = ringing();
        let (other, _) = ringing();
        let (operation, _) = one
            .prepare(
                "accept",
                Some(&call),
                intent(Action::Accept {
                    consent: CaptureConsent {
                        microphone: true,
                        camera: true,
                    },
                }),
            )
            .unwrap();
        assert_eq!(one.invalidate(), Effects::stop());
        assert!(one.persisted(&operation).is_err());
        assert!(one.media_ready(&call).is_err());
        assert!(one
            .notice(notice(&call, 10, 11, 1, Signal::Established))
            .is_err());
        assert_eq!(other.phase(), Some(Phase::Ringing));
        assert!(one.needs_reconciliation());
    }

    #[test]
    fn ending_revokes_a_prepared_accept_before_it_can_be_dispatched() {
        let (mut controller, call) = ringing();
        let (accept, _) = controller
            .prepare(
                "accept",
                Some(&call),
                intent(Action::Accept {
                    consent: CaptureConsent {
                        microphone: true,
                        camera: true,
                    },
                }),
            )
            .unwrap();
        controller
            .prepare(
                "hangup",
                Some(&call),
                intent(Action::End(EndIntent::Hangup)),
            )
            .unwrap();
        assert!(matches!(
            controller.persisted(&accept),
            Err(ControlError::Stale)
        ));
        assert_eq!(
            controller.operations["accept"].state,
            OperationState::Prepared
        );
        assert_eq!(controller.phase(), Some(Phase::Ending));
    }

    #[test]
    fn stale_authority_ingress_room_sequence_and_timer_cannot_change_current_attempt() {
        let (mut controller, call) = ringing();
        for which in 0..7 {
            let mut event = notice(&call, 10, 11, 1, Signal::Finished);
            match which {
                0 => event.authority.account = "other-account".into(),
                1 => event.authority.owner = "other-owner".into(),
                2 => event.authority.authorization = [9; 32],
                3 => event.authority.logout_epoch = Some(vec![9]),
                4 => event.ingress = 2,
                5 => event.identity = RemoteIdentity(vec![99]),
                _ => event.token.attempt += 1,
            }
            assert!(controller.notice(event).is_err());
            assert_eq!(controller.phase(), Some(Phase::Ringing));
        }
        controller
            .notice(notice(&call, 10, 11, 10, Signal::Established))
            .unwrap_err();
        let submission = accept(&mut controller, &call, false);
        controller
            .submission_result(submission, SubmissionOutcome::Confirmed(None))
            .unwrap();
        controller
            .notice(notice(&call, 10, 12, 10, Signal::Established))
            .unwrap();
        assert!(matches!(
            controller.notice(notice(&call, 10, 13, 9, Signal::Finished)),
            Err(ControlError::Stale)
        ));
        controller
            .notice(notice(&call, 10, 14, 11, Signal::Finished))
            .unwrap();
        let (next, _) = incoming(&mut controller, 20, Freshness::VerifiedCurrent).unwrap();
        assert!(controller.deadline(&call).is_err());
        assert_ne!(call, next);
        assert_eq!(controller.phase(), Some(Phase::Ringing));
    }

    #[test]
    fn unknown_signal_never_advances_interpreted_sequence_and_reconnect_is_delivery_only() {
        let (mut controller, call) = ringing();
        assert!(matches!(
            controller.notice(notice(&call, 10, 11, 100, Signal::Uninterpreted)),
            Err(ControlError::Unverified)
        ));
        assert!(controller.session.as_ref().unwrap().sequences.is_empty());
        controller.replace_ingress(2);
        assert_eq!(controller.phase(), Some(Phase::Ringing));
        assert!(controller
            .notice(notice(&call, 10, 12, 1, Signal::Finished))
            .is_err());
        let mut current = notice(&call, 10, 13, 1, Signal::Finished);
        current.ingress = 2;
        controller.notice(current).unwrap();
        assert_eq!(controller.phase(), Some(Phase::Finished));
    }

    #[test]
    fn timeout_stops_local_work_but_a_later_verified_terminal_can_reconcile_it() {
        let (mut controller, call) = ringing();
        assert_eq!(controller.deadline(&call).unwrap(), Effects::stop());
        assert_eq!(controller.phase(), Some(Phase::Uncertain));
        assert!(controller.needs_reconciliation());
        assert!(controller
            .notice(notice(&call, 10, 11, 1, Signal::Established))
            .is_err());
        controller
            .notice(notice(&call, 10, 12, 2, Signal::Finished))
            .unwrap();
        assert_eq!(controller.phase(), Some(Phase::Finished));
        assert!(!controller.needs_reconciliation());
        assert!(incoming(&mut controller, 20, Freshness::VerifiedCurrent).is_ok());
    }

    #[test]
    fn verified_invitation_sequence_fences_older_notifications_in_the_same_domain() {
        let mut controller = Controller::new(authority(), 1, false);
        let (call, _) = controller
            .incoming(Incoming {
                authority: authority(),
                ingress: 1,
                receipt: [10; 32],
                identity: RemoteIdentity(vec![10]),
                freshness: Freshness::VerifiedCurrent,
                mode: Mode::Audio,
                sequence: Some(Sequence {
                    domain: [4; 32],
                    value: 10,
                }),
            })
            .unwrap();
        assert!(matches!(
            controller.notice(notice(&call, 10, 11, 9, Signal::HandledOnOtherDevice)),
            Err(ControlError::Stale)
        ));
        assert_eq!(controller.phase(), Some(Phase::Ringing));
        controller
            .notice(notice(&call, 10, 12, 11, Signal::HandledOnOtherDevice))
            .unwrap();
        assert_eq!(controller.phase(), Some(Phase::Finished));
    }

    #[test]
    fn restart_and_capacity_fail_closed_without_eviction_or_automatic_redial() {
        let mut recovered = Controller::new(authority(), 1, true);
        assert!(matches!(
            incoming(&mut recovered, 10, Freshness::VerifiedCurrent),
            Err(ControlError::NeedsReconciliation)
        ));
        let mut full = Controller::new(authority(), 1, false);
        for n in 0..MAX_RECEIPTS {
            let mut receipt = [0; 32];
            receipt[..8].copy_from_slice(&(n as u64).to_be_bytes());
            full.admit_receipt(receipt).unwrap();
        }
        assert!(matches!(
            full.admit_receipt([99; 32]),
            Err(ControlError::Capacity)
        ));
        assert!(matches!(
            full.admit_receipt([0; 32]),
            Err(ControlError::Duplicate)
        ));
    }

    #[test]
    fn local_decline_cancel_and_audio_capture_are_role_scoped() {
        let (mut incoming_call, call) = ringing();
        assert!(incoming_call
            .prepare(
                "cancel",
                Some(&call),
                intent(Action::End(EndIntent::Cancel))
            )
            .is_err());
        let (_, effects) = incoming_call
            .prepare(
                "decline",
                Some(&call),
                intent(Action::End(EndIntent::Decline)),
            )
            .unwrap();
        assert_eq!(effects, Effects::stop());
        let mut outgoing = Controller::new(authority(), 1, false);
        let (operation, _) = outgoing
            .prepare(
                "offer",
                None,
                intent(Action::Offer {
                    mode: Mode::Audio,
                    consent: CaptureConsent {
                        microphone: true,
                        camera: true,
                    },
                }),
            )
            .unwrap();
        let submission = outgoing.persisted(&operation).unwrap();
        outgoing
            .submission_result(
                submission,
                SubmissionOutcome::Confirmed(Some(RemoteIdentity(vec![10]))),
            )
            .unwrap();
        outgoing
            .notice(notice(&operation.call, 10, 11, 1, Signal::Established))
            .unwrap();
        let effects = outgoing.media_ready(&operation.call).unwrap();
        assert_eq!(
            effects.capture,
            Some(CaptureConsent {
                microphone: true,
                camera: false
            })
        );
    }
}
