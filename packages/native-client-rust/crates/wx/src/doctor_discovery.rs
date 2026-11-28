//! Read-only discovery of local `agent-wechat` daemons and loopback port owners.
//!
//! Kept self-contained so loopback port-owner classification can later be unified
//! with the first-run connection classifier (#749).
use crate::config::{self, Environment, Overrides};
use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) enum DaemonOwner {
    #[serde(rename = "desktop")]
    Desktop,
    #[serde(rename = "wx daemon")]
    WxDaemon,
    #[serde(rename = "systemd")]
    Systemd,
    #[serde(rename = "openclaw")]
    OpenClaw,
    #[serde(rename = "unknown")]
    Unknown,
}

impl DaemonOwner {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::WxDaemon => "wx daemon",
            Self::Systemd => "systemd",
            Self::OpenClaw => "openclaw",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProcessRecord {
    pub(crate) pid: u32,
    pub(crate) start_ticks: String,
    pub(crate) executable: PathBuf,
    pub(crate) endpoint: String,
}

impl ProcessRecord {
    /// True when the recorded PID is still running with the same process start
    /// fingerprint, even if the binary on disk was replaced (e.g. after a `wx`
    /// upgrade on Linux where `/proc/<pid>/exe` ends with ` (deleted)`).
    pub(crate) fn pid_is_live(&self) -> bool {
        wechat_platform::process_identity(self.pid)
            .is_some_and(|(created, _)| created == self.start_ticks)
    }

    /// True when both the start fingerprint and on-disk executable path match.
    pub(crate) fn executable_matches(&self) -> bool {
        wechat_platform::process_identity(self.pid).is_some_and(|(created, executable)| {
            created == self.start_ticks && executable == self.executable
        })
    }
}

/// Read `daemon/process.json` strictly read-only without modifying anything.
pub(crate) fn read_process_record(data_dir: &Path) -> Result<Option<ProcessRecord>, String> {
    let path = data_dir.join("daemon/process.json");
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read process.json metadata: {e}")),
    };
    if wechat_platform::is_link(&meta) || !meta.is_file() {
        return Err("daemon/process.json is not a regular file".into());
    }
    if meta.len() > 16384 {
        return Err("daemon/process.json is unexpectedly large".into());
    }
    let file = wechat_platform::OpenOptions::new()
        .read(true)
        .open(&path)
        .map_err(|e| format!("cannot open daemon/process.json: {e}"))?;
    let mut bytes = Vec::new();
    file.take(16385)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read daemon/process.json: {e}"))?;
    serde_json::from_slice::<ProcessRecord>(&bytes)
        .map(Some)
        .map_err(|e| format!("invalid daemon/process.json: {e}"))
}

#[derive(Debug, Clone)]
pub(crate) struct SystemdUnitInfo {
    pub(crate) path: PathBuf,
    pub(crate) enabled: bool,
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) listen: Option<SocketAddr>,
    pub(crate) server_config: Option<PathBuf>,
    pub(crate) protect_home: Option<String>,
    pub(crate) protect_system: Option<String>,
    pub(crate) read_write_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct RunningWxProcess {
    pub(crate) pid: u32,
    #[cfg(target_os = "linux")]
    pub(crate) uid: Option<u32>,
    pub(crate) parent_comm: Option<String>,
    pub(crate) parent_cmdline: Option<String>,
    pub(crate) cgroup: Option<String>,
    pub(crate) data_dir: Option<PathBuf>,
    pub(crate) listen: SocketAddr,
    pub(crate) server_config: Option<PathBuf>,
    pub(crate) token_file: Option<PathBuf>,
    pub(crate) token_stdin: bool,
    pub(crate) exit_with_parent: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DiscoveredDaemon {
    pub(crate) owner: DaemonOwner,
    pub(crate) address: String,
    pub(crate) listen: Option<SocketAddr>,
    pub(crate) data_dir: PathBuf,
    pub(crate) state: String,
    pub(crate) pid: Option<u32>,
    pub(crate) server_config: Option<PathBuf>,
    pub(crate) token_file: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct PortOwner {
    pub(crate) listening: bool,
    pub(crate) health_ok: bool,
    pub(crate) pid: Option<u32>,
    pub(crate) process_name: Option<String>,
    pub(crate) daemon_owner: Option<DaemonOwner>,
    pub(crate) data_dir: Option<PathBuf>,
}

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn desktop_hub_dir(env: &Environment) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        env.home
            .as_ref()
            .filter(|p| p.is_absolute())
            .map(|h| h.join("Library/Application Support/io.github.thisnick.agent-wechat/hub"))
    }
    #[cfg(target_os = "windows")]
    {
        // Tauri's app_local_data_dir() uses LOCALAPPDATA (env.data_home), not APPDATA.
        env.data_home
            .as_ref()
            .filter(|p| p.is_absolute())
            .map(|root| root.join("io.github.thisnick.agent-wechat/hub"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env.data_home
            .as_ref()
            .filter(|p| p.is_absolute())
            .cloned()
            .or_else(|| {
                env.home
                    .as_ref()
                    .filter(|p| p.is_absolute())
                    .map(|h| h.join(".local/share"))
            })
            .map(|root| root.join("io.github.thisnick.agent-wechat/hub"))
    }
}

/// Path to the desktop app's `settings/desktop.json` under Tauri's `app_config_dir()`.
pub(crate) fn desktop_config_file(env: &Environment) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        env.home.as_ref().filter(|p| p.is_absolute()).map(|h| {
            h.join(
                "Library/Application Support/io.github.thisnick.agent-wechat/settings/desktop.json",
            )
        })
    }
    #[cfg(target_os = "windows")]
    {
        env.config_home
            .as_ref()
            .filter(|p| p.is_absolute())
            .map(|root| root.join("io.github.thisnick.agent-wechat/settings/desktop.json"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env.config_home
            .as_ref()
            .filter(|p| p.is_absolute())
            .cloned()
            .or_else(|| {
                env.home
                    .as_ref()
                    .filter(|p| p.is_absolute())
                    .map(|h| h.join(".config"))
            })
            .map(|root| root.join("io.github.thisnick.agent-wechat/settings/desktop.json"))
    }
}

/// Strip caller-specific `AGENT_WECHAT_DATA_DIR` / `AGENT_WECHAT_SERVER_CONFIG`
/// overrides when resolving defaults for *other* daemons (systemd unit, default folder,
/// or other running `wx serve` processes).
fn base_env_without_overrides(env: &Environment) -> Environment {
    Environment {
        home: env.home.clone(),
        config_home: env.config_home.clone(),
        data_home: env.data_home.clone(),
        server_config: None,
        data_dir: None,
    }
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if wechat_platform::is_link(&meta) || !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    let file = wechat_platform::OpenOptions::new()
        .read(true)
        .open(path)
        .ok()?;
    let mut bytes = Vec::new();
    file.take(max_bytes + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > max_bytes {
        return None;
    }
    Some(bytes)
}

fn desktop_hub_port(env: &Environment) -> u16 {
    if let Some(path) = desktop_config_file(env) {
        if let Some(bytes) = read_bounded_regular_file(&path, 64 * 1024) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(port) = value["localPort"].as_u64() {
                    if let Ok(port) = u16::try_from(port) {
                        if port > 0 {
                            return port;
                        }
                    }
                }
            }
        }
    }
    6174
}

pub(crate) fn systemd_unit_path(env: &Environment) -> Option<PathBuf> {
    let config_root = env
        .config_home
        .as_ref()
        .filter(|p| p.is_absolute())
        .cloned()
        .or_else(|| {
            env.home
                .as_ref()
                .filter(|p| p.is_absolute())
                .map(|h| h.join(".config"))
        })?;
    Some(config_root.join("systemd/user/agent-wechat-native.service"))
}

fn expand_systemd_path(raw: &str, home: Option<&Path>) -> PathBuf {
    let cleaned = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('-');
    if let Some(rest) = cleaned.strip_prefix("%h/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    } else if cleaned == "%h" {
        if let Some(home) = home {
            return home.to_path_buf();
        }
    }
    PathBuf::from(cleaned)
}

fn split_simple_args(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(q) if c == '\\' && q == '"' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(c),
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            None => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

pub(crate) fn parse_systemd_unit(
    path: &Path,
    contents: &str,
    env: &Environment,
    cwd: &Path,
) -> SystemdUnitInfo {
    let home = env.home.as_deref().filter(|p| p.is_absolute());
    let mut exec_start = None;
    let mut protect_home = None;
    let mut protect_system = None;
    let mut read_write_paths = Vec::new();
    let mut unit_env = base_env_without_overrides(env);

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "ExecStart" if !value.is_empty() => exec_start = Some(value.to_owned()),
                "ProtectHome" => protect_home = Some(value.to_owned()),
                "ProtectSystem" => protect_system = Some(value.to_owned()),
                "ReadWritePaths" => {
                    for token in split_simple_args(value) {
                        read_write_paths.push(expand_systemd_path(&token, home));
                    }
                }
                "Environment" => {
                    if value.is_empty() {
                        unit_env.data_dir = None;
                        unit_env.server_config = None;
                    } else {
                        for token in split_simple_args(value) {
                            if let Some((k, v)) = token.split_once('=') {
                                if v.is_empty() {
                                    continue;
                                }
                                match k {
                                    "AGENT_WECHAT_DATA_DIR" => {
                                        unit_env.data_dir = Some(expand_systemd_path(v, home));
                                    }
                                    "AGENT_WECHAT_SERVER_CONFIG" => {
                                        unit_env.server_config = Some(expand_systemd_path(v, home));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let mut data_dir = None;
    let mut listen = None;
    let mut server_config = None;
    if let Some(exec) = &exec_start {
        let tokens = split_simple_args(exec);
        let parsed = parse_wx_serve_args(&tokens, home);
        if let Some(parsed) = parsed {
            data_dir = parsed.data_dir;
            listen = parsed.listen;
            server_config = parsed.server_config;
        }
    }
    if data_dir.is_none() || listen.is_none() {
        let overrides = Overrides {
            server_config: server_config.clone(),
            data_dir: data_dir.clone(),
            listen,
            ..Default::default()
        };
        if let Ok(resolved) = config::resolve(&overrides, &unit_env, cwd) {
            if data_dir.is_none() {
                data_dir = Some(resolved.data_dir);
            }
            if listen.is_none() {
                listen = Some(resolved.listen);
            }
            if server_config.is_none() && resolved.config_present {
                server_config = Some(resolved.config_file);
            }
        }
    }

    let enabled = path
        .parent()
        .map(|dir| dir.join("default.target.wants/agent-wechat-native.service"))
        .is_some_and(|want| fs::symlink_metadata(want).is_ok());

    SystemdUnitInfo {
        path: path.to_path_buf(),
        enabled,
        data_dir,
        listen,
        server_config,
        protect_home,
        protect_system,
        read_write_paths,
    }
}

pub(crate) fn read_systemd_unit(env: &Environment, cwd: &Path) -> Option<SystemdUnitInfo> {
    let path = systemd_unit_path(env)?;
    let bytes = read_bounded_regular_file(&path, 64 * 1024)?;
    let mut combined = String::from_utf8(bytes).ok()?;
    // Also apply sorted `agent-wechat-native.service.d/*.conf` drop-ins (e.g. from `systemctl --user edit`).
    let drop_in_dir = path.with_file_name("agent-wechat-native.service.d");
    if let Ok(entries) = fs::read_dir(&drop_in_dir) {
        let mut confs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("conf"))
            .collect();
        confs.sort();
        for conf in confs.into_iter().take(32) {
            if let Some(extra) = read_bounded_regular_file(&conf, 64 * 1024) {
                if let Ok(text) = String::from_utf8(extra) {
                    combined.push('\n');
                    combined.push_str(&text);
                }
            }
        }
    }
    Some(parse_systemd_unit(&path, &combined, env, cwd))
}

#[derive(Debug, Default)]
struct ParsedServeArgs {
    data_dir: Option<PathBuf>,
    listen: Option<SocketAddr>,
    server_config: Option<PathBuf>,
    token_file: Option<PathBuf>,
    token_stdin: bool,
    exit_with_parent: bool,
}

fn is_wx_executable(arg0: &str) -> bool {
    let name = Path::new(arg0)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(arg0);
    let lower = name.to_ascii_lowercase();
    lower == "wx" || lower == "wx.exe" || lower == "wx-dev" || lower.starts_with("wx-")
}

fn parse_wx_serve_args(args: &[String], home: Option<&Path>) -> Option<ParsedServeArgs> {
    if args.is_empty() {
        return None;
    }
    let first = args[0].trim_start_matches('-');
    // Match only the actual `wx` binary, not a wrapper such as `node .../native-launcher.mjs`,
    // which spawns `wx` as a child process.
    if !is_wx_executable(first) {
        return None;
    }
    let has_serve = args.iter().skip(1).any(|a| a == "serve");
    if !has_serve {
        return None;
    }

    let mut out = ParsedServeArgs::default();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        let next_val = |idx: &mut usize| -> Option<String> {
            if let Some((_, val)) = arg.split_once('=') {
                Some(val.to_owned())
            } else {
                *idx += 1;
                args.get(*idx).cloned()
            }
        };
        if arg == "--data-dir" || arg.starts_with("--data-dir=") {
            if let Some(val) = next_val(&mut i) {
                out.data_dir = Some(expand_systemd_path(&val, home));
            }
        } else if arg == "--listen" || arg.starts_with("--listen=") {
            if let Some(val) = next_val(&mut i) {
                out.listen = val.parse().ok();
            }
        } else if arg == "--server-config" || arg.starts_with("--server-config=") {
            if let Some(val) = next_val(&mut i) {
                out.server_config = Some(expand_systemd_path(&val, home));
            }
        } else if arg == "--token-file" || arg.starts_with("--token-file=") {
            if let Some(val) = next_val(&mut i) {
                out.token_file = Some(expand_systemd_path(&val, home));
            }
        } else if arg == "--token-stdin" {
            out.token_stdin = true;
        } else if arg == "--exit-with-parent" {
            out.exit_with_parent = true;
        }
        i += 1;
    }
    Some(out)
}

#[cfg(target_os = "linux")]
fn linux_proc_uid(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_proc_ppid_and_alive(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    if tail.first() == Some(&"Z") {
        return None;
    }
    tail.get(1)?.parse().ok()
}

#[cfg(target_os = "linux")]
fn linux_proc_comm(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_owned())
}

#[cfg(target_os = "linux")]
fn linux_proc_cmdline(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    if raw.is_empty() {
        return None;
    }
    let joined = raw
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(target_os = "linux")]
fn resolve_against(path: PathBuf, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

#[cfg(target_os = "linux")]
fn linux_proc_env_overrides(pid: u32, base: &Environment) -> Environment {
    let mut env = base.clone();
    let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) else {
        return env;
    };
    for entry in raw.split(|&b| b == 0) {
        if let Ok(text) = std::str::from_utf8(entry) {
            if let Some((k, v)) = text.split_once('=') {
                if v.is_empty() {
                    continue;
                }
                match k {
                    "AGENT_WECHAT_DATA_DIR" => env.data_dir = Some(PathBuf::from(v)),
                    "AGENT_WECHAT_SERVER_CONFIG" => env.server_config = Some(PathBuf::from(v)),
                    _ => {}
                }
            }
        }
    }
    env
}

#[cfg(target_os = "linux")]
fn scan_wx_serve_processes_impl(
    env: &Environment,
    cwd: &Path,
    current_user_only: bool,
) -> Vec<RunningWxProcess> {
    let base_env = base_env_without_overrides(env);
    let my_uid = unsafe { libc::geteuid() };
    let my_pid = std::process::id();
    let mut results = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return results;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == my_pid {
            continue;
        }
        let uid = linux_proc_uid(pid);
        if current_user_only && uid != Some(my_uid) {
            continue;
        }
        let Some(ppid) = linux_proc_ppid_and_alive(pid) else {
            continue;
        };
        let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        let args: Vec<String> = raw
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let Some(parsed) = parse_wx_serve_args(&args, base_env.home.as_deref()) else {
            continue;
        };
        let proc_cwd =
            fs::read_link(format!("/proc/{pid}/cwd")).unwrap_or_else(|_| cwd.to_path_buf());
        let mut data_dir = parsed.data_dir.map(|p| resolve_against(p, &proc_cwd));
        let mut listen = parsed.listen;
        let server_config = parsed.server_config.map(|p| resolve_against(p, &proc_cwd));
        let mut token_file = parsed.token_file.map(|p| resolve_against(p, &proc_cwd));
        if data_dir.is_none() || listen.is_none() || token_file.is_none() {
            let proc_env = linux_proc_env_overrides(pid, &base_env);
            let overrides = Overrides {
                server_config: server_config.clone(),
                data_dir: data_dir.clone(),
                listen,
                token_file: token_file.clone(),
                ..Default::default()
            };
            if let Ok(resolved) = config::resolve(&overrides, &proc_env, &proc_cwd) {
                data_dir.get_or_insert(resolved.data_dir);
                listen.get_or_insert(resolved.listen);
                token_file.get_or_insert(resolved.token_file);
            }
        }
        let parent_comm = linux_proc_comm(ppid);
        let parent_cmdline = linux_proc_cmdline(ppid);
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok();
        results.push(RunningWxProcess {
            pid,
            uid,
            parent_comm,
            parent_cmdline,
            cgroup,
            data_dir,
            listen: listen.unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap()),
            server_config,
            token_file,
            token_stdin: parsed.token_stdin,
            exit_with_parent: parsed.exit_with_parent,
        });
    }
    results
}

#[cfg(target_os = "macos")]
fn macos_proc_argv(pid: u32) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size: libc::size_t = 0;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < 4
        || size > 256 * 1024
    {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || size < 4
    {
        return None;
    }
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf[..4].try_into().ok()?);
    if argc <= 0 {
        return None;
    }
    let mut pos = 4usize;
    // Skip executable path + trailing NUL padding before argv[0].
    while pos < buf.len() && buf[pos] != 0 {
        pos += 1;
    }
    while pos < buf.len() && buf[pos] == 0 {
        pos += 1;
    }
    let mut args = Vec::new();
    for _ in 0..argc {
        if pos >= buf.len() {
            break;
        }
        let start = pos;
        while pos < buf.len() && buf[pos] != 0 {
            pos += 1;
        }
        args.push(String::from_utf8_lossy(&buf[start..pos]).into_owned());
        pos += 1;
    }
    Some(args)
}

#[cfg(target_os = "macos")]
fn scan_wx_serve_processes_impl(
    env: &Environment,
    cwd: &Path,
    current_user_only: bool,
) -> Vec<RunningWxProcess> {
    let base_env = base_env_without_overrides(env);
    let mut results = Vec::new();
    let uid = unsafe { libc::geteuid() };
    let my_pid = std::process::id();
    let mut cmd = std::process::Command::new("ps");
    if current_user_only {
        cmd.args(["-u", &uid.to_string()]);
    } else {
        cmd.arg("-e");
    }
    cmd.args(["-o", "pid="]);
    let Ok(output) = cmd.output() else {
        return results;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let Some(pid) = line.trim().parse::<u32>().ok() else {
            continue;
        };
        if pid == my_pid {
            continue;
        }
        let Some(args) = macos_proc_argv(pid) else {
            continue;
        };
        let Some(parsed) = parse_wx_serve_args(&args, base_env.home.as_deref()) else {
            continue;
        };
        let mut data_dir = parsed.data_dir;
        let mut listen = parsed.listen;
        let mut token_file = parsed.token_file;
        if data_dir.is_none() || listen.is_none() || token_file.is_none() {
            let overrides = Overrides {
                server_config: parsed.server_config.clone(),
                data_dir: data_dir.clone(),
                listen,
                token_file: token_file.clone(),
                ..Default::default()
            };
            if let Ok(resolved) = config::resolve(&overrides, &base_env, cwd) {
                data_dir.get_or_insert(resolved.data_dir);
                listen.get_or_insert(resolved.listen);
                token_file.get_or_insert(resolved.token_file);
            }
        }
        results.push(RunningWxProcess {
            pid,
            parent_comm: None,
            parent_cmdline: None,
            cgroup: None,
            data_dir,
            listen: listen.unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap()),
            server_config: parsed.server_config,
            token_file,
            token_stdin: parsed.token_stdin,
            exit_with_parent: parsed.exit_with_parent,
        });
    }
    results
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn scan_wx_serve_processes_impl(
    _env: &Environment,
    _cwd: &Path,
    _current_user_only: bool,
) -> Vec<RunningWxProcess> {
    Vec::new()
}

pub(crate) fn scan_user_wx_serve_processes(env: &Environment, cwd: &Path) -> Vec<RunningWxProcess> {
    scan_wx_serve_processes_impl(env, cwd, true)
}

#[cfg(target_os = "linux")]
pub(crate) fn scan_other_uid_wx_serve_processes(
    env: &Environment,
    cwd: &Path,
) -> Vec<RunningWxProcess> {
    let my_uid = unsafe { libc::geteuid() };
    scan_wx_serve_processes_impl(env, cwd, false)
        .into_iter()
        .filter(|p| p.uid.is_some_and(|u| u != my_uid))
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn scan_other_uid_wx_serve_processes(
    _env: &Environment,
    _cwd: &Path,
) -> Vec<RunningWxProcess> {
    Vec::new()
}

pub(crate) fn classify_process_owner(
    proc: &RunningWxProcess,
    desktop_dir: Option<&Path>,
    systemd_unit: Option<&SystemdUnitInfo>,
    process_record_live_pid: Option<u32>,
) -> DaemonOwner {
    let canonical_data = proc.data_dir.as_deref().map(canonical_or_original);
    if process_record_live_pid == Some(proc.pid) {
        return DaemonOwner::WxDaemon;
    }
    if let (Some(data), Some(hub)) = (canonical_data.as_deref(), desktop_dir) {
        if data == canonical_or_original(hub) {
            return DaemonOwner::Desktop;
        }
    }
    if proc
        .server_config
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        == Some("desktop-server.toml")
        || proc
            .parent_comm
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("agent-wechat"))
    {
        return DaemonOwner::Desktop;
    }
    if proc
        .cgroup
        .as_deref()
        .is_some_and(|cg| cg.contains("agent-wechat-native.service"))
        || (proc.parent_comm.as_deref() == Some("systemd")
            && systemd_unit
                .and_then(|u| u.data_dir.as_deref())
                .zip(canonical_data.as_deref())
                .is_some_and(|(u, d)| canonical_or_original(u) == d))
    {
        return DaemonOwner::Systemd;
    }
    let node_parent_is_openclaw = proc.parent_comm.as_deref() == Some("node")
        && proc.parent_cmdline.as_deref().is_some_and(|cmd| {
            let lower = cmd.to_ascii_lowercase();
            lower.contains("openclaw") && !lower.contains("native-launcher")
        });
    if proc
        .parent_comm
        .as_deref()
        .is_some_and(|c| matches!(c, "openclaw" | "openclaw-gateway"))
        || node_parent_is_openclaw
        || (proc.token_stdin && proc.exit_with_parent)
    {
        return DaemonOwner::OpenClaw;
    }
    DaemonOwner::Unknown
}

pub(crate) fn discover_daemons(
    env: &Environment,
    cwd: &Path,
    running_procs: &[RunningWxProcess],
    systemd_unit: Option<&SystemdUnitInfo>,
) -> Vec<DiscoveredDaemon> {
    let mut by_dir: BTreeMap<PathBuf, DiscoveredDaemon> = BTreeMap::new();
    let desktop_dir = desktop_hub_dir(env);

    // 1. Running `wx serve` processes for the current user.
    for proc in running_procs {
        let Some(raw_dir) = &proc.data_dir else {
            continue;
        };
        let dir = canonical_or_original(raw_dir);
        let record = read_process_record(&dir).ok().flatten();
        let live_record_pid = record.as_ref().filter(|r| r.pid_is_live()).map(|r| r.pid);
        let owner =
            classify_process_owner(proc, desktop_dir.as_deref(), systemd_unit, live_record_pid);
        by_dir.insert(
            dir.clone(),
            DiscoveredDaemon {
                owner,
                address: format!("http://{}", proc.listen),
                listen: Some(proc.listen),
                data_dir: dir,
                state: format!("running (pid {})", proc.pid),
                pid: Some(proc.pid),
                server_config: proc.server_config.clone(),
                token_file: proc.token_file.clone(),
            },
        );
    }

    // 2. Default folder (if it exists or has a process.json or registry.sqlite).
    let base_env = base_env_without_overrides(env);
    if let Some((default_dir, _)) = config::default_data_dir(&base_env) {
        let dir = canonical_or_original(&default_dir);
        if dir.exists() && !by_dir.contains_key(&dir) {
            let record = read_process_record(&dir).ok().flatten();
            let resolved = config::resolve(&Overrides::default(), &base_env, cwd).ok();
            let (owner, address, listen, state, pid) = match record {
                Some(rec) if rec.pid_is_live() => (
                    DaemonOwner::WxDaemon,
                    rec.endpoint.clone(),
                    rec.endpoint
                        .trim_start_matches("http://")
                        .parse::<SocketAddr>()
                        .ok(),
                    format!("running (pid {})", rec.pid),
                    Some(rec.pid),
                ),
                Some(rec) => (
                    DaemonOwner::WxDaemon,
                    rec.endpoint.clone(),
                    rec.endpoint
                        .trim_start_matches("http://")
                        .parse::<SocketAddr>()
                        .ok(),
                    "stopped (stale process.json)".into(),
                    None,
                ),
                None => {
                    let listen = resolved
                        .as_ref()
                        .map(|c| c.listen)
                        .unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap());
                    let is_systemd = systemd_unit
                        .and_then(|u| u.data_dir.as_deref())
                        .is_some_and(|u| canonical_or_original(u) == dir);
                    let owner = if is_systemd {
                        DaemonOwner::Systemd
                    } else if dir.join("daemon").exists() {
                        DaemonOwner::WxDaemon
                    } else {
                        DaemonOwner::Unknown
                    };
                    (
                        owner,
                        format!("http://{listen}"),
                        Some(listen),
                        "stopped".into(),
                        None,
                    )
                }
            };
            by_dir.insert(
                dir.clone(),
                DiscoveredDaemon {
                    owner,
                    address,
                    listen,
                    data_dir: dir,
                    state,
                    pid,
                    server_config: resolved
                        .as_ref()
                        .filter(|c| c.config_present)
                        .map(|c| c.config_file.clone()),
                    token_file: resolved.map(|c| c.token_file),
                },
            );
        }
    }

    // 3. Desktop app hub folder.
    if let Some(hub_dir) = desktop_dir {
        let dir = canonical_or_original(&hub_dir);
        if dir.exists() && !by_dir.contains_key(&dir) {
            let port = desktop_hub_port(env);
            let listen = SocketAddr::from(([127, 0, 0, 1], port));
            by_dir.insert(
                dir.clone(),
                DiscoveredDaemon {
                    owner: DaemonOwner::Desktop,
                    address: format!("http://{listen}"),
                    listen: Some(listen),
                    data_dir: dir.clone(),
                    state: "stopped".into(),
                    pid: None,
                    server_config: dir
                        .join("desktop-server.toml")
                        .exists()
                        .then(|| dir.join("desktop-server.toml")),
                    token_file: None,
                },
            );
        }
    }

    // 4. Systemd user unit's ExecStart folder.
    if let Some(unit) = systemd_unit {
        if let Some(raw_dir) = &unit.data_dir {
            let dir = canonical_or_original(raw_dir);
            if !by_dir.contains_key(&dir) {
                let listen = unit
                    .listen
                    .unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap());
                by_dir.insert(
                    dir.clone(),
                    DiscoveredDaemon {
                        owner: DaemonOwner::Systemd,
                        address: format!("http://{listen}"),
                        listen: Some(listen),
                        data_dir: dir,
                        state: if unit.enabled {
                            "stopped (systemd enabled)".into()
                        } else {
                            "stopped (systemd unit)".into()
                        },
                        pid: None,
                        server_config: unit.server_config.clone(),
                        token_file: None,
                    },
                );
            }
        }
    }

    by_dir.into_values().collect()
}

/// Probe a loopback TCP address to see if anything is listening and identify its owner.
pub(crate) async fn probe_port_owner(
    addr: SocketAddr,
    running_procs: &[RunningWxProcess],
    desktop_dir: Option<&Path>,
    systemd_unit: Option<&SystemdUnitInfo>,
) -> PortOwner {
    let listening = TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok();
    if !listening {
        return PortOwner {
            listening: false,
            health_ok: false,
            pid: None,
            process_name: None,
            daemon_owner: None,
            data_dir: None,
        };
    }

    let health_ok = check_health(addr).await;

    // First match against discovered `wx serve` processes listening on this address.
    if let Some(proc) = running_procs.iter().find(|p| p.listen == addr) {
        let live_rec = proc
            .data_dir
            .as_deref()
            .and_then(|d| read_process_record(d).ok().flatten())
            .filter(|r| r.pid_is_live())
            .map(|r| r.pid);
        let owner = classify_process_owner(proc, desktop_dir, systemd_unit, live_rec);
        return PortOwner {
            listening: true,
            health_ok,
            pid: Some(proc.pid),
            process_name: Some("wx".into()),
            daemon_owner: Some(owner),
            data_dir: proc.data_dir.clone(),
        };
    }

    // Fallback on Linux: inspect /proc/net/tcp{,6} + /proc/<pid>/fd to identify non-wx owners.
    #[cfg(target_os = "linux")]
    if let Some((pid, comm)) = linux_port_owner(addr) {
        return PortOwner {
            listening: true,
            health_ok,
            pid: Some(pid),
            process_name: comm,
            daemon_owner: None,
            data_dir: None,
        };
    }

    PortOwner {
        listening: true,
        health_ok,
        pid: None,
        process_name: None,
        daemon_owner: None,
        data_dir: None,
    }
}

async fn check_health(addr: SocketAddr) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    let url = format!("http://{addr}/health");
    let Ok(resp) = client.get(url).send().await else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    resp.json::<serde_json::Value>()
        .await
        .is_ok_and(|v| v["status"] == "ok")
}

#[cfg(target_os = "linux")]
fn linux_port_owner(addr: SocketAddr) -> Option<(u32, Option<String>)> {
    let inode = linux_listening_socket_inode(addr)?;
    let needle = format!("socket:[{inode}]");
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = fs::read_link(fd.path()) {
                if target.to_str() == Some(needle.as_str()) {
                    return Some((pid, linux_proc_comm(pid)));
                }
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_listening_socket_inode(addr: SocketAddr) -> Option<u64> {
    let (proc_file, wanted_hex_ip) = match addr.ip() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            (
                "/proc/net/tcp",
                format!("{:02X}{:02X}{:02X}{:02X}", o[3], o[2], o[1], o[0]),
            )
        }
        IpAddr::V6(v6) => {
            let mut hex = String::with_capacity(32);
            for chunk in v6.octets().chunks_exact(4) {
                use std::fmt::Write;
                let _ = write!(
                    &mut hex,
                    "{:02X}{:02X}{:02X}{:02X}",
                    chunk[3], chunk[2], chunk[1], chunk[0]
                );
            }
            ("/proc/net/tcp6", hex)
        }
    };
    let wanted_port = format!("{:04X}", addr.port());
    let any_ip = match addr.ip() {
        IpAddr::V4(_) => "00000000",
        IpAddr::V6(_) => "00000000000000000000000000000000",
    };
    let text = fs::read_to_string(proc_file).ok()?;
    for line in text.lines().skip(1) {
        let cols: Vec<_> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        // State 0A = TCP_LISTEN
        if cols[3] != "0A" {
            continue;
        }
        let Some((ip_hex, port_hex)) = cols[1].split_once(':') else {
            continue;
        };
        if port_hex == wanted_port && (ip_hex == wanted_hex_ip || ip_hex == any_ip) {
            return cols[9].parse::<u64>().ok();
        }
    }
    // Also check IPv4-mapped IPv6 listener on [::]:port if addr is IPv4.
    if matches!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST)) {
        let v6_any = Ipv6Addr::UNSPECIFIED;
        return linux_listening_socket_inode(SocketAddr::new(IpAddr::V6(v6_any), addr.port()));
    }
    None
}

#[cfg(target_os = "linux")]
fn collect_profile_inodes(profile_path: &Path, ids: &mut std::collections::BTreeSet<(u64, u64)>) {
    use std::os::unix::fs::MetadataExt;
    const MAX_PROFILE_INODES: usize = 256;

    let global = profile_path.join("xwechat_files/all_users/config/global_config");
    if let Ok(m) = fs::symlink_metadata(&global) {
        if m.is_file() {
            ids.insert((m.dev(), m.ino()));
        }
    }
    let mut db_dirs = Vec::new();
    let direct_db = profile_path.join("db_storage");
    if direct_db.is_dir() {
        db_dirs.push(direct_db);
    }
    if let Ok(items) = fs::read_dir(profile_path.join("xwechat_files")) {
        for item in items.flatten().take(64) {
            let db_storage = item.path().join("db_storage");
            if db_storage.is_dir() {
                db_dirs.push(db_storage);
            }
        }
    }
    for db_dir in db_dirs {
        if ids.len() >= MAX_PROFILE_INODES {
            break;
        }
        let Ok(entries) = fs::read_dir(&db_dir) else {
            continue;
        };
        for entry in entries.flatten().take(64) {
            if ids.len() >= MAX_PROFILE_INODES {
                break;
            }
            let path = entry.path();
            let Ok(m) = fs::symlink_metadata(&path) else {
                continue;
            };
            if wechat_platform::is_link(&m) {
                continue;
            }
            if m.is_file() {
                ids.insert((m.dev(), m.ino()));
            } else if m.is_dir() {
                // Profile databases live one folder deeper: db_storage/message/message_0.db,
                // db_storage/contact/contact.db, db_storage/session/session.db, etc.
                if let Ok(sub_entries) = fs::read_dir(&path) {
                    for sub in sub_entries.flatten().take(64) {
                        if ids.len() >= MAX_PROFILE_INODES {
                            break;
                        }
                        if let Ok(sm) = fs::symlink_metadata(sub.path()) {
                            if !wechat_platform::is_link(&sm) && sm.is_file() {
                                ids.insert((sm.dev(), sm.ino()));
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_pid_holds_inodes(pid: u32, ids: &std::collections::BTreeSet<(u64, u64)>) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    for fd in fds.flatten() {
        if let Ok(meta) = fd.path().metadata() {
            if ids.contains(&(meta.dev(), meta.ino())) {
                return true;
            }
        }
    }
    false
}

/// Best-effort check whether an official WeChat desktop process appears to be running (warning only).
pub(crate) fn official_wechat_running(profile_path: Option<&Path>) -> bool {
    #[cfg(target_os = "linux")]
    {
        let my_pid = std::process::id();
        let Ok(entries) = fs::read_dir("/proc") else {
            return false;
        };
        let mut profile_inodes = std::collections::BTreeSet::new();
        if let Some(root) = profile_path {
            collect_profile_inodes(root, &mut profile_inodes);
        }
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == my_pid {
                continue;
            }
            if let Some(comm) = linux_proc_comm(pid) {
                let lower = comm.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "wechat" | "wechat-universal" | "wechat.exe" | "wechatappex"
                ) {
                    return true;
                }
            }
            if !profile_inodes.is_empty() {
                if let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) {
                    let args: Vec<String> = raw
                        .split(|&b| b == 0)
                        .filter(|s| !s.is_empty())
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .collect();
                    if parse_wx_serve_args(&args, None).is_some() {
                        continue;
                    }
                }
                if linux_pid_holds_inodes(pid, &profile_inodes) {
                    return true;
                }
            }
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        let _ = profile_path;
        std::process::Command::new("pgrep")
            .args(["-x", "WeChat"])
            .output()
            .is_ok_and(|o| o.status.success())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = profile_path;
        false
    }
}

/// Check if a process running as another UID holds open file descriptors to any database under `profile_path`.
#[cfg(target_os = "linux")]
pub(crate) fn other_uid_holds_profile(profile_path: &Path) -> Option<u32> {
    use std::collections::BTreeSet;

    let my_uid = unsafe { libc::geteuid() };
    let mut ids = BTreeSet::new();
    collect_profile_inodes(profile_path, &mut ids);
    if ids.is_empty() {
        return None;
    }
    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(uid) = linux_proc_uid(pid) else {
            continue;
        };
        if uid == my_uid {
            continue;
        }
        if linux_pid_holds_inodes(pid, &ids) {
            return Some(uid);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn other_uid_holds_profile(_profile_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_systemd_unit_exec_start_and_sandbox_paths() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let env = Environment {
            home: Some(home.to_path_buf()),
            ..Default::default()
        };
        let unit_text = "\
[Service]
ExecStart=%h/.local/bin/wx serve --data-dir %h/custom-data --listen 127.0.0.1:6188
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=%h/custom-data %h/xwechat_files %h/.xwechat
";
        let unit = parse_systemd_unit(
            &home.join(".config/systemd/user/agent-wechat-native.service"),
            unit_text,
            &env,
            home,
        );
        assert_eq!(
            unit.data_dir.as_deref(),
            Some(home.join("custom-data").as_path())
        );
        assert_eq!(unit.listen, Some("127.0.0.1:6188".parse().unwrap()));
        assert_eq!(unit.protect_home.as_deref(), Some("read-only"));
        assert_eq!(unit.protect_system.as_deref(), Some("strict"));
        assert_eq!(
            unit.read_write_paths,
            vec![
                home.join("custom-data"),
                home.join("xwechat_files"),
                home.join(".xwechat"),
            ]
        );
    }

    #[test]
    fn parses_wx_serve_command_line_arguments() {
        let args: Vec<String> = [
            "/usr/local/bin/wx",
            "serve",
            "--data-dir",
            "/tmp/data-1",
            "--listen=127.0.0.1:6199",
            "--token-stdin",
            "--exit-with-parent",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let parsed = parse_wx_serve_args(&args, None).unwrap();
        assert_eq!(parsed.data_dir, Some(PathBuf::from("/tmp/data-1")));
        assert_eq!(parsed.listen, Some("127.0.0.1:6199".parse().unwrap()));
        assert!(parsed.token_stdin);
        assert!(parsed.exit_with_parent);

        let non_serve: Vec<String> = ["wx", "status"].into_iter().map(String::from).collect();
        assert!(parse_wx_serve_args(&non_serve, None).is_none());
    }

    #[test]
    fn systemd_unit_default_data_dir_ignores_callers_env_override() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let env = Environment {
            home: Some(home.to_path_buf()),
            data_dir: Some(home.join("caller-override")),
            ..Default::default()
        };
        let unit = parse_systemd_unit(
            &home.join(".config/systemd/user/agent-wechat-native.service"),
            "[Service]\nExecStart=%h/.local/bin/wx serve\n",
            &env,
            home,
        );
        assert_eq!(
            unit.data_dir.as_deref(),
            Some(home.join(".local/share/agent-wechat").as_path())
        );
    }

    #[test]
    fn systemd_unit_parses_environment_overrides() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let env = Environment {
            home: Some(home.to_path_buf()),
            ..Default::default()
        };
        let unit = parse_systemd_unit(
            &home.join(".config/systemd/user/agent-wechat-native.service"),
            "[Service]\nEnvironment=AGENT_WECHAT_DATA_DIR=%h/env-data\nExecStart=%h/.local/bin/wx serve\n",
            &env,
            home,
        );
        assert_eq!(
            unit.data_dir.as_deref(),
            Some(home.join("env-data").as_path())
        );
    }

    #[test]
    fn desktop_hub_port_reads_settings_desktop_json() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let env = Environment {
            home: Some(home.to_path_buf()),
            config_home: Some(home.join("cfg")),
            data_home: Some(home.join("data")),
            ..Default::default()
        };
        let settings_file = desktop_config_file(&env).unwrap();
        fs::create_dir_all(settings_file.parent().unwrap()).unwrap();
        fs::write(&settings_file, r#"{"localPort": 6299}"#).unwrap();
        assert_eq!(desktop_hub_port(&env), 6299);

        let hub_dir = desktop_hub_dir(&env).unwrap();
        fs::create_dir_all(&hub_dir).unwrap();
        let discovered = discover_daemons(&env, home, &[], None);
        let desktop = discovered
            .iter()
            .find(|d| d.owner == DaemonOwner::Desktop)
            .expect("desktop daemon should be discovered");
        assert_eq!(desktop.address, "http://127.0.0.1:6299");
    }

    #[test]
    fn rejects_native_launcher_wrapper_and_does_not_label_it_openclaw() {
        let launcher_args: Vec<String> = [
            "node",
            "/usr/lib/node_modules/@agent-wechat/cli/bin/native-launcher.mjs",
            "serve",
            "--data-dir",
            "/tmp/d",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(parse_wx_serve_args(&launcher_args, None).is_none());

        let proc = RunningWxProcess {
            pid: 4242,
            #[cfg(target_os = "linux")]
            uid: Some(1000),
            parent_comm: Some("node".into()),
            parent_cmdline: Some(
                "node /usr/lib/node_modules/@agent-wechat/cli/bin/native-launcher.mjs serve".into(),
            ),
            cgroup: None,
            data_dir: Some(PathBuf::from("/tmp/d")),
            listen: "127.0.0.1:6174".parse().unwrap(),
            server_config: None,
            token_file: None,
            token_stdin: false,
            exit_with_parent: false,
        };
        assert_eq!(
            classify_process_owner(&proc, None, None, None),
            DaemonOwner::Unknown
        );

        let openclaw_proc = RunningWxProcess {
            parent_cmdline: Some("node /opt/openclaw/gateway.mjs".into()),
            ..proc
        };
        assert_eq!(
            classify_process_owner(&openclaw_proc, None, None, None),
            DaemonOwner::OpenClaw
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn collect_profile_inodes_and_fd_detection_cover_nested_db_storage_files() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("direct-profile");
        let msg_dir = profile.join("db_storage/message");
        fs::create_dir_all(&msg_dir).unwrap();
        let msg_db = msg_dir.join("message_0.db");
        fs::write(&msg_db, b"SQLite format 3\0").unwrap();
        let meta = fs::symlink_metadata(&msg_db).unwrap();

        let mut ids = std::collections::BTreeSet::new();
        collect_profile_inodes(&profile, &mut ids);
        assert!(
            ids.contains(&(meta.dev(), meta.ino())),
            "nested db_storage/message/message_0.db inode should be collected"
        );

        let _open_file = fs::File::open(&msg_db).unwrap();
        assert!(
            linux_pid_holds_inodes(std::process::id(), &ids),
            "current process holding nested db_storage/message/message_0.db should be detected"
        );
    }
}
