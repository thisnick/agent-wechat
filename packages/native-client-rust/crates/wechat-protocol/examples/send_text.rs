//! Approve a fresh QR login and submit one text message, with no automatic resend.
mod support;

use anyhow::{ensure, Context};
use wechat_protocol::{wire, TextSendOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help") {
        println!("send_text RECIPIENT TASK_ID TEXT\nUses a fresh test-account QR login and submits once.");
        return Ok(());
    }
    ensure!(args.len() == 3, "Usage: send_text RECIPIENT TASK_ID TEXT");
    let (recipient, task_id, text) = (&args[0], &args[1], &args[2]);
    let client_id = wire::client_message_id(task_id.as_bytes());
    ensure!(client_id != 0, "Choose another nonempty task ID");
    // Validate arguments before requesting any login. A real application records
    // this operation and timestamp before submitting it, and retains its result.
    wire::text_send_request(recipient, text, wire::unix_time()?, client_id)?;
    let client = tokio::task::spawn_blocking(support::login).await??;
    let created_at = wire::unix_time()?;
    let receipt = client
        .send_text(
            recipient,
            text,
            TextSendOptions {
                created_at,
                client_message_id: client_id,
                source: None,
            },
        )
        .await
        .context(
            "Send outcome may be uncertain. Investigate before submitting this operation again",
        )?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    ensure!(receipt.accepted, "The server rejected this text send");
    ensure!(
        receipt.returned_destination_matches && receipt.returned_client_id_matches,
        "Receipt identity mismatch; investigate before submitting again"
    );
    client.shutdown().await?;
    Ok(())
}
