//! Shared ephemeral login for the network examples. Never serialize credentials.
use anyhow::{bail, ensure, Result};
use std::{
    io::Write,
    time::{Duration, Instant},
};
use wechat_protocol::{
    is_retryable_login_poll_error, transport::ProtocolTransport, DeviceProfile, LoginStatus,
    NativeClient, NativeOptions,
};

pub async fn login() -> Result<NativeClient> {
    // Tokio TCP streams and pooled HTTP/1.1; answers LAN risk challenges too.
    let transport = ProtocolTransport::native()?;
    let client = NativeClient::with_transport(NativeOptions::default(), transport.clone())?;
    let device = DeviceProfile::generate("UnifiedPCLinux x86_64".into())?;
    let qr_directory = wechat_platform::private_tempdir("wechat-protocol-example-")?;
    let qr_path = qr_directory.path().join("login.png");
    let mut attempt = client
        .start_qr_login_async(device, "Rust protocol example")
        .await?;
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
        match client
            .poll_login_guarded_async(&mut attempt, deadline, || false)
            .await
        {
            Ok(LoginStatus::Approved) => {
                let state = client.complete_login_async(&mut attempt).await?;
                eprintln!("Login complete; credentials remain in memory.");
                return NativeClient::from_account_with_transport(
                    state,
                    NativeOptions::default(),
                    transport,
                );
            }
            Ok(LoginStatus::Pending | LoginStatus::Scanned) => {}
            Ok(status) => bail!("Login stopped: {status:?}"),
            Err(error) if is_retryable_login_poll_error(&error) => {}
            Err(error) => return Err(error),
        }
        // Only polling an already issued challenge is repeated. QR creation,
        // credential exchange and sends are never automatically resubmitted.
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
