//! Direct voice transport, codec state, packet ordering and pacing.
use super::{connect_relay, DirectAudioFrame, DirectMediaHost, DirectMediaOptions, MediaMetric};
use crate::{
    audio::{AudioDecoder, AudioEncoder, AudioFormat},
    opus::{OpusDecoder, OpusEncoder},
    ordering::RtpOrder,
    playout::FramePacer,
    session::NegotiatedMedia,
};
use anyhow::{ensure, Result};
use std::{
    net::SocketAddr,
    sync::mpsc::{Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::calls::{
    audio_packet::{self, Live96, MediaContext, TransmitScope},
    relay::RelayEpoch,
};

pub fn run_direct_media<H: DirectMediaHost>(
    host: &H,
    options: DirectMediaOptions,
    media: NegotiatedMedia,
    audio: Receiver<H::Frame>,
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
    let socket = connect_relay(
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
    let format = AudioFormat::new(16000, 1, 20)?;
    let mut decoder = OpusDecoder::new(format)?;
    let mut encoder = OpusEncoder::new(format, 20_000)?;
    ensure!(host.current(), "Call stopped before transport admission");
    // Current joined peer AV + selected codec were admitted by take_media;
    // Ready establishes this transport. Peer speech is not a prerequisite for
    // transmitting; actual decoded/heard audio remains a separate observation.
    host.connected()?;
    let mut encoder_generation = host.generation();
    let mut pacer = FramePacer::new(format, started.elapsed());
    let mut pending: Option<(Vec<u8>, H::Frame)> = None;
    let mut rtp_sequence = 0u32;
    let mut rtp_timestamp = 0u64;
    let mut rx_order = RtpOrder::new();
    let mut last_peer_activity = Instant::now();
    let media_result = (|| -> Result<()> {
        while host.current() {
            ensure!(
                last_peer_activity.elapsed() < options.peer_idle_timeout,
                "Call peer activity timed out"
            );
            match socket.recv_from(&mut buffer) {
                Ok((n, SocketAddr::V4(source))) if source == endpoint && n <= 8197 => {
                    if buffer[..n].first() == Some(&0x96) {
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
                            } else if let Ok(primary) =
                                audio_packet::parse_ordinary95(opened.payload())
                            {
                                let admission = match rx_order.admit(
                                    primary.last_sequence,
                                    primary.last_timestamp,
                                    primary.primaries.len(),
                                ) {
                                    Ok(admission) => Some(admission),
                                    Err(_) => {
                                        host.record(MediaMetric::InvalidOrder, 1);
                                        None
                                    }
                                };
                                if let Some(admission) = admission {
                                    host.record(MediaMetric::SkippedFrames, admission.skip as u64);
                                    host.record(MediaMetric::Gaps, u64::from(admission.gap));
                                    for packet in primary.primaries.into_iter().skip(admission.skip)
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
                                        host.receive_pcm(&pcm)?;
                                        host.record(MediaMetric::DeliveredFrames, 1);
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
            synchronize_encoder_generation(
                &mut encoder,
                &mut encoder_generation,
                &mut pending,
                host.generation(),
            )?;
            if pending
                .as_ref()
                .is_some_and(|(_, item)| !host.frame_current(item))
            {
                pending = None;
                host.record(MediaMetric::StaleFrames, 1);
                encoder.reset()?;
            }
            if !pacer.poll(started.elapsed())? {
                continue;
            }
            let item = loop {
                match audio.try_recv() {
                    Ok(item) if host.frame_current(&item) => break Some(item),
                    Ok(_) => {
                        host.record(MediaMetric::StaleFrames, 1);
                        continue;
                    }
                    Err(TryRecvError::Empty) => break None,
                    Err(TryRecvError::Disconnected) => break None,
                }
            };
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
            } else {
                host.record(MediaMetric::InputUnderruns, 1);
            }
        }
        Ok(())
    })();
    channel.close();
    media_result
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
}
