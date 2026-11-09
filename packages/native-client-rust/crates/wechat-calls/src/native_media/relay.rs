//! Relay setup shared by direct and conference media workers.
use anyhow::{ensure, Result};
use std::{
    net::{SocketAddr, SocketAddrV4, UdpSocket},
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::{
    calls::relay::{RelayEpoch, RelayProgress, RoomInputs, SelectedRelay, TwoRttRelay},
    crypto,
};

/// Run on an embedding worker. `submit` must recheck account authority at the
/// physical handoff; the earlier `current` check alone does not authorize a send.
/// Callbacks must not hold an account-state lock across this function's waits.
pub fn connect_relay(
    selected: &SelectedRelay,
    relay_epoch: RelayEpoch,
    started: Instant,
    current: impl Fn() -> bool,
    submit: impl Fn(&UdpSocket, SocketAddrV4, &[u8]) -> Result<()>,
) -> Result<UdpSocket> {
    ensure!(current(), "Call stopped during relay setup");
    let clock = || u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let endpoint = selected.endpoint;
    let socket = UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
    // Physical sends are nonblocking while account/clear submission gates are
    // held. Waiting for input happens only outside those gates.
    socket.set_nonblocking(true)?;
    let room = RoomInputs::initial(selected, 0, clock() as u32, clock() as u32);
    let mut checkin = TwoRttRelay::new(room, selected.room_key, endpoint, relay_epoch, 15_000)?;
    let random = crypto::random::<10>()?;
    let packet = checkin.prepare_initial(
        relay_epoch,
        clock(),
        &random[..checkin.random_bytes_needed()?],
    )?;
    submit(&socket, endpoint, &packet)?;
    // One extra byte distinguishes a maximum-size datagram from truncation.
    let mut buffer = [0u8; 8198];
    while !checkin.is_ready() {
        ensure!(current(), "Call stopped during relay setup");
        ensure!(clock() < 15_000, "Call relay deadline exceeded");
        match socket.recv_from(&mut buffer) {
            Ok((n, SocketAddr::V4(source)))
                if source == endpoint && n <= 8197 && buffer[..n].first() == Some(&0xd5) =>
            {
                match checkin.receive(&buffer[..n], source, relay_epoch, clock())? {
                    RelayProgress::FirstCheckin => {
                        let header = clock() as u32;
                        let body = clock() as u32;
                        let random = crypto::random::<10>()?;
                        let n = checkin.continuation_random_bytes_needed(header, body)?;
                        let continuation = checkin.prepare_continuation(
                            relay_epoch,
                            clock(),
                            header,
                            body,
                            &random[..n],
                        )?;
                        submit(&socket, endpoint, &continuation)?;
                    }
                    RelayProgress::Ready
                    | RelayProgress::Opaque
                    | RelayProgress::DuplicateCheckin => {}
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
    }
    Ok(socket)
}
