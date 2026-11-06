//! Scoped conference media probes. PCM and packet contents are not retained.
//! Generated audio requires the separately enabled duplex-probe mode.
use super::*;
#[cfg(test)]
use wechat_calls::conference_media::ConferenceReceiver as Receiver;
#[cfg(test)]
use wechat_calls::native_media::reconcile_roster;
use wechat_calls::native_media::{
    run_conference_media, ConferenceMediaOptions, ConferenceMediaReport as ReceiveReport,
    ConferenceRoster as RosterSnapshot,
};
use wechat_protocol::calls::relay::SelectedRelay;

pub(super) fn run(
    transport: &FencedTransport,
    state: &mut AccountState,
    authority: AppAuthority,
    session: &mut ConferenceSession,
    tasks: &mut Tasks,
    probe: ProbeMode,
    audio: std::sync::mpsc::Receiver<QueuedAudio>,
) -> Result<()> {
    let core = transport.core.upgrade().context("Conference core closed")?;
    let selected = session.take_media(0, 0)?;
    let initial_profile = session.media_profile()?;
    let multi_speaker = matches!(probe, ProbeMode::MultiReceive | ProbeMode::DuplexTone);
    let transmit_grouping = matches!(probe, ProbeMode::Application | ProbeMode::DuplexTone)
        .then_some((initial_profile[1] >> 8) as u8);
    let application = if probe == ProbeMode::Application {
        let ControlAdmission::Owner(sink) = &transport.admission else {
            bail!("Conference application requires an owner")
        };
        Some(Application {
            sink: sink.clone(),
            runtime: transport.runtime.clone(),
            audio,
        })
    } else {
        None
    };
    let roster = Arc::new(Mutex::new(RosterSnapshot {
        revision: 0,
        members: session.members(),
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let media_stop = stop.clone();
    let media_done = done.clone();
    let media_roster = roster.clone();
    let worker = transport.worker.clone();
    let thread = thread::Builder::new()
        .name("wechat-group-receive-probe".into())
        .spawn(move || {
            let report = observe(
                &worker,
                selected,
                &media_roster,
                &media_stop,
                ProbeOptions {
                    multi_speaker,
                    transmit_grouping,
                    application,
                },
            );
            media_done.store(true, Ordering::Release);
            worker.media_stop.store(true, Ordering::Release);
            report
        })?;
    let control = (|| -> Result<()> {
        let mut last_invitation = unix_ms()?;
        let mut last_microphone = 0u64;
        let deadline = Instant::now()
            + Duration::from_secs(if probe == ProbeMode::Application {
                1200
            } else if multi_speaker {
                120
            } else {
                48
            });
        while transport.worker.current()
            && !transport.worker.stop.load(Ordering::Acquire)
            && !done.load(Ordering::Acquire)
            && Instant::now() < deadline
        {
            loop {
                let command = transport
                    .worker
                    .conference_commands
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Conference command queue closed"))?
                    .pop_front();
                let Some(command) = command else { break };
                if command.reply.is_closed() {
                    continue;
                }
                let result = match command.action {
                    CommandAction::Invite(contacts) => {
                        last_invitation = last_invitation
                            .checked_add(1)
                            .context("Conference invitation ID exhausted")?
                            .max(unix_ms()?);
                        session
                            .invite(
                                transport,
                                state,
                                authority,
                                &contacts,
                                last_invitation,
                                tasks.next()?,
                            )
                            .map(|response| {
                                CommandResult::Invitations(super::invitation_results(
                                    &contacts,
                                    &response.invite_results,
                                    response
                                        .verification_url
                                        .as_ref()
                                        .is_some_and(|url| !url.is_empty()),
                                ))
                            })
                    }
                    CommandAction::Mute(muted) => {
                        let now = u64::try_from(
                            SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros(),
                        )?;
                        last_microphone = last_microphone
                            .checked_add(1)
                            .context("Conference microphone clock exhausted")?
                            .max(now);
                        session
                            .set_muted(
                                transport,
                                state,
                                authority,
                                muted,
                                last_microphone,
                                tasks.next()?,
                            )
                            .map(|()| CommandResult::Muted)
                    }
                };
                let result = result.map_err(|error| {
                    if error.is::<CallRpcNotSubmitted>() {
                        service_error(CallErrorCode::Stale)
                    } else {
                        service_error(CallErrorCode::OutcomeUnknown)
                    }
                });
                let _ = command.reply.send(result);
            }
            publish_roster(transport, session, &roster)?;
            match session.poll(transport, state, authority, tasks.next()?) {
                Ok(true) => {}
                Ok(false) => bail!("Conference membership ended"),
                Err(error)
                    if error.is::<CallLiveSyncInterrupted>() && transport.worker.has_commands() =>
                {
                    continue
                }
                Err(error)
                    if error.is::<CallLiveSyncInterrupted>()
                        && (done.load(Ordering::Acquire)
                            || transport.worker.stop.load(Ordering::Acquire)) =>
                {
                    break
                }
                Err(error) => return Err(error),
            }
            session.take_media(0, 0)?;
            if transmit_grouping.is_some() {
                ensure!(
                    same_media_format(initial_profile, session.media_profile()?),
                    "Conference transmit profile changed"
                );
            }
            publish_roster(transport, session, &roster)?;
        }
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    let mut report = thread
        .join()
        .map_err(|_| anyhow::anyhow!("Conference receive worker failed"))?;
    report.ignored_controls = session.ignored_control_count();
    report.control_stopped = control.is_err();
    report.control_failure = control.as_ref().err().map(|error| {
        error
            .downcast_ref::<wechat_protocol::calls::host_transport::CallHostRpcError>()
            .map_or("conference_control_failed", |error| error.code)
    });
    core.store.put_runtime(
        &transport.account,
        &format!("call-conference-receive:{}", transport.call_id),
        &serde_json::to_vec(&report)?,
    )?;
    Ok(())
}

// Both profiles must pass session.take_media first. Channel health policy can
// change without changing the codec, packet grouping or transport flags.
fn same_media_format(initial: [u64; 6], current: [u64; 6]) -> bool {
    initial[..5] == current[..5]
}

fn publish_roster(
    transport: &FencedTransport,
    session: &ConferenceSession,
    roster: &Mutex<RosterSnapshot>,
) -> Result<()> {
    let members = session.members();
    let changed = {
        let mut current = roster
            .lock()
            .map_err(|_| anyhow::anyhow!("Conference roster unavailable"))?;
        if members != current.members {
            current.revision = current
                .revision
                .checked_add(1)
                .context("Conference roster revision exhausted")?;
            current.members = members.clone();
            true
        } else {
            false
        }
    };
    if changed {
        if let ControlAdmission::Owner(sink) = &transport.admission {
            transport.runtime.block_on(
                sink.participants(
                    members
                        .into_iter()
                        .map(|member| CallParticipant {
                            participant_id: format!("member-{}", member.member_id),
                            contact_id: member.username,
                            membership: if member.status == 1 {
                                crate::call_types::CallMembership::Joined
                            } else {
                                crate::call_types::CallMembership::Unknown
                            },
                            microphone_enabled: member.microphone_enabled,
                        })
                        .collect(),
                ),
            )?;
        }
    }
    Ok(())
}

struct Application {
    sink: BackendSink,
    runtime: tokio::runtime::Handle,
    audio: std::sync::mpsc::Receiver<QueuedAudio>,
}
struct ProbeOptions {
    multi_speaker: bool,
    transmit_grouping: Option<u8>,
    application: Option<Application>,
}
fn observe(
    worker: &Worker,
    selected: SelectedRelay,
    roster: &Mutex<RosterSnapshot>,
    stop: &AtomicBool,
    options: ProbeOptions,
) -> ReceiveReport {
    let (application, audio) = if let Some(app) = options.application {
        (Some((app.sink, app.runtime)), Some(app.audio))
    } else {
        (None, None)
    };
    let host = super::super::media::Host {
        worker,
        owner: application.as_ref().map(|(sink, runtime)| (sink, runtime)),
    };
    let options = ConferenceMediaOptions {
        epoch: MediaEpoch {
            account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
            logout_epoch: 0,
            call_generation: nonzero64(worker.identity.call_id().as_bytes()),
            key_generation: 1,
        },
        multi_speaker: options.multi_speaker,
        transmit_grouping: options.transmit_grouping,
    };
    run_conference_media(&host, selected, roster, stop, options, audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wechat_calls::session::ConferenceMember;

    #[test]
    fn admitted_channel_health_updates_preserve_the_running_media_format() {
        let creator = [17, 0x206, 0, 0, 2, 0];
        let joined = [17, 0x206, 0, 0, 2, 3];
        assert!(same_media_format(creator, joined));
        assert!(same_media_format(joined, creator));
        for index in 0..5 {
            let mut changed = creator;
            changed[index] ^= 1;
            assert!(!same_media_format(creator, changed));
        }
    }

    fn epoch() -> MediaEpoch {
        MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        }
    }
    fn selected() -> SelectedRelay {
        SelectedRelay {
            room_id: 42,
            room_key: 77,
            own_member: 0,
            connection_id: 1,
            endpoint: "127.0.0.1:6000".parse().unwrap(),
            media_key: zeroize::Zeroizing::new([7; 32]),
            merge_mode: false,
        }
    }
    fn packet(member: u16) -> Vec<u8> {
        packet_at(member, 0)
    }
    fn packet_at(member: u16, sequence: u16) -> Vec<u8> {
        packet_with_metadata(member, sequence, 0)
    }
    fn packet_with_metadata(member: u16, sequence: u16, metadata: u8) -> Vec<u8> {
        let mut sender = Live96::new(
            [7; 32],
            77,
            MediaContext {
                room_id: 42,
                own_member: member,
                peer_member: if member == 0 { 1 } else { 0 },
                connection_id: 1,
            },
            epoch(),
        )
        .unwrap();
        sender
            .authorize_transmit(
                epoch(),
                TransmitScope::IsolatedCompatibilityTest {
                    intent_id: [8; 32],
                    deadline_ms: 1000,
                    allow_known_audio_overlap: false,
                },
            )
            .unwrap();
        let mut encoder =
            OpusEncoder::new(AudioFormat::new(16000, 1, 20).unwrap(), 20_000).unwrap();
        let encoded = encoder.encode(&[1000; 320]).unwrap();
        let mut packet = Vec::new();
        for index in 0..=sequence {
            packet = sender
                .seal_audio(
                    epoch(),
                    1,
                    &audio_packet::ordinary95_single(
                        metadata,
                        index,
                        u32::from(index) * 320,
                        &encoded,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        packet
    }
    #[test]
    fn authenticated_nonce_reuse_keeps_conference_audio_running_after_mute() {
        let mut receiver = Receiver::new(&selected(), 1, epoch()).unwrap();
        let mut bad = packet(1);
        *bad.last_mut().unwrap() ^= 1;
        receiver.consume(epoch(), &bad, 0, false).unwrap();
        assert_eq!(
            receiver.report.invalid_reasons.get("authentication"),
            Some(&1)
        );
        assert_eq!(receiver.report.invalid_while_inactive, 1);
        receiver.consume(epoch(), &packet(1), 0, true).unwrap();
        receiver
            .consume(epoch(), &packet_with_metadata(1, 0, 9), 0, false)
            .unwrap();
        receiver
            .consume(epoch(), &packet_with_metadata(1, 0, 9), 0, false)
            .unwrap();
        receiver
            .consume(epoch(), &packet_at(1, 1), 20, true)
            .unwrap();
        assert_eq!(
            receiver.report.authenticated_nonce_reuses.get(&0x10),
            Some(&1)
        );
        assert!(!receiver
            .report
            .invalid_reasons
            .contains_key("same_sender_nonce_conflict"));
        assert!(!receiver
            .report
            .invalid_reasons
            .contains_key("closed_or_stale"));
        assert_eq!(receiver.report.replayed_packets, 1);
        assert_eq!(receiver.report.decoded_frames, 2);
    }
    fn member(id: u16, name: &str) -> ConferenceMember {
        ConferenceMember {
            member_id: id,
            username: name.into(),
            status: 1,
            microphone_enabled: None,
        }
    }
    #[test]
    fn microphone_mute_keeps_membership_and_authenticates_without_decoding() {
        let mut receivers = BTreeMap::new();
        let mut unbound = BTreeMap::new();
        let mut remote = member(1, "one");
        remote.microphone_enabled = Some(true);
        reconcile_roster(
            &selected(),
            epoch(),
            &[remote.clone()],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        remote.microphone_enabled = Some(false);
        reconcile_roster(
            &selected(),
            epoch(),
            &[remote.clone()],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        let bound = receivers.get_mut(&1).unwrap();
        assert!(bound.active);
        bound
            .receiver
            .consume(
                epoch(),
                &packet(1),
                0,
                bound.active && bound.microphone_enabled != Some(false),
            )
            .unwrap();
        assert_eq!(bound.receiver.report.authenticated_packets, 1);
        assert_eq!(bound.receiver.report.decoded_frames, 0);
        assert_eq!(bound.receiver.report.left_events, 0);
        remote.microphone_enabled = Some(true);
        reconcile_roster(
            &selected(),
            epoch(),
            &[remote],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        let bound = receivers.get_mut(&1).unwrap();
        bound
            .receiver
            .consume(
                epoch(),
                &packet(1),
                20,
                bound.active && bound.microphone_enabled != Some(false),
            )
            .unwrap();
        bound
            .receiver
            .consume(
                epoch(),
                &packet_at(1, 1),
                40,
                bound.active && bound.microphone_enabled != Some(false),
            )
            .unwrap();
        assert_eq!(bound.receiver.report.replayed_packets, 1);
        assert_eq!(bound.receiver.report.decoded_frames, 1);
        assert_eq!(bound.receiver.report.joined_events, 1);
    }
    #[test]
    fn roster_join_leave_and_rejoin_preserve_other_sources_and_replay_history() {
        let mut receivers = BTreeMap::new();
        let mut unbound = BTreeMap::new();
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(1, "one")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        receivers
            .get_mut(&1)
            .unwrap()
            .receiver
            .consume(epoch(), &packet(1), 0, true)
            .unwrap();
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(1, "one"), member(2, "two")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        receivers
            .get_mut(&2)
            .unwrap()
            .receiver
            .consume(epoch(), &packet(2), 0, true)
            .unwrap();
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(2, "two")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        assert!(!receivers[&1].active);
        assert!(receivers[&2].active);
        receivers
            .get_mut(&2)
            .unwrap()
            .receiver
            .consume(epoch(), &packet_at(2, 1), 1000, true)
            .unwrap();
        assert_eq!(receivers[&2].receiver.report.decoded_frames, 2);
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(1, "one"), member(2, "two")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        let first = receivers.get_mut(&1).unwrap();
        first
            .receiver
            .consume(epoch(), &packet(1), 1000, first.active)
            .unwrap();
        assert_eq!(first.receiver.report.decoded_frames, 1);
        assert_eq!(first.receiver.report.replayed_packets, 1);
        assert_eq!(first.receiver.report.joined_events, 2);
        assert_eq!(first.receiver.report.left_events, 1);
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(1, "different-person"), member(2, "two")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        assert!(!receivers[&1].active);
        assert!(receivers[&1].receiver.report.identity_quarantined);
        assert!(receivers[&2].active);
    }
    #[test]
    fn unbound_packets_never_decode_and_admission_does_not_reset_their_replay_ledger() {
        let mut receivers = BTreeMap::new();
        let mut unbound = BTreeMap::new();
        let mut candidate = Receiver::new(&selected(), 2, epoch()).unwrap();
        candidate.consume(epoch(), &packet(2), 0, false).unwrap();
        assert_eq!(candidate.report.authenticated_packets, 1);
        assert_eq!(candidate.report.decoded_frames, 0);
        unbound.insert(2, candidate);
        reconcile_roster(
            &selected(),
            epoch(),
            &[member(2, "two")],
            &mut receivers,
            &mut unbound,
        )
        .unwrap();
        let bound = receivers.get_mut(&2).unwrap();
        bound
            .receiver
            .consume(epoch(), &packet(2), 1000, bound.active)
            .unwrap();
        assert_eq!(bound.receiver.report.replayed_packets, 1);
        assert_eq!(bound.receiver.report.decoded_frames, 0);
        bound
            .receiver
            .consume(epoch(), &packet_at(2, 1), 1000, bound.active)
            .unwrap();
        assert_eq!(bound.receiver.report.decoded_frames, 1);
    }
    #[test]
    fn relayed_self_header_can_be_classified_without_claiming_remote_audio() {
        let mut self_receiver = Receiver::new(&selected(), 0, epoch()).unwrap();
        self_receiver
            .consume(epoch(), &packet(0), 0, false)
            .unwrap();
        assert_eq!(self_receiver.report.authenticated_packets, 1);
        assert_eq!(self_receiver.report.decoded_frames, 0);
    }
    #[test]
    fn independent_roster_receivers_accept_same_remote_sequence_and_keep_replay_state() {
        let mut first = Receiver::new(&selected(), 1, epoch()).unwrap();
        let mut second = Receiver::new(&selected(), 2, epoch()).unwrap();
        let one = packet(1);
        let two = packet(2);
        first.consume(epoch(), &one, 0, true).unwrap();
        second.consume(epoch(), &two, 0, true).unwrap();
        assert_eq!(first.report.decoded_frames, 1);
        assert_eq!(second.report.decoded_frames, 1);
        first.consume(epoch(), &one, 0, true).unwrap();
        assert_eq!(first.report.replayed_packets, 1);
        assert_eq!(second.report.replayed_packets, 0);
    }
    #[test]
    fn wrong_clear_or_protected_sender_does_not_reach_a_decoder() {
        let mut receiver = Receiver::new(&selected(), 1, epoch()).unwrap();
        receiver.consume(epoch(), &packet(2), 0, true).unwrap();
        let mut changed = packet(2);
        changed[10..12].copy_from_slice(&1u16.to_le_bytes());
        receiver.consume(epoch(), &changed, 0, true).unwrap();
        assert_eq!(receiver.report.decoded_frames, 0);
        assert_eq!(receiver.report.invalid_packets, 2);
    }
}
