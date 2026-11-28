//! Shared connection failure classification, `wx status` client metadata, and
//! interactive first-run prompts.
use crate::{outcome, port_owner::PortOwner, settings::EndpointSource};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::{
    io::{BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// Resolved client connection target used for diagnostics and `wx status`.
#[derive(Debug, Clone)]
pub struct ClientTarget {
    pub endpoint: String,
    pub server_name: Option<String>,
    pub source: EndpointSource,
    /// Token file path in use, or `None` when the token came from the
    /// environment or `--token-stdin`.
    pub token_file: Option<PathBuf>,
    /// Underlying token file path configured for this endpoint (used in error
    /// messages when reading the token file fails).
    pub configured_token_file: PathBuf,
    pub account: Option<String>,
    pub ca_cert_file: Option<PathBuf>,
    /// True when the CLI is targeting the local daemon's default address and
    /// token file without an explicit `--server`, `--server-name`, saved
    /// server, or token override.
    pub is_local_default: bool,
}

impl ClientTarget {
    pub fn parsed_url(&self) -> Option<reqwest::Url> {
        reqwest::Url::parse(&format!("{}/", self.endpoint.trim_end_matches('/'))).ok()
    }

    pub fn is_loopback(&self) -> bool {
        self.parsed_url().is_some_and(|url| is_loopback_url(&url))
    }

    pub fn host_port(&self) -> (String, u16) {
        if let Some(url) = self.parsed_url() {
            let host = url.host_str().unwrap_or("127.0.0.1").to_owned();
            let port = url.port_or_known_default().unwrap_or(6174);
            (host, port)
        } else {
            (self.endpoint.clone(), 6174)
        }
    }

    pub fn display_address(&self) -> String {
        let (host, port) = self.host_port();
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }

    /// Valid `SocketAddr` text suitable for `wx --listen <ADDR>`.
    pub fn listen_hint_address(&self) -> String {
        let (host, port) = self.host_port();
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if bare.eq_ignore_ascii_case("localhost") {
            format!("127.0.0.1:{port}")
        } else if bare.contains(':') {
            format!("[{bare}]:{port}")
        } else {
            format!("{bare}:{port}")
        }
    }

    /// Build the `client` object merged into `wx status` output.
    pub fn status_client_json(&self) -> Value {
        json!({
            "serverName": self.server_name,
            "url": self.endpoint,
            "source": self.source.as_str(),
            "tokenFile": self.token_file,
            "account": self.account,
            "version": env!("CARGO_PKG_VERSION"),
        })
    }
}

pub fn is_loopback_url(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        bare.eq_ignore_ascii_case("localhost")
            || bare
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

/// Pure gating check for interactive terminal prompts.
pub fn can_prompt(
    stdin_is_terminal: bool,
    stderr_is_terminal: bool,
    json: bool,
    ci: Option<&str>,
) -> bool {
    stdin_is_terminal
        && stderr_is_terminal
        && !json
        && ci.is_none_or(|value| value.trim().is_empty())
}

pub fn is_interactive(json: bool) -> bool {
    can_prompt(
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
        json,
        std::env::var("CI").ok().as_deref(),
    )
}

/// Result of an unauthenticated `GET /health` probe against a loopback address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthProbe {
    /// Endpoint answered `200 OK` with `{"status":"ok"}`.
    WxOk,
    /// Nothing accepted a TCP connection on that address.
    NotListening,
    /// TCP accepted the connection, but HTTPS/TLS handshake or certificate verification failed.
    TlsError,
    /// Something accepted the connection, but did not return a wx `/health` OK response.
    OtherService { status: Option<u16> },
}

/// What failed before invoking the classifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionFailure {
    MissingOrInvalidToken,
    ConnectError(String),
    Unauthorized,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClassificationOptions {
    pub is_status_command: bool,
}

/// Unauthenticated loopback probe against `GET /health`.
pub async fn probe_health(url: &reqwest::Url, ca_cert_file: Option<&Path>) -> HealthProbe {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2));
    if let Some(path) = ca_cert_file {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(certificate) = reqwest::Certificate::from_pem(&bytes) {
                builder = builder.add_root_certificate(certificate);
            }
        }
    }
    let Ok(client) = builder.build() else {
        return HealthProbe::OtherService { status: None };
    };
    let Ok(health_url) = url.join("health") else {
        return HealthProbe::OtherService { status: None };
    };
    match client.get(health_url).send().await {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                if let Ok(body) = response.json::<Value>().await {
                    if body["status"].as_str() == Some("ok") {
                        return HealthProbe::WxOk;
                    }
                }
            }
            HealthProbe::OtherService {
                status: Some(status.as_u16()),
            }
        }
        Err(error) => {
            if error.is_connect() {
                if url.scheme() == "https" {
                    let host = url
                        .host_str()
                        .unwrap_or("127.0.0.1")
                        .trim_start_matches('[')
                        .trim_end_matches(']');
                    let port = url.port_or_known_default().unwrap_or(443);
                    if tokio::time::timeout(
                        Duration::from_secs(1),
                        tokio::net::TcpStream::connect((host, port)),
                    )
                    .await
                    .is_ok_and(|res| res.is_ok())
                    {
                        return HealthProbe::TlsError;
                    }
                }
                HealthProbe::NotListening
            } else {
                HealthProbe::OtherService { status: None }
            }
        }
    }
}

/// Probe (on loopback) and classify a connection or authentication failure.
pub async fn diagnose(
    target: &ClientTarget,
    failure: ConnectionFailure,
    options: ClassificationOptions,
) -> (anyhow::Error, Option<HealthProbe>) {
    if !target.is_loopback() {
        return (classify_failure(target, failure, None, None, options), None);
    }
    let probe = match target.parsed_url() {
        Some(url) => probe_health(&url, target.ca_cert_file.as_deref()).await,
        None => HealthProbe::NotListening,
    };
    let owner = match probe {
        HealthProbe::WxOk | HealthProbe::TlsError | HealthProbe::OtherService { .. } => {
            Some(crate::port_owner::identify(target.host_port().1))
        }
        HealthProbe::NotListening => None,
    };
    let error = classify_failure(target, failure, Some(probe.clone()), owner, options);
    (error, Some(probe))
}

fn quote_path(path: &Path) -> String {
    let display = path.display().to_string();
    if display.chars().any(char::is_whitespace) {
        format!("'{}'", display.replace('\'', "'\\''"))
    } else {
        display
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Pure connection-failure classifier; all OS/network probing happens outside
/// so every branch can be unit-tested deterministically.
pub fn classify_failure(
    target: &ClientTarget,
    failure: ConnectionFailure,
    probe: Option<HealthProbe>,
    owner: Option<PortOwner>,
    options: ClassificationOptions,
) -> anyhow::Error {
    if !target.is_loopback() {
        return classify_remote_failure(target, failure);
    }
    let address = target.display_address();
    let mut next: Vec<String> = Vec::new();
    match probe {
        Some(HealthProbe::NotListening) => {
            if target.is_local_default {
                next.push("wx daemon start".into());
            } else if target.host_port().1 != 6174 {
                next.push(format!(
                    "wx --listen {} daemon start",
                    target.listen_hint_address()
                ));
            } else {
                next.push("wx daemon start".into());
            }
            if options.is_status_command {
                next.push("wx daemon doctor".into());
            }
            outcome::failure_with_next(
                3,
                "daemon_not_running",
                format!("No wx daemon is listening on {address}."),
                true,
                next,
            )
        }
        Some(HealthProbe::TlsError) => {
            next.push(format!(
                "wx --server {} --ca-cert-file /path/to/certificate.pem status",
                target.endpoint
            ));
            if options.is_status_command {
                next.push("wx daemon doctor".into());
            }
            outcome::failure_with_next(
                3,
                "endpoint_unavailable",
                format!(
                    "A server is listening on {address}, but TLS verification failed. Pass --ca-cert-file if it uses a private certificate."
                ),
                false,
                next,
            )
        }
        Some(HealthProbe::WxOk) => match failure {
            ConnectionFailure::MissingOrInvalidToken | ConnectionFailure::Unauthorized => {
                let (code, reason) = match failure {
                    ConnectionFailure::MissingOrInvalidToken => (
                        "invalid_api_token",
                        format!(
                            "this CLI does not have its access key (token file {} is missing or invalid)",
                            target.configured_token_file.display()
                        ),
                    ),
                    ConnectionFailure::Unauthorized => (
                        "unauthorized",
                        "it rejected this CLI's API token".to_owned(),
                    ),
                    ConnectionFailure::ConnectError(_) => unreachable!(),
                };
                match owner {
                    Some(PortOwner::DesktopHub { .. }) => {
                        next.push("In AgentWechat, open Settings → Export key for CLI".into());
                        next.push(format!(
                            "wx config servers add desktop --url {} --token-file /path/to/exported-key",
                            target.endpoint
                        ));
                        next.push("wx config servers select desktop".into());
                        if options.is_status_command {
                            next.push("wx daemon doctor".into());
                        }
                        outcome::failure_with_next(
                            4,
                            code,
                            format!(
                                "The AgentWechat desktop app's hub is listening on {address}, and {reason}. Open AgentWechat and go to Settings → Export key for CLI."
                            ),
                            false,
                            next,
                        )
                    }
                    Some(
                        ref daemon @ PortOwner::WxDaemon {
                            ref data_dir,
                            exit_with_parent,
                            ..
                        },
                    ) => {
                        let owner_desc = daemon
                            .describe()
                            .unwrap_or_else(|| "another wx daemon".into());
                        if let Some(dir) = data_dir {
                            if !exit_with_parent {
                                next.push(format!(
                                    "wx --token-file {} status",
                                    quote_path(&dir.join("api-token"))
                                ));
                            } else {
                                next.push(
                                    "Pass --token-file <PATH> or set AGENT_WECHAT_TOKEN for that daemon"
                                        .into(),
                                );
                            }
                        } else {
                            next.push("Pass --token-file <PATH> or set AGENT_WECHAT_TOKEN".into());
                        }
                        if options.is_status_command {
                            next.push("wx daemon doctor".into());
                        } else {
                            next.push("wx status".into());
                        }
                        outcome::failure_with_next(
                            4,
                            code,
                            format!(
                                "A daemon ({owner_desc}) is listening on {address}, and {reason}."
                            ),
                            false,
                            next,
                        )
                    }
                    _ => {
                        next.push("Pass --token-file <PATH> or set AGENT_WECHAT_TOKEN".into());
                        if options.is_status_command {
                            next.push("wx daemon doctor".into());
                        } else {
                            next.push("wx status".into());
                        }
                        outcome::failure_with_next(
                            4,
                            code,
                            format!("A wx daemon is listening on {address}, and {reason}."),
                            false,
                            next,
                        )
                    }
                }
            }
            ConnectionFailure::ConnectError(detail) => {
                if options.is_status_command {
                    next.push("wx daemon doctor".into());
                }
                outcome::failure_with_next(
                    3,
                    "endpoint_unavailable",
                    format!("Cannot reach wx daemon at {} ({detail}).", target.endpoint),
                    true,
                    next,
                )
            }
        },
        Some(HealthProbe::OtherService { status }) => {
            let suffix = status
                .map(|code| format!(" (GET /health returned HTTP {code})"))
                .unwrap_or_default();
            let alt_port = if target.host_port().1 == 6175 {
                6176
            } else {
                6175
            };
            let is_wx_owner = matches!(
                owner,
                Some(PortOwner::DesktopHub { .. } | PortOwner::WxDaemon { .. })
            );
            let owner_desc = match owner.as_ref() {
                Some(owner @ (PortOwner::DesktopHub { .. } | PortOwner::WxDaemon { .. })) => {
                    capitalize(owner.describe().as_deref().unwrap_or("another wx daemon"))
                }
                Some(owner) => owner.describe().unwrap_or_else(|| "Another service".into()),
                None => "Another service".into(),
            };
            let message = if is_wx_owner {
                format!("{owner_desc} is listening on {address}, but GET /health did not succeed{suffix}.")
            } else {
                format!("{owner_desc} is listening on {address}, not a wx daemon{suffix}.")
            };
            next.push(format!("wx --listen 127.0.0.1:{alt_port} daemon start"));
            if options.is_status_command {
                next.push("wx daemon doctor".into());
            } else {
                next.push("wx status".into());
            }
            outcome::failure_with_next(3, "endpoint_unavailable", message, false, next)
        }
        None => match failure {
            ConnectionFailure::MissingOrInvalidToken => outcome::failure_with_next(
                4,
                "invalid_api_token",
                format!(
                    "API bearer token file is missing or invalid ({}).",
                    target.configured_token_file.display()
                ),
                false,
                if options.is_status_command {
                    vec!["wx daemon doctor".into()]
                } else {
                    vec![]
                },
            ),
            ConnectionFailure::Unauthorized => outcome::failure_with_next(
                4,
                "unauthorized",
                format!("The daemon on {address} rejected this CLI's API token."),
                false,
                if options.is_status_command {
                    vec!["wx daemon doctor".into()]
                } else {
                    vec!["wx status".into()]
                },
            ),
            ConnectionFailure::ConnectError(detail) => outcome::failure_with_next(
                3,
                "endpoint_unavailable",
                format!("Cannot reach wx daemon at {} ({detail}).", target.endpoint),
                true,
                if options.is_status_command {
                    vec!["wx daemon doctor".into()]
                } else {
                    vec![]
                },
            ),
        },
    }
}

fn classify_remote_failure(target: &ClientTarget, failure: ConnectionFailure) -> anyhow::Error {
    match failure {
        ConnectionFailure::MissingOrInvalidToken => outcome::failure_with_next(
            4,
            "invalid_api_token",
            format!(
                "API bearer token file is missing or invalid ({}) for {}.",
                target.configured_token_file.display(),
                target.endpoint
            ),
            false,
            vec![
                "Set AGENT_WECHAT_TOKEN or pass --token-file <PATH>".into(),
                "wx config servers list".into(),
            ],
        ),
        ConnectionFailure::ConnectError(detail) => outcome::failure_with_next(
            3,
            "endpoint_unavailable",
            format!("Cannot reach wx server at {} ({detail}).", target.endpoint),
            true,
            vec![
                format!("Check that {} is running and reachable", target.endpoint),
                "wx config servers list".into(),
            ],
        ),
        ConnectionFailure::Unauthorized => outcome::failure_with_next(
            4,
            "unauthorized",
            format!(
                "The wx server at {} rejected this CLI's API token.",
                target.endpoint
            ),
            false,
            vec![
                "Check AGENT_WECHAT_TOKEN or the server's token file".into(),
                "wx config servers list".into(),
            ],
        ),
    }
}

pub fn account_not_configured_error() -> anyhow::Error {
    outcome::failure_with_next(
        2,
        "account_not_configured",
        "No account is configured on this server.",
        false,
        vec![
            "wx accounts create --alias me".into(),
            "wx auth login --official".into(),
        ],
    )
}

pub fn account_ambiguous_error() -> anyhow::Error {
    outcome::failure_with_next(
        2,
        "account_ambiguous",
        "Multiple accounts are configured; pass --account or select a default account.",
        false,
        vec![
            "wx accounts list".into(),
            "wx --account <ID_OR_ALIAS> <COMMAND>".into(),
            "wx accounts select <ID_OR_ALIAS>".into(),
        ],
    )
}

pub fn account_not_found_error(selected: &str) -> anyhow::Error {
    outcome::failure_with_next(
        2,
        "account_not_found",
        format!("Account or alias '{selected}' was not found on this server."),
        false,
        vec!["wx accounts list".into()],
    )
}

fn read_line(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut line = String::new();
    let count = reader
        .read_line(&mut line)
        .context("cannot read terminal input")?;
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim().to_owned()))
}

/// Interactive prompt asking whether to start the local daemon in the background.
pub fn prompt_start_daemon(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<bool> {
    write!(
        writer,
        "No wx daemon is running. Start one in the background now? [Y/n] "
    )?;
    writer.flush()?;
    let Some(answer) = read_line(reader)? else {
        return Ok(false);
    };
    Ok(answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Choice presented during interactive `wx auth login` when no account exists yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAccountChoice {
    CreateNew,
    UseOfficial,
    Cancel,
}

/// Returns whether the `--official` first-run choice should be offered.
pub fn official_choice_available(is_linux: bool, is_loopback: bool, home: Option<&Path>) -> bool {
    is_linux && is_loopback && home.is_some_and(|dir| dir.join("xwechat_files").is_dir())
}

pub fn prompt_no_account_choice(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    offer_official: bool,
) -> Result<NoAccountChoice> {
    writeln!(
        writer,
        "No account is configured yet. Choose how to set one up:"
    )?;
    writeln!(
        writer,
        "  1. Create a new profile (wx accounts create --alias me)"
    )?;
    if offer_official {
        writeln!(
            writer,
            "  2. Use existing WeChat for Linux files in ~/xwechat_files (--official)"
        )?;
        writeln!(writer, "  3. Cancel")?;
        write!(writer, "Choice [1/2/3]: ")?;
    } else {
        writeln!(writer, "  2. Cancel")?;
        write!(writer, "Choice [1/2]: ")?;
    }
    writer.flush()?;
    let Some(answer) = read_line(reader)? else {
        return Ok(NoAccountChoice::Cancel);
    };
    match (offer_official, answer.as_str()) {
        (_, "1") => Ok(NoAccountChoice::CreateNew),
        (true, "2") => Ok(NoAccountChoice::UseOfficial),
        _ => Ok(NoAccountChoice::Cancel),
    }
}

/// Backup command from `docs/src/content/docs/setup/connect.mdx`.
pub const OFFICIAL_BACKUP_COMMAND: &str =
    "tar -czf ~/wechat-profile-backup.tgz -C ~ xwechat_files .xwechat";

pub fn prompt_official_confirmation(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    running_wechat_pids: &[u32],
) -> Result<bool> {
    if !running_wechat_pids.is_empty() {
        let pids = running_wechat_pids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            writer,
            "Warning: WeChat appears to be running (PID {pids}). Quit WeChat before continuing."
        )?;
    }
    writeln!(
        writer,
        "Using existing files modifies ~/xwechat_files in place."
    )?;
    writeln!(
        writer,
        "Quit WeChat for Linux and back up your files first:"
    )?;
    writeln!(writer, "  {OFFICIAL_BACKUP_COMMAND}")?;
    write!(
        writer,
        "Type \"yes\" to confirm WeChat is closed and you have backed up your files: "
    )?;
    writer.flush()?;
    let Some(answer) = read_line(reader)? else {
        return Ok(false);
    };
    Ok(answer.eq_ignore_ascii_case("yes"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountOption {
    pub id: String,
    pub alias: Option<String>,
    pub state: Option<String>,
}

impl AccountOption {
    pub fn label(&self) -> String {
        match &self.alias {
            Some(alias) if !alias.is_empty() && alias != &self.id => {
                format!("{alias} ({})", self.id)
            }
            _ => self.id.clone(),
        }
    }
}

pub fn prompt_select_account(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    accounts: &[AccountOption],
) -> Result<Option<AccountOption>> {
    writeln!(writer, "Multiple accounts are configured. Choose one:")?;
    for (index, account) in accounts.iter().enumerate() {
        let suffix = account
            .state
            .as_deref()
            .filter(|state| !state.is_empty())
            .map(|state| format!(" [{state}]"))
            .unwrap_or_default();
        writeln!(writer, "  {}. {}{suffix}", index + 1, account.label())?;
    }
    write!(writer, "Account [1-{}]: ", accounts.len())?;
    writer.flush()?;
    let Some(answer) = read_line(reader)? else {
        return Ok(None);
    };
    if let Ok(number) = answer.parse::<usize>() {
        if (1..=accounts.len()).contains(&number) {
            return Ok(Some(accounts[number - 1].clone()));
        }
    }
    if let Some(found) = accounts
        .iter()
        .find(|account| account.id == answer || account.alias.as_deref() == Some(answer.as_str()))
    {
        return Ok(Some(found.clone()));
    }
    Ok(None)
}

pub fn prompt_save_default_account(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    account_label: &str,
    server_name: Option<&str>,
) -> Result<bool> {
    match server_name {
        Some(server) => write!(
            writer,
            "Save {account_label} as the default account for server '{server}'? [Y/n] "
        )?,
        None => write!(
            writer,
            "Save {account_label} as the default account? [Y/n] "
        )?,
    }
    writer.flush()?;
    let Some(answer) = read_line(reader)? else {
        return Ok(false);
    };
    Ok(answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Format human-mode next steps after a successful `wx auth login`.
pub fn login_next_steps(operation: &Value, account_label: &str, is_loopback: bool) -> String {
    let who = operation["result"]["profile"]["nickname"]
        .as_str()
        .or_else(|| operation["result"]["profile"]["ownUsername"].as_str())
        .or_else(|| operation["result"]["nickname"].as_str())
        .or_else(|| operation["result"]["username"].as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(account_label);
    let clean: String = who
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}'))
        .collect();
    let display = if clean.trim().is_empty() {
        account_label.to_owned()
    } else {
        clean
    };
    if is_loopback {
        format!("Logged in as {display}. Try `wx chats list`. Keep it running: `wx daemon start`.")
    } else {
        format!("Logged in as {display}. Try `wx chats list`.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn local_target() -> ClientTarget {
        ClientTarget {
            endpoint: "http://127.0.0.1:6174".into(),
            server_name: None,
            source: EndpointSource::Default,
            token_file: Some(PathBuf::from("/tmp/data/api-token")),
            configured_token_file: PathBuf::from("/tmp/data/api-token"),
            account: None,
            ca_cert_file: None,
            is_local_default: true,
        }
    }

    fn remote_target() -> ClientTarget {
        ClientTarget {
            endpoint: "https://wx.example.com".into(),
            server_name: Some("box".into()),
            source: EndpointSource::SavedServer,
            token_file: Some(PathBuf::from("/tmp/box-token")),
            configured_token_file: PathBuf::from("/tmp/box-token"),
            account: Some("me".into()),
            ca_cert_file: None,
            is_local_default: false,
        }
    }

    #[test]
    fn prompt_gating_requires_both_terminals_no_json_and_no_ci() {
        assert!(can_prompt(true, true, false, None));
        assert!(can_prompt(true, true, false, Some("")));
        assert!(!can_prompt(false, true, false, None));
        assert!(!can_prompt(true, false, false, None));
        assert!(!can_prompt(true, true, true, None));
        assert!(!can_prompt(true, true, false, Some("true")));
        assert!(!can_prompt(true, true, false, Some("1")));
    }

    #[test]
    fn loopback_not_listening_classifies_as_daemon_not_running_even_when_token_missing() {
        let error = classify_failure(
            &local_target(),
            ConnectionFailure::MissingOrInvalidToken,
            Some(HealthProbe::NotListening),
            None,
            ClassificationOptions::default(),
        );
        let described = outcome::describe(&error);
        assert_eq!(described.exit, 3);
        assert_eq!(described.code, "daemon_not_running");
        assert!(described.retryable);
        assert_eq!(described.next, vec!["wx daemon start"]);
        assert!(described.message.contains("127.0.0.1:6174"));

        let status_error = classify_failure(
            &local_target(),
            ConnectionFailure::ConnectError("connection refused".into()),
            Some(HealthProbe::NotListening),
            None,
            ClassificationOptions {
                is_status_command: true,
            },
        );
        let status_described = outcome::describe(&status_error);
        assert_eq!(status_described.code, "daemon_not_running");
        assert!(status_described
            .next
            .iter()
            .any(|step| step == "wx daemon doctor"));
    }

    #[test]
    fn desktop_hub_key_mismatch_points_to_export_key_and_never_suggests_daemon_start() {
        for failure in [
            ConnectionFailure::MissingOrInvalidToken,
            ConnectionFailure::Unauthorized,
        ] {
            let error = classify_failure(
                &local_target(),
                failure.clone(),
                Some(HealthProbe::WxOk),
                Some(PortOwner::DesktopHub {
                    pid: Some(4242),
                    data_dir: Some(PathBuf::from(
                        "/home/u/.local/share/io.github.thisnick.agent-wechat/hub",
                    )),
                }),
                ClassificationOptions::default(),
            );
            let described = outcome::describe(&error);
            assert_eq!(described.exit, 4);
            assert!(!described.retryable);
            assert!(described.message.contains("Settings → Export key for CLI"));
            assert!(!described
                .next
                .iter()
                .any(|step| step.contains("daemon start")));
            if matches!(failure, ConnectionFailure::MissingOrInvalidToken) {
                assert_eq!(described.code, "invalid_api_token");
            } else {
                assert_eq!(described.code, "unauthorized");
            }
        }
    }

    #[test]
    fn other_service_on_port_names_owner_and_does_not_claim_daemon_not_running() {
        let error = classify_failure(
            &local_target(),
            ConnectionFailure::MissingOrInvalidToken,
            Some(HealthProbe::OtherService { status: Some(404) }),
            Some(PortOwner::OtherProcess {
                pid: 99,
                name: "nginx".into(),
            }),
            ClassificationOptions::default(),
        );
        let described = outcome::describe(&error);
        assert_eq!(described.exit, 3);
        assert_eq!(described.code, "endpoint_unavailable");
        assert!(described.message.contains("nginx (PID 99)"));
        assert!(described.message.contains("HTTP 404"));
    }

    #[test]
    fn remote_failures_keep_endpoint_unavailable_and_never_suggest_local_actions() {
        let error = classify_failure(
            &remote_target(),
            ConnectionFailure::ConnectError("timed out".into()),
            None,
            None,
            ClassificationOptions {
                is_status_command: true,
            },
        );
        let described = outcome::describe(&error);
        assert_eq!(described.exit, 3);
        assert_eq!(described.code, "endpoint_unavailable");
        assert!(described.retryable);
        assert!(described.message.contains("https://wx.example.com"));
        assert!(!described.next.iter().any(|step| step.contains("daemon")));
    }

    #[test]
    fn status_client_json_includes_all_required_fields() {
        let mut target = remote_target();
        let value = target.status_client_json();
        assert_eq!(value["serverName"], "box");
        assert_eq!(value["url"], "https://wx.example.com");
        assert_eq!(value["source"], "saved_server");
        assert_eq!(value["tokenFile"], "/tmp/box-token");
        assert_eq!(value["account"], "me");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));

        target.token_file = None;
        target.source = EndpointSource::Environment;
        let value = target.status_client_json();
        assert!(value["tokenFile"].is_null());
        assert_eq!(value["source"], "environment");
    }

    #[test]
    fn interactive_prompts_parse_expected_inputs_and_confirmations() {
        let mut out = Vec::new();
        assert!(prompt_start_daemon(&mut Cursor::new("\n"), &mut out).unwrap());
        assert!(prompt_start_daemon(&mut Cursor::new("y\n"), &mut out).unwrap());
        assert!(!prompt_start_daemon(&mut Cursor::new("n\n"), &mut out).unwrap());
        assert!(!prompt_start_daemon(&mut Cursor::new(""), &mut out).unwrap());

        assert_eq!(
            prompt_no_account_choice(&mut Cursor::new("1\n"), &mut out, true).unwrap(),
            NoAccountChoice::CreateNew
        );
        assert_eq!(
            prompt_no_account_choice(&mut Cursor::new("2\n"), &mut out, true).unwrap(),
            NoAccountChoice::UseOfficial
        );
        assert_eq!(
            prompt_no_account_choice(&mut Cursor::new("2\n"), &mut out, false).unwrap(),
            NoAccountChoice::Cancel
        );

        let mut official_out = Vec::new();
        assert!(
            !prompt_official_confirmation(&mut Cursor::new("y\n"), &mut official_out, &[1234])
                .unwrap()
        );
        let text = String::from_utf8(official_out).unwrap();
        assert!(text.contains("PID 1234"));
        assert!(text.contains(OFFICIAL_BACKUP_COMMAND));
        assert!(
            prompt_official_confirmation(&mut Cursor::new("yes\n"), &mut Vec::new(), &[]).unwrap()
        );

        let accounts = vec![
            AccountOption {
                id: "acc-1".into(),
                alias: Some("me".into()),
                state: Some("logged_in".into()),
            },
            AccountOption {
                id: "acc-2".into(),
                alias: None,
                state: None,
            },
        ];
        assert_eq!(
            prompt_select_account(&mut Cursor::new("2\n"), &mut Vec::new(), &accounts)
                .unwrap()
                .map(|item| item.id),
            Some("acc-2".into())
        );
        assert_eq!(
            prompt_select_account(&mut Cursor::new("me\n"), &mut Vec::new(), &accounts)
                .unwrap()
                .map(|item| item.id),
            Some("acc-1".into())
        );
        assert!(
            prompt_select_account(&mut Cursor::new("99\n"), &mut Vec::new(), &accounts)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn official_choice_requires_linux_loopback_and_xwechat_files_dir() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!official_choice_available(true, true, Some(temp.path())));
        std::fs::create_dir(temp.path().join("xwechat_files")).unwrap();
        assert!(official_choice_available(true, true, Some(temp.path())));
        assert!(!official_choice_available(false, true, Some(temp.path())));
        assert!(!official_choice_available(true, false, Some(temp.path())));
    }

    #[test]
    fn localhost_listen_hint_tls_error_and_login_next_steps_fallbacks() {
        let mut target = local_target();
        target.endpoint = "http://localhost:6180".into();
        target.is_local_default = false;
        let error = classify_failure(
            &target,
            ConnectionFailure::ConnectError("refused".into()),
            Some(HealthProbe::NotListening),
            None,
            ClassificationOptions::default(),
        );
        let described = outcome::describe(&error);
        assert_eq!(
            described.next,
            vec!["wx --listen 127.0.0.1:6180 daemon start"]
        );

        target.endpoint = "https://127.0.0.1:7443".into();
        let tls_error = classify_failure(
            &target,
            ConnectionFailure::ConnectError("cert".into()),
            Some(HealthProbe::TlsError),
            None,
            ClassificationOptions::default(),
        );
        let tls_described = outcome::describe(&tls_error);
        assert_eq!(tls_described.code, "endpoint_unavailable");
        assert!(tls_described.message.contains("--ca-cert-file"));

        let steps = login_next_steps(
            &json!({"result": {"profile": {"nickname": "\x1b\u{2028}"}}}),
            "me",
            true,
        );
        assert!(steps.starts_with("Logged in as me."));
    }
}
