use super::*;
use crate::{crypto, wire};
use std::io::Read;

fn fixture(name: &str) -> Vec<u8> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/lan-login.json")).unwrap();
    hex::decode(fixture[name].as_str().unwrap()).unwrap()
}
fn device() -> DeviceProfile {
    DeviceProfile {
        device_id: b"0123456789abcdef".to_vec(),
        device_type: "UnifiedPCLinux fixture".into(),
        client_version: -227271398,
    }
}
fn poll(plain: &[u8]) -> Result<wire::PollResponse> {
    let ciphertext = crypto::cbc(plain, &[7; 16], openssl::symm::Mode::Encrypt).unwrap();
    let reply = p::message(&[
        p::bytes(1, &p::uint(1, 0)),
        p::bytes(
            3,
            &p::message(&[p::bytes(1, &p::buffer(&ciphertext)), p::uint(2, 0)]).unwrap(),
        ),
    ])
    .unwrap();
    wire::decode_poll(&reply, &[7; 16])
}
#[test]
fn native_lan_request_matches_independent_numbered_fixture() {
    let actual = request(
        &device(),
        b"synthetic-qr",
        &[Ipv4Addr::new(192, 168, 1, 23)],
        8050,
        &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    )
    .unwrap();
    assert_eq!(actual, fixture("requestHex"));
    assert_eq!(CGI, 22312);
    assert_eq!(PATH, "/cgi-bin/micromsg-bin/extdeviceuploadlaninfo");
}
#[test]
fn native_risk_extension_is_independent_of_ordinary_qr_status() {
    let notification = poll(&fixture("notifyHex")).unwrap();
    assert_eq!(notification.status, Some(1));
    assert!(notification.lan_challenge);
    assert!(!poll(&p::uint(2, 11)).unwrap().lan_challenge);
    for state in [0, 10, 12, 999] {
        let notify =
            p::message(&[p::uint(2, 1), p::bytes(17, &p::buffer(&p::uint(1, state)))]).unwrap();
        assert!(!poll(&notify).unwrap().lan_challenge);
    }
    let mut approval_challenge = fixture("notifyHex");
    approval_challenge[1] = 2;
    assert!(poll(&approval_challenge).unwrap().lan_challenge);
    assert!(poll(&p::uint(2, 2)).is_err()); // Final approval still needs credentials.
}
#[test]
fn malformed_or_ambiguous_risk_extension_fails_closed() {
    let invalid = [
        p::uint(17, 11),
        p::bytes(17, &[8, 50, 18, 2, 8, 11]), // SKBuffer length mismatch.
        p::bytes(17, &p::buffer(&[8, 11, 8, 12])),
        p::bytes(17, &p::buffer(&p::bytes(1, b"11"))),
        [
            p::bytes(17, &p::buffer(&[8, 11])),
            p::bytes(17, &p::buffer(&[8, 11])),
        ]
        .concat(),
        p::bytes(17, &vec![0; 4097]),
    ];
    for extension in invalid {
        assert!(poll(&[p::uint(2, 1), extension].concat()).is_err());
    }
    let notify = p::message(&[
        p::uint(2, 1),
        p::bytes(
            17,
            &p::buffer(&p::message(&[p::uint(1, 11), p::bytes(500, b"future field")]).unwrap()),
        ),
    ])
    .unwrap();
    assert!(poll(&notify).unwrap().lan_challenge);
}
#[test]
fn token_reply_matches_total_length_framing_and_string_scene_fixture() {
    let token = response(&fixture("responseHex")).unwrap();
    assert_eq!(reply_frame(&token).unwrap().as_slice(), fixture("frameHex"));
    let escaped = reply_frame(b"quoted\"\\\n-token").unwrap();
    assert_eq!(
        u32::from_be_bytes(escaped[..4].try_into().unwrap()) as usize,
        escaped.len()
    );
    let value: serde_json::Value = serde_json::from_slice(&escaped[4..]).unwrap();
    assert_eq!(value["scene"], "xwechat");
    assert_eq!(value["token"], "quoted\"\\\n-token");
}
#[test]
fn rejected_missing_ambiguous_or_oversized_tokens_never_arm_a_reply() {
    for reply in [
        p::message(&[p::bytes(1, &p::int32(1, -1)), p::bytes(2, b"ignored")]).unwrap(),
        p::bytes(1, &p::uint(1, 0)),
        [fixture("responseHex"), p::bytes(2, b"second")].concat(),
        p::message(&[p::bytes(1, &p::uint(1, 0)), p::bytes(2, b"")]).unwrap(),
        p::message(&[p::bytes(1, &p::uint(1, 0)), p::bytes(2, &[255])]).unwrap(),
        p::message(&[
            p::bytes(1, &p::uint(1, 0)),
            p::bytes(2, &vec![b'a'; MAX_TOKEN + 1]),
        ])
        .unwrap(),
    ] {
        assert!(response(&reply).is_err());
    }
    assert!(response(&vec![0; 16385]).is_err());
}

fn listener(scope: Scope) -> (Challenge, std::net::SocketAddr) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = listener.local_addr().unwrap();
    (
        Challenge::from_listener(listener, vec![Ipv4Addr::LOCALHOST], scope).unwrap(),
        address,
    )
}
fn stopped(challenge: &Challenge) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !challenge.state.done.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::sleep(TICK);
    }
    assert!(challenge.state.done.load(Ordering::Acquire));
}
#[test]
fn simulated_phone_waits_for_upload_then_receives_exact_native_frame() {
    let (challenge, address) =
        listener(Scope::new(Instant::now() + Duration::from_secs(5), || {
            false
        }));
    let mut phone = TcpStream::connect(address).unwrap();
    phone
        .set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    let error = phone.read(&mut [0; 1]).unwrap_err();
    assert!(matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ));
    challenge
        .arm(&response(&fixture("responseHex")).unwrap())
        .unwrap();
    phone
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut prefix = [0; 4];
    phone.read_exact(&mut prefix).unwrap();
    let length = u32::from_be_bytes(prefix) as usize;
    assert_eq!(length, fixture("frameHex").len());
    let mut frame = prefix.to_vec();
    frame.resize(length, 0);
    phone.read_exact(&mut frame[4..]).unwrap();
    assert_eq!(frame, fixture("frameHex"));
    let deadline = Instant::now() + Duration::from_secs(2);
    while challenge.status() != "token_sent" && Instant::now() < deadline {
        thread::sleep(TICK);
    }
    assert_eq!(challenge.status(), "token_sent");
    assert!(challenge.arm(b"second token").is_err());
    drop(phone);
    drop(challenge);
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
}
#[test]
fn cancellation_closes_listener_before_upload_can_publish_a_token() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = cancelled.clone();
    let (challenge, address) = listener(Scope::new(
        Instant::now() + Duration::from_secs(5),
        move || guard.load(Ordering::Acquire),
    ));
    cancelled.store(true, Ordering::Release);
    stopped(&challenge);
    assert!(challenge.arm(b"late token").is_err());
    assert_eq!(challenge.status(), "stopped");
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
}
#[test]
fn deadline_closes_armed_listener_without_waiting_for_another_poll() {
    let (challenge, address) = listener(Scope::new(
        Instant::now() + Duration::from_millis(500),
        || false,
    ));
    challenge.arm(b"short lived token").unwrap();
    stopped(&challenge);
    assert!(TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_err());
    assert!(challenge.arm(b"late token").is_err());
}
#[test]
fn occupied_ports_are_skipped_and_an_exhausted_range_is_reported() {
    let occupied = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = occupied.local_addr().unwrap().port();
    let listener = bind_ports([port, 0]).unwrap();
    assert_ne!(listener.local_addr().unwrap().port(), port);
    let error = bind_ports([port]).unwrap_err();
    assert_eq!(login_lan_error_reason(&error), Some("ports_unavailable"));
}
