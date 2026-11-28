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
    retryable: Option<bool>,
    next: Vec<String>,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

/// Structured classification of a CLI failure for exit status, JSON output,
/// and human next-step hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedError {
    pub exit: i32,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub next: Vec<String>,
}

pub fn failure(exit: i32, code: &str, message: &str) -> anyhow::Error {
    Error {
        exit,
        code: code.into(),
        message: message.into(),
        retryable: None,
        next: Vec::new(),
    }
    .into()
}

pub fn failure_with_next(
    exit: i32,
    code: &str,
    message: impl Into<String>,
    retryable: bool,
    next: Vec<String>,
) -> anyhow::Error {
    Error {
        exit,
        code: code.into(),
        message: message.into(),
        retryable: Some(retryable),
        next,
    }
    .into()
}

pub fn classify(error: &anyhow::Error) -> (i32, String) {
    let described = describe(error);
    (described.exit, described.code)
}

pub fn describe(error: &anyhow::Error) -> ClassifiedError {
    if let Some(structured) = error.downcast_ref::<Error>() {
        let message = if error.chain().count() > 1 {
            format!("{error:#}")
        } else {
            structured.message.clone()
        };
        return ClassifiedError {
            exit: structured.exit,
            code: structured.code.clone(),
            message,
            retryable: structured.retryable.unwrap_or(structured.exit == 3),
            next: structured.next.clone(),
        };
    }
    if error.downcast_ref::<reqwest::Error>().is_some() {
        return ClassifiedError {
            exit: 3,
            code: "endpoint_unavailable".into(),
            message: format!("{error:#}"),
            retryable: true,
            next: Vec::new(),
        };
    }
    let message = error.to_string();
    let full = format!("{error:#}");
    let (exit, code) = if message.contains("unsupported_capability") {
        (9, "unsupported_capability")
    } else if message.contains("needs_login") {
        (5, "needs_login")
    } else if message.contains("account_ambiguous") {
        (2, "account_ambiguous")
    } else if message.contains("account_not_configured") {
        (2, "account_not_configured")
    } else if message.contains("account_not_found") {
        (2, "account_not_found")
    } else if message.contains("account_not_") {
        (2, "account_not_selected")
    } else if error.downcast_ref::<std::io::Error>().is_some()
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
        (2, "invalid_configuration_or_input")
    } else {
        (1, "operation_failed")
    };
    ClassifiedError {
        exit,
        code: code.into(),
        message: full,
        retryable: exit == 3,
        next: Vec::new(),
    }
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
        assert_eq!(
            classify(&anyhow::anyhow!("account_not_configured: none")).1,
            "account_not_configured"
        );
    }
    #[test]
    fn structured_errors_preserve_retryable_and_next_steps() {
        let error = failure_with_next(
            3,
            "daemon_not_running",
            "No wx daemon is listening on 127.0.0.1:6174.",
            true,
            vec!["wx daemon start".into()],
        );
        let described = describe(&error);
        assert_eq!(described.exit, 3);
        assert_eq!(described.code, "daemon_not_running");
        assert!(described.retryable);
        assert_eq!(described.next, vec!["wx daemon start"]);
    }
}
