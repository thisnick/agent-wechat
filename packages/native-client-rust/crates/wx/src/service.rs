//! Linux systemd user-unit rendering, installation, removal and restart.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::path::Path;
#[cfg(any(target_os = "linux", all(unix, test)))]
use std::{net::SocketAddr, path::PathBuf};
#[cfg(target_os = "linux")]
use {
    serde_json::{json, Value},
    std::{
        io::{IsTerminal, Write},
        process::Command as ProcessCommand,
        time::{Duration, Instant},
    },
};

#[cfg(any(target_os = "linux", all(unix, test)))]
pub const UNIT_NAME: &str = "agent-wechat-native.service";
#[cfg(target_os = "linux")]
pub const LINGER_HINT: &str = "loginctl enable-linger";

/// Kept identical to `deploy/native/agent-wechat-native.service` (checked by unit test).
#[cfg(any(target_os = "linux", test))]
const SHIPPED_UNIT_TEMPLATE: &str = "\
# Example user unit. Copy/edit/enable only during explicitly approved deployment.
[Unit]
Description=Agent WeChat native daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
# Use an explicitly provisioned native executable, not an npm lifecycle script.
ExecStart=%h/.local/bin/wx serve --server-config %h/.config/agent-wechat/server/config.toml
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=%h/.local/share/agent-wechat
# Direct external profiles and nondefault dataDir require explicit extra
# ReadWritePaths entries and compatible ownership. Never share an active writer.
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=default.target
";

#[cfg(any(target_os = "linux", all(unix, test)))]
#[derive(Debug, Clone)]
pub struct UnitSpec {
    pub executable: PathBuf,
    pub data_dir: PathBuf,
    pub server_config: PathBuf,
    pub listen: Option<SocketAddr>,
    pub token_file: Option<PathBuf>,
    pub network_listen: Option<SocketAddr>,
    pub tls_cert_file: Option<PathBuf>,
    pub tls_key_file: Option<PathBuf>,
    pub read_write_paths: Vec<PathBuf>,
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn unit_path(env: &crate::config::Environment) -> Result<PathBuf> {
    let config_root = env
        .config_home
        .as_ref()
        .filter(|p| p.is_absolute())
        .cloned()
        .or_else(|| {
            env.home
                .as_ref()
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config"))
        })
        .context("HOME or absolute XDG_CONFIG_HOME is required to locate systemd user units")?;
    Ok(config_root.join("systemd/user").join(UNIT_NAME))
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn default_data_dir(env: &crate::config::Environment) -> Option<PathBuf> {
    env.data_home
        .as_ref()
        .filter(|p| p.is_absolute())
        .cloned()
        .or_else(|| {
            env.home
                .as_ref()
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".local/share"))
        })
        .map(|p| p.join("agent-wechat"))
}

/// Prefer the npm-maintained stable binary symlink (`<dataDir>/bin/wx`) when it
/// points to the currently executing `wx` binary; otherwise use the canonical
/// executable path directly (for standalone release binaries or local builds).
#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn resolve_service_executable(
    current_exe: &Path,
    data_dir: &Path,
    env: &crate::config::Environment,
) -> Result<PathBuf> {
    let canonical_exe = std::fs::canonicalize(current_exe)
        .with_context(|| format!("cannot resolve executable {}", current_exe.display()))?;
    let mut candidates = vec![data_dir.join("bin/wx")];
    if let Some(default_dir) = default_data_dir(env) {
        let candidate = default_dir.join("bin/wx");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    for candidate in candidates {
        let Ok(metadata) = std::fs::symlink_metadata(&candidate) else {
            continue;
        };
        if !metadata.is_symlink() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } {
                continue;
            }
        }
        if std::fs::canonicalize(&candidate).ok().as_deref() == Some(canonical_exe.as_path()) {
            return Ok(candidate);
        }
    }
    Ok(canonical_exe)
}

/// Collect `ReadWritePaths` starting with `data_dir`, followed by registered
/// `--official` profile subtrees (`xwechat_files` and `.xwechat`) and `direct`
/// external profiles. Managed profiles live inside `data_dir` and need no extra
/// entry.
#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn collect_read_write_paths(
    data_dir: &Path,
    accounts: &[wechat_store::Account],
) -> Vec<PathBuf> {
    let mut paths = vec![data_dir.to_owned()];
    let push_unique = |list: &mut Vec<PathBuf>, candidate: PathBuf| {
        if list.iter().any(|existing| candidate.starts_with(existing)) {
            return;
        }
        list.push(candidate);
    };
    for account in accounts {
        match account.storage_mode.as_str() {
            "managed" => {}
            "official" => {
                let xwechat_files = account.profile_path.join("xwechat_files");
                let resolved_files = std::fs::canonicalize(&xwechat_files).unwrap_or(xwechat_files);
                push_unique(&mut paths, resolved_files);
                let dot_xwechat = account.profile_path.join(".xwechat");
                if dot_xwechat.exists() {
                    let resolved_dot = std::fs::canonicalize(&dot_xwechat).unwrap_or(dot_xwechat);
                    push_unique(&mut paths, resolved_dot);
                }
            }
            _ => {
                let resolved = std::fs::canonicalize(&account.profile_path)
                    .unwrap_or_else(|_| account.profile_path.clone());
                push_unique(&mut paths, resolved);
            }
        }
    }
    paths
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn format_unit_arg(value: &str, escape_dollar: bool) -> Result<String> {
    if value.is_empty() || value.chars().any(char::is_control) {
        bail!("systemd unit value contains unsupported control characters");
    }
    let needs_quotes = value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';' | '#'));
    let mut escaped = String::with_capacity(value.len() + 4);
    if needs_quotes {
        escaped.push('"');
    }
    for ch in value.chars() {
        match ch {
            '\\' if needs_quotes => escaped.push_str("\\\\"),
            '"' if needs_quotes => escaped.push_str("\\\""),
            '%' => escaped.push_str("%%"),
            '$' if escape_dollar => escaped.push_str("$$"),
            other => escaped.push(other),
        }
    }
    if needs_quotes {
        escaped.push('"');
    }
    Ok(escaped)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn format_unit_path(path: &Path, escape_dollar: bool) -> Result<String> {
    if !path.is_absolute() {
        bail!(
            "systemd unit path must be absolute: {}",
            path.to_string_lossy()
        );
    }
    let text = path
        .to_str()
        .context("systemd unit path must be valid UTF-8")?;
    format_unit_arg(text, escape_dollar)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn render_unit(spec: &UnitSpec) -> Result<String> {
    // ExecStart= expands both % specifiers and $VAR environment variables;
    // ReadWritePaths= expands % specifiers only, not $VAR.
    let mut exec_parts = vec![
        format_unit_path(&spec.executable, true)?,
        "serve".to_owned(),
        "--data-dir".to_owned(),
        format_unit_path(&spec.data_dir, true)?,
        "--server-config".to_owned(),
        format_unit_path(&spec.server_config, true)?,
    ];
    if let Some(listen) = spec.listen {
        exec_parts.push("--listen".to_owned());
        exec_parts.push(listen.to_string());
    }
    if let Some(token_file) = &spec.token_file {
        exec_parts.push("--token-file".to_owned());
        exec_parts.push(format_unit_path(token_file, true)?);
    }
    if let Some(network_listen) = spec.network_listen {
        exec_parts.push("--network-listen".to_owned());
        exec_parts.push(network_listen.to_string());
    }
    if let Some(cert) = &spec.tls_cert_file {
        exec_parts.push("--tls-cert-file".to_owned());
        exec_parts.push(format_unit_path(cert, true)?);
    }
    if let Some(key) = &spec.tls_key_file {
        exec_parts.push("--tls-key-file".to_owned());
        exec_parts.push(format_unit_path(key, true)?);
    }
    let exec_line = format!("ExecStart={}", exec_parts.join(" "));

    if spec.read_write_paths.is_empty() {
        bail!("systemd unit requires at least one ReadWritePaths entry");
    }
    let rw_parts = spec
        .read_write_paths
        .iter()
        .map(|p| format_unit_path(p, false))
        .collect::<Result<Vec<_>>>()?;
    let rw_line = format!("ReadWritePaths={}", rw_parts.join(" "));

    let mut rendered = Vec::new();
    let mut replaced_exec = 0usize;
    let mut replaced_rw = 0usize;
    for line in SHIPPED_UNIT_TEMPLATE.lines() {
        match line {
            "# Example user unit. Copy/edit/enable only during explicitly approved deployment." => {
                rendered.push(
                    "# Managed by wx daemon install. Remove with wx daemon uninstall.".to_owned(),
                );
            }
            "# Use an explicitly provisioned native executable, not an npm lifecycle script."
            | "# Direct external profiles and nondefault dataDir require explicit extra"
            | "# ReadWritePaths entries and compatible ownership. Never share an active writer." => {
            }
            "ExecStart=%h/.local/bin/wx serve --server-config %h/.config/agent-wechat/server/config.toml" => {
                rendered.push(exec_line.clone());
                replaced_exec += 1;
            }
            "ReadWritePaths=%h/.local/share/agent-wechat" => {
                rendered.push(rw_line.clone());
                replaced_rw += 1;
            }
            other => rendered.push(other.to_owned()),
        }
    }
    if replaced_exec != 1 || replaced_rw != 1 {
        bail!("shipped systemd unit template did not match expected placeholders");
    }
    Ok(format!("{}\n", rendered.join("\n")))
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn unquote_systemd_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                } else if q == '"' && ch == '\\' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if ch == '%' && chars.peek() == Some(&'%') {
                    chars.next();
                    current.push('%');
                } else if ch == '$' && chars.peek() == Some(&'$') {
                    chars.next();
                    current.push('$');
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch.is_whitespace() {
                    if in_token {
                        words.push(std::mem::take(&mut current));
                        in_token = false;
                    }
                } else if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    in_token = true;
                } else if ch == '\\' {
                    in_token = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else if ch == '%' && chars.peek() == Some(&'%') {
                    chars.next();
                    in_token = true;
                    current.push('%');
                } else if ch == '$' && chars.peek() == Some(&'$') {
                    chars.next();
                    in_token = true;
                    current.push('$');
                } else {
                    in_token = true;
                    current.push(ch);
                }
            }
        }
    }
    if in_token {
        words.push(current);
    }
    words
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn expand_home_specifier(raw: &str, env: &crate::config::Environment) -> Option<PathBuf> {
    if let Some(rest) = raw.strip_prefix("%h/") {
        let home = env.home.as_ref().filter(|p| p.is_absolute())?;
        return Some(home.join(rest));
    }
    if raw == "%h" {
        return env.home.as_ref().filter(|p| p.is_absolute()).cloned();
    }
    let path = PathBuf::from(raw);
    path.is_absolute().then_some(path)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
#[derive(Default)]
struct ParsedUnitArgs {
    data_dir: Option<PathBuf>,
    server_config: Option<PathBuf>,
    listen: Option<SocketAddr>,
    token_file: Option<PathBuf>,
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn parse_unit_exec_args(
    unit_text: &str,
    env: &crate::config::Environment,
) -> Option<ParsedUnitArgs> {
    let mut in_service = false;
    let mut exec_line = None;
    for raw_line in unit_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_service = line == "[Service]";
            continue;
        }
        if in_service {
            if let Some(rest) = line.strip_prefix("ExecStart=") {
                exec_line = Some(rest.trim());
            }
        }
    }
    let tokens = unquote_systemd_words(exec_line?);
    let mut parsed = ParsedUnitArgs::default();
    let mut idx = 0;
    while idx < tokens.len() {
        if tokens[idx] == "--data-dir" {
            if let Some(val) = tokens.get(idx + 1) {
                parsed.data_dir = expand_home_specifier(val, env);
                idx += 2;
                continue;
            }
        } else if let Some(val) = tokens[idx].strip_prefix("--data-dir=") {
            parsed.data_dir = expand_home_specifier(val, env);
        } else if tokens[idx] == "--server-config" {
            if let Some(val) = tokens.get(idx + 1) {
                parsed.server_config = expand_home_specifier(val, env);
                idx += 2;
                continue;
            }
        } else if let Some(val) = tokens[idx].strip_prefix("--server-config=") {
            parsed.server_config = expand_home_specifier(val, env);
        } else if tokens[idx] == "--listen" {
            if let Some(val) = tokens.get(idx + 1) {
                parsed.listen = val.parse().ok();
                idx += 2;
                continue;
            }
        } else if let Some(val) = tokens[idx].strip_prefix("--listen=") {
            parsed.listen = val.parse().ok();
        } else if tokens[idx] == "--token-file" {
            if let Some(val) = tokens.get(idx + 1) {
                parsed.token_file = expand_home_specifier(val, env);
                idx += 2;
                continue;
            }
        } else if let Some(val) = tokens[idx].strip_prefix("--token-file=") {
            parsed.token_file = expand_home_specifier(val, env);
        }
        idx += 1;
    }
    Some(parsed)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn unit_env_without_caller_overrides(
    env: &crate::config::Environment,
) -> crate::config::Environment {
    crate::config::Environment {
        home: env.home.clone(),
        config_home: env.config_home.clone(),
        data_home: env.data_home.clone(),
        server_config: None,
        data_dir: None,
    }
}

/// Extract the data directory managed by an installed systemd unit file,
/// ignoring the caller's `AGENT_WECHAT_DATA_DIR` / `AGENT_WECHAT_SERVER_CONFIG`.
#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn unit_data_dir(unit_text: &str, env: &crate::config::Environment) -> Option<PathBuf> {
    let parsed = parse_unit_exec_args(unit_text, env)?;
    if parsed.data_dir.is_some() {
        return parsed.data_dir;
    }
    let clean_env = unit_env_without_caller_overrides(env);
    if let Some(config_file) = parsed.server_config {
        let overrides = crate::config::Overrides {
            server_config: Some(config_file.clone()),
            ..Default::default()
        };
        if let Some(cwd) = config_file.parent().filter(|p| p.is_absolute()) {
            if let Ok(resolved) = crate::config::resolve(&overrides, &clean_env, cwd) {
                return Some(resolved.data_dir);
            }
        }
    }
    default_data_dir(&clean_env)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn resolve_unit_server_config(
    unit_text: &str,
    env: &crate::config::Environment,
) -> Option<crate::config::ServerConfig> {
    let parsed = parse_unit_exec_args(unit_text, env)?;
    let clean_env = unit_env_without_caller_overrides(env);
    let overrides = crate::config::Overrides {
        server_config: parsed.server_config.clone(),
        data_dir: parsed.data_dir.clone(),
        listen: parsed.listen,
        token_file: parsed.token_file.clone(),
        ..Default::default()
    };
    let cwd = parsed
        .server_config
        .as_deref()
        .and_then(Path::parent)
        .filter(|p| p.is_absolute())
        .or_else(|| parsed.data_dir.as_deref().filter(|p| p.is_absolute()))
        .or_else(|| clean_env.home.as_deref().filter(|p| p.is_absolute()))
        .unwrap_or(Path::new("/"));
    let mut resolved = crate::config::resolve(&overrides, &clean_env, cwd).ok()?;
    if resolved.data_dir.exists() {
        if let Ok(canon) = std::fs::canonicalize(&resolved.data_dir) {
            resolved.data_dir = canon;
        }
    }
    Some(resolved)
}

#[cfg(any(target_os = "linux", all(unix, test)))]
fn read_installed_unit(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.is_symlink() || metadata.len() > 64 * 1024 {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(any(target_os = "linux", all(unix, test)))]
pub fn installed_unit_matches_data_dir(
    path: &Path,
    target_data_dir: &Path,
    env: &crate::config::Environment,
) -> bool {
    let Some(text) = read_installed_unit(path) else {
        return false;
    };
    let Some(unit_dir) = unit_data_dir(&text, env) else {
        return false;
    };
    let left = std::fs::canonicalize(&unit_dir).unwrap_or(unit_dir);
    let right = std::fs::canonicalize(target_data_dir).unwrap_or_else(|_| target_data_dir.into());
    left == right
}

#[cfg(target_os = "linux")]
pub fn installed_unit_server_config(
    env: &crate::config::Environment,
) -> Option<crate::config::ServerConfig> {
    let path = unit_path(env).ok()?;
    let text = read_installed_unit(&path)?;
    resolve_unit_server_config(&text, env)
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<std::process::Output> {
    ProcessCommand::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "systemctl was not found in PATH; systemd user services require systemd"
                )
            } else {
                anyhow::Error::from(error).context("failed to run systemctl --user")
            }
        })
}

#[cfg(target_os = "linux")]
fn format_systemctl_error(args: &[&str], output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let bus_hint = if detail.contains("Failed to connect to bus")
        || detail.contains("No medium found")
        || detail.contains("XDG_RUNTIME_DIR")
    {
        " (ensure a systemd user session is running with XDG_RUNTIME_DIR/DBUS_SESSION_BUS_ADDRESS)"
    } else {
        ""
    };
    if detail.is_empty() {
        anyhow::anyhow!(
            "`systemctl --user {}` exited with {}{}",
            args.join(" "),
            output.status,
            bus_hint
        )
    } else {
        anyhow::anyhow!(
            "`systemctl --user {}` failed: {}{}",
            args.join(" "),
            detail,
            bus_hint
        )
    }
}

#[cfg(target_os = "linux")]
fn systemctl_checked(args: &[&str]) -> Result<()> {
    let output = run_systemctl(args)?;
    if !output.status.success() {
        return Err(format_systemctl_error(args, &output));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn is_active() -> bool {
    run_systemctl(&["is-active", "--quiet", UNIT_NAME]).is_ok_and(|output| output.status.success())
}

#[cfg(not(target_os = "linux"))]
pub fn is_active() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn unit_active_state() -> Option<String> {
    let output = run_systemctl(&["show", "-p", "ActiveState", "--value", UNIT_NAME]).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub fn check_owner_lock_free(data_dir: &Path) -> Result<()> {
    let lock_path = data_dir.join("owner.lock");
    if !lock_path.exists() {
        return Ok(());
    }
    let file = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("cannot inspect data directory lock {}", lock_path.display()))?;
    if file.try_lock_exclusive().is_err() {
        if data_dir.join("desktop-server.toml").exists() {
            bail!(
                "another owner (such as the AgentWechat desktop app hub) holds data directory {}; quit the desktop app or stop that owner first",
                data_dir.display()
            );
        }
        bail!(
            "another owner (such as `wx serve`, OpenClaw managed mode, or the desktop app hub) holds data directory {}; stop that owner first",
            data_dir.display()
        );
    }
    let _ = FileExt::unlock(&file);
    Ok(())
}

pub fn check_ports_free(config: &crate::config::ServerConfig) -> Result<()> {
    let listener = std::net::TcpListener::bind(config.listen).with_context(|| {
        format!(
            "listen address {} is already in use by another process (for example `wx serve`, the desktop app hub, or another service); stop that owner first or choose a different `--listen` address",
            config.listen
        )
    })?;
    drop(listener);
    if let Some(network_listen) = config.network_listen {
        let net_listener = std::net::TcpListener::bind(network_listen).with_context(|| {
            format!(
                "network listen address {} is already in use by another process; stop that owner first",
                network_listen
            )
        })?;
        drop(net_listener);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_no_conflicting_owner(
    config: &crate::config::ServerConfig,
    env: &crate::config::Environment,
    allow_active_service: bool,
) -> Result<()> {
    if !allow_active_service {
        // UNIT_NAME is a singleton per user: never overwrite an active unit
        // (even if it serves another data directory) or replace a unit
        // configured for a different data directory.
        if is_active() {
            bail!(
                "systemd service {UNIT_NAME} is already active; stop it with `systemctl --user stop {UNIT_NAME}` or remove it with `wx daemon uninstall` before reinstalling"
            );
        }
        if let Ok(path) = unit_path(env) {
            if path.exists() && !installed_unit_matches_data_dir(&path, &config.data_dir, env) {
                let existing = read_installed_unit(&path)
                    .and_then(|text| unit_data_dir(&text, env))
                    .map(|p| format!(" ({})", p.display()))
                    .unwrap_or_default();
                bail!(
                    "systemd service {UNIT_NAME} is already installed for another data directory{existing}; remove it first with `wx daemon uninstall`"
                );
            }
        }
    }
    let record_path = config.data_dir.join("daemon/process.json");
    if let Some(pid) = super::daemon::live_managed_pid(&record_path)? {
        bail!(
            "a local daemon managed by `wx daemon start` (PID {pid}) is running for {}; stop it first with `wx daemon stop`",
            config.data_dir.display()
        );
    }
    if !allow_active_service {
        check_owner_lock_free(&config.data_dir)?;
        check_ports_free(config)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_unit_file(path: &Path, content: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("systemd unit path must have a parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "cannot create systemd user unit directory {}",
            parent.display()
        )
    })?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() || meta.is_symlink() {
            bail!(
                "refusing to overwrite non-regular file at {}",
                path.display()
            );
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    }
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("cannot write systemd unit {}", path.display()))?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_server_config_exists(config: &crate::config::ServerConfig) -> Result<PathBuf> {
    if config.config_file.exists() {
        return Ok(std::fs::canonicalize(&config.config_file)?);
    }
    let parent = config
        .config_file
        .parent()
        .context("server configuration path must have a parent directory")?;
    if let Some(grandparent) = parent.parent() {
        std::fs::create_dir_all(grandparent)?;
    }
    wechat_platform::private_dir(parent)?;
    crate::private_output(&config.config_file, b"schemaVersion = 1\n")?;
    Ok(std::fs::canonicalize(&config.config_file)?)
}

#[cfg(target_os = "linux")]
fn load_accounts_for_unit(data_dir: &Path, dry_run: bool) -> Result<Vec<wechat_store::Account>> {
    if !data_dir.is_dir() || !data_dir.join("registry.sqlite").is_file() {
        return Ok(Vec::new());
    }
    match wechat_store::Store::open_existing(data_dir) {
        Ok(store) => {
            let accounts = store.accounts()?;
            drop(store);
            Ok(accounts)
        }
        Err(error) if dry_run => {
            eprintln!(
                "Note: could not read registered accounts during --dry-run ({error}); ReadWritePaths includes only the data directory."
            );
            Ok(Vec::new())
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
fn build_unit_spec(
    config: &crate::config::ServerConfig,
    overrides: &crate::config::Overrides,
    executable: PathBuf,
    accounts: &[wechat_store::Account],
) -> UnitSpec {
    let read_write_paths = collect_read_write_paths(&config.data_dir, accounts);
    UnitSpec {
        executable,
        data_dir: config.data_dir.clone(),
        server_config: config.config_file.clone(),
        listen: overrides.listen,
        token_file: overrides
            .token_file
            .as_ref()
            .map(|_| config.token_file.clone()),
        network_listen: overrides.network_listen,
        tls_cert_file: if overrides.network_listen.is_some() || overrides.tls_cert_file.is_some() {
            config.tls_cert_file.clone()
        } else {
            None
        },
        tls_key_file: if overrides.network_listen.is_some() || overrides.tls_key_file.is_some() {
            config.tls_key_file.clone()
        } else {
            None
        },
        read_write_paths,
    }
}

#[cfg(target_os = "linux")]
fn confirm_install(
    unit_file: &Path,
    spec: &UnitSpec,
    config_will_be_created: bool,
    json_mode: bool,
) -> Result<bool> {
    if json_mode || !std::io::stdin().is_terminal() {
        bail!(
            "daemon install requires confirmation; pass --yes to install non-interactively, or --dry-run to inspect the unit"
        );
    }
    eprintln!("Will install and enable systemd user service {UNIT_NAME}:");
    eprintln!("  Unit file:      {}", unit_file.display());
    eprintln!("  Executable:     {}", spec.executable.display());
    eprintln!("  Data dir:       {}", spec.data_dir.display());
    if config_will_be_created {
        eprintln!(
            "  Server config:  {} (will be created with schemaVersion = 1)",
            spec.server_config.display()
        );
    } else {
        eprintln!("  Server config:  {}", spec.server_config.display());
    }
    let rw_display = spec
        .read_write_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!("  ReadWritePaths: {rw_display}");
    eprintln!(
        "  Actions:        write unit, run `systemctl --user daemon-reload`, run `systemctl --user enable --now {UNIT_NAME}`"
    );
    eprint!("Install and start {UNIT_NAME}? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let normalized = answer.trim().to_ascii_lowercase();
    Ok(matches!(normalized.as_str(), "y" | "yes"))
}

#[cfg(target_os = "linux")]
pub async fn probe_service_status(config: &crate::config::ServerConfig) -> Option<Value> {
    if !config.token_file.is_file() {
        return None;
    }
    let client = super::daemon::remote_for(config).ok()?;
    let value = tokio::time::timeout(Duration::from_millis(500), client.get("v1/status"))
        .await
        .ok()?
        .ok()?;
    let expected_data_dir =
        std::fs::canonicalize(&config.data_dir).unwrap_or_else(|_| config.data_dir.clone());
    if value["version"]["backend"] == "native"
        && value["dataDir"].as_str() == expected_data_dir.to_str()
    {
        Some(value)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
async fn wait_for_service_ready(config: &crate::config::ServerConfig) -> Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(state) = unit_active_state() {
            if state == "failed" {
                bail!(
                    "systemd service {UNIT_NAME} entered failed state; inspect `systemctl --user status {UNIT_NAME}` and `journalctl --user -u {UNIT_NAME}`"
                );
            }
        }
        if let Some(value) = probe_service_status(config).await {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            bail!(
                "systemd service {UNIT_NAME} did not become ready on {} in time; inspect `systemctl --user status {UNIT_NAME}` and `journalctl --user -u {UNIT_NAME}`",
                config.listen
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[cfg(target_os = "linux")]
fn rollback_new_unit(unit_file: &Path) {
    let _ = run_systemctl(&["disable", "--now", UNIT_NAME]);
    let _ = std::fs::remove_file(unit_file);
    let _ = run_systemctl(&["reset-failed", UNIT_NAME]);
    let _ = run_systemctl(&["daemon-reload"]);
}

pub async fn install(
    #[allow(unused_mut)] mut config: crate::config::ServerConfig,
    overrides: &crate::config::Overrides,
    env: &crate::config::Environment,
    yes: bool,
    dry_run: bool,
    output: crate::output::Output,
) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, overrides, env, yes, dry_run, output);
        bail!(
            "`wx daemon install` currently supports Linux systemd user services only; on this platform use `wx daemon start`"
        );
    }

    #[cfg(target_os = "linux")]
    {
        if config.listen.port() == 0 {
            bail!("daemon install requires an explicit nonzero listen port");
        }
        let unit_file = unit_path(env)?;
        if !dry_run {
            ensure_no_conflicting_owner(&config, env, false)?;
            systemctl_checked(&["show-environment"])?;
        }
        let accounts = load_accounts_for_unit(&config.data_dir, dry_run)?;
        let current_exe = std::env::current_exe().context("cannot determine wx executable path")?;
        let executable = resolve_service_executable(&current_exe, &config.data_dir, env)?;
        let spec = build_unit_spec(&config, overrides, executable, &accounts);
        let rendered = render_unit(&spec)?;

        if dry_run {
            if output.is_json() {
                return output.print(&json!({
                    "dryRun": true,
                    "unitName": UNIT_NAME,
                    "unitPath": unit_file,
                    "executable": spec.executable,
                    "dataDir": spec.data_dir,
                    "serverConfig": spec.server_config,
                    "readWritePaths": spec.read_write_paths,
                    "unit": rendered,
                    "lingerHint": LINGER_HINT,
                }));
            }
            println!("# Target unit path: {}", unit_file.display());
            print!("{rendered}");
            return Ok(());
        }

        if !yes
            && !confirm_install(
                &unit_file,
                &spec,
                !config.config_file.exists(),
                output.is_json(),
            )?
        {
            return output.print(&json!({
                "state": "cancelled",
                "unitName": UNIT_NAME,
                "unitPath": unit_file,
            }));
        }

        // Re-check ownership right before mutating to narrow any TOCTOU window.
        ensure_no_conflicting_owner(&config, env, false)?;

        wechat_platform::private_dir(&config.data_dir)?;
        config.data_dir = std::fs::canonicalize(&config.data_dir)?;
        config.config_file = ensure_server_config_exists(&config)?;
        let accounts = load_accounts_for_unit(&config.data_dir, false)?;
        let executable = resolve_service_executable(&current_exe, &config.data_dir, env)?;
        let final_spec = build_unit_spec(&config, overrides, executable, &accounts);
        let final_rendered = render_unit(&final_spec)?;

        let existed_before = unit_file.exists();
        write_unit_file(&unit_file, &final_rendered)?;
        let activation = async {
            systemctl_checked(&["daemon-reload"])?;
            systemctl_checked(&["enable", "--now", UNIT_NAME])?;
            wait_for_service_ready(&config).await
        }
        .await;
        let status = match activation {
            Ok(status) => status,
            Err(error) => {
                if !existed_before {
                    rollback_new_unit(&unit_file);
                }
                return Err(error);
            }
        };

        let mut response = json!({
            "state": "installed",
            "manager": "systemd",
            "unitName": UNIT_NAME,
            "unitPath": unit_file,
            "executable": final_spec.executable,
            "dataDir": final_spec.data_dir,
            "serverConfig": final_spec.server_config,
            "readWritePaths": final_spec.read_write_paths,
            "lingerHint": LINGER_HINT,
        });
        if let Some(pid) = status["pid"].as_u64() {
            response["pid"] = json!(pid);
        }
        if let Some(server_id) = status["serverId"].as_str() {
            response["serverId"] = json!(server_id);
        }
        output.print(&response)?;
        if !output.is_json() {
            println!(
                "Hint: on a headless machine, run `{LINGER_HINT}` so the service starts without a login session."
            );
        }
        Ok(())
    }
}

pub fn uninstall(
    config: &crate::config::ServerConfig,
    env: &crate::config::Environment,
    output: crate::output::Output,
) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (config, env, output);
        bail!("`wx daemon uninstall` currently supports Linux systemd user services only");
    }

    #[cfg(target_os = "linux")]
    {
        let unit_file = unit_path(env)?;
        let Ok(meta) = std::fs::symlink_metadata(&unit_file) else {
            return output.print(&json!({
                "state": "not_installed",
                "unitName": UNIT_NAME,
                "unitPath": unit_file,
                "dataDir": config.data_dir,
                "dataPreserved": true,
            }));
        };
        if !meta.is_file() || meta.is_symlink() {
            bail!(
                "refusing to remove non-regular file at {}",
                unit_file.display()
            );
        }
        let unit_dir = read_installed_unit(&unit_file)
            .and_then(|text| unit_data_dir(&text, env))
            .unwrap_or_else(|| config.data_dir.clone());
        let disable_result = run_systemctl(&["disable", "--now", UNIT_NAME]);
        match disable_result {
            Ok(out) if !out.status.success() => {
                if is_active() || check_owner_lock_free(&unit_dir).is_err() {
                    return Err(format_systemctl_error(
                        &["disable", "--now", UNIT_NAME],
                        &out,
                    ));
                }
            }
            Err(error) => {
                if check_owner_lock_free(&unit_dir).is_err() {
                    return Err(error);
                }
            }
            Ok(_) => {}
        }
        std::fs::remove_file(&unit_file)
            .with_context(|| format!("cannot remove systemd unit {}", unit_file.display()))?;
        let _ = run_systemctl(&["reset-failed", UNIT_NAME]);
        let _ = run_systemctl(&["daemon-reload"]);
        output.print(&json!({
            "state": "uninstalled",
            "manager": "systemd",
            "unitName": UNIT_NAME,
            "unitPath": unit_file,
            "dataDir": unit_dir,
            "dataPreserved": true,
        }))
    }
}

#[cfg(target_os = "linux")]
pub async fn restart_service(
    config: &crate::config::ServerConfig,
    env: &crate::config::Environment,
    output: crate::output::Output,
) -> Result<()> {
    let probe_config = installed_unit_server_config(env).unwrap_or_else(|| config.clone());
    let active = is_active();
    ensure_no_conflicting_owner(&probe_config, env, active)?;
    systemctl_checked(&["restart", UNIT_NAME])?;
    let mut status = wait_for_service_ready(&probe_config).await?;
    if let Some(obj) = status.as_object_mut() {
        obj.insert("manager".into(), json!("systemd"));
        obj.insert("unitName".into(), json!(UNIT_NAME));
    }
    output.print(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_template_matches_shipped_service_unit() {
        let shipped_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../deploy/native/agent-wechat-native.service");
        if shipped_path.is_file() {
            let shipped = std::fs::read_to_string(&shipped_path)
                .unwrap()
                .replace("\r\n", "\n");
            assert_eq!(
                SHIPPED_UNIT_TEMPLATE, shipped,
                "embedded systemd template must stay identical to deploy/native/agent-wechat-native.service"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn render_unit_sets_exec_start_read_write_paths_and_keeps_hardening() {
        let spec = UnitSpec {
            executable: PathBuf::from("/home/alice/.local/share/agent-wechat/bin/wx"),
            data_dir: PathBuf::from("/home/alice/.local/share/agent-wechat"),
            server_config: PathBuf::from("/home/alice/.config/agent-wechat/server/config.toml"),
            listen: None,
            token_file: None,
            network_listen: None,
            tls_cert_file: None,
            tls_key_file: None,
            read_write_paths: vec![
                PathBuf::from("/home/alice/.local/share/agent-wechat"),
                PathBuf::from("/home/alice/xwechat_files"),
                PathBuf::from("/home/alice/.xwechat"),
            ],
        };
        let unit = render_unit(&spec).unwrap();
        assert!(unit.contains(
            "ExecStart=/home/alice/.local/share/agent-wechat/bin/wx serve --data-dir /home/alice/.local/share/agent-wechat --server-config /home/alice/.config/agent-wechat/server/config.toml\n"
        ));
        assert!(unit.contains(
            "ReadWritePaths=/home/alice/.local/share/agent-wechat /home/alice/xwechat_files /home/alice/.xwechat\n"
        ));
        for directive in [
            "Type=simple",
            "Restart=on-failure",
            "RestartSec=5",
            "TimeoutStopSec=30",
            "KillSignal=SIGTERM",
            "UMask=0077",
            "NoNewPrivileges=yes",
            "PrivateTmp=yes",
            "ProtectSystem=strict",
            "ProtectHome=read-only",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            "WantedBy=default.target",
        ] {
            assert!(unit.contains(directive), "missing {directive} in:\n{unit}");
        }
        assert!(!unit.contains("%h/.local/bin/wx"));
    }

    #[cfg(unix)]
    #[test]
    fn render_unit_quotes_spaces_escapes_percent_and_dollar_and_rejects_control_chars() {
        let spec = UnitSpec {
            executable: PathBuf::from("/opt/my wx/bin/wx"),
            data_dir: PathBuf::from("/tmp/100% $HOME/agent-wechat"),
            server_config: PathBuf::from("/tmp/cfg/server.toml"),
            listen: Some("127.0.0.1:6180".parse().unwrap()),
            token_file: Some(PathBuf::from("/tmp/secret token")),
            network_listen: None,
            tls_cert_file: Some(PathBuf::from("/tmp/tls/cert.pem")),
            tls_key_file: Some(PathBuf::from("/tmp/tls/key.pem")),
            read_write_paths: vec![PathBuf::from("/tmp/100% $HOME/agent-wechat")],
        };
        let unit = render_unit(&spec).unwrap();
        assert!(unit.contains(
            "ExecStart=\"/opt/my wx/bin/wx\" serve --data-dir \"/tmp/100%% $$HOME/agent-wechat\" --server-config /tmp/cfg/server.toml --listen 127.0.0.1:6180 --token-file \"/tmp/secret token\" --tls-cert-file /tmp/tls/cert.pem --tls-key-file /tmp/tls/key.pem\n"
        ));
        // ReadWritePaths= escapes % but does not expand $VAR.
        assert!(unit.contains("ReadWritePaths=\"/tmp/100%% $HOME/agent-wechat\"\n"));

        let env = crate::config::Environment {
            home: Some(PathBuf::from("/home/alice")),
            ..Default::default()
        };
        assert_eq!(
            unit_data_dir(&unit, &env),
            Some(PathBuf::from("/tmp/100% $HOME/agent-wechat"))
        );

        let mut bad = spec;
        bad.data_dir = PathBuf::from("/tmp/bad\nExecStart=/bin/sh");
        assert!(render_unit(&bad).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn collect_read_write_paths_covers_official_and_direct_profiles_without_broadening_home() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let official_home = temp.path().join("home");
        let direct_profile = temp.path().join("direct-profile");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(official_home.join("xwechat_files")).unwrap();
        std::fs::create_dir_all(official_home.join(".xwechat")).unwrap();
        std::fs::create_dir_all(&direct_profile).unwrap();

        let accounts = vec![
            wechat_store::Account {
                id: "managed-1".into(),
                storage_mode: "managed".into(),
                profile_path: data_dir.join("accounts/managed-1/profile"),
                schema_compatibility: "compatible".into(),
            },
            wechat_store::Account {
                id: "official-1".into(),
                storage_mode: "official".into(),
                profile_path: official_home.clone(),
                schema_compatibility: "compatible".into(),
            },
            wechat_store::Account {
                id: "direct-1".into(),
                storage_mode: "direct".into(),
                profile_path: direct_profile.clone(),
                schema_compatibility: "compatible".into(),
            },
        ];

        let paths = collect_read_write_paths(&data_dir, &accounts);
        assert_eq!(paths[0], data_dir);
        assert!(
            paths.contains(&std::fs::canonicalize(official_home.join("xwechat_files")).unwrap())
        );
        assert!(paths.contains(&std::fs::canonicalize(official_home.join(".xwechat")).unwrap()));
        assert!(paths.contains(&std::fs::canonicalize(&direct_profile).unwrap()));
        // Must never grant write access to the entire user HOME directory.
        assert!(!paths.contains(&official_home));
        assert!(!paths.contains(&std::fs::canonicalize(&official_home).unwrap()));
    }

    #[cfg(unix)]
    #[test]
    fn unit_data_dir_ignores_caller_env_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let server_cfg = temp.path().join(".config/agent-wechat/server/config.toml");
        std::fs::create_dir_all(server_cfg.parent().unwrap()).unwrap();
        std::fs::write(&server_cfg, "schemaVersion = 1\n").unwrap();

        let env = crate::config::Environment {
            home: Some(temp.path().to_owned()),
            data_dir: Some(PathBuf::from("/tmp/isolated-override")),
            server_config: Some(PathBuf::from("/tmp/other-server.toml")),
            ..Default::default()
        };
        assert_eq!(
            unit_data_dir(SHIPPED_UNIT_TEMPLATE, &env),
            Some(temp.path().join(".local/share/agent-wechat"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_service_executable_prefers_matching_stable_symlink() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let bin_dir = data_dir.join("bin");
        let versioned = temp.path().join("native/3.2.1/linux-x64-gnu/wx");
        let other = temp.path().join("standalone/wx");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(versioned.parent().unwrap()).unwrap();
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        std::fs::write(&versioned, b"wx").unwrap();
        std::fs::write(&other, b"other").unwrap();
        let stable = bin_dir.join("wx");
        symlink(&versioned, &stable).unwrap();

        let env = crate::config::Environment {
            home: Some(temp.path().to_owned()),
            ..Default::default()
        };
        assert_eq!(
            resolve_service_executable(&versioned, &data_dir, &env).unwrap(),
            stable
        );
        assert_eq!(
            resolve_service_executable(&other, &data_dir, &env).unwrap(),
            std::fs::canonicalize(&other).unwrap()
        );
    }
}
