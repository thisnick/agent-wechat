//! Relay setup shared by direct and conference media workers.
use anyhow::{ensure, Context, Result};
use std::{
    collections::VecDeque,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::{
    calls::relay::{
        RelayEpoch, RelayProgress, RelayTarget, RoomInputs, SelectedRelay, TwoRttRelay,
    },
    crypto,
};

const RELAY_RETRANSMIT_INTERVAL: Duration = Duration::from_millis(200);
const MAX_EARLY_MEDIA_PACKETS: usize = 256;

/// A UDP relay socket whose check-in handshake already reached `Ready` (for
/// example while ringing after `ConfAck`), together with any non-`0xd5`
/// datagrams received from that relay endpoint prior to media handoff.
pub struct PreconnectedRelay {
    target: RelayTarget,
    socket: UdpSocket,
    early_packets: VecDeque<Vec<u8>>,
    ready_at: Instant,
}

impl PreconnectedRelay {
    /// Selected UDP relay endpoint bound by this pre-connected socket.
    pub fn endpoint(&self) -> SocketAddr {
        self.target.endpoint
    }

    /// Selected relay connection identifier bound by this pre-connected socket.
    pub fn connection_id(&self) -> u16 {
        self.target.connection_id
    }

    /// Number of early non-`0xd5` datagrams currently buffered for media handoff.
    pub fn early_packet_count(&self) -> usize {
        self.early_packets.len()
    }

    /// Elapsed duration since this pre-connected relay completed its `Ready` handshake.
    pub fn age(&self) -> Duration {
        self.ready_at.elapsed()
    }

    /// Verify that this pre-connected relay matches the admitted post-`Join` relay identity.
    pub fn matches(&self, selected: &SelectedRelay) -> bool {
        self.target.room_id == selected.room_id
            && self.target.room_key == selected.room_key
            && self.target.own_member == selected.own_member
            && self.target.connection_id == selected.connection_id
            && self.target.endpoint == selected.endpoint
    }

    /// Non-blocking drain of any pending non-`0xd5` datagrams from the selected relay
    /// endpoint into the bounded early-packet buffer (such as an incoming caller's
    /// opening video IDR burst arriving while `Join` is completing).
    pub fn drain_early_packets(&mut self) {
        let mut buffer = [0u8; 8198];
        for _ in 0..64 {
            match self.socket.recv_from(&mut buffer) {
                Ok((n, source)) if source == self.target.endpoint && n > 0 && n <= 8197 => {
                    if buffer[0] != 0xd5 && self.early_packets.len() < MAX_EARLY_MEDIA_PACKETS {
                        self.early_packets.push_back(buffer[..n].to_vec());
                    }
                }
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
    }

    pub(super) fn into_parts(self) -> (UdpSocket, VecDeque<Vec<u8>>) {
        (self.socket, self.early_packets)
    }
}

/// Complete the UDP relay check-in handshake ahead of media start (for example
/// while ringing after `ConfAck`) and return a [`PreconnectedRelay`] for later
/// handoff to [`super::run_direct_media_with_preconnected_relay`].
pub fn preconnect_relay(
    target: &RelayTarget,
    relay_epoch: RelayEpoch,
    started: Instant,
    current: impl Fn() -> bool,
    submit: impl Fn(&UdpSocket, SocketAddr, &[u8]) -> Result<()>,
) -> Result<PreconnectedRelay> {
    let (socket, early_packets) =
        connect_target_with_early(target, relay_epoch, started, current, submit)?;
    Ok(PreconnectedRelay {
        target: target.clone(),
        socket,
        early_packets,
        ready_at: Instant::now(),
    })
}

/// Run on an embedding worker. `submit` must recheck account authority at the
/// physical handoff; the earlier `current` check alone does not authorize a send.
/// Callbacks must not hold an account-state lock across this function's waits.
pub fn connect_relay(
    selected: &SelectedRelay,
    relay_epoch: RelayEpoch,
    started: Instant,
    current: impl Fn() -> bool,
    submit: impl Fn(&UdpSocket, SocketAddr, &[u8]) -> Result<()>,
) -> Result<UdpSocket> {
    let (socket, _early) =
        connect_relay_with_early(selected, relay_epoch, started, current, submit)?;
    Ok(socket)
}

/// Like [`connect_relay`], but preserves non-`0xd5` datagrams arriving from the
/// selected relay endpoint during the check-in handshake (such as an incoming
/// peer's initial audio/video IDR keyframe burst) so callers can feed them into
/// the authenticated media channel instead of discarding them.
pub(super) fn connect_relay_with_early(
    selected: &SelectedRelay,
    relay_epoch: RelayEpoch,
    started: Instant,
    current: impl Fn() -> bool,
    submit: impl Fn(&UdpSocket, SocketAddr, &[u8]) -> Result<()>,
) -> Result<(UdpSocket, VecDeque<Vec<u8>>)> {
    connect_target_with_early(&selected.target(), relay_epoch, started, current, submit)
}

fn connect_target_with_early(
    target: &RelayTarget,
    relay_epoch: RelayEpoch,
    started: Instant,
    current: impl Fn() -> bool,
    submit: impl Fn(&UdpSocket, SocketAddr, &[u8]) -> Result<()>,
) -> Result<(UdpSocket, VecDeque<Vec<u8>>)> {
    ensure!(current(), "Call stopped during relay setup");
    let clock = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let endpoint = target.endpoint;
    let bind_addr: SocketAddr = match endpoint {
        SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let socket = UdpSocket::bind(bind_addr).context("Relay UDP bind failed")?;
    // Physical sends are nonblocking while account/clear submission gates are
    // held. Waiting for input happens only outside those gates.
    socket
        .set_nonblocking(true)
        .context("Relay UDP socket setup failed")?;
    let room = RoomInputs::initial_for_target(target, 4, clock() as u32, clock() as u32);
    let mut checkin = TwoRttRelay::new(room, target.room_key, endpoint, relay_epoch, 15_000)?;
    let random = crypto::random::<10>()?;
    let mut active_packet = checkin
        .prepare_initial(
            relay_epoch,
            clock(),
            &random[..checkin.random_bytes_needed()?],
        )
        .context("Relay check-in preparation failed")?;
    submit(&socket, endpoint, &active_packet).context("Relay UDP send failed")?;
    let mut last_tx = Instant::now();
    // One extra byte distinguishes a maximum-size datagram from truncation.
    let mut buffer = [0u8; 8198];
    let mut early_packets = VecDeque::<Vec<u8>>::new();
    while !checkin.is_ready() {
        ensure!(current(), "Call stopped during relay setup");
        ensure!(clock() < 15_000, "Call relay deadline exceeded");
        if last_tx.elapsed() >= RELAY_RETRANSMIT_INTERVAL {
            if checkin.room().merge_mode {
                let header = clock() as u32;
                let body = clock() as u32;
                let random = crypto::random::<10>()?;
                let n = checkin.merge_fallback_random_bytes_needed(header, body)?;
                active_packet = checkin
                    .prepare_merge_fallback(relay_epoch, clock(), header, body, &random[..n])
                    .context("Relay merge fallback failed")?;
            }
            submit(&socket, endpoint, &active_packet).context("Relay UDP send failed")?;
            last_tx = Instant::now();
        }
        match socket.recv_from(&mut buffer) {
            Ok((n, source))
                if source == endpoint && n <= 8197 && buffer[..n].first() == Some(&0xd5) =>
            {
                match checkin
                    .receive(&buffer[..n], source, relay_epoch, clock())
                    .context("Relay check-in response failed")?
                {
                    RelayProgress::FirstCheckin => {
                        let header = clock() as u32;
                        let body = clock() as u32;
                        let random = crypto::random::<10>()?;
                        let n = checkin.continuation_random_bytes_needed(header, body)?;
                        active_packet = checkin
                            .prepare_continuation(relay_epoch, clock(), header, body, &random[..n])
                            .context("Relay continuation preparation failed")?;
                        submit(&socket, endpoint, &active_packet)
                            .context("Relay UDP send failed")?;
                        last_tx = Instant::now();
                    }
                    RelayProgress::Ready
                    | RelayProgress::Opaque
                    | RelayProgress::DuplicateCheckin => {}
                }
            }
            Ok((n, source)) if source == endpoint && n > 0 && n <= 8197 => {
                if early_packets.len() < MAX_EARLY_MEDIA_PACKETS {
                    early_packets.push_back(buffer[..n].to_vec());
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
            Err(e) => return Err(e).context("Relay UDP receive failed"),
        }
    }
    Ok((socket, early_packets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use wechat_protocol::{
        calls::relay::{decrypt_checkin, encrypt_checkin, frame_outer, unframe_outer},
        protobuf as p,
    };
    use zeroize::Zeroizing;

    fn test_epoch() -> RelayEpoch {
        RelayEpoch {
            account_generation: 1,
            logout_epoch: 0,
            call_generation: 1,
            key_generation: 1,
            socket_generation: 1,
            attempt_id: 1,
        }
    }

    fn checkin_key(room_key: u64) -> [u8; 16] {
        crypto::md5(&room_key.to_le_bytes())
            .try_into()
            .expect("md5 digest is 16 bytes")
    }

    fn decode_checkin_body(packet: &[u8], room_key: u64) -> Vec<p::Field> {
        let outer = p::decode(unframe_outer(packet).unwrap()).unwrap();
        let cipher = p::data(&outer, 3).unwrap();
        let clear = decrypt_checkin(cipher, &checkin_key(room_key), 4096).unwrap();
        p::decode(&clear).unwrap()
    }

    fn relay_reply(room_id: u64, room_key: u64, command: u64, body: &[u8]) -> Vec<u8> {
        let public = p::message(&[p::uint(1, command), p::uint(2, 3), p::uint(3, 0)]).unwrap();
        let header =
            p::message(&[p::bytes(1, &public), p::uint(2, room_id), p::uint(3, 10)]).unwrap();
        let rand = vec![5; (8 - (body.len() + 10) % 8) % 8 + 3];
        let cipher = encrypt_checkin(body, &checkin_key(room_key), &rand).unwrap();
        frame_outer(
            &p::message(&[
                p::bytes(1, &header),
                p::uint(2, cipher.len() as u64),
                p::bytes(3, &cipher),
            ])
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn connect_relay_handles_ipv6_svr_ip_str_on_ipv4_and_native_ipv6_and_merge_fallback() {
        // 1. IPv4 relay returning IPv6 svrIpStr (issue #662 shape).
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = server.local_addr().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let peer = thread::spawn(move || {
            let mut buf = [0u8; 8198];
            let (n, src) = server.recv_from(&mut buf).unwrap();
            let first = decode_checkin_body(&buf[..n], 9);
            assert_eq!(p::scalar(&first, 7).unwrap(), 1);
            let resp = p::message(&[
                p::uint(1, 0),
                p::uint(2, u64::from(u32::from_le_bytes([127, 0, 0, 1]))),
                p::uint(3, u64::from(u32::from_le_bytes([198, 51, 100, 9]))),
                p::uint(4, u64::from(src.port())),
                p::uint(5, 0),
                p::uint(6, 1),
                p::uint(7, 0),
                p::uint(8, 2),
                p::uint(9, 1),
                p::bytes(12, b"127.0.0.1"),
                p::bytes(13, b"2001:db8:10:2212:2e::"),
                p::uint(14, 0),
            ])
            .unwrap();
            server
                .send_to(&relay_reply(7, 9, 0x1002, &resp), src)
                .unwrap();

            let (n, src2) = server.recv_from(&mut buf).unwrap();
            assert_eq!(src2, src);
            let cont = decode_checkin_body(&buf[..n], 9);
            assert_eq!(p::scalar(&cont, 4).unwrap(), 0);
            assert_eq!(p::scalar(&cont, 6).unwrap(), 0);
            assert_eq!(p::data(&cont, 15).unwrap(), b"2001:db8:10:2212:2e::");
            let ready = p::message(&[
                p::uint(1, 0),
                p::uint(2, 7),
                p::uint(3, 0),
                p::uint(5, 0),
                p::uint(6, 1),
                p::uint(7, 0),
            ])
            .unwrap();
            server
                .send_to(&relay_reply(7, 9, 0x1004, &ready), src)
                .unwrap();
        });
        let selected = SelectedRelay {
            room_id: 7,
            room_key: 9,
            own_member: 0,
            connection_id: 1,
            endpoint,
            media_key: Zeroizing::new([3; 32]),
            merge_mode: false,
        };
        let sock = connect_relay(
            &selected,
            test_epoch(),
            Instant::now(),
            || true,
            |socket, dest, pkt| {
                socket.send_to(pkt, dest)?;
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(sock.local_addr().unwrap().ip(), IpAddr::V4(_)));
        peer.join().unwrap();

        // 2. Native IPv6 loopback relay + 1-RTT timeout fallback to 2-RTT.
        if let Ok(server6) = UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)) {
            let endpoint6 = server6.local_addr().unwrap();
            server6
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let peer6 = thread::spawn(move || {
                let mut buf = [0u8; 8198];
                // Drop first 1-RTT packet (f21 = 1) to exercise 2-RTT retransmit fallback.
                let (n, src) = server6.recv_from(&mut buf).unwrap();
                let first = decode_checkin_body(&buf[..n], 9);
                assert_eq!(p::scalar(&first, 4).unwrap(), 0);
                assert_eq!(p::data(&first, 15).unwrap(), b"::1");
                assert_eq!(p::scalar(&first, 21).unwrap(), 1);

                let (n, src2) = server6.recv_from(&mut buf).unwrap();
                assert_eq!(src2, src);
                let retried = decode_checkin_body(&buf[..n], 9);
                assert!(p::optional(&retried, 21).unwrap().is_none());

                let resp = p::message(&[
                    p::uint(1, 0),
                    p::uint(2, 0),
                    p::uint(3, 0),
                    p::uint(4, u64::from(src.port())),
                    p::uint(5, u64::from(endpoint6.port())),
                    p::uint(6, 1),
                    p::uint(7, 0),
                    p::uint(8, 2),
                    p::uint(9, 1),
                    p::bytes(12, b"::1"),
                    p::bytes(13, b"::1"),
                    p::uint(14, 0),
                ])
                .unwrap();
                server6
                    .send_to(&relay_reply(7, 9, 0x1002, &resp), src)
                    .unwrap();

                let (n, src3) = server6.recv_from(&mut buf).unwrap();
                assert_eq!(src3, src);
                let cont = decode_checkin_body(&buf[..n], 9);
                assert_eq!(p::scalar(&cont, 3).unwrap(), 0);
                assert_eq!(p::scalar(&cont, 4).unwrap(), 0);
                assert_eq!(p::data(&cont, 14).unwrap(), b"::1");
                assert_eq!(p::data(&cont, 15).unwrap(), b"::1");
                assert!(p::optional(&cont, 21).unwrap().is_none());
                let ready = p::message(&[
                    p::uint(1, 0),
                    p::uint(2, 7),
                    p::uint(3, 0),
                    p::uint(5, 0),
                    p::uint(6, 1),
                    p::uint(7, 0),
                ])
                .unwrap();
                server6
                    .send_to(&relay_reply(7, 9, 0x1004, &ready), src)
                    .unwrap();
            });
            let selected6 = SelectedRelay {
                room_id: 7,
                room_key: 9,
                own_member: 0,
                connection_id: 1,
                endpoint: endpoint6,
                media_key: Zeroizing::new([3; 32]),
                merge_mode: true,
            };
            let sock6 = connect_relay(
                &selected6,
                test_epoch(),
                Instant::now(),
                || true,
                |socket, dest, pkt| {
                    socket.send_to(pkt, dest)?;
                    Ok(())
                },
            )
            .unwrap();
            assert!(matches!(sock6.local_addr().unwrap().ip(), IpAddr::V6(_)));
            peer6.join().unwrap();
        }
    }

    #[test]
    fn connect_relay_buffers_early_media_packets_during_handshake() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = server.local_addr().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let peer = thread::spawn(move || {
            let mut buf = [0u8; 8198];
            let (n, src) = server.recv_from(&mut buf).unwrap();
            let first = decode_checkin_body(&buf[..n], 9);
            assert_eq!(p::scalar(&first, 13).unwrap(), 4);
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
            server
                .send_to(&relay_reply(7, 9, 0x1002, &resp), src)
                .unwrap();
            // Simulate early peer Live96 media packets (e.g. initial IDR keyframe)
            // arriving before CheckIn #2 Ready (0x1004).
            server.send_to(&[0x96, 0x13, 0xaa, 0xbb], src).unwrap();
            server.send_to(&[0x96, 0x13, 0xcc, 0xdd], src).unwrap();
            let (_n2, src2) = server.recv_from(&mut buf).unwrap();
            assert_eq!(src2, src);
            let ready = p::message(&[
                p::uint(1, 0),
                p::uint(2, 7),
                p::uint(3, 0),
                p::uint(5, 0),
                p::uint(6, 1),
                p::uint(7, 0),
            ])
            .unwrap();
            server
                .send_to(&relay_reply(7, 9, 0x1004, &ready), src)
                .unwrap();
        });
        let selected = SelectedRelay {
            room_id: 7,
            room_key: 9,
            own_member: 0,
            connection_id: 1,
            endpoint,
            media_key: Zeroizing::new([3; 32]),
            merge_mode: false,
        };
        let (_sock, early) = connect_relay_with_early(
            &selected,
            test_epoch(),
            Instant::now(),
            || true,
            |socket, dest, pkt| {
                socket.send_to(pkt, dest)?;
                Ok(())
            },
        )
        .unwrap();
        peer.join().unwrap();
        assert_eq!(early.len(), 2);
        assert_eq!(early[0], vec![0x96, 0x13, 0xaa, 0xbb]);
        assert_eq!(early[1], vec![0x96, 0x13, 0xcc, 0xdd]);
    }

    #[test]
    fn preconnect_relay_drains_post_ready_early_packets_and_matches_selected_relay() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = server.local_addr().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (sent_post_ready, wait_post_ready) = std::sync::mpsc::channel();
        let peer = thread::spawn(move || {
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
            server
                .send_to(&relay_reply(7, 9, 0x1002, &resp), src)
                .unwrap();
            let (_n2, src2) = server.recv_from(&mut buf).unwrap();
            assert_eq!(src2, src);
            let ready = p::message(&[
                p::uint(1, 0),
                p::uint(2, 7),
                p::uint(3, 0),
                p::uint(5, 3),
                p::uint(6, 1),
                p::uint(7, 0),
            ])
            .unwrap();
            server
                .send_to(&relay_reply(7, 9, 0x1004, &ready), src)
                .unwrap();
            // Simulate caller opening IDR packet arriving after Ready (e.g. while Join is in flight)
            // alongside an ignored 0xd5 relay control datagram.
            server.send_to(&[0xd5, 0x00, 0x01, 0x02], src).unwrap();
            server.send_to(&[0x96, 0x13, 0xde, 0xad], src).unwrap();
            let _ = sent_post_ready.send(());
        });
        let target = RelayTarget {
            room_id: 7,
            room_key: 9,
            own_member: 3,
            connection_id: 1,
            endpoint,
            merge_mode: false,
        };
        let mut ready = preconnect_relay(
            &target,
            test_epoch(),
            Instant::now(),
            || true,
            |socket, dest, pkt| {
                socket.send_to(pkt, dest)?;
                Ok(())
            },
        )
        .unwrap();
        wait_post_ready
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        peer.join().unwrap();
        ready.drain_early_packets();
        assert_eq!(ready.endpoint(), endpoint);
        assert_eq!(ready.connection_id(), 1);
        assert_eq!(ready.early_packet_count(), 1);

        let matching = SelectedRelay {
            room_id: 7,
            room_key: 9,
            own_member: 3,
            connection_id: 1,
            endpoint,
            media_key: Zeroizing::new([5; 32]),
            merge_mode: true,
        };
        assert!(ready.matches(&matching));
        let mismatched = SelectedRelay {
            own_member: 4,
            ..matching
        };
        assert!(!ready.matches(&mismatched));
        let (_sock, early) = ready.into_parts();
        assert_eq!(early.len(), 1);
        assert_eq!(early[0], vec![0x96, 0x13, 0xde, 0xad]);
    }
}
