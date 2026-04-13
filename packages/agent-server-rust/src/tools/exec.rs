use crate::ia::types::Session;
use std::collections::HashMap;
use tokio::process::Command;

#[derive(Debug)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub struct ExecOptions {
    pub session: Option<Session>,
    pub timeout_ms: u64,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            session: None,
            timeout_ms: 60_000,
        }
    }
}

/// Execute a command with fixed arguments (no shell interpolation).
///
/// If a session is provided, the command runs with that session's
/// DISPLAY and DBUS_SESSION_BUS_ADDRESS environment.
pub async fn exec_command(command: &str, args: &[&str], options: &ExecOptions) -> CommandResult {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.insert("QT_ACCESSIBILITY".into(), "1".into());
    env.insert("QT_LINUX_ACCESSIBILITY_ALWAYS_ON".into(), "1".into());

    if let Some(session) = &options.session {
        env.insert("DISPLAY".into(), session.display.clone());
        env.insert(
            "DBUS_SESSION_BUS_ADDRESS".into(),
            session.dbus_address.clone().unwrap_or_default(),
        );
        env.insert("HOME".into(), format!("/home/{}", session.linux_user));
    } else {
        env.entry("DISPLAY".into()).or_insert_with(|| ":99".into());
    }

    let timeout = std::time::Duration::from_millis(options.timeout_ms);

    let result = tokio::time::timeout(timeout, async {
        let output = Command::new(command).args(args).envs(&env).output().await;

        match output {
            Ok(out) => CommandResult {
                stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
                exit_code: out.status.code().unwrap_or(1),
            },
            Err(err) => CommandResult {
                stdout: String::new(),
                stderr: err.to_string(),
                exit_code: 1,
            },
        }
    })
    .await;

    match result {
        Ok(r) => r,
        Err(_) => CommandResult {
            stdout: String::new(),
            stderr: "Command timed out".to_string(),
            exit_code: 1,
        },
    }
}
