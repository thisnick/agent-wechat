//! Direct voice and video transport, codec state, packet ordering and pacing.
use super::{
    connect_relay_with_early, rx_trace::RxTrace, DirectAudioFrame, DirectMediaHost,
    DirectMediaOptions, DirectVideoTx, MediaMetric, PreconnectedRelay, TxAudioJitterBuffer,
};
use crate::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
    ordering::RtpOrder,
    playout::FramePacer,
    session::NegotiatedMedia,
    video_stream::{VideoStreamReceiver, VideoTxPacer, VideoTxSendAction},
};
use anyhow::{ensure, Result};
use std::{
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, TransmitScope},
    relay::RelayEpoch,
    rudp::{self, RudpAckTracker},
    video_packet::{
        classify_video_channel_payload, encode_p2s_report, encode_video_nack, parse_s2p,
        parse_video_nack, VideoChannelPayload, VideoS2pCommand, P2S_CODEC_SUPPORT_H264_H265,
    },
    video_qos::KeyframeRequestReason,
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
    run_direct_media_with_preconnected_relay(host, options, media, audio, video, None)
}

/// Run direct voice + optional video transport, reusing a ringing [`PreconnectedRelay`]
/// when its room/member/connection/endpoint matches the admitted `Join` relay.
pub fn run_direct_media_with_preconnected_relay<H: DirectMediaHost>(
    host: &H,
    options: DirectMediaOptions,
    media: NegotiatedMedia,
    audio: Receiver<H::Frame>,
    video: Option<Receiver<DirectVideoTx>>,
    preconnected: Option<PreconnectedRelay>,
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
    let video_trace = std::env::var("WX_CALL_VIDEO_TRACE").as_deref() == Ok("1");
    let (socket, mut early_packets) = match preconnected {
        Some(mut ready)
            if ready.matches(&selected) && ready.age() <= MAX_PRECONNECTED_RELAY_AGE =>
        {
            ready.drain_early_packets();
            if video_trace {
                eprintln!(
                    "[wechat-call-video] direct relay reused preconnected endpoint={} conn={} age_ms={} early_pkts={}",
                    selected.endpoint,
                    selected.connection_id,
                    ready.age().as_millis(),
                    ready.early_packet_count()
                );
            }
            ready.into_parts()
        }
        _ => {
            let (sock, early) = connect_relay_with_early(
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
            if video_trace {
                eprintln!(
                    "[wechat-call-video] direct relay connected fresh endpoint={} conn={} elapsed_ms={} early_pkts={}",
                    selected.endpoint,
                    selected.connection_id,
                    started.elapsed().as_millis(),
                    early.len()
                );
            }
            (sock, early)
        }
    };
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
    let connected_ms = clock();
    let video_enabled = media.video_enabled || video.is_some();
    let mut video_rx = if video_enabled {
        VideoStreamReceiver::new(connected_ms).with_proactive_initial_requests(6)
    } else {
        VideoStreamReceiver::new(connected_ms)
    };
    let mut video_tx = VideoTxPacer::new(selected.own_member, 1, connected_ms);
    let mut last_p2s_tx: Option<Instant> = None;
    let mut p2s_seq = 1u16;
    let mut last_peer_activity = Instant::now();
    let mut trace_last = Instant::now();
    let mut trace_rx_pkts = 0u64;
    let mut trace_frames_ok = 0u64;
    let mut trace_fec_ok = 0u64;
    let mut trace_drops = 0u64;
    let mut trace_last_orient = 0u8;
    let mut trace_vf_logged = false;
    let mut trace_kf_logged = 0u32;
    let mut trace_iframe_frames_logged = 0u32;
    let mut trace_last_iframe_fidx: Option<u32> = None;
    let mut trace_fec_logged = 0u32;
    let mut trace_drop_logged = 0u32;
    let mut trace_ingest_err_logged = 0u32;
    let mut trace_last_s2p: Option<VideoS2pCommand> = None;
    let mut trace_nack_tx_logged = 0u32;
    let mut trace_rx_rtcp_logged = 0u32;
    let mut trace_rx_video_wire_logged = false;
    let mut trace_tx_ireq_wire_logged = 0u32;
    let mut rx_trace = RxTrace::from_env();
    let mut rudp_acks = RudpAckTracker::default();
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
                        rx_trace.datagram(&buffer[..n]);
                        let Ok((_magic, member)) =
                            audio_packet::unverified_live_member(&buffer[..n])
                        else {
                            rx_trace.bad_header();
                            continue;
                        };
                        if member == 0xffff {
                            rx_trace.relay();
                            if let Ok(Some(opened)) = relay_qos.open(epoch, &buffer[..n]) {
                                rx_trace.relay_qos(opened.packed_kind, opened.payload());
                                if opened.packed_kind == 0x30 {
                                    if let Ok(s2p) = parse_s2p(opened.payload()) {
                                        if video_trace {
                                            for &c in &s2p.video_commands {
                                                if trace_last_s2p != Some(c)
                                                    || c.i_request_flag != 0
                                                {
                                                    trace_last_s2p = Some(c);
                                                    eprintln!(
                                                        "[wechat-call-video] rx s2p alg={} kbps={} fps={} res={} i_period={} rs={} ireq={} codec={} stream={}",
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
                        let opened = match channel.open(epoch, &buffer[..n]) {
                            Ok(v) => {
                                if v.is_none() {
                                    rx_trace.open_dropped();
                                }
                                v
                            }
                            Err(e) => {
                                rx_trace.open_error(&buffer[..n], &e);
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
                            rx_trace.opened(opened.packed_kind);
                            if opened.packed_kind == 0x70 {
                                if let Some(reply) =
                                    channel.reply_to_probe(epoch, clock(), &opened)?
                                {
                                    host.submit(&socket, endpoint, &reply, &[])?;
                                }
                            } else if matches!(opened.packed_kind & 0xf0, 0x20 | 0x60) {
                                if let Ok(kind) = classify_video_channel_payload(opened.payload()) {
                                    match kind {
                                        VideoChannelPayload::IRequest(req) => {
                                            if video_trace && trace_rx_rtcp_logged < 32 {
                                                trace_rx_rtcp_logged += 1;
                                                eprintln!(
                                                    "[wechat-call-video] rx ireq magic=0x{:02x} kind=0x{:02x} ext={} seq={} from={} stream={} target={} tstream={} ts={} raw={:02x?}",
                                                    opened.magic(),
                                                    opened.packed_kind,
                                                    opened.extension_byte(),
                                                    opened.sequence32(),
                                                    req.local_member,
                                                    req.local_stream_id,
                                                    req.target_member,
                                                    req.target_stream_id,
                                                    req.timestamp_ms,
                                                    opened.payload()
                                                );
                                            }
                                            host.on_peer_keyframe_requested();
                                        }
                                        VideoChannelPayload::Nack(raw_nack) => {
                                            if video_trace && trace_rx_rtcp_logged < 32 {
                                                trace_rx_rtcp_logged += 1;
                                                if let Ok(parsed) = parse_video_nack(raw_nack) {
                                                    eprintln!(
                                                        "[wechat-call-video] rx nack magic=0x{:02x} kind=0x{:02x} ext={} seq={} arq={} from={} target={} stream={} ranges={:?} raw={:02x?}",
                                                        opened.magic(),
                                                        opened.packed_kind,
                                                        opened.extension_byte(),
                                                        opened.sequence32(),
                                                        parsed.arq_seq,
                                                        parsed.local_member,
                                                        parsed.target_member,
                                                        parsed.target_stream_id,
                                                        parsed.ranges,
                                                        raw_nack
                                                    );
                                                }
                                            }
                                        }
                                        VideoChannelPayload::RsmVideo(raw) => {
                                            if (opened.packed_kind & 0xf0) != 0x20 {
                                                continue;
                                            }
                                            if video_trace && !trace_rx_video_wire_logged {
                                                trace_rx_video_wire_logged = true;
                                                eprintln!(
                                                    "[wechat-call-video] rx video_wire magic=0x{:02x} kind=0x{:02x} ext={} seq={}",
                                                    opened.magic(),
                                                    opened.packed_kind,
                                                    opened.extension_byte(),
                                                    opened.sequence32()
                                                );
                                            }
                                            let now_ms = clock();
                                            let channel_stream_id = opened.packed_kind & 0x0f;
                                            let batch =
                                                video_rx.ingest_rsm(channel_stream_id, raw, now_ms);
                                            trace_rx_pkts += 1;
                                            if let Some(hdr) = batch.header {
                                                if video_trace
                                                    && !trace_vf_logged
                                                    && hdr.is_virtual_frame
                                                {
                                                    trace_vf_logged = true;
                                                    eprintln!(
                                                        "[wechat-call-video] rx vf ext_byte={} stream={} hdr={:02x?}",
                                                        opened.extension_byte(),
                                                        channel_stream_id,
                                                        &raw[..raw.len().min(32)]
                                                    );
                                                }
                                                if video_trace
                                                    && trace_iframe_frames_logged < 32
                                                    && hdr.is_iframe
                                                    && trace_last_iframe_fidx
                                                        != Some(hdr.frame_index)
                                                {
                                                    trace_last_iframe_fidx = Some(hdr.frame_index);
                                                    trace_iframe_frames_logged += 1;
                                                    eprintln!(
                                                        "[wechat-call-video] rx iframe_frame #{trace_iframe_frames_logged} fidx={} stream={} s={} r={} x={} g={} pkg={} f={}/{} fc={} ext_len={} bytes={} hdr={:02x?}",
                                                        hdr.frame_index,
                                                        channel_stream_id,
                                                        hdr.s_per_group,
                                                        hdr.r_per_group,
                                                        hdr.x_groups,
                                                        hdr.group_idx_x,
                                                        hdr.pkg_idx_in_group,
                                                        hdr.frame_pkt_idx,
                                                        hdr.frame_total_pkts,
                                                        hdr.frame_count,
                                                        hdr.payload_offset.saturating_sub(24),
                                                        raw.len(),
                                                        &raw[..raw.len().min(32)]
                                                    );
                                                }
                                            }
                                            if let Some(err) = batch.ingest_error {
                                                trace_drops += 1;
                                                if video_trace && trace_ingest_err_logged < 12 {
                                                    trace_ingest_err_logged += 1;
                                                    eprintln!(
                                                        "[wechat-call-video] rx ingest err #{trace_ingest_err_logged}: {err:#} hdr={:02x?}",
                                                        &raw[..raw.len().min(32)]
                                                    );
                                                }
                                            } else if batch.assembly_dropped
                                                && batch.frames.is_empty()
                                            {
                                                trace_drops += 1;
                                            }
                                            for frame in batch.frames {
                                                ensure!(
                                                    host.current(),
                                                    "Call authority revoked before video delivery"
                                                );
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
                                                            && trace_kf_logged < 32)
                                                        || (frame.fec_recovered
                                                            && trace_fec_logged < 16)
                                                        || (frame.prior_frames_dropped
                                                            && trace_drop_logged < 16))
                                                {
                                                    if frame.is_keyframe {
                                                        trace_kf_logged =
                                                            trace_kf_logged.saturating_add(1);
                                                    }
                                                    if frame.fec_recovered {
                                                        trace_fec_logged =
                                                            trace_fec_logged.saturating_add(1);
                                                    }
                                                    if frame.prior_frames_dropped {
                                                        trace_drop_logged =
                                                            trace_drop_logged.saturating_add(1);
                                                    }
                                                    eprintln!(
                                                        "[wechat-call-video] rx frame #{trace_frames_ok} fidx={} codec={:?} stream={} kf={} cfg={} fec={} drop={} orient={} bytes={}",
                                                        frame.frame_index,
                                                        frame.codec,
                                                        frame.stream_id,
                                                        frame.is_keyframe,
                                                        frame.has_codec_config,
                                                        frame.fec_recovered,
                                                        frame.prior_frames_dropped,
                                                        frame.orientation,
                                                        frame.payload.len()
                                                    );
                                                }
                                                if frame.is_keyframe {
                                                    // A recovery IDR (self-contained or bare, with
                                                    // cached parameter sets prepended in media.rs)
                                                    // satisfied any pending keyframe request from
                                                    // earlier drops. If an initial bare IDR still
                                                    // needs parameter sets, media.rs queues a
                                                    // fresh local request after receive_video.
                                                    let _ = host.take_local_keyframe_request();
                                                }
                                                host.receive_video(frame)?;
                                            }
                                            if video_trace
                                                && trace_last.elapsed() >= Duration::from_secs(2)
                                            {
                                                eprintln!(
                                                    "[wechat-call-video] rx_pkts={trace_rx_pkts} frames={trace_frames_ok} fec={trace_fec_ok} drops={trace_drops} orient={trace_last_orient}"
                                                );
                                                trace_last = Instant::now();
                                            }
                                        }
                                    }
                                }
                            } else if opened.packed_kind == 0x10 {
                                let parsed = audio_packet::parse_ordinary95(opened.payload());
                                match &parsed {
                                    Ok(primary) => rx_trace.audio_parsed(opened.payload(), primary),
                                    Err(error) => rx_trace.audio_unparsed(opened.payload(), error),
                                }
                                if let Ok(primary) = parsed {
                                    let primary_count = primary.primaries.len();
                                    let admission = match rx_order.admit(
                                        primary.last_sequence,
                                        primary.last_timestamp,
                                        primary_count,
                                    ) {
                                        Ok(admission) => Some(admission),
                                        Err(_) => {
                                            host.record(MediaMetric::InvalidOrder, 1);
                                            rx_trace.order_rejected();
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
                                            let pcm = match decoder.decode(packet) {
                                                Ok(pcm) => pcm,
                                                Err(error) => {
                                                    rx_trace.decode_error(packet, &error);
                                                    return Err(error.into());
                                                }
                                            };
                                            ensure!(
                                                host.current(),
                                                "Call authority revoked before audio delivery"
                                            );
                                            host.receive_pcm_at(sample_offset, &pcm)?;
                                            host.record(MediaMetric::DeliveredFrames, 1);
                                            rx_trace.pcm(&pcm);
                                        }
                                    }
                                }
                            } else if opened.packed_kind == 0x50 {
                                // Acknowledge in-band RUDP data so the peer stops
                                // retransmitting it; the payloads are not interpreted.
                                if let Ok(Some(data)) = rudp::parse_rudp_data(opened.payload()) {
                                    rx_trace.rudp_data(&data);
                                    if let Some(acked) =
                                        rudp_acks.record(data.stream_id, data.pkt_num)
                                    {
                                        // Official clients stamp RUDP packets with wall-clock
                                        // milliseconds, as in relay check-in.
                                        if let Ok(feedback) = rudp::encode_rudp_feedback(
                                            super::relay::epoch_ms32(),
                                            data.stream_id,
                                            acked,
                                            media.peer_member,
                                        ) {
                                            if let Ok(Some(datagram)) =
                                                channel.seal_rudp(epoch, clock(), &feedback)
                                            {
                                                if host.submit(&socket, endpoint, &datagram, &[])? {
                                                    rx_trace.rudp_ack(data.stream_id, acked);
                                                }
                                            }
                                        }
                                    }
                                }
                            } else {
                                rx_trace.other_kind(opened.packed_kind, opened.payload());
                            }
                        }
                    }
                    Ok(_) => {
                        drained += 1;
                        rx_trace.foreign();
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        if drained == 0 {
                            let sleep_ms = if video_tx.has_pending_packets() { 2 } else { 5 };
                            thread::sleep(Duration::from_millis(sleep_ms));
                        }
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            rx_trace.report();
            if video_enabled || video_rx.last_stream_id().is_some() {
                let video_now = Instant::now();
                let now_ms = clock();
                let rx_stream_id = video_rx.last_stream_id().unwrap_or(1);
                if let Some((arq_seq, ranges)) = video_rx.poll_due_nack(now_ms) {
                    let nack = encode_video_nack(
                        selected.own_member,
                        media.peer_member,
                        rx_stream_id,
                        arq_seq,
                        &ranges,
                    );
                    if let Ok(datagram) =
                        channel.seal_video_nack(epoch, now_ms, rx_stream_id, &nack)
                    {
                        let _ = host.submit(&socket, endpoint, &datagram, &[]);
                        if video_trace && trace_nack_tx_logged < 64 {
                            trace_nack_tx_logged += 1;
                            if trace_nack_tx_logged <= 4 {
                                eprintln!(
                                    "[wechat-call-video] tx nack #{trace_nack_tx_logged} seq={arq_seq} stream={rx_stream_id} target={} ranges={ranges:?} magic=0x{:02x} hdr={:02x?}",
                                    media.peer_member,
                                    datagram.first().copied().unwrap_or(0),
                                    &datagram[..datagram.len().min(21)]
                                );
                            } else {
                                eprintln!(
                                    "[wechat-call-video] tx nack #{trace_nack_tx_logged} seq={arq_seq} stream={rx_stream_id} target={} ranges={ranges:?}",
                                    media.peer_member
                                );
                            }
                        }
                    }
                }
                if let Some(member) = host.take_local_keyframe_request() {
                    // Arm a small bounded retry budget (3 attempts at 1000 ms) so an
                    // attach-time or client-triggered keyframe request survives single
                    // UDP loss and alternates normal/forced escalation; cancelled
                    // immediately once a self-contained IDR arrives.
                    video_rx.queue_local_keyframe_request_for(member, 3);
                }
                for req in
                    video_rx.poll_due_irequests(selected.own_member, media.peer_member, now_ms)
                {
                    let Ok(datagram) =
                        channel.seal_video_rtcp(epoch, now_ms, req.stream_id, &req.payload)
                    else {
                        break;
                    };
                    let _ = host.submit(&socket, endpoint, &datagram, &[]);
                    if video_trace {
                        let base_reason = match req.reason {
                            KeyframeRequestReason::FrameDrop => "drop",
                            KeyframeRequestReason::LocalRequest => "local",
                            KeyframeRequestReason::InitialSync => "initial",
                        };
                        if req.is_stream0_fallback {
                            eprintln!(
                                "[wechat-call-video] tx ireq reason={base_reason}_s0 stream=0 target={} lost_seq={} force={}",
                                req.target_member, req.lost_point_seq, req.force
                            );
                        } else if trace_tx_ireq_wire_logged < 4 {
                            trace_tx_ireq_wire_logged += 1;
                            eprintln!(
                                "[wechat-call-video] tx ireq reason={base_reason} stream={} target={} lost_seq={} force={} wire_hdr={:02x?} req={:02x?}",
                                req.stream_id,
                                req.target_member,
                                req.lost_point_seq,
                                req.force,
                                &datagram[..datagram.len().min(21)],
                                &req.payload
                            );
                        } else {
                            eprintln!(
                                "[wechat-call-video] tx ireq reason={base_reason} stream={} target={} lost_seq={} force={}",
                                req.stream_id, req.target_member, req.lost_point_seq, req.force
                            );
                        }
                    }
                }
                if video_enabled {
                    let p2s_interval =
                        Duration::from_millis(video_rx.recommended_p2s_interval_ms());
                    if last_p2s_tx.is_none_or(|t| t.elapsed() >= p2s_interval) {
                        let report = video_rx.snapshot_p2s(
                            media.peer_member,
                            u32::from(selected.connection_id),
                            P2S_CODEC_SUPPORT_H264_H265,
                            now_ms,
                        );
                        // Report only once video has arrived and the sender's codec is known,
                        // so the server is never offered a codec the sender cannot encode.
                        let encoded = if !video_rx.has_rx_video() {
                            last_p2s_tx = Some(video_now);
                            Err(anyhow::anyhow!("sender codec not known yet"))
                        } else {
                            encode_p2s_report(p2s_seq, 2, selected.own_member, &report)
                        };
                        if let Ok(p2s) = encoded {
                            let sent_seq = p2s_seq;
                            p2s_seq = p2s_seq.wrapping_add(1);
                            if let Ok(datagram) = channel.seal_qos_p2s(epoch, now_ms, &p2s) {
                                let _ = host.submit(&socket, endpoint, &datagram, &[]);
                                last_p2s_tx = Some(video_now);
                                if video_trace
                                    && (report.loss_pct > 0
                                        || report.heavy_freeze_cnt > 0
                                        || report.light_freeze_cnt > 0
                                        || report.recv_arq_resp_pkgs > 0
                                        || sent_seq.is_multiple_of(2))
                                {
                                    eprintln!(
                                        "[wechat-call-video] tx p2s seq={sent_seq} peer={} stream={} codec={} size={} rx_kbps={} est_bw={} rx_fps={} play_fps={} loss={}%({}pkts,long={}%) arq={} freeze=L{}/{}%,H{}/{}% gap={}ms",
                                        report.remote_member_id,
                                        report.stream_id,
                                        report.decode_type,
                                        report.max_size_format,
                                        report.recv_kbps,
                                        report.est_bw_kbps,
                                        report.recv_fps,
                                        report.play_fps,
                                        report.loss_pct,
                                        report.loss_pkts,
                                        report.long_loss_pct,
                                        report.recv_arq_resp_pkgs,
                                        report.light_freeze_cnt,
                                        report.light_freeze_time_ratio,
                                        report.heavy_freeze_cnt,
                                        report.heavy_freeze_time_ratio,
                                        report.recv_data_interval_ms
                                    );
                                }
                            }
                        }
                    }
                }
                if let Some(video_rx_ch) = &video {
                    // Pace multi-packet keyframes across loop ticks to avoid UDP micro-burst loss.
                    video_tx.drain_and_pace(
                        video_rx_ch,
                        host.video_generation(),
                        now_ms,
                        4,
                        |bytes| video_rx.observe_tx_frame(bytes),
                        |pkt_gen, pkt| {
                            if !host.current() || host.video_generation() != pkt_gen {
                                return Ok(VideoTxSendAction::StaleGeneration);
                            }
                            match channel.seal_video(epoch, clock(), 1, true, pkt) {
                                Ok(datagram) => {
                                    let _ = host.submit(&socket, endpoint, &datagram, &[])?;
                                    Ok(VideoTxSendAction::Sent)
                                }
                                Err(_) => Ok(VideoTxSendAction::SequenceExhausted),
                            }
                        },
                    )?;
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
    rx_trace.finish(media_result.as_ref().err());
    relay_qos.close();
    channel.close();
    media_result
}

/// Maximum age of a ringing pre-connected UDP relay socket before post-`Join`
/// media startup falls back to a fresh check-in (avoids reusing stale NAT/relay
/// bindings after long ringing).
const MAX_PRECONNECTED_RELAY_AGE: Duration = Duration::from_secs(20);

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
                video_packet::{ReassembledVideoFrame, RsmVideoPacketizer, VideoWireCodec},
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

    #[test]
    fn preconnected_relay_delivers_buffered_seq0_idr_without_second_checkin() {
        use super::super::preconnect_relay;
        use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc, Arc, Mutex,
        };
        use wechat_protocol::{
            calls::{
                audio_packet::MediaEpoch,
                relay::{encrypt_checkin, frame_outer, RelayTarget, SelectedRelay},
                video_packet::{ReassembledVideoFrame, RsmVideoPacketizer, VideoWireCodec},
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

        let key = [0x55u8; 32];
        let room_id = 88u64;
        let room_key = 101u64;
        let epoch = MediaEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
        };
        let relay_epoch = RelayEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
            socket_generation: 1,
            attempt_id: 1,
        };

        let mut h264_idr = Vec::new();
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x16, 0xac, 0xd2]);
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80]);
        h264_idr.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88]);
        h264_idr.extend((0..200u16).map(|i| (i & 0xff) as u8));

        let mut tx_video = RsmVideoPacketizer::new(7, 1)
            .with_orientation(1)
            .with_chunk_bytes(100)
            .unwrap();
        let rsm_pkts = tx_video
            .packetize(VideoWireCodec::H264, &h264_idr, 100, 15, 500)
            .unwrap();

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
        let idr_datagrams: Vec<Vec<u8>> = rsm_pkts
            .iter()
            .map(|pkt| peer_chan.seal_video(epoch, 0, 1, true, pkt).unwrap())
            .collect();

        let running = Arc::new(AtomicBool::new(true));
        let peer_running = running.clone();
        let checkins = Arc::new(AtomicUsize::new(0));
        let peer_checkins = checkins.clone();
        let (idr_sent_tx, idr_sent_rx) = mpsc::channel();
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
            peer_checkins.fetch_add(1, Ordering::AcqRel);
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
            let (_n2, src2) = server.recv_from(&mut buf).unwrap();
            assert_eq!(src2, src);
            peer_checkins.fetch_add(1, Ordering::AcqRel);
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
            // Stream caller's opening seq=0 IDR to the already-preconnected socket
            // BEFORE run_direct_media_with_preconnected_relay even begins.
            for dg in &idr_datagrams {
                server.send_to(dg, src).unwrap();
            }
            let _ = idr_sent_tx.send(());
            let _ = server.set_nonblocking(true);
            let deadline = Instant::now() + Duration::from_secs(3);
            while peer_running.load(Ordering::Acquire) && Instant::now() < deadline {
                if let Ok((n, _)) = server.recv_from(&mut buf) {
                    if n > 0 && buf[0] == 0xd5 {
                        peer_checkins.fetch_add(1, Ordering::AcqRel);
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        });

        let target = RelayTarget {
            room_id,
            room_key,
            own_member: 3,
            connection_id: 1,
            endpoint,
            merge_mode: false,
        };
        let mut preconnected = preconnect_relay(
            &target,
            relay_epoch,
            Instant::now(),
            || true,
            |socket, dest, pkt| {
                socket.send_to(pkt, dest)?;
                Ok(())
            },
        )
        .unwrap();
        idr_sent_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        preconnected.drain_early_packets();
        assert_eq!(preconnected.early_packet_count(), rsm_pkts.len());

        let frames = Arc::new(Mutex::new(Vec::new()));
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
        run_direct_media_with_preconnected_relay(
            &host,
            DirectMediaOptions {
                epoch,
                peer_idle_timeout: Duration::from_secs(3),
            },
            media,
            audio_rx,
            None,
            Some(preconnected),
        )
        .unwrap();
        peer_thread.join().unwrap();

        // Only the 2 ringing CheckIn packets were ever sent (no duplicate CheckIn after Join).
        assert_eq!(checkins.load(Ordering::Acquire), 2);
        let delivered = frames.lock().unwrap();
        assert_eq!(delivered.len(), 1);
        assert!(delivered[0].is_keyframe);
        assert!(delivered[0].has_codec_config);
        assert_eq!(delivered[0].payload, h264_idr);
    }
}
