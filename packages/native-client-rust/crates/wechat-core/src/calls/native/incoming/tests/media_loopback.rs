//! Generated-media plumbing only: no external host, device or reference client.
//! The relay uses independently specified synthetic D5 fields. Using maintained
//! media helpers at both ends does not establish wire interoperability or a
//! production-safe cryptographic lifetime.
use super::*;
use std::net::Ipv4Addr;
use wechat_calls::{
    frame::{self, MediaFrame, MediaHeader},
    tone::PulseTone,
};
use wechat_protocol::calls::relay::{decrypt_checkin, encrypt_checkin, frame_outer, unframe_outer};

const FRAMES: usize = 40;
const OWN_MEMBER: u16 = 3;
const OWNER_HZ: f64 = 880.0;
const PEER_HZ: f64 = 1320.0;

struct PeerStats {
    checkins: usize,
    sent_primaries: usize,
    received_primaries: usize,
    other_datagrams: usize,
    overlaps: u64,
    samples: Vec<i16>,
}
#[derive(Default)]
struct PeerDiagnostic {
    stage: AtomicU32,
    failure: Mutex<Option<&'static str>>,
}
struct LocalPeer {
    endpoint: SocketAddrV4,
    key: [u8; 32],
    start: SyncSender<(AudioFormat, MediaEpoch)>,
    stop: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<Result<PeerStats>>>,
    diagnostic: Arc<PeerDiagnostic>,
}
impl LocalPeer {
    fn new(frames_per_packet: u8) -> Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        socket.set_nonblocking(true)?;
        let SocketAddr::V4(endpoint) = socket.local_addr()? else {
            bail!("Synthetic relay requires IPv4")
        };
        let key = crypto::random::<32>()?;
        let intent = crypto::random::<32>()?;
        let stop = Arc::new(AtomicBool::new(false));
        let (start, receive_start) = mpsc::sync_channel(1);
        let stopping = stop.clone();
        let diagnostic = Arc::new(PeerDiagnostic::default());
        let recording = diagnostic.clone();
        let task = thread::Builder::new()
            .name("generated-call-peer".into())
            .spawn(move || {
                let result = peer(
                    socket,
                    ReadyMedia {
                        endpoint,
                        key,
                        frames_per_packet,
                    },
                    intent,
                    receive_start,
                    &stopping,
                    &recording,
                );
                if let Err(error) = &result {
                    *recording.failure.lock().unwrap() = Some(peer_failure(error));
                }
                result
            })?;
        Ok(Self {
            endpoint,
            key,
            start,
            stop,
            task: Some(task),
            diagnostic,
        })
    }
    fn finish(&mut self) -> Result<PeerStats> {
        self.task
            .take()
            .context("Synthetic peer already joined")?
            .join()
            .map_err(|_| anyhow::anyhow!("Synthetic peer panicked"))?
    }
}
impl Drop for LocalPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}
struct Harness {
    fixture: Fixture,
    peer: LocalPeer,
    owner_state: Mutex<Option<CallState>>,
}
impl Harness {
    fn new(frames_per_packet: u8) -> Result<Self> {
        let peer = LocalPeer::new(frames_per_packet)?;
        let fixture = Fixture::with_media(
            Case::Normal,
            Some(ReadyMedia {
                endpoint: peer.endpoint,
                key: peer.key,
                frames_per_packet,
            }),
        );
        Ok(Self {
            fixture,
            peer,
            owner_state: Mutex::new(None),
        })
    }
    fn diagnose(&self) {
        let stage = self.peer.diagnostic.stage.load(Ordering::Acquire);
        let failure = *self.peer.diagnostic.failure.lock().unwrap();
        let seen = self.fixture.host.seen();
        let count = |route, command| {
            seen.iter()
                .filter(|entry| **entry == (route, command))
                .count()
        };
        eprintln!("local media failure: peer_stage={stage}, peer_code={}, owner_state={:?}, auth={}, ack={}, join={}, sync={}, exit={}", failure.unwrap_or("none"), *self.owner_state.lock().unwrap(), count(CallHostRoute::AppAuth, None), count(CallHostRoute::RingingAck, None), count(CallHostRoute::Transfer, Some(3361)), count(CallHostRoute::LiveSync, None), count(CallHostRoute::Transfer, Some(3380)));
        if let Some(worker) = self.fixture.backend.inner.active.lock().unwrap().clone() {
            let outcome = match *worker.done.lock().unwrap() {
                None => "running",
                Some(BackendOutcome::Ended) => "ended",
                Some(BackendOutcome::Uncertain) => "uncertain",
                _ => "nonterminal",
            };
            let records = self
                .fixture
                .core
                .store
                .call_action_records("one", worker.identity.call_id());
            let media_error = match self.fixture.core.store.runtime(
                "one",
                &format!("call-media-error:{}", worker.identity.call_id()),
            ) {
                Ok(Some(_)) => "present",
                Ok(None) => "absent",
                Err(_) => "unavailable",
            };
            eprintln!("local media worker: outcome={outcome}, lease_current={}, stop={}, media_stop={}, media_error={}, journal_ok={}, journal_actions={}, received={}, unknown={}, rx_frames={}, tx_datagrams={}", worker.lease.current(), worker.stop.load(Ordering::Acquire), worker.media_stop.load(Ordering::Acquire), media_error, records.is_ok(), records.as_ref().map_or(0, Vec::len), records.as_ref().map_or(0, |rows| rows.iter().filter(|row| row.outcome == "received").count()), records.as_ref().map_or(0, |rows| rows.iter().filter(|row| row.outcome == "needs_review").count()), worker.metrics.rx_delivered_frames.load(Ordering::Relaxed), worker.metrics.tx_submitted_datagrams.load(Ordering::Relaxed));
        }
    }
}
fn peer_failure(error: &anyhow::Error) -> &'static str {
    // Only fixed local categories are exported. Never format a dynamic error,
    // packet, account, key or codec input into failure diagnostics.
    match error.to_string().as_str() {
        "Synthetic relay stopped or timed out" => "receive_deadline",
        "Synthetic checkin public fields" => "public_header",
        "Synthetic checkin context" => "context_header",
        "Synthetic checkin extent" => "cipher_extent",
        "Synthetic checkin room/sequence/member" => "room_sequence_member",
        "Synthetic continuation did not install learned addresses" => "learned_addresses",
        "Synthetic initial checkin already has learned address" => "initial_addresses",
        "Synthetic continuation changed socket" => "continuation_source",
        "Synthetic owner attach deadline" => "owner_attach_deadline",
        "Synthetic owner closed" => "owner_closed",
        "Synthetic media peer deadline" => "media_deadline",
        _ if error.is::<std::io::Error>() => "io",
        _ => "validation_or_codec",
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if thread::panicking() {
            self.diagnose();
        }
        // Also runs during assertion unwinding. Actor::drop joins its media
        // thread before Worker.done; Fixture::drop waits for that completion.
        if let Some(worker) = self.fixture.backend.inner.active.lock().unwrap().clone() {
            worker.stop.store(true, Ordering::Release);
            worker.media_stop.store(true, Ordering::Release);
        }
        self.peer.stop.store(true, Ordering::Release);
    }
}
fn checkin_key() -> Result<[u8; 16]> {
    let hash = openssl::hash::hash(openssl::hash::MessageDigest::md5(), &ROOM_KEY.to_le_bytes())?;
    Ok(hash.as_ref().try_into()?)
}
fn relay_response(command: u64, body: &[u8]) -> Result<Vec<u8>> {
    let public = p::message(&[p::uint(1, command), p::uint(2, 3), p::uint(3, 0)])?;
    let header = p::message(&[p::bytes(1, &public), p::uint(2, ROOM), p::uint(3, 10)])?;
    let random = vec![5; (8 - (body.len() + 10) % 8) % 8 + 3];
    let encrypted = encrypt_checkin(body, &checkin_key()?, &random)?;
    frame_outer(&p::message(&[
        p::bytes(1, &header),
        p::uint(2, encrypted.len() as u64),
        p::bytes(3, &encrypted),
    ])?)
}
fn receive(
    socket: &UdpSocket,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<(Vec<u8>, SocketAddrV4)> {
    let mut buffer = [0; 8198];
    loop {
        ensure!(
            !stop.load(Ordering::Acquire) && Instant::now() < deadline,
            "Synthetic relay stopped or timed out"
        );
        match socket.recv_from(&mut buffer) {
            Ok((n, SocketAddr::V4(source))) => {
                ensure!(
                    source.ip().is_loopback() && n <= 8197,
                    "Synthetic relay source or extent"
                );
                return Ok((buffer[..n].to_vec(), source));
            }
            Ok(_) => bail!("Synthetic relay requires IPv4"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2))
            }
            Err(error) => return Err(error.into()),
        }
    }
}
fn checkin(
    packet: &[u8],
    continued: bool,
    endpoint: SocketAddrV4,
    source: SocketAddrV4,
) -> Result<()> {
    let outer = p::decode(unframe_outer(packet)?)?;
    let header = p::decode(p::data(&outer, 1)?)?;
    let public = p::decode(p::data(&header, 1)?)?;
    ensure!(
        p::scalar(&public, 1)? == 2 && p::scalar(&public, 2)? == 3 && p::scalar(&public, 3)? == 0,
        "Synthetic checkin public fields"
    );
    ensure!(
        p::scalar(&header, 2)? == ROOM
            && p::scalar(&header, 3)? == 10
            && p::scalar(&header, 4)? == u64::from(OWN_MEMBER)
            && p::data(&header, 5)?.is_empty()
            && p::scalar(&header, 6)? == u64::from(OWN_MEMBER)
            && p::scalar(&header, 9)? == 0
            && p::scalar(&header, 10)? == 1,
        "Synthetic checkin context"
    );
    let cipher = p::data(&outer, 3)?;
    ensure!(
        p::scalar(&outer, 2)? == cipher.len() as u64,
        "Synthetic checkin extent"
    );
    let clear = decrypt_checkin(cipher, &checkin_key()?, 4096)?;
    let body = p::decode(&clear)?;
    ensure!(
        p::scalar(&body, 2)? == ROOM
            && p::scalar(&body, 7)? == 1
            && p::scalar(&body, 8)? == u64::from(OWN_MEMBER)
            && p::scalar(&body, 9)? == u64::from(OWN_MEMBER)
            && p::scalar(&body, 20)? == 1,
        "Synthetic checkin room/sequence/member"
    );
    if continued {
        ensure!(
            p::data(&body, 14)? == b"127.0.0.1"
                && p::data(&body, 15)? == b"127.0.0.1"
                && p::scalar(&body, 5)? == u64::from(source.port())
                && p::scalar(&body, 6)? == u64::from(endpoint.port()),
            "Synthetic continuation did not install learned addresses"
        );
    } else {
        ensure!(
            p::data(&body, 14)?.is_empty() && p::scalar(&body, 5)? == 0,
            "Synthetic initial checkin already has learned address"
        );
    }
    Ok(())
}
fn peer(
    socket: UdpSocket,
    media: ReadyMedia,
    intent: [u8; 32],
    start: Receiver<(AudioFormat, MediaEpoch)>,
    stop: &AtomicBool,
    diagnostic: &PeerDiagnostic,
) -> Result<PeerStats> {
    let ReadyMedia {
        endpoint,
        key,
        frames_per_packet,
    } = media;
    let began = Instant::now();
    diagnostic.stage.store(1, Ordering::Release);
    let (initial, source) = receive(&socket, stop, began + Duration::from_secs(3))?;
    diagnostic.stage.store(2, Ordering::Release);
    checkin(&initial, false, endpoint, source)?;
    // S2C CheckinResp scalars1..9, SN6=1, unmerged14=0 and learned texts12/13.
    let body = p::message(&[
        p::uint(1, ROOM),
        p::uint(
            2,
            u64::from(u32::from_le_bytes(Ipv4Addr::LOCALHOST.octets())),
        ),
        p::uint(3, 0),
        p::uint(4, u64::from(source.port())),
        p::uint(5, u64::from(endpoint.port())),
        p::uint(6, 1),
        p::uint(7, 0),
        p::uint(8, 0),
        p::uint(9, 0),
        p::bytes(12, b"127.0.0.1"),
        p::bytes(13, b"127.0.0.1"),
        p::uint(14, 0),
    ])?;
    diagnostic.stage.store(3, Ordering::Release);
    socket.send_to(&relay_response(0x1002, &body)?, source)?;
    diagnostic.stage.store(4, Ordering::Release);
    let (continuation, same_source) = receive(&socket, stop, began + Duration::from_secs(3))?;
    ensure!(
        same_source == source,
        "Synthetic continuation changed socket"
    );
    diagnostic.stage.store(5, Ordering::Release);
    checkin(&continuation, true, endpoint, source)?;
    // Ready is not an ACK request. No control70/probe is synthesized.
    let ready = p::message(&[
        p::uint(1, 0),
        p::uint(2, ROOM),
        p::uint(3, 0),
        p::uint(5, u64::from(OWN_MEMBER)),
        p::uint(6, 1),
        p::uint(7, 0),
    ])?;
    diagnostic.stage.store(6, Ordering::Release);
    socket.send_to(&relay_response(0x1004, &ready)?, source)?;
    diagnostic.stage.store(7, Ordering::Release);
    let (format, epoch) = loop {
        ensure!(
            !stop.load(Ordering::Acquire) && began.elapsed() < Duration::from_secs(4),
            "Synthetic owner attach deadline"
        );
        match start.try_recv() {
            Ok(start) => break start,
            Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(2)),
            Err(TryRecvError::Disconnected) => bail!("Synthetic owner closed"),
        }
    };
    diagnostic.stage.store(8, Ordering::Release);
    let mut channel = Live96::new(
        key,
        ROOM_KEY,
        MediaContext {
            room_id: ROOM,
            own_member: PEER_MEMBER,
            peer_member: OWN_MEMBER,
            connection_id: 1,
        },
        epoch,
    )?;
    channel.authorize_transmit(
        epoch,
        TransmitScope::IsolatedCompatibilityTest {
            intent_id: intent,
            deadline_ms: 10_000,
            allow_known_audio_overlap: true,
        },
    )?;
    let mut encoder = OpusEncoder::new(format, 20_000)?;
    let mut decoder = OpusDecoder::new(format)?;
    let mut tone = tone(format, PEER_HZ)?;
    let mut result = PeerStats {
        checkins: 2,
        sent_primaries: 0,
        received_primaries: 0,
        other_datagrams: 0,
        overlaps: 0,
        samples: vec![],
    };
    let mut next = Instant::now();
    let mut buffer = [0; 8198];
    diagnostic.stage.store(9, Ordering::Release);
    while !stop.load(Ordering::Acquire) && began.elapsed() < Duration::from_secs(7) {
        if result.sent_primaries < FRAMES && Instant::now() >= next {
            let first = encoder.encode(&tone.next_frame()?)?;
            let second = encoder.encode(&tone.next_frame()?)?;
            let last = result.sent_primaries + 1;
            let conf = audio_packet::ordinary95_pair(
                0,
                last as u16,
                (last * format.samples_per_frame()) as u32,
                &first,
                &second,
            )?;
            let packet = channel.seal_audio(epoch, began.elapsed().as_millis() as u64, &conf)?;
            ensure!(
                socket.send_to(&packet, source)? == packet.len(),
                "Synthetic media short write"
            );
            result.sent_primaries += 2;
            next = Instant::now() + Duration::from_millis(40);
        }
        match socket.recv_from(&mut buffer) {
            Ok((n, from)) if from == SocketAddr::V4(source) && n <= 8197 => {
                let opened = channel
                    .open(epoch, &buffer[..n])?
                    .context("Synthetic replay")?;
                if opened.packed_kind == 0x10 {
                    let audio = audio_packet::parse_ordinary95(opened.payload())?;
                    ensure!(
                        audio.primaries.len() == usize::from(frames_per_packet),
                        "Synthetic native sender packet grouping mismatch"
                    );
                    for packet in audio.primaries {
                        result.samples.extend(decoder.decode(packet)?);
                        result.received_primaries += 1;
                    }
                    ensure!(
                        result.received_primaries <= FRAMES + 8,
                        "Synthetic native media budget"
                    );
                } else {
                    result.other_datagrams += 1;
                    ensure!(
                        result.other_datagrams <= 4,
                        "Synthetic protocol traffic budget"
                    );
                }
            }
            Ok(_) => bail!("Unexpected synthetic UDP source or extent"),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        if result.sent_primaries == FRAMES && result.received_primaries >= FRAMES {
            if frames_per_packet == 2 {
                // Ongoing malformed traffic must not keep a silent peer alive.
                for _ in 0..40 {
                    socket.send_to(&[0x96, 0x10], source)?;
                    thread::sleep(Duration::from_millis(20));
                }
            }
            result.overlaps = channel.known_audio_overlaps();
            channel.close();
            diagnostic.stage.store(10, Ordering::Release);
            return Ok(result);
        }
        thread::sleep(Duration::from_millis(2));
    }
    bail!("Synthetic media peer deadline")
}
fn tone(format: AudioFormat, hz: f64) -> Result<PulseTone> {
    Ok(PulseTone::new(
        format,
        hz,
        0.2,
        Duration::from_millis(100),
        Duration::from_millis(100),
    )?)
}
fn check_tone(samples: &[i16], format: AudioFormat, expected: f64, other: f64) {
    assert!(samples.len() >= FRAMES * format.samples_per_frame());
    // Frequency energy is insensitive to Opus lookahead/phase. Exclude the first
    //100ms rather than pretending a lossy codec reproduces exact source bytes.
    let samples = &samples[format.sample_rate() as usize / 10..];
    let power = |hz: f64| {
        let (mut real, mut imaginary) = (0.0, 0.0);
        for (i, sample) in samples.iter().enumerate() {
            let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(format.sample_rate());
            real += f64::from(*sample) * phase.cos();
            imaginary += f64::from(*sample) * phase.sin();
        }
        real * real + imaginary * imaginary
    };
    assert!(power(expected) > 10.0 * power(other).max(1.0));
    let energies = samples
        .chunks_exact(format.samples_per_frame())
        .map(|frame| {
            frame
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / frame.len() as f64
        })
        .collect::<Vec<_>>();
    let maximum = energies.iter().copied().fold(0.0, f64::max);
    let loud = energies
        .iter()
        .filter(|&&energy| energy > maximum / 10.0)
        .count();
    let quiet = energies
        .iter()
        .filter(|&&energy| energy < maximum / 20.0)
        .count();
    assert!(
        maximum > 100_000.0 && loud >= 8 && quiet >= 8,
        "Generated tone needs sustained pulses and quiet gaps"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn generated_audio_crosses_native_queues_opus_encryption_and_local_udp() -> Result<()> {
    for frames_per_packet in [1, 2] {
        let mut h = Harness::new(frames_per_packet)?;
        let test = async {
            let f = &h.fixture;
            let service = f.core.call_service();
            let mut client = service.connect("one", "fixture", true).await?;
            let Some(CallEvent::Hello { audio_format, .. }) = client.events.recv().await else {
                bail!("Synthetic missing Hello")
            };
            ensure!(
                audio_format == CallAudioFormat::default(),
                "Synthetic selected codec format differs from announced format"
            );
            let format = AudioFormat::new(
                audio_format.sample_rate,
                audio_format.channels,
                audio_format.frame_ms,
            )?;
            service
                .command(
                    &client.client_id,
                    CallCommand::Ready {
                        request_id: "ready".into(),
                        ready: true,
                        format: audio_format,
                        scope: Some(CallReadyScope::All),
                    },
                )
                .await?;
            assert_eq!(
                result(&mut client, "ready").await,
                CallCommandStatus::Accepted
            );
            f.stage(false, 0);
            let offer = f.offer().await;
            service
                .command(
                    &client.client_id,
                    CallCommand::Answer {
                        request_id: "answer".into(),
                        call_id: offer.call_id.clone(),
                        expected_revision: offer.revision,
                    },
                )
                .await?;
            // GET/current is observer-safe and intentionally omits this private
            // owner view. Preserve owner events even if Connected precedes the
            // Answer result instead of querying the observer snapshot for it.
            let (token, call) = tokio::time::timeout(Duration::from_secs(3), async {
                let (mut token, mut connected) = (None, None);
                loop {
                    match client
                        .events
                        .recv()
                        .await
                        .context("Synthetic owner control closed")?
                    {
                        CallEvent::CommandResult {
                            request_id,
                            status,
                            owner_token,
                            ..
                        } if request_id == "answer" => {
                            ensure!(
                                status == CallCommandStatus::Accepted,
                                "Synthetic Answer rejected"
                            );
                            token = Some(owner_token.context("Synthetic missing owner token")?);
                        }
                        CallEvent::State { call } if call.call_id == offer.call_id => {
                            *h.owner_state.lock().unwrap() = Some(call.state);
                            if call.state == CallState::Connected {
                                connected = Some(call);
                            }
                        }
                        _ => {}
                    }
                    if let (Some(token), Some(call)) = (&token, &connected) {
                        return Ok::<_, anyhow::Error>((token.clone(), call.clone()));
                    }
                }
            })
            .await
            .context("Synthetic transport did not connect")??;
            let mut media = service
                .attach_media("one", "fixture", &offer.call_id, &client.client_id, &token)
                .await?;
            let CallEvent::MediaReady {
                send_track,
                receive_track,
            } = &media.ready
            else {
                bail!("Synthetic missing tracks")
            };
            ensure!(
                send_track.format == audio_format
                    && receive_track.format == audio_format
                    && send_track.direction == CallTrackDirection::Send
                    && receive_track.direction == CallTrackDirection::Receive,
                "Synthetic announced track format/direction"
            );
            let send_track = send_track.clone();
            let receive_track = receive_track.clone();
            let worker = f
                .backend
                .inner
                .active
                .lock()
                .unwrap()
                .clone()
                .context("Synthetic missing worker")?;
            let epoch = MediaEpoch {
                account_generation: nonzero64(worker.lease.authority().generation.as_bytes()),
                logout_epoch: 0,
                call_generation: nonzero64(worker.identity.call_id().as_bytes()),
                key_generation: 1,
            };
            h.peer.start.send((format, epoch))?;
            let media_id = media.id.clone();
            let feed = async {
                let mut tone = tone(format, OWNER_HZ)?;
                for n in 0..FRAMES {
                    let raw = frame::encode_pcm(
                        MediaHeader {
                            track_id: send_track.id,
                            generation: send_track.generation,
                            sequence: n as u64,
                            timestamp_micros: n as u64 * u64::from(audio_format.frame_ms) * 1000,
                        },
                        format,
                        &tone.next_frame()?,
                    )?;
                    service.media_frame(&media_id, &raw).await?;
                    tokio::time::sleep(Duration::from_millis(u64::from(audio_format.frame_ms)))
                        .await;
                }
                Ok::<_, anyhow::Error>(())
            };
            let receive = async {
                let mut samples = vec![];
                for n in 0..FRAMES {
                    let raw = media
                        .frames
                        .recv()
                        .await
                        .context("Synthetic owner media closed")?;
                    let frame = MediaFrame::decode(&raw, format)?;
                    ensure!(
                        frame.header.track_id == receive_track.id
                            && frame.header.generation == receive_track.generation
                            && frame.header.sequence == n as u64,
                        "Synthetic owner receive ordering"
                    );
                    samples.extend(frame.samples());
                }
                Ok::<_, anyhow::Error>(samples)
            };
            let (sent, received) = tokio::join!(feed, receive);
            sent?;
            let received = received?;
            let peer = h.peer.finish()?;
            check_tone(&received, format, PEER_HZ, OWNER_HZ);
            check_tone(&peer.samples, format, OWNER_HZ, PEER_HZ);
            assert_eq!(peer.checkins, 2);
            assert_eq!(peer.sent_primaries, FRAMES);
            assert!((FRAMES..=FRAMES + 8).contains(&peer.received_primaries));
            assert!(peer.overlaps > 0);
            // Delivery publishes PCM before the media worker updates its counter.
            tokio::time::timeout(Duration::from_millis(500), async {
                while worker.metrics.rx_delivered_frames.load(Ordering::Relaxed) < FRAMES as u64 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .context("Synthetic RX counter publication deadline")?;
            assert_eq!(
                worker.metrics.rx_delivered_frames.load(Ordering::Relaxed),
                FRAMES as u64
            );
            assert!(
                worker
                    .metrics
                    .tx_submitted_datagrams
                    .load(Ordering::Relaxed)
                    >= (FRAMES / 2) as u64
            );
            eprintln!("generated local media: owner_samples={}, peer_samples={}, peer_sent_primaries={}, peer_received_native_primaries={}, extra_native_primaries={}, other_datagrams={}, diagnostic_audio_overlaps={}", received.len(), peer.samples.len(), peer.sent_primaries, peer.received_primaries, peer.received_primaries - FRAMES, peer.other_datagrams, peer.overlaps);
            if frames_per_packet == 1 {
                service
                    .command(
                        &client.client_id,
                        CallCommand::Hangup {
                            request_id: "end".into(),
                            call_id: call.call_id,
                            expected_revision: call.revision,
                        },
                    )
                    .await?;
                assert_eq!(
                    result(&mut client, "end").await,
                    CallCommandStatus::Accepted
                );
            }
            f.finished().await;
            ensure!(
                !f.core.store.has_unresolved_call("one")?,
                "Synthetic call journal unresolved"
            );
            let seen = f.host.seen();
            assert_eq!(seen.iter().filter(|(_, cmd)| *cmd == Some(3380)).count(), 1);
            assert!(!seen
                .iter()
                .any(|(route, _)| matches!(route, CallHostRoute::Invite | CallHostRoute::Cancel)));
            let records = f.core.store.call_action_records("one", &offer.call_id)?;
            assert!(records.iter().all(|record| {
                record.encrypted_request.is_some()
                    && ((record.outcome == "received" && record.encrypted_reply.is_some())
                        || (record.route == 9262
                            && record.outcome == "not_submitted"
                            && record.encrypted_reply.is_none()))
            }));
            Ok::<_, anyhow::Error>(())
        };
        let outcome = tokio::time::timeout(Duration::from_secs(8), test).await;
        if !matches!(&outcome, Ok(Ok(()))) {
            h.diagnose();
        }
        outcome.context("Generated native-media test deadline")??;
    }
    Ok(())
}
