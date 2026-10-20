use super::*;
use std::sync::atomic::{AtomicI32, Ordering};
static EXIT: AtomicI32 = AtomicI32::new(0);
pub fn set_exit(code: i32) {
    EXIT.store(code, Ordering::Relaxed);
}
pub fn exit() -> i32 {
    EXIT.load(Ordering::Relaxed)
}
#[derive(Debug)]
struct Error {
    exit: i32,
    code: String,
    message: String,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
pub fn failure(exit: i32, code: &str, message: &str) -> anyhow::Error {
    Error {
        exit,
        code: code.into(),
        message: message.into(),
    }
    .into()
}
pub fn classify(error: &anyhow::Error) -> (i32, String) {
    if let Some(error) = error.downcast_ref::<Error>() {
        return (error.exit, error.code.clone());
    }
    if error.downcast_ref::<reqwest::Error>().is_some() {
        return (3, "endpoint_unavailable".into());
    }
    let message = error.to_string();
    if message.contains("unsupported_capability") {
        return (9, "unsupported_capability".into());
    }
    if message.contains("needs_login") {
        return (5, "needs_login".into());
    }
    if message.contains("account_ambiguous") {
        return (2, "account_ambiguous".into());
    }
    if message.contains("account_not_") {
        return (2, "account_not_selected".into());
    }
    if error.downcast_ref::<std::io::Error>().is_some()
        || [
            "config",
            "must ",
            "invalid ",
            "select ",
            "choose ",
            "required",
            "conflict",
            "already exists",
            "not found",
        ]
        .iter()
        .any(|s| message.contains(s))
    {
        return (2, "invalid_configuration_or_input".into());
    }
    (1, "operation_failed".into())
}
pub fn result_exit(value: &Value) -> i32 {
    if value["clientWaitExpired"] == true {
        return 7;
    }
    if value["clientInterrupted"] == true {
        return 130;
    }
    match value["state"].as_str() {
        Some("needs_review" | "unknown") => 8,
        Some("needs_login") => 5,
        Some("approval_required" | "challenge_required") => 6,
        Some(
            "failed" | "rejected" | "cancelled" | "expired" | "unavailable" | "unsupported"
            | "not_found" | "not_cached",
        ) => 9,
        _ => 0,
    }
}
pub fn record(value: &Value) {
    if exit() == 0 {
        set_exit(result_exit(value));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uncertain_and_pending_results_are_distinct_from_success() {
        assert_eq!(result_exit(&json!({"state":"queued"})), 0);
        assert_eq!(
            result_exit(&json!({"state":"queued","clientWaitExpired":true})),
            7
        );
        assert_eq!(result_exit(&json!({"state":"needs_review"})), 8);
        assert_eq!(result_exit(&json!({"state":"needs_login"})), 5);
        assert_eq!(result_exit(&json!({"state":"expired"})), 9);
    }
    #[test]
    fn access_and_endpoint_errors_are_separate() {
        assert_eq!(classify(&failure(4, "unauthorized", "access denied")).0, 4);
        assert_eq!(
            classify(&anyhow::anyhow!("account_ambiguous: choose one")).0,
            2
        );
    }
}
