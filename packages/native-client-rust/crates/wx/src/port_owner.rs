//! Best-effort identification of the local process owning a loopback TCP port.
//!
//! Designed for reuse by connection error diagnostics and `wx daemon doctor`.
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Local process identified as listening on a loopback port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PortOwner {
    /// The AgentWechat desktop app's bundled hub (`wx serve --exit-with-parent`
    /// with a data directory ending in `io.github.thisnick.agent-wechat/hub`).
    DesktopHub {
        pid: Option<u32>,
        data_dir: Option<PathBuf>,
    },
    /// Another local `wx serve` daemon process.
    WxDaemon {
        pid: Option<u32>,
        data_dir: Option<PathBuf>,
        exit_with_parent: bool,
    },
    /// A non-`wx` local process listening on the port.
    OtherProcess { pid: u32, name: String },
    /// Could not determine which process owns the port.
    Unknown,
}

impl PortOwner {
    /// Human-readable description of the owner, when identified.
    pub fn describe(&self) -> Option<String> {
        match self {
            Self::DesktopHub { pid, .. } => Some(match pid {
                Some(pid) => format!("the AgentWechat desktop app's hub (PID {pid})"),
                None => "the AgentWechat desktop app's hub".into(),
            }),
            Self::WxDaemon {
                pid,
                data_dir,
                exit_with_parent,
            } => {
                let mut parts = Vec::new();
                if let Some(pid) = pid {
                    parts.push(format!("PID {pid}"));
                }
                if let Some(dir) = data_dir {
                    parts.push(format!("data folder {}", dir.display()));
                }
                if *exit_with_parent {
                    parts.push("managed by a parent process".into());
                }
                if parts.is_empty() {
                    Some("another wx daemon".into())
                } else {
                    Some(format!("another wx daemon ({})", parts.join(", ")))
                }
            }
            Self::OtherProcess { pid, name } => Some(format!("{name} (PID {pid})")),
            Self::Unknown => None,
        }
    }
}

/// Returns true when `path` ends with the desktop app's hub data directory
/// (`io.github.thisnick.agent-wechat/hub`), accepting `/` or `\` separators.
pub fn is_desktop_hub_data_dir(path: &Path) -> bool {
    let raw = path.to_string_lossy();
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    trimmed == "io.github.thisnick.agent-wechat/hub"
        || trimmed.ends_with("/io.github.thisnick.agent-wechat/hub")
}

/// Parsed command-line metadata for a `wx serve` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedWxServe {
    data_dir: Option<PathBuf>,
    listen_port: Option<u16>,
    has_server_config: bool,
    exit_with_parent: bool,
}

fn executable_stem(argv0: &str) -> String {
    let base = argv0.rsplit(['/', '\\']).next().unwrap_or(argv0);
    base.strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base)
        .to_ascii_lowercase()
}

fn parse_listen_port(value: &str) -> Option<u16> {
    if let Ok(addr) = value.parse::<std::net::SocketAddr>() {
        return Some(addr.port());
    }
    value.rsplit_once(':')?.1.parse::<u16>().ok()
}

/// Global flags accepted by `wx` that consume the next argument as a value.
fn takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--server-config"
            | "--data-dir"
            | "--token-file"
            | "--listen"
            | "--network-listen"
            | "--tls-cert-file"
            | "--tls-key-file"
            | "--ca-cert-file"
            | "--config"
            | "--client-config"
            | "--server"
            | "--url"
            | "--server-name"
            | "--local-profile"
            | "--account"
    )
}

fn parse_wx_serve_args(args: &[String]) -> Option<ParsedWxServe> {
    let first = args.first()?;
    let stem = executable_stem(first);
    if !matches!(stem.as_str(), "wx" | "wx-native") {
        return None;
    }
    let mut is_serve = false;
    let mut data_dir = None;
    let mut listen_port = None;
    let mut has_server_config = false;
    let mut exit_with_parent = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if !is_serve && !arg.starts_with('-') {
            if arg == "serve" {
                is_serve = true;
                index += 1;
                continue;
            }
            // Another subcommand (e.g. `wx contacts find serve`) is not `wx serve`.
            return None;
        }
        if arg == "--exit-with-parent" {
            exit_with_parent = true;
            index += 1;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--data-dir=") {
            if !value.is_empty() {
                data_dir = Some(PathBuf::from(value));
            }
            index += 1;
            continue;
        }
        if arg == "--data-dir" {
            if let Some(value) = args.get(index + 1) {
                if !value.is_empty() {
                    data_dir = Some(PathBuf::from(value));
                }
                index += 2;
                continue;
            }
        }
        if let Some(value) = arg.strip_prefix("--listen=") {
            listen_port = parse_listen_port(value);
            index += 1;
            continue;
        }
        if arg == "--listen" {
            if let Some(value) = args.get(index + 1) {
                listen_port = parse_listen_port(value);
                index += 2;
                continue;
            }
        }
        if arg == "--server-config" {
            has_server_config = true;
            index += if args.get(index + 1).is_some() { 2 } else { 1 };
            continue;
        }
        if arg.starts_with("--server-config=") {
            has_server_config = true;
            index += 1;
            continue;
        }
        if takes_value(arg) {
            index += if args.get(index + 1).is_some() { 2 } else { 1 };
            continue;
        }
        index += 1;
    }
    if !is_serve {
        return None;
    }
    Some(ParsedWxServe {
        data_dir,
        listen_port,
        has_server_config,
        exit_with_parent,
    })
}

/// Split a `ps -ww` command line into argv tokens while preserving spaces in
/// executable paths before `serve` and in path-valued flags such as
/// `--data-dir /Users/.../Application Support/io.github.thisnick.agent-wechat/hub`.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_args_line(line: &str) -> Vec<String> {
    let raw: Vec<&str> = line.split_whitespace().collect();
    if raw.is_empty() {
        return Vec::new();
    }
    // Rejoin spaces in argv[0] up to the first flag or `serve` token.
    let mut args = Vec::new();
    let mut index = 0;
    let mut exe = String::new();
    while index < raw.len() {
        let token = raw[index];
        if index > 0 && (token.starts_with('-') || token == "serve") {
            break;
        }
        if !exe.is_empty() {
            exe.push(' ');
        }
        exe.push_str(token);
        index += 1;
    }
    if !exe.is_empty() {
        args.push(exe);
    }
    while index < raw.len() {
        let token = raw[index];
        if takes_value(token) {
            args.push(token.to_owned());
            index += 1;
            if index < raw.len() {
                let mut value = raw[index].to_owned();
                index += 1;
                while index < raw.len() && !raw[index].starts_with('-') && raw[index] != "serve" {
                    value.push(' ');
                    value.push_str(raw[index]);
                    index += 1;
                }
                args.push(value);
            }
            continue;
        }
        if let Some((flag, first_part)) = token.split_once('=') {
            if takes_value(flag) {
                let mut combined = format!("{flag}={first_part}");
                index += 1;
                while index < raw.len() && !raw[index].starts_with('-') && raw[index] != "serve" {
                    combined.push(' ');
                    combined.push_str(raw[index]);
                    index += 1;
                }
                args.push(combined);
                continue;
            }
        }
        args.push(token.to_owned());
        index += 1;
    }
    args
}

/// Classify a process whose PID is already known to own the listening socket.
pub fn classify_wx_args_for_pid(pid: Option<u32>, args: &[String]) -> Option<PortOwner> {
    let parsed = parse_wx_serve_args(args)?;
    Some(owner_from_parsed(pid, parsed))
}

/// Classify a `wx serve` process by its arguments when scanning processes for
/// `expected_port`.
pub fn classify_wx_args(
    pid: Option<u32>,
    args: &[String],
    expected_port: u16,
) -> Option<PortOwner> {
    let parsed = parse_wx_serve_args(args)?;
    let matches_port = match parsed.listen_port {
        Some(port) => port == expected_port,
        None => !parsed.has_server_config && expected_port == 6174,
    };
    if !matches_port {
        return None;
    }
    Some(owner_from_parsed(pid, parsed))
}

fn owner_from_parsed(pid: Option<u32>, parsed: ParsedWxServe) -> PortOwner {
    if parsed.exit_with_parent
        && parsed
            .data_dir
            .as_deref()
            .is_some_and(is_desktop_hub_data_dir)
    {
        PortOwner::DesktopHub {
            pid,
            data_dir: parsed.data_dir,
        }
    } else {
        PortOwner::WxDaemon {
            pid,
            data_dir: parsed.data_dir,
            exit_with_parent: parsed.exit_with_parent,
        }
    }
}

/// Best-effort identification of the local process listening on loopback `port`.
pub fn identify(port: u16) -> PortOwner {
    #[cfg(target_os = "linux")]
    {
        linux::identify(port)
    }
    #[cfg(target_os = "macos")]
    {
        macos::identify(port)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = port;
        PortOwner::Unknown
    }
}

/// Best-effort, warning-only check for running official WeChat desktop processes
/// on Linux. The daemon's own file-lock checks remain authoritative.
pub fn find_running_wechat_pids() -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        linux::find_running_wechat_pids()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_net_tcp_inodes(content: &str, expected_port: u16, ipv6: bool) -> Vec<u64> {
    let mut inodes = Vec::new();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 {
            continue;
        }
        // State 0A is TCP_LISTEN.
        if fields[3] != "0A" {
            continue;
        }
        let Some((ip_hex, port_hex)) = fields[1].split_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        if port != expected_port {
            continue;
        }
        let is_local_bind = if ipv6 {
            parse_proc_ipv6(ip_hex).is_some_and(|ip| {
                ip.is_loopback()
                    || ip.is_unspecified()
                    || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            })
        } else {
            parse_proc_ipv4(ip_hex).is_some_and(|ip| ip.is_loopback() || ip.is_unspecified())
        };
        if !is_local_bind {
            continue;
        }
        if let Ok(inode) = fields[9].parse::<u64>() {
            if inode > 0 {
                inodes.push(inode);
            }
        }
    }
    inodes
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_ipv4(hex: &str) -> Option<std::net::Ipv4Addr> {
    if hex.len() != 8 {
        return None;
    }
    let raw = u32::from_str_radix(hex, 16).ok()?;
    Some(std::net::Ipv4Addr::from(raw.to_ne_bytes()))
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_ipv6(hex: &str) -> Option<std::net::Ipv6Addr> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, chunk) in bytes.chunks_exact_mut(4).enumerate() {
        let word = u32::from_str_radix(&hex[index * 8..(index + 1) * 8], 16).ok()?;
        chunk.copy_from_slice(&word.to_ne_bytes());
    }
    Some(std::net::Ipv6Addr::from(bytes))
}

#[cfg(any(target_os = "linux", test))]
fn is_official_wechat_process(comm: &str, args: &[String]) -> bool {
    let comm_lower = comm.trim().to_ascii_lowercase();
    let argv0 = args
        .first()
        .map(|value| executable_stem(value))
        .unwrap_or_default();
    if comm_lower.contains("agent-wechat")
        || args.iter().any(|arg| arg.contains("agent-wechat"))
        || matches!(argv0.as_str(), "wx" | "wx-native")
    {
        return false;
    }
    matches!(comm_lower.as_str(), "wechat" | "weixin" | "wechat-app")
        || matches!(argv0.as_str(), "wechat" | "weixin" | "wechat-app")
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::HashSet;

    fn read_cmdline(pid: u32) -> Option<Vec<String>> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        if bytes.is_empty() {
            return None;
        }
        let args = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        (!args.is_empty()).then_some(args)
    }

    fn read_comm(pid: u32) -> Option<String> {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    fn listening_inodes(port: u16) -> (HashSet<u64>, bool) {
        let mut set = HashSet::new();
        let mut readable = false;
        if let Ok(content) = std::fs::read_to_string("/proc/net/tcp") {
            readable = true;
            set.extend(parse_proc_net_tcp_inodes(&content, port, false));
        }
        if let Ok(content) = std::fs::read_to_string("/proc/net/tcp6") {
            readable = true;
            set.extend(parse_proc_net_tcp_inodes(&content, port, true));
        }
        (set, readable)
    }

    fn pid_owns_inode(pid: u32, inodes: &HashSet<u64>) -> bool {
        let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            return false;
        };
        for entry in entries.flatten() {
            let Ok(target) = std::fs::read_link(entry.path()) else {
                continue;
            };
            let target = target.to_string_lossy();
            if let Some(rest) = target
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                if rest
                    .parse::<u64>()
                    .is_ok_and(|inode| inodes.contains(&inode))
                {
                    return true;
                }
            }
        }
        false
    }

    fn proc_pids() -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect()
    }

    pub(super) fn identify(port: u16) -> PortOwner {
        let (inodes, proc_net_readable) = listening_inodes(port);
        if proc_net_readable && inodes.is_empty() {
            // Nothing is listening on this port.
            return PortOwner::Unknown;
        }
        let pids = proc_pids();
        if !inodes.is_empty() {
            for &pid in &pids {
                if pid_owns_inode(pid, &inodes) {
                    if let Some(args) = read_cmdline(pid) {
                        if let Some(owner) = classify_wx_args_for_pid(Some(pid), &args) {
                            return owner;
                        }
                        let name = read_comm(pid).unwrap_or_else(|| executable_stem(&args[0]));
                        return PortOwner::OtherProcess { pid, name };
                    }
                    if let Some(name) = read_comm(pid) {
                        return PortOwner::OtherProcess { pid, name };
                    }
                }
            }
        }
        // Fallback when another user owns the listening socket (so /proc/<pid>/fd
        // is unreadable) or /proc/net/tcp is restricted: scan readable cmdlines.
        let self_pid = std::process::id();
        for pid in pids {
            if pid == self_pid {
                continue;
            }
            if let Some(args) = read_cmdline(pid) {
                if let Some(owner) = classify_wx_args(Some(pid), &args, port) {
                    return owner;
                }
            }
        }
        PortOwner::Unknown
    }

    pub(super) fn find_running_wechat_pids() -> Vec<u32> {
        let self_pid = std::process::id();
        let mut found = Vec::new();
        for pid in proc_pids() {
            if pid == self_pid {
                continue;
            }
            let comm = read_comm(pid).unwrap_or_default();
            let args = read_cmdline(pid).unwrap_or_default();
            if is_official_wechat_process(&comm, &args) {
                found.push(pid);
            }
        }
        found.sort_unstable();
        found
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::process::Command;

    pub(super) fn identify(port: u16) -> PortOwner {
        let Ok(output) = Command::new("ps")
            .args(["-ax", "-ww", "-o", "pid=,args="])
            .output()
        else {
            return PortOwner::Unknown;
        };
        if !output.status.success() {
            return PortOwner::Unknown;
        }
        let self_pid = std::process::id();
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            let Some((pid_str, rest)) = trimmed.split_once(char::is_whitespace) else {
                continue;
            };
            let Ok(pid) = pid_str.parse::<u32>() else {
                continue;
            };
            if pid == self_pid {
                continue;
            }
            let args = parse_ps_args_line(rest);
            if let Some(owner) = classify_wx_args(Some(pid), &args, port) {
                return owner;
            }
        }
        PortOwner::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn desktop_hub_is_recognised_across_platform_paths_and_flag_forms() {
        for args in [
            strings(&[
                "/opt/AgentWechat/wx",
                "serve",
                "--exit-with-parent",
                "--data-dir",
                "/home/alice/.local/share/io.github.thisnick.agent-wechat/hub",
                "--server-config",
                "/home/alice/.local/share/io.github.thisnick.agent-wechat/hub/desktop-server.toml",
                "--listen",
                "127.0.0.1:6174",
                "--token-stdin",
            ]),
            strings(&[
                "/Applications/AgentWechat.app/Contents/MacOS/wx",
                "serve",
                "--exit-with-parent",
                "--data-dir=/Users/alice/Library/Application Support/io.github.thisnick.agent-wechat/hub/",
                "--listen=127.0.0.1:6174",
            ]),
            strings(&[
                r"C:\Program Files\AgentWechat\wx.exe",
                "--data-dir",
                r"C:\Users\alice\AppData\Local\io.github.thisnick.agent-wechat\hub",
                "--listen",
                "127.0.0.1:6174",
                "serve",
                "--exit-with-parent",
            ]),
        ] {
            assert!(
                matches!(
                    classify_wx_args(Some(42), &args, 6174),
                    Some(PortOwner::DesktopHub { pid: Some(42), .. })
                ),
                "expected DesktopHub for {args:?}"
            );
        }
    }

    #[test]
    fn non_desktop_wx_serve_processes_are_not_misclassified_as_desktop_hub() {
        let managed = strings(&[
            "/usr/local/bin/wx",
            "serve",
            "--data-dir",
            "/home/alice/.local/share/agent-wechat",
            "--listen",
            "127.0.0.1:6174",
            "--token-file",
            "/home/alice/.local/share/agent-wechat/api-token",
        ]);
        assert_eq!(
            classify_wx_args(Some(10), &managed, 6174),
            Some(PortOwner::WxDaemon {
                pid: Some(10),
                data_dir: Some(PathBuf::from("/home/alice/.local/share/agent-wechat")),
                exit_with_parent: false,
            })
        );

        let openclaw = strings(&[
            "/usr/local/bin/wx",
            "serve",
            "--exit-with-parent",
            "--data-dir",
            "/home/alice/.openclaw/wechat-hub",
            "--listen",
            "127.0.0.1:6174",
            "--token-stdin",
        ]);
        assert_eq!(
            classify_wx_args(Some(11), &openclaw, 6174),
            Some(PortOwner::WxDaemon {
                pid: Some(11),
                data_dir: Some(PathBuf::from("/home/alice/.openclaw/wechat-hub")),
                exit_with_parent: true,
            })
        );

        let wrong_port = strings(&["wx", "serve", "--listen", "127.0.0.1:6180"]);
        assert_eq!(classify_wx_args(Some(12), &wrong_port, 6174), None);
        assert!(matches!(
            classify_wx_args(Some(12), &wrong_port, 6180),
            Some(PortOwner::WxDaemon { pid: Some(12), .. })
        ));

        let bare_serve = strings(&["wx", "serve"]);
        assert!(matches!(
            classify_wx_args(Some(13), &bare_serve, 6174),
            Some(PortOwner::WxDaemon { pid: Some(13), .. })
        ));
        assert_eq!(classify_wx_args(Some(13), &bare_serve, 6180), None);

        let non_serve = strings(&["wx", "status"]);
        assert_eq!(classify_wx_args(Some(14), &non_serve, 6174), None);

        let subcommand_with_serve_arg = strings(&["wx", "contacts", "find", "serve"]);
        assert_eq!(
            classify_wx_args(Some(15), &subcommand_with_serve_arg, 6174),
            None
        );
    }

    #[test]
    fn macos_ps_line_preserves_spaces_in_application_support_data_dir() {
        let line = "/Applications/AgentWechat.app/Contents/MacOS/wx serve --exit-with-parent --data-dir /Users/alice/Library/Application Support/io.github.thisnick.agent-wechat/hub --server-config /Users/alice/Library/Application Support/io.github.thisnick.agent-wechat/hub/desktop-server.toml --listen 127.0.0.1:6174 --token-stdin";
        let args = parse_ps_args_line(line);
        assert_eq!(
            classify_wx_args(Some(77), &args, 6174),
            Some(PortOwner::DesktopHub {
                pid: Some(77),
                data_dir: Some(PathBuf::from(
                    "/Users/alice/Library/Application Support/io.github.thisnick.agent-wechat/hub"
                )),
            })
        );
    }

    #[test]
    fn proc_net_tcp_parser_extracts_listening_loopback_inodes_only() {
        let ipv4_sample = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:181E 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0
   1: 0100007F:181E 0100007F:9999 01 00000000:00000000 00:00000000 00000000  1000        0 99999 1 0000000000000000 100 0 0 10 0
   2: 00000000:181E 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 54321 1 0000000000000000 100 0 0 10 0
   3: 0100007F:181F 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 77777 1 0000000000000000 100 0 0 10 0
";
        let expected = if cfg!(target_endian = "little") {
            vec![12345, 54321]
        } else {
            vec![54321]
        };
        assert_eq!(
            parse_proc_net_tcp_inodes(ipv4_sample, 6174, false),
            expected
        );

        let ipv6_sample = "\
  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:181E 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 24680 1 0000000000000000 100 0 0 10 0
";
        if cfg!(target_endian = "little") {
            assert_eq!(
                parse_proc_net_tcp_inodes(ipv6_sample, 6174, true),
                vec![24680]
            );
        }
    }

    #[test]
    fn official_wechat_process_detection_excludes_agent_wechat() {
        assert!(is_official_wechat_process(
            "wechat",
            &strings(&["/usr/bin/wechat"])
        ));
        assert!(!is_official_wechat_process(
            "wx",
            &strings(&["/usr/local/bin/wx", "serve"])
        ));
        assert!(!is_official_wechat_process(
            "agent-wechat",
            &strings(&["/opt/agent-wechat/agent-wechat"])
        ));
    }
}
