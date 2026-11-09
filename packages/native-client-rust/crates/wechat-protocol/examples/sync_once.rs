//! Inspect one initial sync page after QR login. No cursor or history is committed.
mod support;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().skip(1).any(|arg| arg == "--help") {
        println!("sync_once: approve a test-account QR login and summarize one initial sync page.");
        return Ok(());
    }
    let client = tokio::task::spawn_blocking(support::login).await??;
    let identity = client.account_identity()?;
    let batch = client.sync(&[]).await?;
    let mut messages = 0;
    for command in &batch.commands {
        if command.id == 5 {
            let _message = wechat_protocol::wire::decode_add_message(
                &command.payload,
                &identity.own_username,
            )?;
            messages += 1;
        }
    }
    println!(
        "commands={} add_messages={} other_commands={} continuation={}",
        batch.commands.len(),
        messages,
        batch.commands.len() - messages,
        batch.continuation
    );
    // This is a preview, not a sync consumer: all raw commands remain in `batch`,
    // and we never save its returned cursor. A persistent client must retain and
    // handle every required command before committing that cursor.
    println!("Preview only. No history or sync cursor was saved.");
    client.shutdown().await?;
    Ok(())
}
