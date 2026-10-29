//! Approve one fresh QR login; keep credentials only until this process exits.
mod support;

fn main() -> anyhow::Result<()> {
    if std::env::args().skip(1).any(|arg| arg == "--help") {
        println!("qr_login: approve an ephemeral test-account QR login; no messages are sent.");
        return Ok(());
    }
    let (_client, _state) = support::login()?;
    println!("Authenticated. No account state was saved.");
    Ok(())
}
