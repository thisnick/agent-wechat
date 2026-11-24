//! Direct voice and video transport, codec state, packet ordering and pacing.
use super::{
    connect_relay_with_early, DirectAudioFrame, DirectMediaHost, DirectMediaOptions, DirectVideoTx,
    MediaMetric, PreconnectedRelay, TxAudioJitterBuffer, TxVideoQueue,
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
    collections::{BTreeMap, VecDeque},
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, TransmitScope},
    relay::RelayEpoch,
    video_packet::{
        classify_video_channel_payload, encode_irequest_with_force, encode_p2s_report,
        encode_video_nack, parse_s2p, parse_video_nack, RsmIngestOutcome, RsmVideoPacketizer,
        RsmVideoReassembler, VideoChannelPayload, VideoP2sReport, VideoS2pCommand, VideoWireCodec,
        MAX_VIDEO_NACK_RANGES,
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
        _ => connect_relay_with_early(
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
        )?,
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
    let mut rx_codec_config_seen = false;
    let mut rx_keyframe_seen = false;
    let mut initial_irequests_sent = 0u8;
    let connected_at = Instant::now();
    let mut last_p2s_tx: Option<Instant> = None;
    let mut p2s_seq = 1u16;
    let mut nack_tracker = VideoNackTracker::new();
    let mut rx_telemetry = DirectVideoRxTelemetry::new(connected_at);
    let mut last_peer_activity = Instant::now();
    let video_enabled = media.video_enabled || video.is_some();
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
                                            let pkt_now = Instant::now();
                                            last_rx_video_stream_id = opened.packed_kind & 0x0f;
                                            last_rx_total_pkg_seq = advance_rx_total_pkg_seq(
                                                last_rx_total_pkg_seq,
                                                raw,
                                            );
                                            let arq_recovered =
                                                nack_tracker.observe_raw_packet(raw, pkt_now);
                                            rx_telemetry.observe_raw_packet(
                                                raw,
                                                arq_recovered,
                                                pkt_now,
                                            );
                                            trace_rx_pkts += 1;
                                            if video_trace
                                                && !trace_vf_logged
                                                && raw.len() >= 24
                                                && (raw[3] & 0x03) == 3
                                                && (raw[4] >> 2) == 3
                                                && (raw[4] & 0x03) == 0
                                                && (raw[15] >> 2) == 1
                                                && raw[19] == 0
                                            {
                                                trace_vf_logged = true;
                                                eprintln!(
                                                    "[wechat-call-video] rx vf ext_byte={} stream={} hdr={:02x?}",
                                                    opened.extension_byte(),
                                                    last_rx_video_stream_id,
                                                    &raw[..raw.len().min(32)]
                                                );
                                            }
                                            if video_trace
                                                && trace_iframe_frames_logged < 32
                                                && raw.len() >= 24
                                                && (raw[3] & 0x03) == 3
                                                && (raw[3] & 0x04) != 0
                                            {
                                                let fidx_lo = u32::from(raw[3] >> 3);
                                                let ext_len = usize::from(raw[23]);
                                                let mut s_ovf_hi = 0usize;
                                                let mut pkg_ovf_hi = 0usize;
                                                let mut f_pkt_hi = 0usize;
                                                let mut f_tot_hi = 0usize;
                                                let mut fidx_hi = 0u32;
                                                if ext_len >= 2 && 24 + ext_len <= raw.len() {
                                                    let ext = &raw[24..24 + ext_len];
                                                    let mask = u16::from_be_bytes([ext[0], ext[1]]);
                                                    let mut pos = 2usize;
                                                    if mask & 0x0001 != 0 && pos + 2 <= ext_len {
                                                        s_ovf_hi = usize::from(ext[pos] & 0x0f);
                                                        pkg_ovf_hi = usize::from(ext[pos] >> 4);
                                                        f_pkt_hi = usize::from(ext[pos + 1] & 0x0f);
                                                        f_tot_hi = usize::from(ext[pos + 1] >> 4);
                                                        pos += 2;
                                                    }
                                                    if mask & 0x0002 != 0 && pos + 2 <= ext_len {
                                                        pos += 2;
                                                    }
                                                    if mask & 0x0004 != 0 && pos + 2 <= ext_len {
                                                        fidx_hi = u32::from(ext[pos])
                                                            | (u32::from(ext[pos + 1] & 0x07) << 8);
                                                    }
                                                }
                                                let fidx = (fidx_hi << 5) | fidx_lo;
                                                if trace_last_iframe_fidx != Some(fidx) {
                                                    trace_last_iframe_fidx = Some(fidx);
                                                    trace_iframe_frames_logged += 1;
                                                    let r_sum = usize::from(raw[4] & 0x03)
                                                        + usize::from(raw[16] & 0x03)
                                                        + usize::from(raw[17] & 0x0f);
                                                    let s_lo = usize::from(raw[15] >> 2);
                                                    let s_ovf = usize::from(raw[19]);
                                                    let pkg_lo = usize::from(raw[4] >> 2);
                                                    let pkg_ovf = usize::from(raw[20]);
                                                    let x_grp = usize::from(raw[18] & 0x07).max(1);
                                                    eprintln!(
                                                        "[wechat-call-video] rx iframe_frame #{trace_iframe_frames_logged} fidx={fidx} stream={} s={} r={} x={} g={} pkg={} f={}/{} fc={} ext_len={} bytes={} hdr={:02x?}",
                                                        last_rx_video_stream_id,
                                                        s_lo + s_ovf + (s_ovf_hi << 8),
                                                        r_sum,
                                                        x_grp,
                                                        raw[17] >> 5,
                                                        pkg_lo + pkg_ovf + (pkg_ovf_hi << 8),
                                                        usize::from(raw[21]) + (f_pkt_hi << 8),
                                                        usize::from(raw[22]) + (f_tot_hi << 8),
                                                        (raw[15] & 0x03) + 1,
                                                        ext_len,
                                                        raw.len(),
                                                        &raw[..raw.len().min(32)]
                                                    );
                                                }
                                            }
                                            let mut need_keyframe = false;
                                            let mut ready_frames = Vec::new();
                                            match video_rx.ingest(raw) {
                                                Ok(RsmIngestOutcome::Frame(frame)) => {
                                                    ready_frames.push(frame);
                                                }
                                                Ok(RsmIngestOutcome::FrameDroppedNeedKeyframe) => {
                                                    trace_drops += 1;
                                                    need_keyframe = true;
                                                }
                                                Err(err) => {
                                                    trace_drops += 1;
                                                    need_keyframe = true;
                                                    if video_trace && trace_ingest_err_logged < 12 {
                                                        trace_ingest_err_logged += 1;
                                                        eprintln!(
                                                            "[wechat-call-video] rx ingest err #{trace_ingest_err_logged}: {err:#} hdr={:02x?}",
                                                            &raw[..raw.len().min(32)]
                                                        );
                                                    }
                                                }
                                                Ok(RsmIngestOutcome::Pending) => {}
                                            }
                                            while let Some(next_frame) = video_rx.pop_ready() {
                                                ready_frames.push(next_frame);
                                            }
                                            for frame in ready_frames {
                                                ensure!(
                                                    host.current(),
                                                    "Call authority revoked before video delivery"
                                                );
                                                last_rx_video_stream_id = frame.stream_id;
                                                rx_video_seen = true;
                                                rx_telemetry.observe_frame(frame.codec, pkt_now);
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
                                                record_rx_video_frame_sync(
                                                    &mut rx_codec_config_seen,
                                                    &mut rx_keyframe_seen,
                                                    frame.is_keyframe,
                                                    frame.has_codec_config,
                                                );
                                                if frame.prior_frames_dropped && !frame.is_keyframe
                                                {
                                                    need_keyframe = true;
                                                } else if frame.is_keyframe {
                                                    // A recovery IDR (self-contained or bare, with
                                                    // cached parameter sets prepended in media.rs)
                                                    // satisfied any pending keyframe request from
                                                    // earlier drops. If an initial bare IDR still
                                                    // needs parameter sets, media.rs queues a
                                                    // fresh local request after receive_video.
                                                    pending_local_irequest = None;
                                                    irequest_streak = 0;
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
                                            if need_keyframe
                                                && last_irequest_tx.is_none_or(|t| {
                                                    t.elapsed() >= Duration::from_millis(1200)
                                                })
                                            {
                                                let lost_seq = if rx_keyframe_seen {
                                                    last_rx_total_pkg_seq.unwrap_or(0)
                                                } else {
                                                    0
                                                };
                                                let force = should_force_irequest(
                                                    irequest_streak,
                                                    last_rx_total_pkg_seq.is_some(),
                                                    rx_keyframe_seen,
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
                                                        if trace_tx_ireq_wire_logged < 4 {
                                                            trace_tx_ireq_wire_logged += 1;
                                                            eprintln!(
                                                                "[wechat-call-video] tx ireq reason=drop stream={} target={} lost_seq={} force={} wire_hdr={:02x?} req={:02x?}",
                                                                last_rx_video_stream_id,
                                                                media.peer_member,
                                                                lost_seq,
                                                                force,
                                                                &datagram[..datagram.len().min(21)],
                                                                &req
                                                            );
                                                        } else {
                                                            eprintln!(
                                                                "[wechat-call-video] tx ireq reason=drop stream={} target={} lost_seq={} force={}",
                                                                last_rx_video_stream_id,
                                                                media.peer_member,
                                                                lost_seq,
                                                                force
                                                            );
                                                        }
                                                    }
                                                    if irequest_streak >= 2
                                                        && irequest_streak.is_multiple_of(2)
                                                        && last_rx_video_stream_id != 0
                                                    {
                                                        let req0 = encode_irequest_with_force(
                                                            selected.own_member,
                                                            media.peer_member,
                                                            0,
                                                            clock(),
                                                            lost_seq,
                                                            force,
                                                        );
                                                        if let Ok(dg0) = channel.seal_video_rtcp(
                                                            epoch,
                                                            clock(),
                                                            0,
                                                            &req0,
                                                        ) {
                                                            let _ = host.submit(
                                                                &socket,
                                                                endpoint,
                                                                &dg0,
                                                                &[],
                                                            );
                                                            if video_trace {
                                                                eprintln!(
                                                                    "[wechat-call-video] tx ireq reason=drop_s0 stream=0 target={} lost_seq={} force={}",
                                                                    media.peer_member,
                                                                    lost_seq,
                                                                    force
                                                                );
                                                            }
                                                        }
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
                let video_now = Instant::now();
                if let Some((arq_seq, ranges)) = nack_tracker.poll_due_ranges(video_now) {
                    let nack = encode_video_nack(
                        selected.own_member,
                        media.peer_member,
                        last_rx_video_stream_id,
                        arq_seq,
                        &ranges,
                    );
                    if let Ok(datagram) =
                        channel.seal_video_nack(epoch, clock(), last_rx_video_stream_id, &nack)
                    {
                        let _ = host.submit(&socket, endpoint, &datagram, &[]);
                        if video_trace && trace_nack_tx_logged < 64 {
                            trace_nack_tx_logged += 1;
                            if trace_nack_tx_logged <= 4 {
                                eprintln!(
                                    "[wechat-call-video] tx nack #{trace_nack_tx_logged} seq={arq_seq} stream={last_rx_video_stream_id} target={} ranges={ranges:?} magic=0x{:02x} hdr={:02x?}",
                                    media.peer_member,
                                    datagram.first().copied().unwrap_or(0),
                                    &datagram[..datagram.len().min(21)]
                                );
                            } else {
                                eprintln!(
                                    "[wechat-call-video] tx nack #{trace_nack_tx_logged} seq={arq_seq} stream={last_rx_video_stream_id} target={} ranges={ranges:?}",
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
                        let lost_seq = if rx_keyframe_seen {
                            last_rx_total_pkg_seq.unwrap_or(0)
                        } else {
                            0
                        };
                        let force = should_force_irequest(
                            irequest_streak,
                            last_rx_total_pkg_seq.is_some(),
                            rx_keyframe_seen,
                        );
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
                                if trace_tx_ireq_wire_logged < 4 {
                                    trace_tx_ireq_wire_logged += 1;
                                    eprintln!(
                                        "[wechat-call-video] tx ireq reason=local stream={} target={} lost_seq={} force={} wire_hdr={:02x?} req={:02x?}",
                                        last_rx_video_stream_id,
                                        target_member,
                                        lost_seq,
                                        force,
                                        &datagram[..datagram.len().min(21)],
                                        &req
                                    );
                                } else {
                                    eprintln!(
                                        "[wechat-call-video] tx ireq reason=local stream={} target={} lost_seq={} force={}",
                                        last_rx_video_stream_id,
                                        target_member,
                                        lost_seq,
                                        force
                                    );
                                }
                            }
                            if irequest_streak >= 2
                                && irequest_streak.is_multiple_of(2)
                                && last_rx_video_stream_id != 0
                            {
                                let req0 = encode_irequest_with_force(
                                    selected.own_member,
                                    target_member,
                                    0,
                                    clock(),
                                    lost_seq,
                                    force,
                                );
                                if let Ok(dg0) = channel.seal_video_rtcp(epoch, clock(), 0, &req0) {
                                    let _ = host.submit(&socket, endpoint, &dg0, &[]);
                                    if video_trace {
                                        eprintln!(
                                            "[wechat-call-video] tx ireq reason=local_s0 stream=0 target={} lost_seq={} force={}",
                                            target_member,
                                            lost_seq,
                                            force
                                        );
                                    }
                                }
                            }
                            irequest_streak = irequest_streak.wrapping_add(1);
                            last_irequest_tx = Some(Instant::now());
                        }
                        pending_local_irequest =
                            (remaining > 1).then_some((target_member, remaining.saturating_sub(1)));
                    }
                }
                if last_p2s_tx.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                    let report = rx_telemetry.snapshot_p2s(
                        media.peer_member,
                        last_rx_video_stream_id,
                        u32::from(selected.connection_id),
                        0x1d,
                        video_now,
                    );
                    if let Ok(p2s) = encode_p2s_report(p2s_seq, 2, &report) {
                        let sent_seq = p2s_seq;
                        p2s_seq = p2s_seq.wrapping_add(1);
                        if let Ok(datagram) = channel.seal_qos_p2s(epoch, clock(), &p2s) {
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
                                    "[wechat-call-video] tx p2s seq={sent_seq} peer={} stream={} codec={} rx_kbps={} est_bw={} rx_fps={} play_fps={} loss={}%({}pkts,long={}%) arq={} freeze=L{}/{}%,H{}/{}% gap={}ms",
                                    report.remote_member_id,
                                    report.stream_id,
                                    report.decode_type,
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
                                rx_telemetry.observe_tx_frame(frame.annexb.len());
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
/// Normal mid-stream video keyframe requests use `force = false` on the first
/// attempt when an initial keyframe and `lost_point_seq` have already been
/// observed, then alternate `force = true` on retries to bypass sender
/// `lost_point_seq < prev_iframe_seq` gating. Before the first keyframe has
/// been received (or when no packets have arrived yet), set `force = true`
/// immediately so late-joining receivers trigger an immediate IDR.
pub(crate) fn should_force_irequest(streak: u32, has_rx_seq: bool, has_rx_keyframe: bool) -> bool {
    !has_rx_keyframe || !has_rx_seq || streak % 2 == 1
}

/// Update codec-config and keyframe sync flags for an inbound reassembled video frame.
/// Once codec parameter sets have been observed on the stream (either in-band on the
/// keyframe or in an earlier access unit), any subsequent IDR satisfies `keyframe_seen`
/// so mid-stream recovery can resume unforced first-attempt `IRequest`s.
pub(crate) fn record_rx_video_frame_sync(
    codec_config_seen: &mut bool,
    keyframe_seen: &mut bool,
    is_keyframe: bool,
    has_codec_config: bool,
) {
    if has_codec_config {
        *codec_config_seen = true;
    }
    if is_keyframe && *codec_config_seen {
        *keyframe_seen = true;
    }
}

pub(crate) fn advance_rx_total_pkg_seq(prev: Option<u64>, raw: &[u8]) -> Option<u64> {
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

/// Maximum age of a ringing pre-connected UDP relay socket before post-`Join`
/// media startup falls back to a fresh check-in (avoids reusing stale NAT/relay
/// bindings after long ringing).
const MAX_PRECONNECTED_RELAY_AGE: Duration = Duration::from_secs(20);

/// Extract the 16-bit media packet sequence (`pkg_seq`, `EncFecHeader[4..6]`) from a kind-3
/// RSM video packet, ignoring virtual/metadata and degenerate/padding packets using the
/// same header rules as `RsmVideoReassembler`.
pub(crate) fn extract_rsm_pkg_seq(raw: &[u8]) -> Option<u16> {
    if raw.len() < 24 || (raw[3] & 0x03) != 3 {
        return None;
    }
    let hdr = &raw[3..];
    let ext_len = usize::from(hdr[20]);
    if 21 + ext_len > hdr.len() {
        return None;
    }
    let r_lo2 = usize::from(hdr[1] & 0x03);
    let pkg_idx_lo6 = usize::from(hdr[1] >> 2);
    let s_lo6 = usize::from(hdr[12] >> 2);
    let r_mid2 = usize::from(hdr[13] & 0x03);
    let r_hi4 = usize::from(hdr[14] & 0x0f);
    let x_groups = usize::from(hdr[15] & 0x07).max(1);
    let s_ovf_lo = usize::from(hdr[16]);
    let pkg_ovf_lo = usize::from(hdr[17]);
    let f_pkt_lo = usize::from(hdr[18]);
    let f_tot_lo = usize::from(hdr[19]);
    let (mut s_ovf_hi, mut pkg_ovf_hi, mut f_pkt_hi, mut f_tot_hi) =
        (0usize, 0usize, 0usize, 0usize);
    let ext = &hdr[21..21 + ext_len];
    if ext_len >= 4 {
        let ext_mask = u16::from_be_bytes([ext[0], ext[1]]);
        if ext_mask & 0x0001 != 0 {
            s_ovf_hi = usize::from(ext[2] & 0x0f);
            pkg_ovf_hi = usize::from(ext[2] >> 4);
            f_pkt_hi = usize::from(ext[3] & 0x0f);
            f_tot_hi = usize::from(ext[3] >> 4);
        }
    }
    let r_full = r_lo2 + r_mid2 + r_hi4;
    let s_per_group = s_lo6 + s_ovf_lo + (s_ovf_hi << 8);
    let pkg_idx_in_group = pkg_idx_lo6 + pkg_ovf_lo + (pkg_ovf_hi << 8);
    let f_pkt_idx = f_pkt_lo + (f_pkt_hi << 8);
    let f_tot_pkts = f_tot_lo + (f_tot_hi << 8);

    // Virtual-frame metadata packets (`s = 1, r = 0, x = 1, pkg_idx = 3`) and
    // degenerate/padding packets do not belong to the media sequence space.
    if (s_per_group == 1 && r_full == 0 && x_groups == 1 && pkg_idx_in_group == 3)
        || f_tot_pkts == 0
        || f_pkt_idx == 0
        || s_per_group == 0
        || pkg_idx_in_group == 0
    {
        return None;
    }
    Some(u16::from_be_bytes([raw[7], raw[8]]))
}

const NACK_MAX_GAP: u64 = 32;
const NACK_MAX_HISTORY: u64 = 48;
const NACK_REORDER_DELAY: Duration = Duration::from_millis(35);
const NACK_RETRY_INTERVAL: Duration = Duration::from_millis(90);
const NACK_MAX_AGE: Duration = Duration::from_millis(350);
const NACK_MIN_DATAGRAM_SPACING: Duration = Duration::from_millis(40);
const NACK_MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug)]
struct PendingNack {
    first_missing: Instant,
    last_sent: Option<Instant>,
    attempts: u8,
}

/// Tracks missing 16-bit `pkg_seq` numbers on inbound RSM video and emits bounded
/// `0x06` ARQ/NACK half-open `[start, end)` ranges after a short reorder grace window.
pub(crate) struct VideoNackTracker {
    highest_seq: Option<u64>,
    missing: BTreeMap<u64, PendingNack>,
    last_datagram_sent: Option<Instant>,
    arq_seq: u8,
    arq_recovered_total: u64,
}

impl VideoNackTracker {
    pub(crate) fn new() -> Self {
        Self {
            highest_seq: None,
            missing: BTreeMap::new(),
            last_datagram_sent: None,
            arq_seq: 0,
            arq_recovered_total: 0,
        }
    }

    pub(crate) fn observe_raw_packet(&mut self, raw: &[u8], now: Instant) -> bool {
        let Some(seq16) = extract_rsm_pkg_seq(raw) else {
            return false;
        };
        self.observe_seq(seq16, now)
    }

    fn observe_seq(&mut self, seq16: u16, now: Instant) -> bool {
        let Some(highest) = self.highest_seq else {
            self.highest_seq = Some(u64::from(seq16));
            return false;
        };
        let delta = seq16.wrapping_sub(highest as u16) as i16;
        if delta > 0 {
            let step = delta as u64;
            let new_highest = highest.wrapping_add(step);
            let gap = step.saturating_sub(1);
            if gap > 0 && gap <= NACK_MAX_GAP {
                for missing_seq in (highest + 1)..new_highest {
                    self.missing.entry(missing_seq).or_insert(PendingNack {
                        first_missing: now,
                        last_sent: None,
                        attempts: 0,
                    });
                }
            } else if gap > NACK_MAX_GAP {
                // Sequence discontinuity or severe outage: drop stale entries instead of
                // flooding NACKs for an unrecoverable window.
                self.missing.clear();
            }
            self.highest_seq = Some(new_highest);
            self.missing
                .retain(|&seq, _| new_highest.saturating_sub(seq) <= NACK_MAX_HISTORY);
            false
        } else if delta < 0 {
            let back = u64::from(delta.unsigned_abs());
            if let Some(seq) = highest.checked_sub(back) {
                if let Some(entry) = self.missing.remove(&seq) {
                    if entry.attempts > 0 {
                        self.arq_recovered_total = self.arq_recovered_total.saturating_add(1);
                        return true;
                    }
                }
            }
            false
        } else {
            false
        }
    }

    pub(crate) fn poll_due_ranges(&mut self, now: Instant) -> Option<(u8, Vec<(u16, u16)>)> {
        let highest = self.highest_seq?;
        // Keep attempt-exhausted entries until NACK_MAX_AGE / NACK_MAX_HISTORY so a
        // retransmission triggered by the final attempt still registers as ARQ-recovered.
        self.missing.retain(|&seq, entry| {
            highest.saturating_sub(seq) <= NACK_MAX_HISTORY
                && now.saturating_duration_since(entry.first_missing) <= NACK_MAX_AGE
        });
        if self.missing.is_empty() {
            return None;
        }
        if self
            .last_datagram_sent
            .is_some_and(|t| now.saturating_duration_since(t) < NACK_MIN_DATAGRAM_SPACING)
        {
            return None;
        }
        let due_seqs: Vec<u64> = self
            .missing
            .iter()
            .filter_map(|(&seq, entry)| {
                if entry.attempts >= NACK_MAX_ATTEMPTS {
                    return None;
                }
                let is_due = if entry.attempts == 0 {
                    now.saturating_duration_since(entry.first_missing) >= NACK_REORDER_DELAY
                } else {
                    entry
                        .last_sent
                        .is_some_and(|t| now.saturating_duration_since(t) >= NACK_RETRY_INTERVAL)
                };
                is_due.then_some(seq)
            })
            .collect();
        if due_seqs.is_empty() {
            return None;
        }
        let mut ranges: Vec<(u16, u16)> = Vec::new();
        let mut sent_seqs: Vec<u64> = Vec::new();
        let mut run_start = due_seqs[0];
        let mut run_end = run_start + 1;
        let mut run_members = vec![run_start];
        for &seq in &due_seqs[1..] {
            if seq == run_end && (run_end - run_start) < NACK_MAX_GAP {
                run_end += 1;
                run_members.push(seq);
            } else {
                if ranges.len() < MAX_VIDEO_NACK_RANGES {
                    ranges.push((run_start as u16, run_end as u16));
                    sent_seqs.extend_from_slice(&run_members);
                }
                run_start = seq;
                run_end = seq + 1;
                run_members.clear();
                run_members.push(seq);
            }
        }
        if ranges.len() < MAX_VIDEO_NACK_RANGES {
            ranges.push((run_start as u16, run_end as u16));
            sent_seqs.extend_from_slice(&run_members);
        }
        if ranges.is_empty() {
            return None;
        }
        for seq in sent_seqs {
            if let Some(entry) = self.missing.get_mut(&seq) {
                entry.attempts = entry.attempts.saturating_add(1);
                entry.last_sent = Some(now);
            }
        }
        self.arq_seq = self.arq_seq.wrapping_add(1);
        self.last_datagram_sent = Some(now);
        Some((self.arq_seq, ranges))
    }
}

/// 1-second sliding-window video receiver/sender telemetry feeding `0x0b` MP P2S QoS reports.
struct DirectVideoRxTelemetry {
    window_start: Instant,
    ever_received_video: bool,
    rx_pkts_in_window: u32,
    rx_bytes_in_window: u64,
    arq_recovered_in_window: u32,
    highest_seq: Option<u64>,
    window_base_seq: Option<u64>,
    long_loss_ewma_x100: u32,
    last_pkt_arrival: Option<Instant>,
    prev_pkt_gap_ms: Option<u32>,
    max_pkt_gap_ms: u32,
    jitter_ms_x16: u32,
    frames_in_window: u32,
    total_play_frames: u32,
    last_frame_at: Option<Instant>,
    freeze_cursor: Option<Instant>,
    counted_light_stall: bool,
    counted_heavy_stall: bool,
    light_freeze_cnt: u32,
    heavy_freeze_cnt: u32,
    light_freeze_ms: u32,
    heavy_freeze_ms: u32,
    last_rx_codec: VideoWireCodec,
    est_bw_kbps: u32,
    tx_bytes_in_window: u64,
    tx_frames_in_window: u32,
}

impl DirectVideoRxTelemetry {
    fn new(now: Instant) -> Self {
        Self {
            window_start: now,
            ever_received_video: false,
            rx_pkts_in_window: 0,
            rx_bytes_in_window: 0,
            arq_recovered_in_window: 0,
            highest_seq: None,
            window_base_seq: None,
            long_loss_ewma_x100: 0,
            last_pkt_arrival: None,
            prev_pkt_gap_ms: None,
            max_pkt_gap_ms: 0,
            jitter_ms_x16: 0,
            frames_in_window: 0,
            total_play_frames: 0,
            last_frame_at: None,
            freeze_cursor: None,
            counted_light_stall: false,
            counted_heavy_stall: false,
            light_freeze_cnt: 0,
            heavy_freeze_cnt: 0,
            light_freeze_ms: 0,
            heavy_freeze_ms: 0,
            last_rx_codec: VideoWireCodec::H265,
            est_bw_kbps: 500,
            tx_bytes_in_window: 0,
            tx_frames_in_window: 0,
        }
    }

    fn observe_raw_packet(&mut self, raw: &[u8], arq_recovered: bool, now: Instant) {
        if raw.is_empty() {
            return;
        }
        let Some(seq16) = extract_rsm_pkg_seq(raw) else {
            return;
        };
        let codec = VideoWireCodec::from_wire(raw[0]);
        if matches!(codec, VideoWireCodec::H264 | VideoWireCodec::H265) {
            self.last_rx_codec = codec;
        }
        self.ever_received_video = true;
        self.rx_pkts_in_window = self.rx_pkts_in_window.saturating_add(1);
        self.rx_bytes_in_window = self.rx_bytes_in_window.saturating_add(raw.len() as u64);
        if arq_recovered {
            self.arq_recovered_in_window = self.arq_recovered_in_window.saturating_add(1);
        }
        if let Some(prev) = self.last_pkt_arrival {
            let gap_ms =
                u32::try_from(now.saturating_duration_since(prev).as_millis()).unwrap_or(u32::MAX);
            self.max_pkt_gap_ms = self.max_pkt_gap_ms.max(gap_ms);
            if let Some(prev_gap) = self.prev_pkt_gap_ms {
                let diff = gap_ms.abs_diff(prev_gap);
                let cur = self.jitter_ms_x16 / 16;
                self.jitter_ms_x16 = self.jitter_ms_x16.saturating_sub(cur).saturating_add(diff);
            }
            self.prev_pkt_gap_ms = Some(gap_ms);
        }
        self.last_pkt_arrival = Some(now);

        match self.highest_seq {
            None => {
                let seq = u64::from(seq16);
                self.highest_seq = Some(seq);
                self.window_base_seq = Some(seq);
            }
            Some(highest) => {
                let delta = seq16.wrapping_sub(highest as u16) as i16;
                if delta > 0 {
                    let step = delta as u64;
                    let new_highest = highest.wrapping_add(step);
                    self.highest_seq = Some(new_highest);
                    if step > 256 {
                        // Reset baseline across large sequence discontinuities.
                        self.window_base_seq = Some(new_highest);
                    } else if self.window_base_seq.is_none() {
                        self.window_base_seq = Some(highest);
                    }
                }
            }
        }
    }

    fn observe_frame(&mut self, codec: VideoWireCodec, now: Instant) {
        if matches!(codec, VideoWireCodec::H264 | VideoWireCodec::H265) {
            self.last_rx_codec = codec;
        }
        self.ever_received_video = true;
        self.account_freeze_until(now);
        self.last_frame_at = Some(now);
        self.freeze_cursor = Some(now);
        self.counted_light_stall = false;
        self.counted_heavy_stall = false;
        self.frames_in_window = self.frames_in_window.saturating_add(1);
        self.total_play_frames = self.total_play_frames.saturating_add(1);
    }

    fn observe_tx_frame(&mut self, bytes: usize) {
        self.tx_bytes_in_window = self.tx_bytes_in_window.saturating_add(bytes as u64);
        self.tx_frames_in_window = self.tx_frames_in_window.saturating_add(1);
    }

    fn account_freeze_until(&mut self, now: Instant) {
        let Some(last_frame) = self.last_frame_at else {
            return;
        };
        let total_gap_ms = u32::try_from(now.saturating_duration_since(last_frame).as_millis())
            .unwrap_or(u32::MAX);
        let cursor = self.freeze_cursor.unwrap_or(last_frame);
        let slice_ms =
            u32::try_from(now.saturating_duration_since(cursor).as_millis()).unwrap_or(u32::MAX);
        if total_gap_ms >= 500 {
            if !self.counted_heavy_stall {
                self.heavy_freeze_cnt = self.heavy_freeze_cnt.saturating_add(1);
                self.counted_heavy_stall = true;
            }
            self.heavy_freeze_ms = self.heavy_freeze_ms.saturating_add(slice_ms);
        } else if total_gap_ms >= 200 {
            if !self.counted_light_stall {
                self.light_freeze_cnt = self.light_freeze_cnt.saturating_add(1);
                self.counted_light_stall = true;
            }
            self.light_freeze_ms = self.light_freeze_ms.saturating_add(slice_ms);
        }
        self.freeze_cursor = Some(now);
    }

    fn snapshot_p2s(
        &mut self,
        remote_member: u16,
        stream_id: u8,
        conn_id: u32,
        codec_support_mask: u32,
        now: Instant,
    ) -> VideoP2sReport {
        let window_ms = u32::try_from(now.saturating_duration_since(self.window_start).as_millis())
            .unwrap_or(1000)
            .max(1);
        let send_kbps = ((self.tx_bytes_in_window.saturating_mul(8)) / u64::from(window_ms)) as u32;
        let send_fps = self
            .tx_frames_in_window
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);

        if !self.ever_received_video || (self.rx_pkts_in_window == 0 && self.frames_in_window == 0)
        {
            // No inbound video packets arrived during this reporting window (either video
            // has not started or the remote camera is off/paused). Report idle instead of
            // 100% loss/heavy freeze, preserve `est_bw_kbps`, and clear per-stream gap
            // anchors so resuming video does not record a synthetic freeze or loss spike.
            self.window_start = now;
            self.rx_pkts_in_window = 0;
            self.rx_bytes_in_window = 0;
            self.arq_recovered_in_window = 0;
            self.highest_seq = None;
            self.window_base_seq = None;
            self.long_loss_ewma_x100 = 0;
            self.last_pkt_arrival = None;
            self.prev_pkt_gap_ms = None;
            self.max_pkt_gap_ms = 0;
            self.jitter_ms_x16 = 0;
            self.frames_in_window = 0;
            self.last_frame_at = None;
            self.freeze_cursor = None;
            self.counted_light_stall = false;
            self.counted_heavy_stall = false;
            self.light_freeze_cnt = 0;
            self.heavy_freeze_cnt = 0;
            self.light_freeze_ms = 0;
            self.heavy_freeze_ms = 0;
            self.tx_bytes_in_window = 0;
            self.tx_frames_in_window = 0;
            return VideoP2sReport {
                remote_member_id: u32::from(remote_member),
                stream_id: u32::from(stream_id),
                conn_id,
                recv_data_interval_ms: 0,
                delay_ms: 0,
                est_bw_kbps: self.est_bw_kbps,
                send_kbps,
                recv_kbps: 0,
                send_fps,
                recv_fps: 0,
                capt_fps: send_fps,
                loss_pct: 0,
                codec_support_mask,
                loss_pkts: 0,
                decode_type: u32::from(self.last_rx_codec.wire()),
                play_fps: 0,
                play_frames: self.total_play_frames,
                light_freeze_cnt: 0,
                heavy_freeze_cnt: 0,
                light_freeze_time_ratio: 0,
                heavy_freeze_time_ratio: 0,
                short_loss_pct: 0,
                long_loss_pct: 0,
                recv_pkgs: 0,
                expected_member_num: 1,
                actual_member_num: 0,
                recv_arq_resp_pkgs: 0,
            };
        }

        self.account_freeze_until(now);
        let recv_kbps = ((self.rx_bytes_in_window.saturating_mul(8)) / u64::from(window_ms)) as u32;
        let recv_fps = self
            .frames_in_window
            .saturating_mul(1000)
            .div_ceil(window_ms)
            .min(60);

        let expected_pkts = match (self.window_base_seq, self.highest_seq) {
            (Some(base), Some(high)) if self.rx_pkts_in_window > 0 && high >= base => {
                let span = high - base + 1;
                if span <= 1024 {
                    (span as u32).max(self.rx_pkts_in_window)
                } else {
                    self.rx_pkts_in_window
                }
            }
            _ => self.rx_pkts_in_window,
        };
        let loss_pkts = expected_pkts.saturating_sub(self.rx_pkts_in_window);
        let short_loss_pct = if expected_pkts > 0 {
            loss_pkts
                .saturating_mul(100)
                .div_ceil(expected_pkts)
                .min(100)
        } else {
            0
        };
        // EWMA with alpha = 1/4 for smoothed long-term loss reporting.
        self.long_loss_ewma_x100 =
            (self.long_loss_ewma_x100 * 3 + short_loss_pct.saturating_mul(100)) / 4;
        let long_loss_pct = ((self.long_loss_ewma_x100 + 50) / 100).min(100);

        let stale_pkt_gap = self.last_pkt_arrival.map_or(0, |t| {
            u32::try_from(now.saturating_duration_since(t).as_millis()).unwrap_or(u32::MAX)
        });
        let recv_data_interval_ms = self.max_pkt_gap_ms.max(stale_pkt_gap);
        let delay_ms = self.jitter_ms_x16 / 16;

        let light_ratio = self
            .light_freeze_ms
            .saturating_mul(100)
            .checked_div(window_ms)
            .unwrap_or(0)
            .min(100);
        let heavy_ratio = self
            .heavy_freeze_ms
            .saturating_mul(100)
            .checked_div(window_ms)
            .unwrap_or(0)
            .min(100);

        if short_loss_pct >= 5 || self.heavy_freeze_cnt > 0 || heavy_ratio >= 50 {
            let retention = 100u32.saturating_sub(short_loss_pct.min(70));
            let base = recv_kbps.max(self.est_bw_kbps / 2).max(100);
            self.est_bw_kbps = ((base.saturating_mul(retention)) / 100).clamp(100, 1500);
        } else if self.rx_pkts_in_window > 0 && self.frames_in_window > 0 {
            let target = (recv_kbps.saturating_mul(12) / 10).clamp(300, 1500);
            self.est_bw_kbps = if self.est_bw_kbps < target {
                self.est_bw_kbps.saturating_add(50).min(target)
            } else {
                target
            };
        }

        let report = VideoP2sReport {
            remote_member_id: u32::from(remote_member),
            stream_id: u32::from(stream_id),
            conn_id,
            recv_data_interval_ms,
            delay_ms,
            est_bw_kbps: self.est_bw_kbps,
            send_kbps,
            recv_kbps,
            send_fps,
            recv_fps,
            capt_fps: send_fps,
            loss_pct: short_loss_pct,
            codec_support_mask,
            loss_pkts,
            decode_type: u32::from(self.last_rx_codec.wire()),
            play_fps: recv_fps,
            play_frames: self.total_play_frames,
            light_freeze_cnt: self.light_freeze_cnt,
            heavy_freeze_cnt: self.heavy_freeze_cnt,
            light_freeze_time_ratio: light_ratio,
            heavy_freeze_time_ratio: heavy_ratio,
            short_loss_pct,
            long_loss_pct,
            recv_pkgs: self.rx_pkts_in_window,
            expected_member_num: 1,
            actual_member_num: u32::from(self.rx_pkts_in_window > 0),
            recv_arq_resp_pkgs: self.arq_recovered_in_window,
        };

        self.window_start = now;
        self.rx_pkts_in_window = 0;
        self.rx_bytes_in_window = 0;
        self.arq_recovered_in_window = 0;
        self.window_base_seq = self.highest_seq.map(|h| h.saturating_add(1));
        self.max_pkt_gap_ms = 0;
        self.frames_in_window = 0;
        self.light_freeze_cnt = 0;
        self.heavy_freeze_cnt = 0;
        self.light_freeze_ms = 0;
        self.heavy_freeze_ms = 0;
        self.tx_bytes_in_window = 0;
        self.tx_frames_in_window = 0;
        report
    }
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
    fn should_force_irequest_forces_before_keyframe_and_alternates_after() {
        // After an initial keyframe has been received, send an unforced request first
        // so encoder callbacks run, then alternate forced/unforced retries.
        assert!(!should_force_irequest(0, true, true));
        assert!(should_force_irequest(1, true, true));
        assert!(!should_force_irequest(2, true, true));
        assert!(should_force_irequest(3, true, true));

        // Before any keyframe has arrived (even if P-frame packets are arriving), force immediately.
        assert!(should_force_irequest(0, true, false));
        assert!(should_force_irequest(2, true, false));

        // Before any video packet has arrived (lost_seq == 0), force immediately.
        assert!(should_force_irequest(0, false, false));
        assert!(should_force_irequest(1, false, true));
    }

    #[test]
    fn record_rx_video_frame_sync_accepts_bare_idr_after_cached_codec_config() {
        let mut cfg_seen = false;
        let mut kf_seen = false;

        // Bare IDR before any parameter sets does not satisfy initial keyframe sync.
        record_rx_video_frame_sync(&mut cfg_seen, &mut kf_seen, true, false);
        assert!(!cfg_seen);
        assert!(!kf_seen);

        // Separate parameter-set access unit marks codec config known without falsely claiming an IDR.
        record_rx_video_frame_sync(&mut cfg_seen, &mut kf_seen, false, true);
        assert!(cfg_seen);
        assert!(!kf_seen);

        // Subsequent bare IDR (without in-band parameter sets) completes keyframe sync.
        record_rx_video_frame_sync(&mut cfg_seen, &mut kf_seen, true, false);
        assert!(cfg_seen);
        assert!(kf_seen);

        // Self-contained IDR sets both flags immediately.
        let (mut cfg2, mut kf2) = (false, false);
        record_rx_video_frame_sync(&mut cfg2, &mut kf2, true, true);
        assert!(cfg2);
        assert!(kf2);
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

    #[test]
    fn video_nack_tracker_handles_reorder_wraparound_recovery_and_expiry() {
        let t0 = Instant::now();
        let mut tracker = VideoNackTracker::new();

        // Anchor near 16-bit wraparound and skip 0xffff and 0x0000.
        assert!(!tracker.observe_seq(0xfffe, t0));
        assert!(!tracker.observe_seq(0x0001, t0 + Duration::from_millis(5)));
        // Before 35 ms reorder grace delay, no NACK is emitted.
        assert!(tracker
            .poll_due_ranges(t0 + Duration::from_millis(25))
            .is_none());

        // If 0xffff arrives during the reorder grace window, only [0x0000, 0x0001) is NACKed.
        assert!(!tracker.observe_seq(0xffff, t0 + Duration::from_millis(28)));
        let (seq1, ranges1) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(45))
            .expect("missing 0x0000 should trigger NACK after reorder delay");
        assert_eq!(seq1, 1);
        assert_eq!(ranges1, vec![(0x0000, 0x0001)]);

        // When 0x0000 arrives after a NACK attempt, it is counted as ARQ-recovered.
        assert!(tracker.observe_seq(0x0000, t0 + Duration::from_millis(60)));
        assert_eq!(tracker.arq_recovered_total, 1);
        assert!(tracker
            .poll_due_ranges(t0 + Duration::from_millis(200))
            .is_none());

        // Missing contiguous range [0x0002, 0x0005) retries up to 3 times then expires.
        assert!(!tracker.observe_seq(0x0005, t0 + Duration::from_millis(210)));
        let (_, r_a) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(250))
            .expect("attempt 1");
        assert_eq!(r_a, vec![(0x0002, 0x0005)]);
        // Min spacing prevents immediate resend.
        assert!(tracker
            .poll_due_ranges(t0 + Duration::from_millis(270))
            .is_none());
        let (_, r_b) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(345))
            .expect("attempt 2");
        assert_eq!(r_b, vec![(0x0002, 0x0005)]);
        let (_, r_c) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(440))
            .expect("attempt 3");
        assert_eq!(r_c, vec![(0x0002, 0x0005)]);
        // Exhausted after 3 attempts.
        assert!(tracker
            .poll_due_ranges(t0 + Duration::from_millis(540))
            .is_none());
    }

    #[test]
    fn video_nack_tracker_ignores_virtual_orientation_packets() {
        let mut tx = RsmVideoPacketizer::new(2, 1).with_orientation(1);
        let vf = tx
            .build_orientation_metadata_packet(VideoWireCodec::H264, 100, 15)
            .unwrap();
        assert_eq!(extract_rsm_pkg_seq(&vf), None);
    }

    #[test]
    fn direct_video_rx_telemetry_reports_loss_freezes_and_active_codec() {
        let t0 = Instant::now();
        let mut tel = DirectVideoRxTelemetry::new(t0);
        let make_pkt = |codec: u8, seq: u16| -> Vec<u8> {
            let mut pkt = vec![0u8; 200];
            pkt[0] = codec;
            pkt[3] = 3; // kind-3
            pkt[4] = 1 << 2; // pkg_idx_in_group = 1
            pkt[7..9].copy_from_slice(&seq.to_be_bytes());
            pkt[15] = 1 << 2; // s_per_group = 1
            pkt[18] = 1; // x_groups = 1
            pkt[21] = 1; // f_pkt_idx = 1
            pkt[22] = 1; // f_tot_pkts = 1
            pkt
        };

        // Receive 5 of 10 H.264 packets (50% loss) and 2 frames separated by a 600 ms freeze.
        for (i, seq) in [1u16, 2, 5, 8, 10].into_iter().enumerate() {
            let ts = t0 + Duration::from_millis(i as u64 * 150);
            tel.observe_raw_packet(&make_pkt(9, seq), seq == 5, ts);
        }
        tel.observe_frame(VideoWireCodec::H264, t0 + Duration::from_millis(100));
        tel.observe_frame(VideoWireCodec::H264, t0 + Duration::from_millis(750));

        let report = tel.snapshot_p2s(7, 1, 99, 0x1d, t0 + Duration::from_secs(1));
        assert_eq!(report.remote_member_id, 7);
        assert_eq!(report.stream_id, 1);
        assert_eq!(report.conn_id, 99);
        assert_eq!(report.decode_type, 9);
        assert_eq!(report.recv_pkgs, 5);
        assert_eq!(report.loss_pkts, 5);
        assert_eq!(report.loss_pct, 50);
        assert_eq!(report.short_loss_pct, 50);
        assert_eq!(report.recv_arq_resp_pkgs, 1);
        assert_eq!(report.heavy_freeze_cnt, 1);
        assert!(report.heavy_freeze_time_ratio >= 60);
        assert!(report.est_bw_kbps < 500);
        assert_eq!(report.actual_member_num, 1);
    }

    #[test]
    fn direct_video_rx_telemetry_treats_camera_off_idle_windows_without_loss_or_bw_decay() {
        let t0 = Instant::now();
        let mut tel = DirectVideoRxTelemetry::new(t0);
        let make_pkt = |codec: u8, seq: u16| -> Vec<u8> {
            let mut pkt = vec![0u8; 400];
            pkt[0] = codec;
            pkt[3] = 3; // kind-3
            pkt[4] = 1 << 2; // pkg_idx_in_group = 1
            pkt[7..9].copy_from_slice(&seq.to_be_bytes());
            pkt[15] = 1 << 2; // s_per_group = 1
            pkt[18] = 1; // x_groups = 1
            pkt[21] = 1; // f_pkt_idx = 1
            pkt[22] = 1; // f_tot_pkts = 1
            pkt
        };

        // 1. Active healthy video in [0s, 1s].
        for i in 0..10u16 {
            let ts = t0 + Duration::from_millis(u64::from(i) * 100);
            tel.observe_raw_packet(&make_pkt(15, 1 + i), false, ts);
            tel.observe_frame(VideoWireCodec::H265, ts);
        }
        let active = tel.snapshot_p2s(7, 1, 99, 0x1d, t0 + Duration::from_secs(1));
        assert_eq!(active.loss_pct, 0);
        assert_eq!(active.heavy_freeze_cnt, 0);
        assert_eq!(active.actual_member_num, 1);
        let bw_before_pause = active.est_bw_kbps;
        assert!(bw_before_pause >= 300);

        // 2. Peer turns camera off for 5 seconds: every idle window reports 0 loss,
        // 0 freeze, actual_member_num = 0, and preserves est_bw_kbps.
        for sec in 2..=6u64 {
            let idle = tel.snapshot_p2s(7, 1, 99, 0x1d, t0 + Duration::from_secs(sec));
            assert_eq!(idle.loss_pct, 0);
            assert_eq!(idle.short_loss_pct, 0);
            assert_eq!(idle.long_loss_pct, 0);
            assert_eq!(idle.heavy_freeze_cnt, 0);
            assert_eq!(idle.heavy_freeze_time_ratio, 0);
            assert_eq!(idle.recv_data_interval_ms, 0);
            assert_eq!(idle.est_bw_kbps, bw_before_pause);
            assert_eq!(idle.actual_member_num, 0);
            assert_eq!(idle.play_frames, 10);
        }

        // 3. Peer turns camera back on at t = 6.2s with fresh sequence numbers:
        // resumption does not emit a phantom multi-second freeze or loss spike.
        for i in 0..5u16 {
            let ts = t0 + Duration::from_millis(6200 + u64::from(i) * 100);
            tel.observe_raw_packet(&make_pkt(15, 50 + i), false, ts);
            tel.observe_frame(VideoWireCodec::H265, ts);
        }
        let resumed = tel.snapshot_p2s(7, 1, 99, 0x1d, t0 + Duration::from_secs(7));
        assert_eq!(resumed.loss_pct, 0);
        assert_eq!(resumed.heavy_freeze_cnt, 0);
        assert_eq!(resumed.heavy_freeze_time_ratio, 0);
        assert!(resumed.recv_data_interval_ms < 500);
        assert_eq!(resumed.actual_member_num, 1);
        assert_eq!(resumed.play_frames, 15);
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

    #[test]
    fn extract_rsm_pkg_seq_skips_virtual_and_padding_packets() {
        use wechat_protocol::calls::video_packet::VideoWireCodec;
        let mut pktz = RsmVideoPacketizer::new(7, 1).with_orientation(1);
        let idr = vec![0, 0, 0, 1, 0x26, 0x01, 0xaa, 0xbb, 0xcc, 0xdd];
        let media_pkts = pktz
            .packetize(VideoWireCodec::H265, &idr, 100, 15, 500)
            .unwrap();
        assert_eq!(extract_rsm_pkg_seq(&media_pkts[0]), Some(1));

        // Virtual orientation metadata frame (`s = 1, r = 0, x = 1, pkg_idx = 3`) must be skipped.
        let vf = pktz
            .build_orientation_metadata_packet(VideoWireCodec::H265, 100, 15)
            .unwrap();
        assert_eq!(extract_rsm_pkg_seq(&vf), None);

        // Padding/degenerate packet (`f_tot_pkts == 0`) must be skipped.
        let mut padding = media_pkts[0].clone();
        padding[22] = 0; // hdr[19] = f_tot_lo
        assert_eq!(extract_rsm_pkg_seq(&padding), None);
    }

    #[test]
    fn video_nack_tracker_emits_wraparound_range_and_counts_final_attempt_recovery() {
        let t0 = Instant::now();
        let mut tracker = VideoNackTracker::new();
        assert!(!tracker.observe_seq(0xfffe, t0));
        // Skip 0xffff and 0x0000 across the 16-bit wrap boundary.
        assert!(!tracker.observe_seq(0x0001, t0 + Duration::from_millis(5)));
        let (arq0, ranges0) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(45))
            .expect("wrap-straddling range [0xffff, 0x0001) should be emitted");
        assert_eq!(arq0, 1);
        assert_eq!(ranges0, vec![(0xffff, 0x0001)]);

        // Send attempts 2 and 3.
        let (arq1, ranges1) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(140))
            .expect("attempt 2");
        assert_eq!(arq1, 2);
        assert_eq!(ranges1, vec![(0xffff, 0x0001)]);
        let (arq2, ranges2) = tracker
            .poll_due_ranges(t0 + Duration::from_millis(235))
            .expect("attempt 3");
        assert_eq!(arq2, 3);
        assert_eq!(ranges2, vec![(0xffff, 0x0001)]);

        // A poll tick 5 ms after attempt 3 emits no 4th NACK, yet keeps entries tracked
        // until NACK_MAX_AGE so retransmissions triggered by attempt 3 still count as recovered.
        assert!(tracker
            .poll_due_ranges(t0 + Duration::from_millis(240))
            .is_none());
        assert!(tracker.observe_seq(0xffff, t0 + Duration::from_millis(260)));
        assert!(tracker.observe_seq(0x0000, t0 + Duration::from_millis(265)));
        assert_eq!(tracker.arq_recovered_total, 2);
    }
}
