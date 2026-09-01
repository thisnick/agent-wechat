use super::exec::{exec_command, ExecOptions};
use base64::Engine;

/// Capture a screenshot and return as base64-encoded PNG.
pub async fn capture_screenshot(options: &ExecOptions) -> Result<String, String> {
    let result = exec_command("screenshot", &[], options).await;

    if result.exit_code != 0 {
        return Err(format!("Screenshot failed: {}", result.stderr));
    }

    let filepath = result.stdout.trim().to_string();

    let buffer = tokio::fs::read(&filepath)
        .await
        .map_err(|e| format!("Failed to read screenshot: {e}"))?;

    // Clean up temp file
    let _ = tokio::fs::remove_file(&filepath).await;

    Ok(base64::engine::general_purpose::STANDARD.encode(&buffer))
}
