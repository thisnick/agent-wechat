//! Shared ephemeral login for the network examples. Never serialize credentials.
use anyhow::{bail, ensure, Result};
use std::{
    io::Write,
    thread,
    time::{Duration, Instant},
};
use wechat_protocol::{
    is_retryable_login_poll_error, AccountState, DeviceProfile, LoginStatus, NativeClient,
    NativeOptions,
};

pub fn login() -> Result<(NativeClient, AccountState)> {
    let client = NativeClient::new(NativeOptions::default())?;
    let device = DeviceProfile::generate("UnifiedPCLinux x86_64".into())?;
    let qr_directory = wechat_platform::private_tempdir("wechat-protocol-example-")?;
    let qr_path = qr_directory.path().join("login.png");
    let mut attempt = client.start_qr_login(device, "Rust protocol example")?;
    let mut qr = wechat_platform::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&qr_path)?;
    qr.write_all(&attempt.qr_png)?;
    drop(qr);
    eprintln!("Scan the private QR image: {}", qr_path.display());
    eprintln!("Approve this example on the test account's phone.");
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        ensure!(Instant::now() < deadline, "Login example timed out");
        match client.poll_login_guarded(&mut attempt, deadline, || false) {
            Ok(LoginStatus::Approved) => {
                let state = client.complete_login(&mut attempt)?;
                eprintln!("Login complete; credentials remain in memory.");
                return Ok((client, state));
            }
            Ok(LoginStatus::Pending | LoginStatus::Scanned) => {}
            Ok(status) => bail!("Login stopped: {status:?}"),
            Err(error) if is_retryable_login_poll_error(&error) => {}
            Err(error) => return Err(error),
        }
        // Only polling an already issued challenge is repeated. QR creation,
        // credential exchange and sends are never automatically resubmitted.
        thread::sleep(Duration::from_secs(2));
    }
}
