//! Direct voice and video transport, codec state, packet ordering and pacing.
use super::{
    connect_relay_with_early, DirectAudioFrame, DirectMediaHost, DirectMediaOptions, DirectVideoTx,
    MediaMetric, TxAudioJitterBuffer, TxVideoQueue,
};
use crate::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
    ordering::RtpOrder,
    playout::FramePacer,
    session::NegotiatedMedia,
};
use anyhow::{ensure, Result};
use std::{
    collections::VecDeque,
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, TransmitScope},
    relay::RelayEpoch,
    video_packet::{
        classify_video_channel_payload, encode_irequest_with_force, encode_p2s, parse_s2p,
        RsmIngestOutcome, RsmVideoPacketizer, RsmVideoReassembler, VideoChannelPayload,
    },
};

pub fn run_direct_media<H: DirectMediaHost>(
    host: &H,
    options: DirectMediaOptions,
    media: NegotiatedMedia,
    audio: Receiver<H::Frame>,
) -> Result<()> {
    run_direct_media_with_video(host, options, media, audio, None)
}

/// Run direct voice + optional video transport over the negotiated UDP relay.
pub fn run_direct_media_with_video<H: DirectMediaHost>(
    host: &H,
    options: DirectMediaOptions,
    media: NegotiatedMedia,
    audio: Receiver<H::Frame>,
    video: Option<Receiver<DirectVideoTx>>,
) -> Result<()> {
    let started = Instant::now();
    ensure!(
        matches!(media.frames_per_packet, 1 | 2),
        "Unsupported call packet grouping"
    );
    let clock = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let epoch = options.epoch;
    let relay_epoch = RelayEpoch {
        account_generation: epoch.account_generation,
        logout_epoch: epoch.logout_epoch,
        call_generation: epoch.call_generation,
        key_generation: epoch.key_generation,
        socket_generation: 1,
        attempt_id: 1,
    };
    let selected = media.relay;
    let endpoint = selected.endpoint;
    let (socket, mut early_packets) = connect_relay_with_early(
        &selected,
        relay_epoch,
        started,
        || host.current(),
        |socket, endpoint, packet| {
            ensure!(
                host.submit(socket, endpoint, packet, &[])?,
                "Call relay handoff stopped"
            );
            Ok(())
        },
    )?;
    let mut buffer = [0u8; 8198];
    let mut channel = Live96::new(
        *selected.media_key,
        selected.room_key,
        MediaContext {
            room_id: selected.room_id,
            own_member: selected.own_member,
            peer_member: media.peer_member,
            connection_id: selected.connection_id,
        },
        epoch,
    )?;
    channel.authorize_transmit(
        epoch,
        TransmitScope::LegacyRelayAudio {
            key_generation: epoch.key_generation,
        },
    )?;
    channel.set_prefer_live98_tx(host.prefer_live98_tx());
    let mut relay_qos = Live96::new_relay_qos(
        *selected.media_key,
        selected.room_key,
        selected.room_id,
        selected.own_member,
        selected.connection_id,
        epoch,
    )?;
    let format = AudioFormat::new(16000, 1, 20)?;
    let mut decoder = OpusDecoder::new(format)?;
    let mut encoder = OpusEncoder::new(format, 20_000)?;
    ensure!(host.current(), "Call stopped before transport admission");
    // Current joined peer AV + selected codec were admitted by take_media;
    // Ready establishes this transport. Peer speech is not a prerequisite for
    // transmitting; actual decoded/heard audio remains a separate observation.
    host.connected()?;
    let probe = channel.seal_probe_init(epoch, clock())?;
    ensure!(
        host.submit(&socket, endpoint, &probe, &[])?,
        "Call stopped during initial probe"
    );
    let mut encoder_generation = host.generation();
    let mut pacer = FramePacer::new(format, started.elapsed());
    let mut tx_audio_buf = TxAudioJitterBuffer::<H::Frame>::new();
    let mut pending: Option<(Vec<u8>, H::Frame)> = None;
    let mut rtp_sequence = 0u32;
    let mut rtp_timestamp = 0u64;
    let mut rx_order = RtpOrder::new();
    let mut rx_timeline = DirectRxAudioTimeline::new();
    let mut video_rx = RsmVideoReassembler::new();
    // Outgoing wire frames are pre-rotated 90° CCW to match mobile WeChat receivers,
    // which apply +90° CW display rotation; orientation = 1 signals +90° CW for desktop/native peers.
    let mut video_tx = RsmVideoPacketizer::new(selected.own_member, 1).with_orientation(1);
    let mut tx_video_queue = TxVideoQueue::new();
    let mut tx_video_packets = VecDeque::<(u32, Vec<u8>)>::new();
    let mut video_tx_exhausted = false;
    let mut last_vf_tx: Option<Instant> = None;
    let mut last_irequest_tx: Option<Instant> = None;
    let mut pending_local_irequest: Option<(u16, u8)> = None;
    let mut irequest_streak = 0u32;
    let mut last_rx_video_stream_id = 1u8;
    let mut last_rx_total_pkg_seq: Option<u64> = None;
    let mut rx_video_seen = false;
    let mut initial_irequests_sent = 0u8;
    let connected_at = Instant::now();
    let mut last_p2s_tx: Option<Instant> = None;
    let mut p2s_seq = 1u16;
    let mut last_peer_activity = Instant::now();
    let video_enabled = media.video_enabled || video.is_some();
    let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
    let mut trace_last = Instant::now();
    let mut trace_rx_pkts = 0u64;
    let mut trace_frames_ok = 0u64;
    let mut trace_fec_ok = 0u64;
    let mut trace_drops = 0u64;
    let mut trace_last_orient = 0u8;
    let mut trace_vf_logged = false;
    let mut trace_kf_logged = 0u32;
    let media_result = (|| -> Result<()> {
        while host.current() {
            ensure!(
                last_peer_activity.elapsed() < options.peer_idle_timeout,
                "Call peer activity timed out"
            );
            // Drain multiple ready UDP datagrams per loop tick so video bursts
            // do not fall behind while still keeping audio pacing tight.
            let mut drained = 0usize;
            while drained < 64 {
                let recv_outcome = if let Some(early) = early_packets.pop_front() {
                    let n = early.len().min(buffer.len());
                    buffer[..n].copy_from_slice(&early[..n]);
                    Ok((n, endpoint))
                } else {
                    socket.recv_from(&mut buffer)
                };
                match recv_outcome {
                    Ok((n, source)) if source == endpoint && n <= 8197 => {
                        drained += 1;
                        let Ok((_magic, member)) =
                            audio_packet::unverified_live_member(&buffer[..n])
                        else {
                            continue;
                        };
                        if member == 0xffff {
                            if let Ok(Some(opened)) = relay_qos.open(epoch, &buffer[..n]) {
                                if opened.packed_kind == 0x30 {
                                    if let Ok(s2p) = parse_s2p(opened.payload()) {
                                        if s2p.video_commands.iter().any(|c| c.i_request_flag != 0)
                                        {
                                            host.on_peer_keyframe_requested();
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                        let opened = match channel.open(epoch, &buffer[..n]) {
                            Ok(v) => v,
                            Err(e) => {
                                if e.to_string().contains("nonce")
                                    || e.to_string().contains("generation")
                                {
                                    return Err(e);
                                }
                                None
                            }
                        };
                        if let Some(opened) = opened {
                            // Only a fresh, authenticated packet from the selected
                            // peer renews liveness. Relay traffic and bad/replayed
                            // packets do not establish that the peer is still here.
                            last_peer_activity = Instant::now();
                            if opened.packed_kind == 0x70 {
                                if let Some(reply) =
                                    channel.reply_to_probe(epoch, clock(), &opened)?
                                {
                                    host.submit(&socket, endpoint, &reply, &[])?;
                                }
                            } else if (opened.packed_kind & 0xf0) == 0x20 {
                                if let Ok(kind) = classify_video_channel_payload(opened.payload()) {
                                    match kind {
                                        VideoChannelPayload::IRequest(_) => {
                                            host.on_peer_keyframe_requested();
                                        }
                                        VideoChannelPayload::Nack(_) => {}
                                        VideoChannelPayload::RsmVideo(raw) => {
                                            last_rx_video_stream_id = opened.packed_kind & 0x0f;
                                            last_rx_total_pkg_seq = advance_rx_total_pkg_seq(
                                                last_rx_total_pkg_seq,
                                                raw,
                                            );
                                            trace_rx_pkts += 1;
                                            if video_trace
                                                && !trace_vf_logged
                                                && raw.len() >= 24
                                                && (raw[4] >> 2) == 3
                                                && (raw[4] & 0x03) == 0
                                            {
                                                trace_vf_logged = true;
                                                eprintln!(
                                                    "[wechat-call-video] rx vf ext_byte={} stream={} hdr={:02x?}",
                                                    opened.extension_byte(),
                                                    last_rx_video_stream_id,
                                                    &raw[..raw.len().min(32)]
                                                );
                                            }
                                            let mut need_keyframe = false;
                                            match video_rx.ingest(raw) {
                                                Ok(RsmIngestOutcome::Frame(frame)) => {
                                                    ensure!(
                                                        host.current(),
                                                        "Call authority revoked before video delivery"
                                                    );
                                                    last_rx_video_stream_id = frame.stream_id;
                                                    rx_video_seen = true;
                                                    trace_frames_ok += 1;
                                                    if frame.fec_recovered {
                                                        trace_fec_ok += 1;
                                                    }
                                                    if frame.prior_frames_dropped {
                                                        trace_drops += 1;
                                                    }
                                                    trace_last_orient = frame.orientation;
                                                    if video_trace
                                                        && (trace_frames_ok <= 3
                                                            || (frame.is_keyframe
                                                                && trace_kf_logged < 16))
                                                    {
                                                        if frame.is_keyframe {
                                                            trace_kf_logged =
                                                                trace_kf_logged.saturating_add(1);
                                                        }
                                                        eprintln!(
                                                            "[wechat-call-video] rx frame #{trace_frames_ok} codec={:?} stream={} kf={} cfg={} orient={} bytes={}",
                                                            frame.codec,
                                                            frame.stream_id,
                                                            frame.is_keyframe,
                                                            frame.has_codec_config,
                                                            frame.orientation,
                                                            frame.payload.len()
                                                        );
                                                    }
                                                    if frame.prior_frames_dropped
                                                        && !frame.is_keyframe
                                                    {
                                                        need_keyframe = true;
                                                    } else if frame.is_keyframe
                                                        && frame.has_codec_config
                                                    {
                                                        // A self-contained recovery IDR satisfied any
                                                        // pending keyframe request from earlier drops.
                                                        pending_local_irequest = None;
                                                        irequest_streak = 0;
                                                        let _ = host.take_local_keyframe_request();
                                                    }
                                                    host.receive_video(frame)?;
                                                }
                                                Ok(RsmIngestOutcome::FrameDroppedNeedKeyframe)
                                                | Err(_) => {
                                                    trace_drops += 1;
                                                    need_keyframe = true;
                                                }
                                                Ok(RsmIngestOutcome::Pending) => {}
                                            }
                                            if video_trace
                                                && trace_last.elapsed() >= Duration::from_secs(2)
                                            {
                                                eprintln!(
                                                    "[wechat-call-video] rx_pkts={trace_rx_pkts} frames={trace_frames_ok} fec={trace_fec_ok} drops={trace_drops} orient={trace_last_orient}"
                                                );
                                                trace_last = Instant::now();
                                            }
                                            if need_keyframe
                                                && last_irequest_tx.is_none_or(|t| {
                                                    t.elapsed() >= Duration::from_millis(1200)
                                                })
                                            {
                                                let lost_seq = last_rx_total_pkg_seq.unwrap_or(0);
                                                let force = should_force_irequest(
                                                    irequest_streak,
                                                    last_rx_total_pkg_seq.is_some(),
                                                );
                                                let req = encode_irequest_with_force(
                                                    selected.own_member,
                                                    media.peer_member,
                                                    last_rx_video_stream_id,
                                                    clock(),
                                                    lost_seq,
                                                    force,
                                                );
                                                if let Ok(datagram) = channel.seal_video_rtcp(
                                                    epoch,
                                                    clock(),
                                                    last_rx_video_stream_id,
                                                    &req,
                                                ) {
                                                    let _ = host.submit(
                                                        &socket,
                                                        endpoint,
                                                        &datagram,
                                                        &[],
                                                    );
                                                    if video_trace {
                                                        eprintln!(
                                                            "[wechat-call-video] tx ireq reason=drop stream={} target={} lost_seq={} force={}",
                                                            last_rx_video_stream_id,
                                                            media.peer_member,
                                                            lost_seq,
                                                            force
                                                        );
                                                    }
                                                    irequest_streak =
                                                        irequest_streak.wrapping_add(1);
                                                    last_irequest_tx = Some(Instant::now());
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if opened.packed_kind == 0x10 {
                                if let Ok(primary) =
                                    audio_packet::parse_ordinary95(opened.payload())
                                {
                                    let primary_count = primary.primaries.len();
                                    let admission = match rx_order.admit(
                                        primary.last_sequence,
                                        primary.last_timestamp,
                                        primary_count,
                                    ) {
                                        Ok(admission) => Some(admission),
                                        Err(_) => {
                                            host.record(MediaMetric::InvalidOrder, 1);
                                            None
                                        }
                                    };
                                    if let Some(admission) = admission {
                                        host.record(
                                            MediaMetric::SkippedFrames,
                                            admission.skip as u64,
                                        );
                                        host.record(MediaMetric::Gaps, u64::from(admission.gap));
                                        let offsets = rx_timeline.admit_offsets(
                                            primary_count,
                                            admission.skip,
                                            admission.gap,
                                            clock(),
                                            Instant::now(),
                                        );
                                        for (packet, sample_offset) in primary
                                            .primaries
                                            .into_iter()
                                            .skip(admission.skip)
                                            .zip(offsets)
                                        {
                                            ensure!(
                                                host.current(),
                                                "Call authority revoked before decode"
                                            );
                                            let pcm = decoder.decode(packet)?;
                                            ensure!(
                                                host.current(),
                                                "Call authority revoked before audio delivery"
                                            );
                                            host.receive_pcm_at(sample_offset, &pcm)?;
                                            host.record(MediaMetric::DeliveredFrames, 1);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(_) => {
                        drained += 1;
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        if drained == 0 {
                            let sleep_ms = if tx_video_packets.is_empty() { 5 } else { 2 };
                            thread::sleep(Duration::from_millis(sleep_ms));
                        }
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            if video_enabled {
                if let Some(member) = host.take_local_keyframe_request() {
                    // Arm a small bounded retry budget (3 attempts at 1000 ms) so an
                    // attach-time or client-triggered keyframe request survives single
                    // UDP loss and reaches `force = true` escalation on attempt 3;
                    // cancelled immediately once a self-contained IDR arrives.
                    pending_local_irequest = Some((member, 3));
                }
                if pending_local_irequest.is_none()
                    && !rx_video_seen
                    && initial_irequests_sent < 6
                    && connected_at.elapsed() >= Duration::from_millis(500)
                    && last_irequest_tx.is_none_or(|t| t.elapsed() >= Duration::from_millis(1500))
                {
                    pending_local_irequest = Some((media.peer_member, 1));
                    initial_irequests_sent += 1;
                }
                if last_irequest_tx.is_none_or(|t| t.elapsed() >= Duration::from_millis(1000)) {
                    if let Some((target_member, remaining)) = pending_local_irequest {
                        let lost_seq = last_rx_total_pkg_seq.unwrap_or(0);
                        let force =
                            should_force_irequest(irequest_streak, last_rx_total_pkg_seq.is_some());
                        let req = encode_irequest_with_force(
                            selected.own_member,
                            target_member,
                            last_rx_video_stream_id,
                            clock(),
                            lost_seq,
                            force,
                        );
                        if let Ok(datagram) =
                            channel.seal_video_rtcp(epoch, clock(), last_rx_video_stream_id, &req)
                        {
                            let _ = host.submit(&socket, endpoint, &datagram, &[]);
                            if video_trace {
                                eprintln!(
                                    "[wechat-call-video] tx ireq reason=local stream={} target={} lost_seq={} force={}",
                                    last_rx_video_stream_id,
                                    target_member,
                                    lost_seq,
                                    force
                                );
                            }
                            irequest_streak = irequest_streak.wrapping_add(1);
                            last_irequest_tx = Some(Instant::now());
                        }
                        pending_local_irequest =
                            (remaining > 1).then_some((target_member, remaining.saturating_sub(1)));
                    }
                }
                if last_p2s_tx.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                    if let Ok(p2s) = encode_p2s(p2s_seq, 2, 0x1d, 15) {
                        p2s_seq = p2s_seq.wrapping_add(1);
                        if let Ok(datagram) = channel.seal_qos_p2s(epoch, clock(), &p2s) {
                            let _ = host.submit(&socket, endpoint, &datagram, &[]);
                            last_p2s_tx = Some(Instant::now());
                        }
                    }
                }
                if let Some(video_rx_ch) = &video {
                    let current_vgen = host.video_generation();
                    if current_vgen == 0 || video_tx_exhausted {
                        tx_video_queue.clear();
                        tx_video_packets.clear();
                        last_vf_tx = None;
                    } else {
                        tx_video_packets.retain(|(gen_id, _)| *gen_id == current_vgen);
                        if tx_video_packets.is_empty() {
                            if let Some(frame) =
                                tx_video_queue.next_frame(video_rx_ch, current_vgen)
                            {
                                if let Ok(pkts) = video_tx.packetize(
                                    frame.codec,
                                    &frame.annexb,
                                    frame.timestamp_ms,
                                    frame.fps,
                                    frame.kbps,
                                ) {
                                    for pkt in pkts {
                                        tx_video_packets.push_back((current_vgen, pkt));
                                    }
                                    if last_vf_tx
                                        .is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
                                    {
                                        if let Ok(vf) = video_tx.build_orientation_metadata_packet(
                                            frame.codec,
                                            frame.timestamp_ms,
                                            frame.fps,
                                        ) {
                                            tx_video_packets.push_back((current_vgen, vf));
                                            last_vf_tx = Some(Instant::now());
                                        }
                                    }
                                }
                            }
                        }
                        // Pace multi-packet keyframes across loop ticks to avoid UDP micro-burst loss.
                        for _ in 0..4 {
                            let Some((pkt_gen, pkt)) = tx_video_packets.pop_front() else {
                                break;
                            };
                            if !host.current() || host.video_generation() != pkt_gen {
                                tx_video_packets.clear();
                                break;
                            }
                            match channel.seal_video(epoch, clock(), 1, true, &pkt) {
                                Ok(datagram) => {
                                    let _ = host.submit(&socket, endpoint, &datagram, &[])?;
                                }
                                Err(_) => {
                                    // Stop outgoing video cleanly if the 16-bit video sequence
                                    // space is exhausted rather than aborting audio.
                                    tx_video_packets.clear();
                                    tx_video_queue.clear();
                                    video_tx_exhausted = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            let current_generation = host.generation();
            if encoder_generation != current_generation {
                tx_audio_buf.clear();
            }
            synchronize_encoder_generation(
                &mut encoder,
                &mut encoder_generation,
                &mut pending,
                current_generation,
            )?;
            if pending
                .as_ref()
                .is_some_and(|(_, item)| !host.frame_current(item))
            {
                pending = None;
                host.record(MediaMetric::StaleFrames, 1);
                encoder.reset()?;
            }
            let now = Instant::now();
            tx_audio_buf.drain_from(
                &audio,
                now,
                |item| host.frame_current(item),
                || host.record(MediaMetric::StaleFrames, 1),
            );
            if !pacer.poll(started.elapsed())? {
                continue;
            }
            let item = tx_audio_buf.pop(
                now,
                |item| host.frame_current(item),
                || host.record(MediaMetric::StaleFrames, 1),
            );
            if let Some(item) = item {
                ensure!(
                    rtp_sequence <= u32::from(u16::MAX) && rtp_timestamp <= u64::from(u32::MAX),
                    "Call RTP sequence exhausted"
                );
                synchronize_encoder_generation(
                    &mut encoder,
                    &mut encoder_generation,
                    &mut pending,
                    item.generation(),
                )?;
                let encoded = encoder.encode(item.samples())?;
                let sequence = rtp_sequence as u16;
                let timestamp = rtp_timestamp as u32;
                rtp_sequence += 1;
                rtp_timestamp += 320;
                if !host.frame_current(&item) {
                    host.record(MediaMetric::StaleFrames, 1 + u64::from(pending.is_some()));
                    pending = None;
                    encoder.reset()?;
                    continue;
                }
                if media.frames_per_packet == 1 {
                    let conf = audio_packet::ordinary95_single(0, sequence, timestamp, &encoded)?;
                    let datagram = channel.seal_audio(epoch, clock(), &conf)?;
                    if !host.submit(&socket, endpoint, &datagram, &[&item])? {
                        encoder.reset()?;
                    }
                } else if let Some((first, first_item)) = pending.take() {
                    if host.frame_current(&first_item) && host.frame_current(&item) {
                        let conf = audio_packet::ordinary95_pair(
                            0, sequence, timestamp, &first, &encoded,
                        )?;
                        let datagram = channel.seal_audio(epoch, clock(), &conf)?;
                        if !host.submit(&socket, endpoint, &datagram, &[&first_item, &item])? {
                            encoder.reset()?;
                        }
                    } else {
                        host.record(MediaMetric::StaleFrames, 2);
                        encoder.reset()?;
                    }
                } else {
                    pending = Some((encoded, item));
                }
            } else if !tx_audio_buf.is_priming() {
                host.record(MediaMetric::InputUnderruns, 1);
            }
        }
        Ok(())
    })();
    relay_qos.close();
    channel.close();
    media_result
}
/// Normal video keyframe requests use `force = false` so peers dispatch their
/// encoder keyframe-request callback (required by hardware encoders in addition
/// to software/QoS request flags). Escalate to `force = true` after unanswered
/// retries to bypass sender sequence gating if `lost_point_seq` lags behind.
fn should_force_irequest(streak: u32, has_rx_seq: bool) -> bool {
    (!has_rx_seq && streak > 0) || streak % 3 == 2
}

fn advance_rx_total_pkg_seq(prev: Option<u64>, raw: &[u8]) -> Option<u64> {
    if raw.len() < 24 || (raw[3] & 0x03) != 3 {
        return prev;
    }
    let pkt_seq = u16::from_be_bytes([raw[5], raw[6]]);
    Some(match prev {
        None => u64::from(pkt_seq),
        Some(current) => {
            let delta = pkt_seq.wrapping_sub(current as u16) as i16;
            if delta > 0 {
                current.wrapping_add(delta as u64)
            } else {
                current
            }
        }
    })
}

fn synchronize_encoder_generation<T>(
    encoder: &mut impl AudioEncoder,
    current: &mut u32,
    pending: &mut Option<T>,
    next: u32,
) -> Result<()> {
    if *current != next {
        *pending = None;
        encoder.reset()?;
        *current = next;
    }
    Ok(())
}

/// Aligns inbound direct-call Opus primaries onto a monotonic 16 kHz
/// sample-offset timeline (320-sample / 20 ms slots) using RTP sequence gaps
/// and wall-clock DTX re-anchoring, independent of the sender's RTP clock rate.
struct DirectRxAudioTimeline {
    last_offset: Option<u64>,
    last_rx_at: Option<Instant>,
}

impl DirectRxAudioTimeline {
    fn new() -> Self {
        Self {
            last_offset: None,
            last_rx_at: None,
        }
    }

    fn admit_offsets(
        &mut self,
        primary_count: usize,
        skip: usize,
        gap: u16,
        elapsed_ms: u64,
        now: Instant,
    ) -> Vec<u64> {
        let emitted = primary_count.saturating_sub(skip);
        if emitted == 0 {
            return Vec::new();
        }
        let wall_offset = (elapsed_ms / 20).saturating_mul(320);
        let first_wall = wall_offset.saturating_sub(((emitted - 1) as u64).saturating_mul(320));
        let inter_arrival_gap = self
            .last_rx_at
            .is_some_and(|prev| now.saturating_duration_since(prev) > Duration::from_millis(120));

        let mut next_offset = match self.last_offset {
            None => first_wall,
            Some(last) => {
                let min_next = last.saturating_add(320);
                if inter_arrival_gap {
                    // Sender paused (DTX silence) or stalled: re-anchor monotonically
                    // to wall time so resumed speech starts on time without building lag.
                    first_wall.max(min_next)
                } else {
                    // Advance by exact sequence gap (bounded so a sequence jump never
                    // pushes the presentation clock far past wall time).
                    let bounded_gap = u64::from(gap).min(25);
                    min_next.saturating_add(bounded_gap.saturating_mul(320))
                }
            }
        };

        let mut out = Vec::with_capacity(emitted);
        for _ in 0..emitted {
            out.push(next_offset);
            self.last_offset = Some(next_offset);
            next_offset = next_offset.saturating_add(320);
        }
        self.last_rx_at = Some(now);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clearing_generation_discards_pending_and_resets_encoder_lookahead() {
        let format = AudioFormat::new(16000, 1, 20).unwrap();
        let mut encoder = OpusEncoder::new(format, 20_000).unwrap();
        let mut impulse = vec![0; 320];
        impulse[..160].fill(12_000);
        let old = encoder.encode(&impulse).unwrap();
        let mut pending = Some(old);
        let mut generation = 1;
        synchronize_encoder_generation(&mut encoder, &mut generation, &mut pending, 2).unwrap();
        assert_eq!(generation, 2);
        assert!(pending.is_none());
        let reset_zeros = encoder.encode(&[0; 320]).unwrap();
        let mut fresh = OpusEncoder::new(format, 20_000).unwrap();
        assert_eq!(reset_zeros, fresh.encode(&[0; 320]).unwrap());
        // Same-generation encoding keeps its ordinary stream state.
        pending = Some(vec![1]);
        synchronize_encoder_generation(&mut encoder, &mut generation, &mut pending, 2).unwrap();
        assert!(pending.is_some());
    }

    #[test]
    fn direct_rx_timeline_advances_across_loss_and_dtx_pauses() {
        let mut tl = DirectRxAudioTimeline::new();
        let t0 = Instant::now();
        // Continuous frames advance cleanly by 320 samples (20 ms), regardless of sender RTP clock rate.
        assert_eq!(tl.admit_offsets(1, 0, 0, 100, t0), vec![1600]);
        assert_eq!(
            tl.admit_offsets(1, 0, 0, 120, t0 + Duration::from_millis(20)),
            vec![1920]
        );
        // Paired primary packet (2 frames) emits two contiguous 320-sample slots.
        assert_eq!(
            tl.admit_offsets(2, 0, 0, 160, t0 + Duration::from_millis(60)),
            vec![2240, 2560]
        );
        // 3 lost frames (gap = 3): advances cleanly past 3 missing slots (3 * 320 = 960).
        assert_eq!(
            tl.admit_offsets(1, 0, 3, 240, t0 + Duration::from_millis(140)),
            vec![3840]
        );
        // 3-second DTX silence pause: re-anchors monotonically to wall clock so live speech
        // resumes immediately without falling behind playback.
        assert_eq!(
            tl.admit_offsets(1, 0, 0, 3240, t0 + Duration::from_millis(3140)),
            vec![51_840]
        );
        // Next continuous frame follows smoothly from the re-anchored offset.
        assert_eq!(
            tl.admit_offsets(1, 0, 0, 3260, t0 + Duration::from_millis(3160)),
            vec![52_160]
        );
    }

    #[test]
    fn advance_rx_total_pkg_seq_handles_high_start_wraparound_reordering_and_invalid_headers() {
        let make_pkt = |hkind: u8, seq: u16| -> Vec<u8> {
            let mut pkt = vec![0u8; 24];
            pkt[3] = hkind;
            pkt[5..7].copy_from_slice(&seq.to_be_bytes());
            pkt
        };
        // Truncated or non-kind-3 headers are ignored.
        assert_eq!(advance_rx_total_pkg_seq(None, &[0; 7]), None);
        assert_eq!(advance_rx_total_pkg_seq(None, &make_pkt(2, 100)), None);

        // High initial sequence (>= 0x8000) anchors cleanly instead of stalling at 0.
        let mut seq = advance_rx_total_pkg_seq(None, &make_pkt(3, 0xfffe));
        assert_eq!(seq, Some(0xfffe));

        // Out-of-order older packet does not move the unwrapped sequence backward.
        seq = advance_rx_total_pkg_seq(seq, &make_pkt(3, 0xfffc));
        assert_eq!(seq, Some(0xfffe));

        // 16-bit wraparound advances monotonically into 64-bit space.
        seq = advance_rx_total_pkg_seq(seq, &make_pkt(3, 0xffff));
        assert_eq!(seq, Some(0xffff));
        seq = advance_rx_total_pkg_seq(seq, &make_pkt(3, 0x0000));
        assert_eq!(seq, Some(0x1_0000));
        seq = advance_rx_total_pkg_seq(seq, &make_pkt(3, 0x0005));
        assert_eq!(seq, Some(0x1_0005));

        // Late packet from before wraparound is still rejected as older.
        seq = advance_rx_total_pkg_seq(seq, &make_pkt(3, 0xfffe));
        assert_eq!(seq, Some(0x1_0005));
    }

    #[test]
    fn should_force_irequest_starts_unforced_and_escalates_periodically() {
        // When inbound video packets are arriving, send two unforced requests
        // first so hardware encoders receive the callback, then escalate on the 3rd.
        assert!(!should_force_irequest(0, true));
        assert!(!should_force_irequest(1, true));
        assert!(should_force_irequest(2, true));
        assert!(!should_force_irequest(3, true));
        assert!(!should_force_irequest(4, true));
        assert!(should_force_irequest(5, true));

        // Before any video packet has arrived, the first request is still unforced
        // and subsequent retries escalate immediately.
        assert!(!should_force_irequest(0, false));
        assert!(should_force_irequest(1, false));
    }

    #[test]
    fn early_handshake_bwe_flagged_idr_burst_is_drained_opened_and_reassembled() {
        use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Mutex,
        };
        use wechat_protocol::{
            calls::{
                audio_packet::MediaEpoch,
                relay::{encrypt_checkin, frame_outer, SelectedRelay},
                video_packet::{ReassembledVideoFrame, VideoWireCodec},
            },
            crypto, protobuf as p,
        };
        use zeroize::Zeroizing;

        struct DummyFrame(Vec<i16>);
        impl DirectAudioFrame for DummyFrame {
            fn generation(&self) -> u32 {
                1
            }
            fn samples(&self) -> &[i16] {
                &self.0
            }
        }
        struct CaptureHost {
            running: Arc<AtomicBool>,
            frames: Arc<Mutex<Vec<ReassembledVideoFrame>>>,
        }
        impl DirectMediaHost for CaptureHost {
            type Frame = DummyFrame;
            fn current(&self) -> bool {
                self.running.load(Ordering::Acquire)
            }
            fn generation(&self) -> u32 {
                1
            }
            fn frame_current(&self, _frame: &Self::Frame) -> bool {
                true
            }
            fn connected(&self) -> Result<()> {
                Ok(())
            }
            fn receive_pcm(&self, _samples: &[i16]) -> Result<()> {
                Ok(())
            }
            fn record(&self, _metric: MediaMetric, _count: u64) {}
            fn submit(
                &self,
                socket: &UdpSocket,
                endpoint: SocketAddr,
                packet: &[u8],
                _frames: &[&Self::Frame],
            ) -> Result<bool> {
                socket.send_to(packet, endpoint)?;
                Ok(true)
            }
            fn receive_video(&self, frame: ReassembledVideoFrame) -> Result<()> {
                self.frames.lock().unwrap().push(frame);
                self.running.store(false, Ordering::Release);
                Ok(())
            }
        }

        let key = [0x42u8; 32];
        let room_id = 77u64;
        let room_key = 99u64;
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };

        let mut h264_idr = Vec::new();
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0xac, 0xd2]);
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]);
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        h264_idr.extend((0..360u16).map(|i| (i.wrapping_mul(17) & 0xff) as u8));

        let mut tx_video = RsmVideoPacketizer::new(7, 1)
            .with_orientation(5)
            .with_chunk_bytes(100)
            .unwrap();
        let mut rsm_pkts = tx_video
            .packetize(VideoWireCodec::H264, &h264_idr, 1234, 15, 500)
            .unwrap();
        // Mark the initial IDR packets with the mobile BWE probe bit (hdr[13] |= 0x10).
        for pkt in rsm_pkts.iter_mut().take(3) {
            pkt[3 + 13] |= 0x10;
        }

        let mut peer_chan = Live96::new(
            key,
            room_key,
            MediaContext {
                room_id,
                own_member: 7,
                peer_member: 3,
                connection_id: 1,
            },
            epoch,
        )
        .unwrap();
        peer_chan
            .authorize_transmit(
                epoch,
                TransmitScope::VerifiedGeneration { key_generation: 1 },
            )
            .unwrap();
        let early_probe = peer_chan.seal_probe_init(epoch, 0).unwrap();
        let early_video_datagrams: Vec<Vec<u8>> = rsm_pkts
            .iter()
            .map(|pkt| peer_chan.seal_video(epoch, 0, 1, true, pkt).unwrap())
            .collect();

        let running = Arc::new(AtomicBool::new(true));
        let peer_running = running.clone();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = server.local_addr().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let peer_thread = thread::spawn(move || {
            let checkin_key: [u8; 16] = crypto::md5(&room_key.to_le_bytes()).try_into().unwrap();
            let relay_reply = |cmd: u64, body: &[u8]| -> Vec<u8> {
                let public = p::message(&[p::uint(1, cmd), p::uint(2, 3), p::uint(3, 0)]).unwrap();
                let header =
                    p::message(&[p::bytes(1, &public), p::uint(2, room_id), p::uint(3, 10)])
                        .unwrap();
                let rand = vec![5; (8 - (body.len() + 10) % 8) % 8 + 3];
                let cipher = encrypt_checkin(body, &checkin_key, &rand).unwrap();
                frame_outer(
                    &p::message(&[
                        p::bytes(1, &header),
                        p::uint(2, cipher.len() as u64),
                        p::bytes(3, &cipher),
                    ])
                    .unwrap(),
                )
                .unwrap()
            };
            let mut buf = [0u8; 8198];
            let (_n, src) = server.recv_from(&mut buf).unwrap();
            let resp = p::message(&[
                p::uint(1, 0),
                p::uint(2, u64::from(u32::from_le_bytes([127, 0, 0, 1]))),
                p::uint(3, u64::from(u32::from_le_bytes([127, 0, 0, 1]))),
                p::uint(4, u64::from(src.port())),
                p::uint(5, u64::from(endpoint.port())),
                p::uint(6, 1),
                p::uint(7, 0),
                p::uint(8, 2),
                p::uint(9, 1),
                p::bytes(12, b"127.0.0.1"),
                p::bytes(13, b"127.0.0.1"),
                p::uint(14, 0),
            ])
            .unwrap();
            server.send_to(&relay_reply(0x1002, &resp), src).unwrap();
            // Send peer 0x70 seq=0 probe + early BWE-flagged IDR burst BEFORE 0x1004 Ready.
            server.send_to(&early_probe, src).unwrap();
            for datagram in &early_video_datagrams {
                server.send_to(datagram, src).unwrap();
            }
            let (_n2, src2) = server.recv_from(&mut buf).unwrap();
            assert_eq!(src2, src);
            let ready = p::message(&[
                p::uint(1, 0),
                p::uint(2, room_id),
                p::uint(3, 0),
                p::uint(5, 3),
                p::uint(6, 1),
                p::uint(7, 0),
            ])
            .unwrap();
            server.send_to(&relay_reply(0x1004, &ready), src).unwrap();
            // Keep the UDP socket bound until the client finishes draining so Windows
            // does not surface ICMP Port Unreachable (WSAECONNRESET) on outbound probes.
            let deadline = Instant::now() + Duration::from_secs(3);
            while peer_running.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        });

        let host = CaptureHost {
            running: running.clone(),
            frames: frames.clone(),
        };
        let (_audio_tx, audio_rx) = mpsc::channel();
        let media = NegotiatedMedia {
            relay: SelectedRelay {
                room_id,
                room_key,
                own_member: 3,
                connection_id: 1,
                endpoint,
                media_key: Zeroizing::new(key),
                merge_mode: false,
            },
            peer_member: 7,
            frames_per_packet: 1,
            video_enabled: true,
            video_dec_cap_mask: 0x1d,
        };
        run_direct_media_with_video(
            &host,
            DirectMediaOptions {
                epoch,
                peer_idle_timeout: Duration::from_secs(3),
            },
            media,
            audio_rx,
            None,
        )
        .unwrap();
        peer_thread.join().unwrap();

        let delivered = frames.lock().unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].codec, VideoWireCodec::H264);
        assert!(delivered[0].is_keyframe);
        assert!(delivered[0].has_codec_config);
        assert_eq!(delivered[0].orientation, 5);
        assert_eq!(delivered[0].payload, h264_idr);
    }
}
