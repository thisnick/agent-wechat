//! Pending group invitations and isolated Join/profile/Exit supervision.
use super::*;
use crate::{
    call_types::{CallConferenceInvitation, CallInvitationStatus, CallParticipant},
    calls::notifications::IncomingNotification,
};
use wechat_calls::session::{ConferenceCreateTarget, ConferenceSession, ConferenceTarget};
use wechat_protocol::calls::incoming::{ConferenceJoinContext, NotificationPayload};

mod receive;

fn participant(member: wechat_calls::session::ConferenceMember) -> CallParticipant {
    use crate::call_types::{
        CallMembership, CallParticipantStatus, CallParticipantStatusKind as Kind,
    };
    let membership = match member.status {
        1 => CallMembership::Joined,
        2 | 3 => CallMembership::Invited,
        4 => CallMembership::Left,
        _ => CallMembership::Unknown,
    };
    CallParticipant {
        participant_id: format!("member-{}", member.member_id),
        contact_id: member.username,
        membership,
        status: Some(CallParticipantStatus {
            code: member.status,
            kind: match member.status {
                0 => Kind::Undefined,
                1 => Kind::Joined,
                2 => Kind::Invited,
                3 => Kind::Acknowledged,
                4 => Kind::Left,
                5 => Kind::Cancelled,
                6 => Kind::Busy,
                7 => Kind::Rejected,
                8 => Kind::NoResponse,
                _ => Kind::Unknown,
            },
        }),
        microphone_enabled: if membership == CallMembership::Left {
            None
        } else {
            member.microphone_enabled
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeMode {
    Application,
    Profile,
    Receive,
    MultiReceive,
    DuplexTone,
}

pub(super) struct Pending {
    invitation_id: String,
    context: ConferenceJoinContext,
    note: Arc<IncomingNotification>,
    expires: Instant,
    withdrawn: AtomicBool,
    spent: AtomicBool,
}

// Retain fixed diagnostic labels only. An error chain may contain upstream
// data, so never serialize its text into the operational record.
pub(super) fn record_failure(core: &Core, worker: &Worker, error: &anyhow::Error) {
    if !worker.is_conference() {
        return;
    }
    let code = failure_code(error);
    let _ = core.store.put_runtime(
        &worker.lease.authority().account_id,
        &format!("call-conference-failure:{}", worker.identity.call_id()),
        code.as_bytes(),
    );
}

fn failure_code(error: &anyhow::Error) -> &'static str {
    if error.is::<UnsupportedCallMedia>() {
        return "unsupported_media_profile";
    }
    for cause in error.chain() {
        let code = match cause.to_string().as_str() {
            "Call authority changed during host operation" => "authority_changed",
            "Call account lineage changed" => "lineage_changed",
            "session_superseded" => "session_superseded",
            "auth_required" => "auth_required",
            "Call accepted policy changed" => "policy_changed",
            "Current call state not accepted" => "current_state_unaccepted",
            "Unexpected call host state change" => "host_state_changed",
            "Call account changed beyond its cookie" => "concurrent_state_changed",
            "Call accepted credentials changed" => "credentials_changed",
            "Selected call host route unsupported" => "host_route_unsupported",
            "invalid_call_action" => "journal_route_rejected",
            "call_needs_review" => "cleanup_blocked_by_uncertain_action",
            "Conference entry no longer current" => "entry_expired",
            "Call control request was not submitted" => "admission_revoked",
            "Call host outcome unknown" => "host_outcome_unknown",
            "Conference room differs from current invitation" => "room_mismatch",
            "Conference entry returned no room" => "missing_room",
            "Personal call host operation rejected" => "join_rejected",
            "App auth host operation rejected" => "app_auth_rejected",
            "App auth base operation rejected" => "app_auth_base_rejected",
            "App auth detail operation rejected" => "app_auth_detail_rejected",
            "Unsupported app encryption negotiation" => "app_encryption_unsupported",
            _ => continue,
        };
        return code;
    }
    "unclassified"
}
impl Pending {
    pub(super) fn current(&self) -> bool {
        self.note.lease.current()
            && !self.withdrawn.load(Ordering::Acquire)
            && Instant::now() < self.expires
    }
}

pub(super) fn observe(inner: &Inner, note: Arc<IncomingNotification>, ttl: Duration) {
    if !inner.config.conference_join_profile || ttl.is_zero() || !note.lease.current() {
        return;
    }
    let Some(core) = inner.core.upgrade() else {
        return;
    };
    if !note
        .core
        .upgrade()
        .is_some_and(|source| Arc::ptr_eq(&source, &core))
        || !note.header_from.ends_with("@chatroom")
        || note.control.source_sender.as_deref() != Some(note.caller_username.as_str())
    {
        return;
    }
    let Ok(mut pending) = inner.conferences.lock() else {
        return;
    };
    // Keep withdrawn/spent locators until their original deadline. A delayed
    // retransmission cannot give that same room a new invitation identity.
    pending.retain(|_, item| item.note.lease.current() && Instant::now() < item.expires);
    match &note.control.payload {
        NotificationPayload::Invite(_) => {
            let Ok(context) = note.control.carrier.conference_join_context() else {
                return;
            };
            if context.group_id != note.header_from
                || context.caller_username != note.caller_username
                || context.callee_username != note.own_username
            {
                return;
            }
            let key = (
                note.lease.authority().account_id.clone(),
                context.group_id.clone(),
            );
            if !pending.contains_key(&key)
                && pending
                    .keys()
                    .filter(|(account, _)| account == &key.0)
                    .count()
                    >= 16
            {
                return;
            }
            let item = Arc::new(Pending {
                invitation_id: uuid::Uuid::new_v4().to_string(),
                context,
                note,
                expires: Instant::now() + ttl.min(Duration::from_secs(60)),
                withdrawn: AtomicBool::new(false),
                spent: AtomicBool::new(false),
            });
            if let Some(prior) = pending.get(&key) {
                // Retransmission cannot restore a spent entry or reset its deadline.
                if prior.context.room_id == item.context.room_id
                    && prior.context.room_key == item.context.room_key
                    && prior.context.own_member == item.context.own_member
                    && prior.context.context_id == item.context.context_id
                {
                    return;
                }
                prior.withdrawn.store(true, Ordering::Release);
            }
            pending.insert(key, item);
        }
        NotificationPayload::Cancel(cancel) => {
            let key = (
                note.lease.authority().account_id.clone(),
                note.header_from.clone(),
            );
            if let Some(prior) = pending.get(&key) {
                if cancel.room_id == Some(prior.context.room_id)
                    && cancel.room_key == Some(prior.context.room_key)
                {
                    prior.withdrawn.store(true, Ordering::Release);
                }
            }
            // This withdraws pending entry only. It does not prove a joined room ended.
        }
    }
}

pub(super) fn invitations(
    inner: &Inner,
    authority: &AccountAuthority,
) -> Vec<CallConferenceInvitation> {
    let Ok(pending) = inner.conferences.lock() else {
        return vec![];
    };
    pending
        .values()
        .filter(|entry| {
            entry.current()
                && !entry.spent.load(Ordering::Acquire)
                && entry.note.lease.authority() == authority
        })
        .map(|entry| CallConferenceInvitation {
            invitation_id: entry.invitation_id.clone(),
            revision: 1,
            group_id: entry.context.group_id.clone(),
            caller_id: entry.context.caller_username.clone(),
        })
        .collect()
}

pub(super) fn claim(
    inner: &Inner,
    group: &str,
    authority: &AccountAuthority,
    expected: &CallConferenceInvitation,
) -> Option<Arc<Pending>> {
    if !inner.config.conference_join_profile {
        return None;
    }
    let pending = inner
        .conferences
        .lock()
        .ok()?
        .get(&(authority.account_id.clone(), group.to_owned()))?
        .clone();
    if expected.invitation_id != pending.invitation_id
        || expected.revision != 1
        || expected.group_id != group
        || !pending.current()
        || pending.note.lease.authority() != authority
        || pending
            .spent
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return None;
    }
    Some(pending)
}

pub(super) struct Command {
    pub(super) action: CommandAction,
    pub(super) reply: oneshot::Sender<std::result::Result<CommandResult, CallServiceError>>,
}

pub(super) enum CommandAction {
    Invite(Vec<String>),
    Mute(bool),
}
pub(super) enum CommandResult {
    Invitations(Vec<CallInvitationResult>),
    Muted,
}

pub(super) struct RunInput {
    pub(super) admitted: oneshot::Sender<std::result::Result<BackendOutcome, CallServiceError>>,
    pub(super) probe: ProbeMode,
    pub(super) audio: Receiver<QueuedAudio>,
    pub(super) request_id: String,
}

fn invitation_results(
    contacts: &[String],
    results: &[wechat_protocol::calls::conference::InvitationResult],
    verification_required: bool,
) -> Vec<CallInvitationResult> {
    let mut used = std::collections::BTreeSet::new();
    contacts
        .iter()
        .map(|contact| {
            let result = results
                .iter()
                .enumerate()
                .find(|(index, result)| result.contact_id == *contact && !used.contains(index));
            let code = result.and_then(|(index, result)| {
                used.insert(index);
                result.service_code
            });
            CallInvitationResult {
                contact_id: contact.clone(),
                status: match code {
                    Some(0) => CallInvitationStatus::Submitted,
                    Some(_) => CallInvitationStatus::Rejected,
                    None => CallInvitationStatus::Unknown,
                },
                service_code: code,
                verification_required,
            }
        })
        .collect()
}

pub(super) fn run(
    transport: &FencedTransport,
    mut state: AccountState,
    authority: AppAuthority,
    parameters: VoiceParameters,
    mut tasks: Tasks,
    input: RunInput,
) -> Result<()> {
    let RunInput {
        admitted,
        probe,
        audio,
        request_id,
    } = input;
    let core = transport.core.upgrade().context("Conference core closed")?;
    let worker = &transport.worker;
    let account = &transport.account;
    let call_id = &transport.call_id;
    let pending = worker.conference.as_ref();
    ensure!(
        pending.is_some() || worker.conference_start.is_some(),
        "Conference entry context missing"
    );
    let ControlAdmission::Owner(sink) = &transport.admission else {
        bail!("Conference entry requires an owner")
    };
    ensure!(
        pending.is_none_or(|entry| entry.current())
            && worker.current()
            && !worker.stop.load(Ordering::Acquire),
        "Conference entry no longer current"
    );
    core.store.put_runtime(
        account,
        &format!("call-conference-phase:{call_id}"),
        b"app_auth",
    )?;
    let mut session = ConferenceSession::authenticate(
        transport,
        &mut state,
        authority,
        tasks.host,
        crypto::random::<32>()?,
        parameters,
    )?;
    if pending.is_some_and(|entry| !entry.current())
        || !worker.current()
        || worker.stop.load(Ordering::Acquire)
    {
        core.store.put_runtime(
            account,
            &format!("call-conference-phase:{call_id}"),
            if pending.is_some_and(|entry| !entry.current()) {
                b"entry_withdrawn_before_join"
            } else if !worker.current() {
                b"authority_revoked_before_join"
            } else {
                b"stopped_before_join"
            },
        )?;
        core.store.close_call_locally(
            account,
            call_id,
            wechat_store::CallLocalEndReason::NoRemoteMutation,
        )?;
        let _ = admitted.send(Ok(BackendOutcome::Ended));
        return Ok(());
    }
    core.store.put_runtime(
        account,
        &format!("call-conference-phase:{call_id}"),
        b"join",
    )?;
    let joined = if let Some(pending) = pending {
        session.join(
            transport,
            &mut state,
            authority,
            ConferenceTarget {
                group_id: &pending.context.group_id,
                cloud_group_id: &pending.context.context_id,
                join_id: unix_ms()?,
            },
            tasks.next()?,
        )
    } else {
        session.create(
            transport,
            &mut state,
            authority,
            ConferenceCreateTarget {
                group_id: &worker.peer_id,
                invitees: worker
                    .conference_start
                    .as_deref()
                    .context("Conference invitees missing")?,
                invite_id: unix_ms()?,
            },
            tasks.next()?,
        )
    };
    if let (Some(contacts), Some(response)) =
        (&worker.conference_start, session.creation_response())
    {
        let results = invitation_results(
            contacts,
            &response.invite_results,
            response
                .verification_url
                .as_ref()
                .is_some_and(|url| !url.is_empty()),
        );
        transport
            .runtime
            .block_on(sink.invitation_result(request_id, results))?;
    }
    if !session.has_room() {
        joined?;
        bail!("Conference entry returned no room");
    }
    // A different returned room is not permission to submit an unbound Exit.
    if let Some(pending) = pending {
        ensure!(
            session.matches_room(pending.context.room_id, pending.context.room_key),
            "Conference room differs from current invitation"
        );
    }
    let _ = admitted.send(Ok(BackendOutcome::Admitted));
    let inspection = (|| -> Result<()> {
        joined?;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if !worker.current() || worker.stop.load(Ordering::Acquire) {
                bail!("Conference inspection stopped");
            }
            if !session.join_pending() && session.own_membership_ready() {
                if let Ok(profile) = session.media_profile() {
                    if profile[0] != 0 {
                        let participants = session
                            .members()
                            .into_iter()
                            .map(participant)
                            .collect::<Vec<_>>();
                        let count = participants.len();
                        transport
                            .runtime
                            .block_on(sink.participants(participants))?;
                        core.store.put_runtime(account, &format!("call-conference-profile:{call_id}"),
                            &serde_json::to_vec(&serde_json::json!({"profile":profile,"members":count,"ownMembershipReady":true,"roomMatched":true}))?)?;
                        return Ok(());
                    }
                }
            }
            ensure!(
                Instant::now() < deadline,
                "Conference negotiation timed out"
            );
            session.poll(transport, &mut state, authority, tasks.next()?)?;
        }
    })();
    core.store.put_runtime(
        account,
        &format!("call-conference-phase:{call_id}"),
        if inspection.is_ok() {
            b"profile_captured"
        } else {
            b"negotiation_incomplete"
        },
    )?;
    if inspection.is_ok() && probe != ProbeMode::Profile {
        core.store.put_runtime(
            account,
            &format!("call-conference-phase:{call_id}"),
            b"receiving",
        )?;
        let observation = receive::run(
            transport,
            &mut state,
            authority,
            &mut session,
            &mut tasks,
            probe,
            audio,
        );
        if let Err(error) = &observation {
            record_failure(&core, worker, error);
        }
        core.store.put_runtime(
            account,
            &format!("call-conference-phase:{call_id}"),
            if observation.is_ok() {
                b"receive_probe_complete"
            } else {
                b"receive_probe_failed"
            },
        )?;
    }
    // Neither probe transmits audio. Remove only this authenticated room
    // membership after the profile capture or short receive observation.
    let _ = transport.runtime.block_on(sink.ending());
    let exit = session.exit(transport, &mut state, authority, tasks.next()?);
    core.store.finish_call(account, call_id, exit.is_ok())?;
    exit
}

#[cfg(test)]
fn claim_current(inner: &Inner, group: &str, authority: &AccountAuthority) -> Option<Arc<Pending>> {
    let expected = invitations(inner, authority)
        .into_iter()
        .find(|entry| entry.group_id == group)?;
    claim(inner, group, authority, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine};
    use wechat_protocol::wire::{self, SyncBatch, SyncCommand};

    #[test]
    fn failure_diagnostic_uses_fixed_labels_without_error_data() {
        let error = anyhow::anyhow!("Unexpected call host state change")
            .context("private upstream detail must not be recorded");
        assert_eq!(failure_code(&error), "host_state_changed");
        assert_eq!(
            failure_code(&anyhow::Error::new(UnsupportedCallMedia).context("private detail")),
            "unsupported_media_profile"
        );
        assert_eq!(
            failure_code(&anyhow::anyhow!("private unknown detail")),
            "unclassified"
        );
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        core: Arc<Core>,
        backend: Arc<NativeCallBackend>,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_host(None)
        }
        fn with_host(host: Option<Arc<dyn TestCallIo>>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let store = Arc::new(crate::Store::open(&temp.path().join("data")).unwrap());
            let core = Core::new(store.clone(), None).unwrap();
            for (index, account) in ["one", "two"].iter().enumerate() {
                store.create_account(account).unwrap();
                store.put_runtime(account, "desired", b"connected").unwrap();
                store
                    .mark_validated_if_active(account, &core.ownership_generation)
                    .unwrap();
                let state = AccountState {
                    schema: 1,
                    device: wechat_protocol::DeviceProfile::generate("Synthetic Linux".into())
                        .unwrap(),
                    uin: index as u32 + 1,
                    own_username: format!("self-{account}"),
                    cookie: vec![],
                    session_key: vec![1; 16],
                    request_key: vec![2; 24],
                    response_key: vec![3; 24],
                    auto_auth_key: vec![],
                    key_info: vec![],
                    database_password: vec![],
                    auth_snapshot: {
                        let mut snapshot = wechat_protocol::AuthSnapshot::default();
                        snapshot.response =
                            p::message(&[p::bytes(1, &[8, 0]), p::uint(2, 0)]).unwrap();
                        snapshot
                    },
                    long_host: "synthetic-unused.weixin.qq.com".into(),
                    short_host: "synthetic-unused.weixin.qq.com".into(),
                };
                core.save_state(account, &state).unwrap();
                assert!(core.call_authority(account).is_ok());
            }
            let mut backend = NativeCallBackend::new(Arc::downgrade(&core));
            Arc::get_mut(&mut backend.inner)
                .unwrap()
                .config
                .conference_join_profile = true;
            Arc::get_mut(&mut backend.inner).unwrap().test_io = host;
            let backend = Arc::new(backend);
            Core::configure_calls(&core, backend.clone()).unwrap();
            Self {
                _temp: temp,
                core,
                backend,
            }
        }
        fn note(
            &self,
            account: &str,
            room: u64,
            cancel: bool,
            receipt: &str,
        ) -> Arc<IncomingNotification> {
            let own = format!("self-{account}");
            let inner = if cancel {
                p::message(&[
                    p::uint(1, room),
                    p::uint(2, 43),
                    p::uint(3, 0),
                    p::uint(4, 1),
                ])
                .unwrap()
            } else {
                p::message(&[
                    p::uint(1, room),
                    p::uint(2, 43),
                    p::uint(3, 2),
                    p::uint(4, 1),
                    p::bytes(5, b"caller-open"),
                    p::bytes(6, b"callee-open"),
                    p::bytes(7, b"caller"),
                    p::bytes(8, own.as_bytes()),
                    p::bytes(10, b"wechat"),
                    p::bytes(11, b"voip-mt"),
                    p::bytes(13, b"test@chatroom"),
                ])
                .unwrap()
            };
            let conference =
                p::message(&[p::uint(1, if cancel { 3 } else { 2 }), p::bytes(2, &inner)]).unwrap();
            let carrier = if cancel {
                p::message(&[p::uint(1, room), p::bytes(2, &conference)]).unwrap()
            } else {
                p::message(&[
                    p::bytes(1, b"test@chatroom"),
                    p::uint(2, room),
                    p::bytes(3, b"opaque-context"),
                    p::bytes(4, b"caller-open"),
                    p::bytes(5, b"callee-open"),
                    p::bytes(6, &conference),
                    p::bytes(8, b"caller"),
                    p::uint(9, 123),
                    p::uint(10, 0),
                ])
                .unwrap()
            };
            let child = if cancel { "cancelinvite" } else { "invite" };
            let xml = format!(
                "caller:\n<sysmsg type='voipmt'><voipmt><{child}>{}</{child}></voipmt></sysmsg>",
                STANDARD.encode(carrier)
            );
            let now = chrono::Utc::now().timestamp() as u64;
            let original = p::message(&[
                p::uint(1, 1),
                p::bytes(2, &p::bytes(1, b"test@chatroom")),
                p::bytes(3, &p::bytes(1, own.as_bytes())),
                p::uint(4, 10002),
                p::bytes(5, &p::bytes(1, xml.as_bytes())),
                p::uint(6, 0),
                p::uint(7, 0),
                p::bytes(8, &p::buffer(&[])),
                p::uint(9, now),
                p::uint(12, room),
            ])
            .unwrap();
            let message = wire::decode_add_message(&original, &own).unwrap();
            let control = crate::calls::incoming::decode_system_control(&message)
                .unwrap()
                .unwrap();
            Arc::new(IncomingNotification {
                lease: self.core.call_authority(account).unwrap(),
                receipt_id: receipt.into(),
                source_created_at: now,
                received_at_unix_ms: now * 1000,
                own_username: own,
                caller_username: "caller".into(),
                original_message: original.into(),
                control,
                core: Arc::downgrade(&self.core),
                header_from: "test@chatroom".into(),
            })
        }
        fn stage(&self, account: &str, note: &IncomingNotification, cursor: u8) {
            self.core
                .commit_sync_batch(
                    account,
                    SyncBatch {
                        code: 0,
                        flags: 0,
                        cursor: vec![cursor],
                        commands: vec![SyncCommand {
                            id: 5,
                            payload: note.original_message.to_vec(),
                        }],
                        continuation: false,
                    },
                    "synthetic",
                    "synthetic",
                )
                .unwrap();
        }
    }
    struct EntryBoundaryHost {
        routes: Mutex<Vec<CallHostRoute>>,
    }
    impl TestCallIo for EntryBoundaryHost {
        fn call(
            &self,
            state: &mut AccountState,
            route: CallHostRoute,
            _: &[u8],
            handoff: &mut dyn FnMut() -> Result<()>,
        ) -> Result<Vec<u8>> {
            self.routes.lock().unwrap().push(route);
            if route == CallHostRoute::ConferenceJoin {
                // Deliberately stop at the boundary: this fixture never joins
                // a remote room or needs a compensating Exit.
                return Err(CallRpcNotSubmitted.into());
            }
            ensure!(
                route == CallHostRoute::AppAuth,
                "Unexpected synthetic route"
            );
            handoff()?;
            state.cookie = b"new-cookie".to_vec();
            let auth = p::message(&[p::bytes(9, b"synthetic-server"), p::uint(11, 1)])?;
            let unify = p::message(&[p::bytes(2, &auth), p::bytes(3, &p::uint(1, 17))])?;
            let inner = p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &unify)])?;
            p::message(&[p::bytes(1, &[8, 0]), p::bytes(2, &inner)])
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn owned_group_entry_reaches_join_after_auth_cookie_update() {
        use crate::call_types::{CallAudioFormat, CallCommand, CallReadyScope};
        let host = Arc::new(EntryBoundaryHost {
            routes: Mutex::new(vec![]),
        });
        let f = Fixture::with_host(Some(host.clone()));
        let service = f.core.call_service("one").unwrap();
        let client = service.connect("one", "group-entry", true).await.unwrap();
        service
            .command(
                &client.client_id,
                CallCommand::Ready {
                    request_id: "ready".into(),
                    ready: true,
                    format: CallAudioFormat::default(),
                    scope: Some(CallReadyScope::Outgoing),
                },
            )
            .await
            .unwrap();
        let note = f.note("one", 42, false, "current-invitation");
        observe(&f.backend.inner, note, Duration::from_secs(30));
        let invitation = invitations(
            &f.backend.inner,
            f.core.call_authority("one").unwrap().authority(),
        )
        .remove(0);
        service
            .command(
                &client.client_id,
                CallCommand::Join {
                    request_id: "join".into(),
                    invitation_id: invitation.invitation_id,
                    invitation_revision: invitation.revision,
                },
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let done = f
                    .backend
                    .inner
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|worker| worker.done.lock().unwrap().is_some());
                if done {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let call_id = f
            .backend
            .inner
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .identity
            .call_id()
            .to_owned();
        let failure = f
            .core
            .store
            .runtime("one", &format!("call-conference-failure:{call_id}"))
            .unwrap();
        let phase = f
            .core
            .store
            .runtime("one", &format!("call-conference-phase:{call_id}"))
            .unwrap();
        assert_eq!(
            *host.routes.lock().unwrap(),
            [CallHostRoute::AppAuth, CallHostRoute::ConferenceJoin],
            "failure={failure:?} phase={phase:?}"
        );
        assert_eq!(f.core.account_state("one").unwrap().cookie, b"new-cookie");
        let records = f.core.store.call_action_records("one", &call_id).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].route, 14754);
        assert_eq!(records[1].outcome, "not_submitted");
        assert!(records[1].encrypted_request.is_some());
        assert!(!f.core.store.has_unresolved_call("one").unwrap());
    }
    #[tokio::test]
    async fn replaced_invitation_cannot_claim_the_new_room_and_retransmission_cannot_rearm() {
        let f = Fixture::new();
        let first = f.note("one", 41, false, "first");
        let authority = first.lease.authority().clone();
        observe(&f.backend.inner, first.clone(), Duration::from_secs(30));
        let old = invitations(&f.backend.inner, &authority).remove(0);
        observe(&f.backend.inner, first, Duration::from_secs(30));
        assert_eq!(
            invitations(&f.backend.inner, &authority).as_slice(),
            std::slice::from_ref(&old)
        );
        let replacement = f.note("one", 42, false, "replacement");
        observe(
            &f.backend.inner,
            replacement.clone(),
            Duration::from_secs(30),
        );
        let new = invitations(&f.backend.inner, &authority).remove(0);
        assert_ne!(old.invitation_id, new.invitation_id);
        assert!(claim(&f.backend.inner, "test@chatroom", &authority, &old).is_none());
        assert!(claim(&f.backend.inner, "test@chatroom", &authority, &new).is_some());
        observe(&f.backend.inner, replacement, Duration::from_secs(30));
        assert!(invitations(&f.backend.inner, &authority).is_empty());
        assert!(claim(&f.backend.inner, "test@chatroom", &authority, &new).is_none());
    }

    #[tokio::test]
    async fn cancellation_tombstone_prevents_delayed_invitation_from_rearming() {
        let f = Fixture::new();
        let note = f.note("one", 42, false, "original");
        let authority = note.lease.authority().clone();
        observe(&f.backend.inner, note.clone(), Duration::from_secs(30));
        let locator = invitations(&f.backend.inner, &authority).remove(0);
        observe(
            &f.backend.inner,
            f.note("one", 42, true, "cancel"),
            Duration::from_secs(30),
        );
        observe(&f.backend.inner, note, Duration::from_secs(30));
        assert!(invitations(&f.backend.inner, &authority).is_empty());
        assert!(claim(&f.backend.inner, "test@chatroom", &authority, &locator).is_none());
    }

    #[tokio::test]
    async fn staged_group_entry_stays_passive_and_catchup_cannot_arm_it() {
        let f = Fixture::new();
        let first = f.note("one", 42, false, "catchup");
        f.stage("one", &first, 1);
        assert!(
            claim_current(&f.backend.inner, "test@chatroom", first.lease.authority()).is_none()
        );
        let live = f.note("one", 44, false, "live");
        f.stage("one", &live, 2);
        let pending =
            claim_current(&f.backend.inner, "test@chatroom", live.lease.authority()).unwrap();
        assert_eq!(pending.context.room_id, 44);
        assert!(f.backend.inner.active.lock().unwrap().is_none());
        assert!(claim_current(&f.backend.inner, "test@chatroom", live.lease.authority()).is_none());
    }
    #[tokio::test]
    async fn group_entries_are_account_scoped_single_use_and_withdrawn_by_bound_cancel() {
        let f = Fixture::new();
        let one = f.note("one", 42, false, "first");
        let two = f.note("two", 42, false, "second-account");
        observe(&f.backend.inner, one.clone(), Duration::from_secs(30));
        observe(&f.backend.inner, two.clone(), Duration::from_secs(30));
        let mut wrong = one.lease.authority().clone();
        wrong.generation = "wrong-generation".into();
        assert!(claim_current(&f.backend.inner, "test@chatroom", &wrong).is_none());
        let entry =
            claim_current(&f.backend.inner, "test@chatroom", one.lease.authority()).unwrap();
        observe(
            &f.backend.inner,
            f.note("one", 42, false, "duplicate-receipt"),
            Duration::from_secs(60),
        );
        assert!(claim_current(&f.backend.inner, "test@chatroom", one.lease.authority()).is_none());
        observe(
            &f.backend.inner,
            f.note("one", 99, true, "unrelated-cancel"),
            Duration::from_secs(30),
        );
        assert!(entry.current());
        observe(
            &f.backend.inner,
            f.note("one", 42, true, "matching-cancel"),
            Duration::from_secs(30),
        );
        assert!(!entry.current());
        assert!(claim_current(&f.backend.inner, "test@chatroom", two.lease.authority()).is_some());
        assert!(f.backend.inner.active.lock().unwrap().is_none());
    }
    #[tokio::test]
    async fn expired_or_foreign_core_invitation_cannot_arm_entry() {
        let f = Fixture::new();
        let note = f.note("one", 42, false, "expired");
        observe(&f.backend.inner, note.clone(), Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(claim_current(&f.backend.inner, "test@chatroom", note.lease.authority()).is_none());
        let foreign = Fixture::new();
        observe(
            &f.backend.inner,
            foreign.note("one", 43, false, "foreign"),
            Duration::from_secs(30),
        );
        assert!(claim_current(&f.backend.inner, "test@chatroom", note.lease.authority()).is_none());
    }
}
