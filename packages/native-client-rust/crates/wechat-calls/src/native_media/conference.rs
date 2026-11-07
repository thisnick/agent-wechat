//! Conference receive tracks, one paced uplink and optional generated diagnostics.
use super::{
    connect_relay, reconcile_roster, BoundConferenceReceiver, DirectAudioFrame, DirectMediaHost,
    MediaMetric,
};
use crate::{
    conference_media::{ConferenceReceiver as Receiver, DecodedAudio, StreamReport},
    conference_send::ConferenceSender,
    session::ConferenceMember,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::{SocketAddr, SocketAddrV4, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, MediaEpoch},
    relay::{RelayEpoch, SelectedRelay},
};
#[path = "conference_tone.rs"]
mod tone;

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceMediaReport {
    pub relay_ready: bool,
    pub datagrams: u64,
    pub unselected_source: u64,
    pub unselected_member: u64,
    pub unsupported_envelopes: BTreeMap<u8, u64>,
    pub streams: BTreeMap<u16, StreamReport>,
    pub failure: Option<&'static str>,
    pub control_stopped: bool,
    pub control_failure: Option<&'static str>,
    pub transmits_audio: bool,
    pub sends_control_replies: bool,
    pub ignored_controls: u64,
    pub unattributed: BTreeMap<u16, StreamReport>,
    pub roster_updates: u64,
    pub peak_joined_remote_members: usize,
    pub observation_started_at_ms: Option<u64>,
    pub two_remote_members_observed: bool,
    pub max_sources_in_one_second: usize,
    pub overlapping_signal_second_bins: usize,
    pub second_speaker_wait_expired: bool,
    pub all_remotes_left: bool,
    pub transmit: tone::ToneReport,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConferenceMediaInputDiagnostics {
    pub elapsed_ms: u64,
    pub datagrams: u64,
    pub authenticated_packets: u64,
    pub decoded_frames: u64,
    pub nonzero_frames: u64,
    pub delivered_frames: u64,
    pub invalid_packets: u64,
    pub ordering_drops: u64,
    pub sources: usize,
    pub active_sources: usize,
    pub muted_sources: usize,
    pub finished: bool,
    pub failure: Option<&'static str>,
}

fn input_diagnostics(
    started: Instant,
    report: &ConferenceMediaReport,
    receivers: &BTreeMap<u16, BoundConferenceReceiver>,
    delivered_frames: u64,
    finished: bool,
) -> ConferenceMediaInputDiagnostics {
    ConferenceMediaInputDiagnostics {
        elapsed_ms: started.elapsed().as_millis() as u64,
        datagrams: report.datagrams,
        authenticated_packets: receivers
            .values()
            .map(|r| r.receiver.report.authenticated_packets)
            .sum(),
        decoded_frames: receivers
            .values()
            .map(|r| r.receiver.report.decoded_frames)
            .sum(),
        nonzero_frames: receivers
            .values()
            .map(|r| r.receiver.report.nonzero_frames)
            .sum(),
        delivered_frames,
        invalid_packets: receivers
            .values()
            .map(|r| r.receiver.report.invalid_packets)
            .sum(),
        ordering_drops: receivers
            .values()
            .map(|r| r.receiver.report.ordering_drops)
            .sum(),
        sources: receivers.len(),
        active_sources: receivers.values().filter(|r| r.active).count(),
        muted_sources: receivers
            .values()
            .filter(|r| r.active && r.microphone_enabled == Some(false))
            .count(),
        finished,
        failure: report.failure,
    }
}

pub struct ConferenceRoster {
    pub revision: u64,
    pub members: Vec<ConferenceMember>,
}

#[derive(Clone, Copy)]
pub struct ConferenceMediaOptions {
    pub epoch: MediaEpoch,
    pub multi_speaker: bool,
    pub transmit_grouping: Option<u8>,
}

pub struct ConferenceReceiveTrack {
    pub member: u16,
    pub generation: u32,
}

pub trait ConferenceMediaHost: DirectMediaHost {
    fn input_diagnostics(&self, _diagnostics: ConferenceMediaInputDiagnostics) {}
    fn receive_tracks(&self, tracks: Vec<ConferenceReceiveTrack>) -> Result<()>;
    fn receive_track_pcm(&self, member: u16, generation: u32, frame: DecodedAudio) -> Result<()>;
    fn submit_generation(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddrV4,
        packet: &[u8],
        frames: &[&Self::Frame],
        generation: u32,
    ) -> Result<bool>;
}

pub fn run_conference_media<H: ConferenceMediaHost>(
    host: &H,
    selected: SelectedRelay,
    roster: &Mutex<ConferenceRoster>,
    stop: &AtomicBool,
    options: ConferenceMediaOptions,
    audio: Option<std::sync::mpsc::Receiver<H::Frame>>,
) -> ConferenceMediaReport {
    let mut report = ConferenceMediaReport::default();
    let started = Instant::now();
    let epoch = options.epoch;
    let relay_epoch = RelayEpoch {
        account_generation: epoch.account_generation,
        logout_epoch: epoch.logout_epoch,
        call_generation: epoch.call_generation,
        key_generation: epoch.key_generation,
        socket_generation: 1,
        attempt_id: 1,
    };
    let mut stage = "relay_setup_failed";
    let mut sender: Option<tone::Tone> = None;
    let mut app_sender = None;
    let mut source_permits = Vec::<H::Frame>::new();
    let mut output_generation = host.generation();
    let mut seen_remote = false;
    let mut delivered_frames = 0;
    let mut last_diagnostics = started;
    let mut receivers = BTreeMap::<u16, BoundConferenceReceiver>::new();
    let mut unattributed = BTreeMap::<u16, Receiver>::new();
    let result = (|| -> Result<()> {
        let socket = connect_relay(
            &selected,
            relay_epoch,
            started,
            || host.current() && !stop.load(Ordering::Acquire),
            |socket, endpoint, packet| {
                ensure!(
                    host.submit(socket, endpoint, packet, &[])?,
                    "Call relay handoff stopped"
                );
                Ok(())
            },
        )?;
        report.relay_ready = true;
        stage = "receive_failed";
        let mut application_connected = false;
        let ready_at = Instant::now();
        let mut observation_deadline = if audio.is_some() || options.multi_speaker {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(30))
        };
        if !options.multi_speaker {
            report.observation_started_at_ms = Some(started.elapsed().as_millis() as u64);
        }
        let mut last_revision = None;
        let mut buffer = [0u8; 8198];
        while host.current() && !stop.load(Ordering::Acquire) {
            let mut idle = false;
            {
                // Publishing a roster change and decoding a frame share this
                // short gate. No account/profile or network wait occurs inside it.
                let snapshot = roster
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Conference roster unavailable"))?;
                if last_revision != Some(snapshot.revision) {
                    reconcile_roster(
                        &selected,
                        epoch,
                        &snapshot.members,
                        &mut receivers,
                        &mut unattributed,
                    )?;
                    report.roster_updates += 1;
                    last_revision = Some(snapshot.revision);
                    if audio.is_some() {
                        let tracks = receivers
                            .iter()
                            .filter(|(_, bound)| {
                                bound.active && !bound.receiver.report.identity_quarantined
                            })
                            .map(|(id, bound)| ConferenceReceiveTrack {
                                member: *id,
                                generation: bound.generation,
                            })
                            .collect();
                        host.receive_tracks(tracks)?;
                    }
                }
                let active = receivers.values().filter(|bound| bound.active).count();
                // A creator may have a relay while every invitee is still
                // ringing. Media ownership becomes ready only with a joined
                // remote source, so application PCM cannot fill an idle uplink.
                if active > 0 && !application_connected && audio.is_some() {
                    host.connected()?;
                    application_connected = true;
                }
                report.peak_joined_remote_members = report.peak_joined_remote_members.max(active);
                let joined_remote = snapshot
                    .members
                    .iter()
                    .any(|member| member.status == 1 && member.member_id != selected.own_member);
                if joined_remote {
                    seen_remote = true;
                } else if seen_remote {
                    report.all_remotes_left = true;
                    break;
                }
                if sender.is_none() && audio.is_none() {
                    if let (Some(grouping), Some(reference)) = (
                        options.transmit_grouping,
                        receivers
                            .iter()
                            .find(|(_, bound)| bound.active)
                            .map(|(id, _)| *id),
                    ) {
                        sender = Some(tone::Tone::new(&selected, reference, epoch, grouping)?);
                    }
                }
                if active >= 2 && audio.is_none() {
                    report.two_remote_members_observed = true;
                    if observation_deadline.is_none() {
                        observation_deadline = Some(Instant::now() + Duration::from_secs(45));
                        report.observation_started_at_ms =
                            Some(started.elapsed().as_millis() as u64);
                    }
                }
                if observation_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    break;
                }
                if audio.is_none()
                    && observation_deadline.is_none()
                    && ready_at.elapsed() >= Duration::from_secs(45)
                {
                    report.second_speaker_wait_expired = true;
                    break;
                }
                if let Some(tone) = sender.as_mut() {
                    stage = "transmit_failed";
                    let microphone_enabled = snapshot
                        .members
                        .iter()
                        .find(|member| member.member_id == selected.own_member)
                        .is_none_or(|member| member.microphone_enabled != Some(false));
                    if let Some(packet) = tone.poll(ready_at.elapsed(), microphone_enabled)? {
                        if host.submit(&socket, selected.endpoint, &packet, &[])? {
                            tone.report.submitted_datagrams += 1;
                        }
                    }
                    stage = "receive_failed";
                }
                if let Some(audio) = &audio {
                    if app_sender.is_none() {
                        if let Some(reference) = receivers
                            .iter()
                            .find(|(_, bound)| bound.active)
                            .map(|(id, _)| *id)
                        {
                            app_sender = Some(ConferenceSender::new(
                                &selected,
                                reference,
                                epoch,
                                options
                                    .transmit_grouping
                                    .context("Conference grouping missing")?,
                                ready_at.elapsed(),
                            )?);
                        }
                    }
                    if let Some(sender) = &mut app_sender {
                        stage = "transmit_failed";
                        let generation = host.generation();
                        if generation != output_generation {
                            source_permits.clear();
                            output_generation = generation;
                        }
                        let microphone = snapshot
                            .members
                            .iter()
                            .find(|member| member.member_id == selected.own_member)
                            .is_none_or(|member| member.microphone_enabled != Some(false));
                        let packet =
                            sender.poll(ready_at.elapsed(), generation, microphone, || loop {
                                match audio.try_recv() {
                                    Ok(item) if host.frame_current(&item) => {
                                        let samples = item.samples().to_vec();
                                        source_permits.push(item);
                                        break Some(samples);
                                    }
                                    Ok(_) => {
                                        host.record(MediaMetric::StaleFrames, 1);
                                    }
                                    Err(_) => break None,
                                }
                            })?;
                        if let Some(packet) = packet {
                            let permits = source_permits.iter().collect::<Vec<_>>();
                            let submitted = host.submit_generation(
                                &socket,
                                selected.endpoint,
                                &packet.packet,
                                &permits,
                                packet.generation,
                            )?;
                            report.transmit.submitted_datagrams += u64::from(submitted);
                            source_permits.clear();
                        }
                    }
                }
                stage = "receive_failed";
                match socket.recv_from(&mut buffer) {
                    Ok((n, SocketAddr::V4(source))) if source == selected.endpoint && n <= 8197 => {
                        report.datagrams += 1;
                        let member = match audio_packet::unverified_live96_member(&buffer[..n]) {
                            Ok(member) => member,
                            Err(_) => {
                                if n != 0 {
                                    *report.unsupported_envelopes.entry(buffer[0]).or_default() +=
                                        1;
                                }
                                continue;
                            }
                        };
                        let elapsed = started.elapsed().as_millis() as u64;
                        if let Some(bound) = receivers.get_mut(&member) {
                            ensure!(
                                host.current() && !stop.load(Ordering::Acquire),
                                "Conference receive admission revoked"
                            );
                            let frames = bound.receiver.consume(
                                epoch,
                                &buffer[..n],
                                elapsed,
                                bound.active && bound.microphone_enabled != Some(false),
                            )?;
                            if audio.is_some() {
                                for frame in frames {
                                    host.receive_track_pcm(member, bound.generation, frame)?;
                                    delivered_frames += 1;
                                }
                            }
                        } else {
                            report.unselected_member += 1;
                            if !unattributed.contains_key(&member) && unattributed.len() < 8 {
                                unattributed
                                    .insert(member, Receiver::new(&selected, member, epoch)?);
                            }
                            if let Some(receiver) = unattributed.get_mut(&member) {
                                receiver.consume(epoch, &buffer[..n], elapsed, false)?;
                            }
                        }
                    }
                    Ok(_) => report.unselected_source += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => idle = true,
                    Err(error) => return Err(error.into()),
                }
            }
            if last_diagnostics.elapsed() >= Duration::from_secs(10) {
                host.input_diagnostics(input_diagnostics(
                    started,
                    &report,
                    &receivers,
                    delivered_frames,
                    false,
                ));
                last_diagnostics = Instant::now();
            }
            if idle {
                thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    })();
    if result.is_err() {
        report.failure = Some(stage);
    }
    host.input_diagnostics(input_diagnostics(
        started,
        &report,
        &receivers,
        delivered_frames,
        true,
    ));
    let mut signal_sources = BTreeMap::<u16, usize>::new();
    for (member, mut bound) in receivers {
        bound.receiver.close();
        for (bin, frames) in &bound.receiver.report.nonzero_second_bins {
            if *frames > 0 {
                *signal_sources.entry(*bin).or_default() += 1;
            }
        }
        report
            .streams
            .insert(member, std::mem::take(&mut bound.receiver.report));
    }
    for (member, mut receiver) in unattributed {
        receiver.close();
        report
            .unattributed
            .insert(member, std::mem::take(&mut receiver.report));
    }
    if let Some(tone) = sender {
        report.transmit = tone.finish();
    }
    report.transmits_audio = report.transmit.submitted_datagrams > 0;
    report.max_sources_in_one_second = signal_sources.values().copied().max().unwrap_or(0);
    report.overlapping_signal_second_bins =
        signal_sources.values().filter(|count| **count >= 2).count();
    report
}
