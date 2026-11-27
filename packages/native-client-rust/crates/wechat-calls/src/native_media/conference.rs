//! Conference receive tracks, one paced uplink and optional generated diagnostics.
use super::{
    connect_relay_with_early, reconcile_roster, BoundConferenceReceiver, DirectAudioFrame,
    DirectMediaHost, DirectVideoTx, MediaMetric, TxAudioJitterBuffer,
};
use crate::{
    conference_media::{ConferenceReceiver as Receiver, DecodedAudio, StreamReport},
    conference_send::ConferenceSender,
    session::ConferenceMember,
    video_stream::{VideoTxPacer, VideoTxSendAction},
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaEpoch},
    relay::{RelayEpoch, SelectedRelay},
    rudp,
    video_packet::{
        annexb_nalu_types, encode_p2s_report, encode_video_nack, parse_s2p, VideoS2pCommand,
        P2S_CODEC_SUPPORT_H264_H265,
    },
    video_qos::KeyframeRequestReason,
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

pub struct ConferenceRoster {
    pub revision: u64,
    pub members: Vec<ConferenceMember>,
}

#[derive(Clone, Copy)]
pub struct ConferenceMediaOptions {
    pub epoch: MediaEpoch,
    pub multi_speaker: bool,
    pub transmit_grouping: Option<u8>,
    pub video_enabled: bool,
}

pub struct ConferenceReceiveTrack {
    pub member: u16,
    pub generation: u32,
}

pub trait ConferenceMediaHost: DirectMediaHost {
    fn receive_tracks(&self, tracks: Vec<ConferenceReceiveTrack>) -> Result<()>;
    fn receive_track_pcm(&self, member: u16, generation: u32, frame: DecodedAudio) -> Result<()>;
    fn submit_generation(
        &self,
        socket: &UdpSocket,
        endpoint: SocketAddr,
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
    run_conference_media_with_video(host, selected, roster, stop, options, audio, None)
}

/// Run conference audio + optional video media transport over the negotiated relay.
pub fn run_conference_media_with_video<H: ConferenceMediaHost>(
    host: &H,
    selected: SelectedRelay,
    roster: &Mutex<ConferenceRoster>,
    stop: &AtomicBool,
    options: ConferenceMediaOptions,
    audio: Option<std::sync::mpsc::Receiver<H::Frame>>,
    video: Option<std::sync::mpsc::Receiver<DirectVideoTx>>,
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
    let mut tx_audio_buf = TxAudioJitterBuffer::<H::Frame>::new();
    let mut source_permits = Vec::<H::Frame>::new();
    let mut output_generation = host.generation();
    let mut seen_remote = false;
    let mut receivers = BTreeMap::<u16, BoundConferenceReceiver>::new();
    let mut unattributed = BTreeMap::<u16, Receiver>::new();
    let mut last_p2s_tx: Option<Instant> = None;
    let mut p2s_seq = 1u16;
    let video_enabled = options.video_enabled || video.is_some();
    let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
    let mut trace_last_s2p: Option<VideoS2pCommand> = None;
    let mut trace_frames_ok = 0u64;
    let mut trace_kf_logged = 0u32;
    let mut trace_drop_logged = 0u32;
    let mut trace_fec_logged = 0u32;
    let mut trace_nack_tx_logged = 0u32;
    let mut trace_rudp_acks = 0u32;
    let mut trace_last = Instant::now();
    let mut trace_total_fec = 0u64;
    let mut trace_total_drops = 0u64;
    let result = (|| -> Result<()> {
        let mut relay_qos = Live96::new_relay_qos(
            *selected.media_key,
            selected.room_key,
            selected.room_id,
            selected.own_member,
            selected.connection_id,
            epoch,
        )?;
        let (socket, mut early_packets) = connect_relay_with_early(
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
        let mut video_tx = VideoTxPacer::new(selected.own_member, 1);
        let mut observation_deadline = if audio.is_some() || options.multi_speaker {
            None
        } else {
            Some(Instant::now() + Duration::from_secs(30))
        };
        if !options.multi_speaker {
            report.observation_started_at_ms = Some(started.elapsed().as_millis() as u64);
        }
        let mut last_revision = None;
        let mut cached_members = Vec::<ConferenceMember>::new();
        let mut buffer = [0u8; 8198];
        while host.current() && !stop.load(Ordering::Acquire) {
            let mut idle = false;
            // Hold the roster lock only long enough to detect a new revision and
            // clone the member list. Never hold it across media encode/decode or
            // UDP recv loops, or continuous inbound traffic starves publish_roster.
            let roster_changed = {
                let snapshot = roster
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Conference roster unavailable"))?;
                if last_revision != Some(snapshot.revision) {
                    cached_members = snapshot.members.clone();
                    last_revision = Some(snapshot.revision);
                    true
                } else {
                    false
                }
            };
            if roster_changed {
                reconcile_roster(
                    &selected,
                    epoch,
                    &cached_members,
                    &mut receivers,
                    &mut unattributed,
                )?;
                let anchor_ms = started.elapsed().as_millis() as u64;
                for bound in receivers.values_mut() {
                    if bound.active {
                        bound.receiver.anchor_video_clock(anchor_ms);
                    }
                }
                report.roster_updates += 1;
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
            let joined_remote = cached_members
                .iter()
                .any(|member| member.status == 1 && member.member_id != selected.own_member);
            let pending_remote = cached_members.iter().any(|member| {
                matches!(member.status, 2 | 3) && member.member_id != selected.own_member
            });
            if joined_remote {
                seen_remote = true;
            } else if seen_remote && !pending_remote {
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
                    report.observation_started_at_ms = Some(started.elapsed().as_millis() as u64);
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
                let microphone_enabled = cached_members
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
                        let mut conf_sender = ConferenceSender::new(
                            &selected,
                            reference,
                            epoch,
                            options
                                .transmit_grouping
                                .context("Conference grouping missing")?,
                            ready_at.elapsed(),
                        )?;
                        conf_sender.set_prefer_live98_tx(host.prefer_live98_tx());
                        app_sender = Some(conf_sender);
                    }
                }
                if let Some(sender) = &mut app_sender {
                    stage = "transmit_failed";
                    let generation = host.generation();
                    if generation != output_generation {
                        source_permits.clear();
                        tx_audio_buf.clear();
                        output_generation = generation;
                    }
                    let now = Instant::now();
                    tx_audio_buf.drain_from(
                        audio,
                        now,
                        |item| host.frame_current(item),
                        || host.record(MediaMetric::StaleFrames, 1),
                    );
                    let microphone = cached_members
                        .iter()
                        .find(|member| member.member_id == selected.own_member)
                        .is_none_or(|member| member.microphone_enabled != Some(false));
                    let packet = sender.poll(ready_at.elapsed(), generation, microphone, || {
                        tx_audio_buf
                            .pop(
                                now,
                                |item| host.frame_current(item),
                                || host.record(MediaMetric::StaleFrames, 1),
                            )
                            .map(|item| {
                                let samples = item.samples().to_vec();
                                source_permits.push(item);
                                samples
                            })
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
                    let any_rx_video = receivers
                        .values()
                        .any(|b| b.active && b.receiver.last_video_stream_id().is_some());
                    if video_enabled || any_rx_video {
                        let video_now = Instant::now();
                        let now_ms = started.elapsed().as_millis() as u64;
                        while let Some(member_id) = host.take_local_keyframe_request() {
                            if let Some(bound) = receivers.get_mut(&member_id) {
                                bound.receiver.queue_local_keyframe_request(3);
                            }
                        }
                        for (&member_id, bound) in &mut receivers {
                            if !bound.active {
                                continue;
                            }
                            if let Some((arq_seq, ranges)) = bound.receiver.poll_due_nack(now_ms) {
                                let stream_id = bound.receiver.last_video_stream_id().unwrap_or(1);
                                let nack = encode_video_nack(
                                    selected.own_member,
                                    member_id,
                                    stream_id,
                                    &ranges,
                                );
                                if let Ok(datagram) =
                                    sender.seal_video_nack(ready_at.elapsed(), stream_id, &nack)
                                {
                                    let _ = host.submit(&socket, selected.endpoint, &datagram, &[]);
                                    if video_trace && trace_nack_tx_logged < 64 {
                                        trace_nack_tx_logged += 1;
                                        eprintln!(
                                            "[wechat-call-video] conf tx nack #{trace_nack_tx_logged} seq={arq_seq} stream={stream_id} target={member_id} ranges={ranges:?}"
                                        );
                                    }
                                }
                            }
                            for req in bound.receiver.poll_due_irequests(
                                selected.own_member,
                                member_id,
                                now_ms,
                            ) {
                                let Ok(datagram) = sender.seal_video_rtcp(
                                    ready_at.elapsed(),
                                    req.stream_id,
                                    &req.payload,
                                ) else {
                                    break;
                                };
                                let _ = host.submit(&socket, selected.endpoint, &datagram, &[]);
                                if video_trace && !req.is_stream0_fallback {
                                    let reason = match req.reason {
                                        KeyframeRequestReason::FrameDrop => "drop",
                                        KeyframeRequestReason::LocalRequest => "local",
                                        KeyframeRequestReason::InitialSync => "initial",
                                    };
                                    eprintln!(
                                        "[wechat-call-video] conf tx ireq reason={reason} stream={} target={member_id} lost_seq={} force={}",
                                        req.stream_id, req.lost_point_seq, req.force
                                    );
                                }
                            }
                        }
                        if video_enabled {
                            let awaiting_sync = receivers.values().any(|b| {
                                b.active
                                    && b.receiver.has_active_video_window()
                                    && !b.receiver.is_video_keyframe_synced()
                            });
                            let p2s_interval = if awaiting_sync {
                                Duration::from_millis(550)
                            } else {
                                Duration::from_secs(1)
                            };
                            if last_p2s_tx.is_none_or(|t| t.elapsed() >= p2s_interval) {
                                // Snapshot every active bound receiver so paused/camera-off streams
                                // clear stale gap anchors and multi-sender streams roll their 1s
                                // windows evenly without starving higher member IDs.
                                let mut active_reports = Vec::new();
                                for (&mid, b) in &mut receivers {
                                    if !b.active {
                                        continue;
                                    }
                                    let was_active = b.receiver.has_active_video_window();
                                    let synced = b.receiver.is_video_keyframe_synced();
                                    let rep = b.receiver.snapshot_video_p2s(
                                        mid,
                                        u32::from(selected.connection_id),
                                        P2S_CODEC_SUPPORT_H264_H265,
                                        now_ms,
                                    );
                                    if was_active {
                                        active_reports.push((synced, rep));
                                    }
                                }
                                let unsynced_reports: Vec<
                                    &wechat_protocol::calls::video_packet::VideoP2sReport,
                                > = active_reports
                                    .iter()
                                    .filter(|(synced, _)| !*synced)
                                    .map(|(_, rep)| rep)
                                    .collect();
                                let selected_rep = if !unsynced_reports.is_empty() {
                                    Some(
                                        unsynced_reports
                                            [usize::from(p2s_seq) % unsynced_reports.len()],
                                    )
                                } else if !active_reports.is_empty() {
                                    Some(
                                        &active_reports
                                            [usize::from(p2s_seq) % active_reports.len()]
                                        .1,
                                    )
                                } else {
                                    None
                                };
                                // Report only a member whose video arrived in this interval, as
                                // direct calls do: a report without one would name no real
                                // sender and offer codecs that sender may not encode.
                                let encoded = match selected_rep {
                                    Some(p2s_rep) => {
                                        encode_p2s_report(p2s_seq, 2, selected.own_member, p2s_rep)
                                    }
                                    None => {
                                        last_p2s_tx = Some(video_now);
                                        Err(anyhow::anyhow!("no member video in this interval"))
                                    }
                                };
                                if let Ok(p2s) = encoded {
                                    p2s_seq = p2s_seq.wrapping_add(1);
                                    if let Ok(datagram) =
                                        sender.seal_qos_p2s(ready_at.elapsed(), &p2s)
                                    {
                                        let _ =
                                            host.submit(&socket, selected.endpoint, &datagram, &[]);
                                        last_p2s_tx = Some(video_now);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(video_rx_ch) = &video {
                        let now_ms = started.elapsed().as_millis() as u64;
                        video_tx.drain_and_pace(
                            video_rx_ch,
                            host.video_generation(),
                            now_ms,
                            4,
                            |_| {},
                            |pkt_gen, pkt| {
                                if !host.current() || host.video_generation() != pkt_gen {
                                    return Ok(VideoTxSendAction::StaleGeneration);
                                }
                                match sender.seal_video(ready_at.elapsed(), 1, true, pkt) {
                                    Ok(datagram) => {
                                        let _ = host.submit(
                                            &socket,
                                            selected.endpoint,
                                            &datagram,
                                            &[],
                                        )?;
                                        Ok(VideoTxSendAction::Sent)
                                    }
                                    Err(_) => Ok(VideoTxSendAction::SequenceExhausted),
                                }
                            },
                        )?;
                    }
                }
            }
            stage = "receive_failed";
            let mut drained = 0usize;
            while drained < 64 {
                let recv_outcome = if let Some(early) = early_packets.pop_front() {
                    let n = early.len().min(buffer.len());
                    buffer[..n].copy_from_slice(&early[..n]);
                    Ok((n, selected.endpoint))
                } else {
                    socket.recv_from(&mut buffer)
                };
                match recv_outcome {
                    Ok((n, source)) if source == selected.endpoint && n <= 8197 => {
                        drained += 1;
                        report.datagrams += 1;
                        let member = match audio_packet::unverified_live_member(&buffer[..n]) {
                            Ok((_, member)) => member,
                            Err(_) => {
                                if n != 0 {
                                    *report.unsupported_envelopes.entry(buffer[0]).or_default() +=
                                        1;
                                }
                                continue;
                            }
                        };
                        if member == 0xffff {
                            if let Ok(Some(opened)) = relay_qos.open(epoch, &buffer[..n]) {
                                if opened.packed_kind == 0x30 {
                                    if let Ok(s2p) = parse_s2p(opened.payload()) {
                                        if video_trace {
                                            for &c in &s2p.video_commands {
                                                if trace_last_s2p != Some(c)
                                                    || c.i_request_flag != 0
                                                {
                                                    trace_last_s2p = Some(c);
                                                    eprintln!(
                                                        "[wechat-call-video] conf rx s2p alg={} kbps={} fps={} res={} i_period={} rs={} ireq={} codec={} stream={}",
                                                        c.alg,
                                                        c.kbps,
                                                        c.fps,
                                                        c.resolution,
                                                        c.i_period,
                                                        c.rs_level,
                                                        c.i_request_flag,
                                                        c.codec_type,
                                                        c.stream_id
                                                    );
                                                }
                                            }
                                        }
                                        if s2p.video_commands.iter().any(|c| c.i_request_flag != 0)
                                        {
                                            host.on_peer_keyframe_requested();
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        let elapsed = started.elapsed().as_millis() as u64;
                        if let Some(bound) = receivers.get_mut(&member) {
                            ensure!(
                                host.current() && !stop.load(Ordering::Acquire),
                                "Conference receive admission revoked"
                            );
                            let consumed = bound.receiver.consume_media(
                                epoch,
                                &buffer[..n],
                                elapsed,
                                bound.active && bound.microphone_enabled != Some(false),
                            )?;
                            if consumed.peer_requested_keyframe {
                                host.on_peer_keyframe_requested();
                            }
                            // Acknowledgements need the member-keyed sender. Data that arrives
                            // before it exists is acknowledged when the member retransmits it.
                            if let (Some((stream_id, max_pkt)), Some(sender)) =
                                (consumed.rudp_ack, app_sender.as_mut())
                            {
                                if let Ok(feedback) = rudp::encode_rudp_feedback(
                                    super::relay::epoch_ms32(),
                                    stream_id,
                                    max_pkt,
                                    member,
                                ) {
                                    if let Ok(Some(datagram)) =
                                        sender.seal_rudp(ready_at.elapsed(), &feedback)
                                    {
                                        // Best effort, like NACK and P2S: a failed send
                                        // is retried by the member's retransmission.
                                        let sent = matches!(
                                            host.submit(
                                                &socket,
                                                selected.endpoint,
                                                &datagram,
                                                &[],
                                            ),
                                            Ok(true)
                                        );
                                        if video_trace && sent && trace_rudp_acks < 16 {
                                            trace_rudp_acks += 1;
                                            eprintln!(
                                                "[wechat-call-rudp] conf tx ack member={member} stream={stream_id} max_pkt={max_pkt}"
                                            );
                                        }
                                    }
                                }
                            }
                            if audio.is_some() {
                                for frame in consumed.audio {
                                    host.receive_track_pcm(member, bound.generation, frame)?;
                                }
                            }
                            let mut ready_frames = Vec::new();
                            if let Some(video_frame) = consumed.video {
                                ready_frames.push(video_frame);
                            }
                            while let Some(video_frame) = bound.receiver.pop_ready_video() {
                                ready_frames.push(video_frame);
                            }
                            for video_frame in ready_frames {
                                trace_frames_ok += 1;
                                if video_frame.fec_recovered {
                                    trace_total_fec += 1;
                                }
                                if video_frame.prior_frames_dropped {
                                    trace_total_drops += 1;
                                }
                                if video_trace
                                    && (trace_frames_ok <= 15
                                        || (video_frame.is_keyframe && trace_kf_logged < 32)
                                        || (video_frame.fec_recovered && trace_fec_logged < 16)
                                        || (video_frame.prior_frames_dropped
                                            && trace_drop_logged < 24))
                                {
                                    if video_frame.is_keyframe {
                                        trace_kf_logged = trace_kf_logged.saturating_add(1);
                                    }
                                    if video_frame.fec_recovered {
                                        trace_fec_logged = trace_fec_logged.saturating_add(1);
                                    }
                                    if video_frame.prior_frames_dropped {
                                        trace_drop_logged = trace_drop_logged.saturating_add(1);
                                    }
                                    let nalus =
                                        annexb_nalu_types(video_frame.codec, &video_frame.payload);
                                    eprintln!(
                                        "[wechat-call-video] conf rx frame #{trace_frames_ok} member={member} fidx={} ts={} codec={:?} stream={} kf={} cfg={} fec={} drop={} orient={} bytes={} nalus={nalus:?}",
                                        video_frame.frame_index,
                                        video_frame.timestamp_ms,
                                        video_frame.codec,
                                        video_frame.stream_id,
                                        video_frame.is_keyframe,
                                        video_frame.has_codec_config,
                                        video_frame.fec_recovered,
                                        video_frame.prior_frames_dropped,
                                        video_frame.orientation,
                                        video_frame.payload.len()
                                    );
                                }
                                host.receive_video(video_frame)?;
                            }
                            if video_trace
                                && trace_frames_ok > 0
                                && trace_last.elapsed() >= Duration::from_secs(2)
                            {
                                eprintln!(
                                    "[wechat-call-video] conf rx_summary frames={trace_frames_ok} fec={trace_total_fec} drops={trace_total_drops}"
                                );
                                trace_last = Instant::now();
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
                    Ok(_) => {
                        drained += 1;
                        report.unselected_source += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if drained == 0 {
                            idle = true;
                        }
                        break;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            if idle {
                let sleep_ms = if video_tx.has_pending_packets() { 2 } else { 5 };
                thread::sleep(Duration::from_millis(sleep_ms));
            }
        }
        relay_qos.close();
        Ok(())
    })();
    if result.is_err() {
        report.failure = Some(stage);
    }
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
