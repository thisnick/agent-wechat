//! Read-only diagnostics for a local daemon's data folder (`wx daemon doctor`).
//!
//! When the target daemon is not running and `--fix` is not passed, this module
//! never opens a database for writing, never takes `owner.lock` or `management.lock`,
//! never starts a daemon, and never contacts WeChat.
use crate::{
    config::{self, DataDirSource, Environment, Overrides, ServerConfig},
    doctor_discovery::{
        self, canonical_or_original, DaemonOwner, DiscoveredDaemon, ProcessRecord,
        RunningWxProcess, SystemdUnitInfo,
    },
    outcome,
    output::Output,
    Remote,
};
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Seek, SeekFrom},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::Duration,
};
use unicode_width::UnicodeWidthStr;
use wechat_store::{DatabaseInspection, WalInspection};

const TROUBLESHOOTING_DAEMON_URL: &str =
    "https://thisnick.github.io/agent-wechat/reference/troubleshooting/#starting-the-daemon";
const DOCKER_HINT: &str =
    "For daemons running in Docker, run `docker exec <container> wx daemon doctor`.";
const WARN_FREE_BYTES: u64 = 256 * 1024 * 1024;
const FAIL_FREE_BYTES: u64 = 32 * 1024 * 1024;
const WARN_CONTROL_BYTES: i64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Info,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Info => "INFO",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Check {
    pub(crate) id: String,
    pub(crate) status: CheckStatus,
    pub(crate) message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next: Option<String>,
}

impl Check {
    fn ok(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Ok,
            message: message.into(),
            next: None,
        }
    }
    fn info(id: impl Into<String>, message: impl Into<String>, next: Option<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Info,
            message: message.into(),
            next,
        }
    }
    fn warn(id: impl Into<String>, message: impl Into<String>, next: Option<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Warn,
            message: message.into(),
            next,
        }
    }
    fn fail(id: impl Into<String>, message: impl Into<String>, next: Option<String>) -> Self {
        Self {
            id: id.into(),
            status: CheckStatus::Fail,
            message: message.into(),
            next,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TargetSummary {
    pub(crate) data_dir: PathBuf,
    pub(crate) source: DataDirSource,
    pub(crate) server_config: PathBuf,
    pub(crate) server_config_present: bool,
    pub(crate) listen: String,
    pub(crate) token_file: PathBuf,
    pub(crate) daemon_running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) daemon_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) daemon_owner: Option<DaemonOwner>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OtherDaemonEntry {
    pub(crate) owner: DaemonOwner,
    pub(crate) address: String,
    pub(crate) data_dir: PathBuf,
    pub(crate) state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) checks: Option<Vec<Check>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bug_report: Option<BugReport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FilesystemReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fs_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) free_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) free_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) total_inodes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedactedDbFile {
    pub(crate) suffix: String,
    pub(crate) bytes: u64,
    pub(crate) private: bool,
    pub(crate) is_link: bool,
    pub(crate) header_hex: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedactedDatabaseReport {
    pub(crate) role: String,
    pub(crate) valid_sqlite_header: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) schema_version: Option<i64>,
    pub(crate) supported_schema_version: i64,
    pub(crate) settled: bool,
    pub(crate) files: Vec<RedactedDbFile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) wal_state: Option<String>,
    pub(crate) leftover_admission_dirs: usize,
    pub(crate) recovery_backup_dirs: usize,
}

/// Technical snapshot with personal paths, account IDs and secrets removed,
/// printed when doctor encounters a failed or unrecognized state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BugReport {
    pub(crate) wx_version: String,
    pub(crate) target: String,
    pub(crate) os: String,
    pub(crate) registry_schema_max: i64,
    pub(crate) account_schema_max: i64,
    pub(crate) filesystem: FilesystemReport,
    pub(crate) databases: Vec<RedactedDatabaseReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) unrecognized_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DoctorReport {
    pub(crate) target: TargetSummary,
    pub(crate) checks: Vec<Check>,
    pub(crate) others: Vec<OtherDaemonEntry>,
    pub(crate) docker_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bug_report: Option<BugReport>,
}

#[derive(Debug, Clone)]
pub(crate) struct DoctorOptions {
    pub(crate) all: bool,
    pub(crate) fix: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct LogErrorMatch {
    pub(crate) line: String,
    pub(crate) known_cause: Option<&'static str>,
    pub(crate) next_step: Option<String>,
}

/// Entry point called from `main.rs` for `wx daemon doctor`.
pub(crate) async fn run_cli(
    overrides: &Overrides,
    env: &Environment,
    cwd: &Path,
    options: DoctorOptions,
    output: Output,
    json_mode: bool,
) -> Result<()> {
    let report = diagnose(overrides, env, cwd, &options).await?;
    let any_fail = report.checks.iter().any(|c| c.status == CheckStatus::Fail)
        || report.others.iter().any(|o| {
            o.checks
                .as_ref()
                .is_some_and(|cs| cs.iter().any(|c| c.status == CheckStatus::Fail))
        });
    if json_mode {
        output.print(&report)?;
    } else {
        print!("{}", format_human(&report));
    }
    if any_fail {
        outcome::set_exit(1);
    }
    Ok(())
}

/// Core diagnostic routine, separated from printing so unit tests can inspect structured reports.
pub(crate) async fn diagnose(
    overrides: &Overrides,
    env: &Environment,
    cwd: &Path,
    options: &DoctorOptions,
) -> Result<DoctorReport> {
    let running_procs = doctor_discovery::scan_user_wx_serve_processes(env, cwd);
    let other_uid_procs = doctor_discovery::scan_other_uid_wx_serve_processes(env, cwd);
    let systemd_unit = doctor_discovery::read_systemd_unit(env, cwd);
    let desktop_dir = doctor_discovery::desktop_hub_dir(env);
    let discovered =
        doctor_discovery::discover_daemons(env, cwd, &running_procs, systemd_unit.as_ref());

    let (config, source, config_error) = match config::resolve_with_source(overrides, env, cwd) {
        Ok((cfg, src)) => (cfg, src, None),
        Err(err) => {
            // Even when server config is invalid, construct a fallback target so doctor can
            // still report the config error and inspect the likely data folder read-only.
            let config_file = overrides
                .server_config
                .clone()
                .or_else(|| env.server_config.clone())
                .or_else(|| config::default_config_file(env))
                .unwrap_or_else(|| cwd.join("server/config.toml"));
            let (data_dir, source) = if let Some(p) = &overrides.data_dir {
                (
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        cwd.join(p)
                    },
                    DataDirSource::Flag,
                )
            } else if let Some(p) = &env.data_dir {
                (
                    if p.is_absolute() {
                        p.clone()
                    } else {
                        cwd.join(p)
                    },
                    DataDirSource::Env,
                )
            } else {
                config::default_data_dir(env)
                    .unwrap_or_else(|| (cwd.join("agent-wechat"), DataDirSource::Default))
            };
            let listen = overrides
                .listen
                .unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap());
            let token_file = overrides
                .token_file
                .clone()
                .unwrap_or_else(|| data_dir.join("api-token"));
            let fallback = ServerConfig {
                schema_version: 1,
                config_file: config_file.clone(),
                config_present: config_file.exists(),
                data_dir,
                token_file,
                token_explicit: overrides.token_file.is_some(),
                storage_key_file: None,
                profile_format_key_file: None,
                listen,
                network_listen: None,
                tls_cert_file: None,
                tls_key_file: None,
            };
            (fallback, source, Some(err.to_string()))
        }
    };

    let target_dir = canonical_or_original(&config.data_dir);
    let mut official_profiles_by_dir: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for entry in &discovered {
        let profiles = read_official_profiles_readonly(&entry.data_dir);
        if !profiles.is_empty() {
            official_profiles_by_dir.insert(canonical_or_original(&entry.data_dir), profiles);
        }
    }
    let target_profiles = read_official_profiles_readonly(&target_dir);
    if !target_profiles.is_empty() {
        official_profiles_by_dir.insert(target_dir.clone(), target_profiles);
    }

    let (target_summary, checks, bug_report) = inspect_folder(
        &config,
        source,
        config_error.as_deref(),
        &running_procs,
        &other_uid_procs,
        desktop_dir.as_deref(),
        systemd_unit.as_ref(),
        &discovered,
        &official_profiles_by_dir,
        options.fix,
    )
    .await;

    let mut others = Vec::new();
    for item in &discovered {
        let item_dir = canonical_or_original(&item.data_dir);
        if item_dir == target_dir {
            continue;
        }
        let (other_checks, other_bug_report) = if options.all {
            let other_cfg = ServerConfig {
                schema_version: 1,
                config_file: item
                    .server_config
                    .clone()
                    .unwrap_or_else(|| item_dir.join("server.toml")),
                config_present: item.server_config.as_ref().is_some_and(|p| p.exists()),
                data_dir: item_dir.clone(),
                token_file: item
                    .token_file
                    .clone()
                    .unwrap_or_else(|| item_dir.join("api-token")),
                token_explicit: false,
                storage_key_file: None,
                profile_format_key_file: None,
                listen: item
                    .listen
                    .unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap()),
                network_listen: None,
                tls_cert_file: None,
                tls_key_file: None,
            };
            let (_, cs, br) = inspect_folder(
                &other_cfg,
                DataDirSource::Default,
                None,
                &running_procs,
                &other_uid_procs,
                desktop_dir.as_deref(),
                systemd_unit.as_ref(),
                &discovered,
                &official_profiles_by_dir,
                options.fix,
            )
            .await;
            (Some(cs), br)
        } else {
            (None, None)
        };
        others.push(OtherDaemonEntry {
            owner: item.owner,
            address: item.address.clone(),
            data_dir: item.data_dir.clone(),
            state: item.state.clone(),
            pid: item.pid,
            checks: other_checks,
            bug_report: other_bug_report,
        });
    }

    Ok(DoctorReport {
        target: target_summary,
        checks,
        others,
        docker_hint: DOCKER_HINT.into(),
        bug_report,
    })
}

#[allow(clippy::too_many_arguments)]
async fn inspect_folder(
    config: &ServerConfig,
    source: DataDirSource,
    config_error: Option<&str>,
    running_procs: &[RunningWxProcess],
    other_uid_procs: &[RunningWxProcess],
    desktop_dir: Option<&Path>,
    systemd_unit: Option<&SystemdUnitInfo>,
    discovered: &[DiscoveredDaemon],
    official_profiles_by_dir: &BTreeMap<PathBuf, Vec<PathBuf>>,
    fix: bool,
) -> (TargetSummary, Vec<Check>, Option<BugReport>) {
    let raw_data_dir = &config.data_dir;
    let raw_data_dir_meta = fs::symlink_metadata(raw_data_dir);
    let data_dir = canonical_or_original(raw_data_dir);
    let mut checks = Vec::new();
    let mut db_reports = Vec::new();
    let mut unrecognized_errors = Vec::new();
    let mut need_bug_report = false;

    // 1. Server configuration check
    if let Some(err) = config_error {
        checks.push(Check::fail(
            "server_config",
            format!(
                "Server configuration error at {}: {err}",
                config.config_file.display()
            ),
            Some(format!(
                "Fix or remove {} (must have schemaVersion = 1 and only supported fields).",
                config.config_file.display()
            )),
        ));
    } else if config.config_present {
        checks.push(Check::ok(
            "server_config",
            format!("Server config valid ({})", config.config_file.display()),
        ));
    } else {
        checks.push(Check::ok(
            "server_config",
            format!(
                "Using default server settings (no file at {})",
                config.config_file.display()
            ),
        ));
    }

    // Determine whether a daemon is currently running for this data_dir.
    let process_record_res = doctor_discovery::read_process_record(&data_dir);
    let live_process_record = process_record_res
        .as_ref()
        .ok()
        .and_then(|opt| opt.as_ref())
        .filter(|rec| rec.pid_is_live());

    let matching_proc = running_procs.iter().find(|p| {
        p.data_dir
            .as_deref()
            .is_some_and(|d| canonical_or_original(d) == data_dir)
    });

    let (
        daemon_running,
        daemon_pid,
        daemon_owner,
        active_listen,
        uses_token_stdin,
        active_token_file,
    ) = if let Some(proc) = matching_proc {
        let owner = doctor_discovery::classify_process_owner(
            proc,
            desktop_dir,
            systemd_unit,
            live_process_record.map(|r| r.pid),
        );
        let token_file = if !config.token_explicit {
            proc.token_file
                .clone()
                .unwrap_or_else(|| config.token_file.clone())
        } else {
            config.token_file.clone()
        };
        (
            true,
            Some(proc.pid),
            Some(owner),
            proc.listen,
            proc.token_stdin,
            token_file,
        )
    } else if let Some(rec) = live_process_record {
        let listen = rec
            .endpoint
            .trim_start_matches("http://")
            .parse::<SocketAddr>()
            .unwrap_or(config.listen);
        (
            true,
            Some(rec.pid),
            Some(DaemonOwner::WxDaemon),
            listen,
            false,
            config.token_file.clone(),
        )
    } else {
        let inferred_desktop = desktop_dir
            .is_some_and(|hub| canonical_or_original(hub) == data_dir)
            || data_dir.join("desktop-server.toml").exists();
        (
            false,
            None,
            None,
            config.listen,
            inferred_desktop,
            config.token_file.clone(),
        )
    };

    let target_summary = TargetSummary {
        data_dir: data_dir.clone(),
        source,
        server_config: config.config_file.clone(),
        server_config_present: config.config_present,
        listen: active_listen.to_string(),
        token_file: active_token_file.clone(),
        daemon_running,
        daemon_pid,
        daemon_owner,
    };

    // 2. Data folder existence, symlink, owner & permissions (checked on the raw path before canonicalizing)
    let data_dir_exists = match &raw_data_dir_meta {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            checks.push(Check::info(
                "data_dir",
                format!(
                    "Data folder does not exist yet ({}); it will be created on first start",
                    raw_data_dir.display()
                ),
                Some("Run `wx daemon start` to initialize and start the daemon.".into()),
            ));
            false
        }
        Ok(meta) if wechat_platform::is_link(meta) => {
            checks.push(check_private_path(
                raw_data_dir,
                "data_dir",
                "Data folder",
                true,
            ));
            data_dir.is_dir()
        }
        Ok(_) => {
            checks.push(check_private_path(
                raw_data_dir,
                "data_dir",
                "Data folder",
                true,
            ));
            data_dir.is_dir()
        }
        Err(_) => {
            checks.push(check_private_path(
                raw_data_dir,
                "data_dir",
                "Data folder",
                true,
            ));
            false
        }
    };

    // 3. Token file owner, permissions & format (never prints token value)
    checks.push(check_token_file(
        &active_token_file,
        config.token_explicit,
        daemon_running,
        uses_token_stdin,
    ));

    // 4. Process record (`daemon/process.json`)
    check_process_record(
        &data_dir,
        &process_record_res,
        daemon_running,
        daemon_pid,
        fix,
        &mut checks,
    );

    // 5. Port check when daemon is not running
    if !daemon_running {
        let port_owner = doctor_discovery::probe_port_owner(
            config.listen,
            running_procs,
            desktop_dir,
            systemd_unit,
        )
        .await;
        if port_owner.listening {
            let detail = match (
                port_owner.daemon_owner,
                port_owner.pid,
                port_owner.process_name.as_deref(),
                port_owner.data_dir.as_deref(),
            ) {
                (Some(owner), Some(pid), _, Some(dir)) => format!(
                    "Port {} is held by {} (PID {}, data folder {})",
                    config.listen,
                    owner.as_str(),
                    pid,
                    dir.display()
                ),
                (Some(owner), Some(pid), _, None) => format!(
                    "Port {} is held by {} (PID {})",
                    config.listen,
                    owner.as_str(),
                    pid
                ),
                (None, Some(pid), Some(name), _) => format!(
                    "Port {} is held by another program ({name}, PID {})",
                    config.listen, pid
                ),
                _ if port_owner.health_ok => format!(
                    "Port {} is held by another agent-wechat daemon (likely under another user or container)",
                    config.listen
                ),
                _ => format!(
                    "Port {} is already in use by another program",
                    config.listen
                ),
            };
            let next = if port_owner.daemon_owner == Some(DaemonOwner::Desktop) {
                "Connect to the Desktop app hub (Settings -> Export key for CLI), stop the Desktop hub, or pass a different --listen port.".into()
            } else {
                "Stop the program holding this port, connect to that daemon instead, or configure a different --listen port.".into()
            };
            checks.push(Check::fail("port", detail, Some(next)));
        } else {
            checks.push(Check::ok(
                "port",
                format!("Port {} is available", config.listen),
            ));
        }
    }

    // 6. Multiple configured owners check
    check_multiple_owners(
        &data_dir,
        active_listen,
        daemon_owner,
        systemd_unit,
        desktop_dir,
        discovered,
        &mut checks,
    );

    // 7. SQLite registry & account control databases (read-only admission checks!)
    let mut max_copy_needed: u64 = 0;
    let mut registered_accounts = Vec::new();
    let mut redaction_account_ids: BTreeSet<String> = BTreeSet::new();
    let registry_path = data_dir.join("registry.sqlite");
    if data_dir_exists {
        let reg_inspect = wechat_store::inspect_database_readonly(
            &registry_path,
            wechat_store::REGISTRY_SCHEMA_VERSION,
        );
        max_copy_needed = max_copy_needed.max(reg_inspect.copy_validation_bytes);
        let (check, redacted, is_problem) = evaluate_db_inspection(
            "sqlite.registry",
            "registry",
            "registry.sqlite",
            &registry_path,
            &reg_inspect,
            daemon_running,
        );
        if is_problem {
            need_bug_report = true;
        }
        if let Some(rep) = redacted {
            db_reports.push(rep);
        }
        checks.push(check);

        // Read registered accounts strictly read-only (`immutable=1`) if registry header is valid.
        if reg_inspect.exists && reg_inspect.valid_sqlite_header {
            if let Ok(accounts) = wechat_store::read_accounts_immutable(&registry_path) {
                registered_accounts = accounts;
            }
        }

        // Collect all account IDs from both filesystem (`accounts/*`) and registry.
        // Every non-dot folder name under `accounts/` is added to `redaction_account_ids`
        // so WAL-only or unreadable-registry accounts are still scrubbed from bug reports.
        let mut account_ids: BTreeSet<String> =
            registered_accounts.iter().map(|a| a.id.clone()).collect();
        redaction_account_ids.extend(account_ids.iter().cloned());
        let accounts_root = data_dir.join("accounts");
        if let Ok(entries) = fs::read_dir(&accounts_root) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if !name.starts_with('.') {
                        redaction_account_ids.insert(name.to_owned());
                        if entry.path().join("agent/control.sqlite").exists() {
                            account_ids.insert(name.to_owned());
                        }
                    }
                }
            }
        }

        for (index, account_id) in account_ids.iter().enumerate() {
            let control_path = data_dir
                .join("accounts")
                .join(account_id)
                .join("agent/control.sqlite");
            let inspect = wechat_store::inspect_database_readonly(
                &control_path,
                wechat_store::ACCOUNT_SCHEMA_VERSION,
            );
            max_copy_needed = max_copy_needed.max(inspect.copy_validation_bytes);
            let (check, redacted, is_problem) = evaluate_db_inspection(
                &format!("sqlite.account.{account_id}"),
                &format!("account[{index}]"),
                &format!("accounts/{account_id}/agent/control.sqlite"),
                &control_path,
                &inspect,
                daemon_running,
            );
            if is_problem {
                need_bug_report = true;
            }
            if let Some(rep) = redacted {
                db_reports.push(rep);
            }
            checks.push(check);
        }
    }

    // 8. Disk space, inodes & filesystem type
    let fs_report = inspect_filesystem(&data_dir);
    check_filesystem(
        &data_dir,
        &fs_report,
        if daemon_running { 0 } else { max_copy_needed },
        daemon_running,
        &mut checks,
    );

    // 9. Daemon log permissions & last fatal error
    let log_path = data_dir.join("daemon/daemon.log");
    let known_account_ids: Vec<String> = redaction_account_ids.into_iter().collect();
    check_daemon_log(
        &log_path,
        &data_dir,
        &known_account_ids,
        daemon_running,
        &mut checks,
        &mut unrecognized_errors,
        &mut need_bug_report,
    );

    // 10. Official / direct profile checks
    check_official_profiles(
        &data_dir,
        &registered_accounts,
        systemd_unit,
        other_uid_procs,
        official_profiles_by_dir,
        &mut checks,
    );

    // 11. FFmpeg & FFprobe availability
    checks.push(check_media_tools());

    // 12. Live running-daemon checks
    if daemon_running {
        check_running_daemon(
            &data_dir,
            &active_token_file,
            active_listen,
            daemon_pid,
            daemon_owner,
            uses_token_stdin,
            &mut checks,
        )
        .await;
    }

    let bug_report = if need_bug_report {
        let ver = wechat_api::version();
        let ver_json = serde_json::to_value(&ver).unwrap_or(Value::Null);
        Some(BugReport {
            wx_version: ver_json["version"]
                .as_str()
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .to_owned(),
            target: ver_json["target"].as_str().unwrap_or("unknown").to_owned(),
            os: std::env::consts::OS.to_owned(),
            registry_schema_max: wechat_store::REGISTRY_SCHEMA_VERSION,
            account_schema_max: wechat_store::ACCOUNT_SCHEMA_VERSION,
            filesystem: fs_report,
            databases: db_reports,
            unrecognized_errors,
        })
    } else {
        None
    };

    (target_summary, checks, bug_report)
}

fn check_private_path(path: &Path, id: &str, label: &str, expect_dir: bool) -> Check {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return Check::fail(
                id,
                format!("{label} cannot be inspected ({}): {e}", path.display()),
                Some(format!(
                    "Check parent directory permissions for {}.",
                    path.display()
                )),
            );
        }
    };
    if wechat_platform::is_link(&meta) {
        return Check::fail(
            id,
            format!(
                "{label} is a symlink ({}); the daemon refuses symlinks",
                path.display()
            ),
            Some(format!(
                "Use the real target path directly instead of symlink {}.",
                path.display()
            )),
        );
    }
    if expect_dir && !meta.is_dir() {
        return Check::fail(
            id,
            format!("{label} is not a directory ({})", path.display()),
            Some(format!(
                "Choose a directory path for {}.",
                label.to_lowercase()
            )),
        );
    }
    if !expect_dir && !meta.is_file() {
        return Check::fail(
            id,
            format!("{label} is not a regular file ({})", path.display()),
            Some(format!(
                "Replace {} with a regular private file.",
                path.display()
            )),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let my_uid = unsafe { libc::geteuid() };
        let owner_uid = meta.uid();
        let mode = meta.mode() & 0o777;
        if owner_uid != my_uid {
            let hint = if owner_uid == 0 {
                format!(
                    "{label} is owned by root (UID 0), not current UID {my_uid} ({})",
                    path.display()
                )
            } else if owner_uid == 10001 {
                format!(
                    "{label} is owned by container UID 10001, not current UID {my_uid} ({})",
                    path.display()
                )
            } else {
                format!(
                    "{label} is owned by UID {owner_uid}, not current UID {my_uid} ({})",
                    path.display()
                )
            };
            let next = if owner_uid == 10001 {
                "If this folder belongs to a Docker container, run `docker exec <container> wx daemon doctor`; otherwise transfer ownership with `sudo chown`.".into()
            } else {
                format!(
                    "Transfer ownership back to your user (for example `sudo chown -R $(id -u):$(id -g) {}`).",
                    path.display()
                )
            };
            return Check::fail(id, hint, Some(next));
        }
        if mode & 0o077 != 0 {
            let expected = if expect_dir { "700" } else { "600" };
            return Check::fail(
                id,
                format!(
                    "{label} has overly broad permissions {mode:04o} ({}); daemon requires owner-only 0{expected}",
                    path.display()
                ),
                Some(format!("Run `chmod {expected} {}`.", path.display())),
            );
        }
        Check::ok(
            id,
            format!(
                "{label} is owned by UID {owner_uid} with mode {mode:04o} ({})",
                path.display()
            ),
        )
    }
    #[cfg(not(unix))]
    {
        match wechat_platform::private_path(path) {
            Ok(true) => Check::ok(id, format!("{label} is private ({})", path.display())),
            Ok(false) => Check::fail(
                id,
                format!(
                    "{label} permissions are not owner-only ({})",
                    path.display()
                ),
                Some(format!(
                    "Restrict ACL/permissions on {} to your user.",
                    path.display()
                )),
            ),
            Err(e) => Check::fail(
                id,
                format!(
                    "Cannot verify {label} permissions ({}): {e}",
                    path.display()
                ),
                None,
            ),
        }
    }
}

fn check_token_file(
    token_file: &Path,
    token_explicit: bool,
    daemon_running: bool,
    uses_token_stdin: bool,
) -> Check {
    if !token_file.exists() && fs::symlink_metadata(token_file).is_err() {
        if uses_token_stdin {
            return Check::info(
                "token_file",
                format!(
                    "No API token file at {}; this daemon receives its key over stdin (--token-stdin)",
                    token_file.display()
                ),
                None,
            );
        }
        if token_explicit {
            return Check::fail(
                "token_file",
                format!("Configured API token file is missing ({})", token_file.display()),
                Some(format!(
                    "Create a private 0600 token file at {} or remove tokenFile from server config.",
                    token_file.display()
                )),
            );
        }
        if daemon_running {
            return Check::warn(
                "token_file",
                format!(
                    "API token file is missing ({}) while daemon is running",
                    token_file.display()
                ),
                Some("Check whether the daemon was started with --token-stdin or a different --token-file.".into()),
            );
        }
        return Check::ok(
            "token_file",
            format!(
                "Default API token file does not exist yet ({}); created on first start",
                token_file.display()
            ),
        );
    }

    let perm_check = check_private_path(token_file, "token_file", "API token file", false);
    if perm_check.status != CheckStatus::Ok {
        return perm_check;
    }

    match wechat_api::read_token(token_file) {
        Ok(_) => Check::ok(
            "token_file",
            format!(
                "API token file is present, private and valid ({})",
                token_file.display()
            ),
        ),
        Err(e) => Check::fail(
            "token_file",
            format!("API token file is invalid ({}): {e}", token_file.display()),
            Some(format!(
                "Replace {} with a 32..1024 byte printable ASCII token (mode 0600).",
                token_file.display()
            )),
        ),
    }
}

fn check_process_record(
    data_dir: &Path,
    record_res: &Result<Option<ProcessRecord>, String>,
    daemon_running: bool,
    daemon_pid: Option<u32>,
    fix: bool,
    checks: &mut Vec<Check>,
) {
    let record_path = data_dir.join("daemon/process.json");
    match record_res {
        Ok(None) => {
            checks.push(Check::ok(
                "process_record",
                "No stale daemon/process.json found",
            ));
        }
        Err(err) => {
            checks.push(Check::warn(
                "process_record",
                format!("Cannot parse daemon/process.json: {err}"),
                Some(format!(
                    "Check whether a daemon is running, then remove {}.",
                    record_path.display()
                )),
            ));
        }
        Ok(Some(rec)) => {
            let pid_live = rec.pid_is_live();
            if pid_live && rec.executable_matches() {
                checks.push(Check::ok(
                    "process_record",
                    format!(
                        "Managed daemon process.json matches running PID {} ({})",
                        rec.pid, rec.endpoint
                    ),
                ));
            } else if pid_live || daemon_pid == Some(rec.pid) {
                let message = if pid_live {
                    format!(
                        "Managed daemon PID {} is still running ({}), but its binary on disk no longer matches {} (for example after a wx upgrade)",
                        rec.pid,
                        rec.endpoint,
                        rec.executable.display()
                    )
                } else {
                    format!(
                        "Daemon PID {} is still running ({}); daemon/process.json was kept untouched",
                        rec.pid, rec.endpoint
                    )
                };
                checks.push(Check::warn(
                    "process_record",
                    message,
                    Some("Restart the daemon when convenient if you upgraded wx; do not delete process.json while the daemon is running.".into()),
                ));
            } else if fix {
                let daemon_dir = data_dir.join("daemon");
                let dir_safe = fs::symlink_metadata(&daemon_dir)
                    .is_ok_and(|m| m.is_dir() && !wechat_platform::is_link(&m))
                    && wechat_platform::private_path(&daemon_dir).unwrap_or(false);
                // Re-read process.json right before unlinking so we cannot remove a record
                // written by a concurrent `wx daemon start` or owned by a live PID.
                let still_stale = doctor_discovery::read_process_record(data_dir)
                    .ok()
                    .flatten()
                    .is_some_and(|fresh| !fresh.pid_is_live() && daemon_pid != Some(fresh.pid));
                let removable = dir_safe
                    && still_stale
                    && fs::symlink_metadata(&record_path)
                        .is_ok_and(|m| m.is_file() && !wechat_platform::is_link(&m))
                    && wechat_platform::private_path(&record_path).unwrap_or(false);
                if removable && fs::remove_file(&record_path).is_ok() {
                    checks.push(Check::ok(
                        "process_record",
                        format!(
                            "Removed stale daemon/process.json for dead PID {} (--fix)",
                            rec.pid
                        ),
                    ));
                } else {
                    checks.push(Check::warn(
                        "process_record",
                        format!(
                            "Could not safely remove stale daemon/process.json for dead PID {}",
                            rec.pid
                        ),
                        Some(format!(
                            "Inspect and remove {} manually.",
                            record_path.display()
                        )),
                    ));
                }
            } else {
                let state_note = if daemon_running {
                    "another daemon process is running for this folder"
                } else {
                    "process is no longer running"
                };
                checks.push(Check::warn(
                    "process_record",
                    format!(
                        "Leftover daemon/process.json for dead PID {} ({state_note})",
                        rec.pid
                    ),
                    Some("Run `wx daemon doctor --fix` to remove the stale process.json.".into()),
                ));
            }
        }
    }
}

fn check_multiple_owners(
    data_dir: &Path,
    active_listen: SocketAddr,
    active_owner: Option<DaemonOwner>,
    systemd_unit: Option<&SystemdUnitInfo>,
    desktop_dir: Option<&Path>,
    discovered: &[DiscoveredDaemon],
    checks: &mut Vec<Check>,
) {
    let mut owners_for_dir: BTreeSet<DaemonOwner> = BTreeSet::new();
    if let Some(owner) = active_owner {
        owners_for_dir.insert(owner);
    }
    if data_dir.join("daemon/process.json").exists() {
        owners_for_dir.insert(DaemonOwner::WxDaemon);
    }
    if systemd_unit
        .and_then(|u| u.data_dir.as_deref())
        .is_some_and(|u| canonical_or_original(u) == data_dir)
    {
        owners_for_dir.insert(DaemonOwner::Systemd);
    }
    if desktop_dir.is_some_and(|d| canonical_or_original(d) == data_dir) {
        owners_for_dir.insert(DaemonOwner::Desktop);
    }

    let same_port_others: Vec<&DiscoveredDaemon> = discovered
        .iter()
        .filter(|d| {
            canonical_or_original(&d.data_dir) != data_dir && d.listen == Some(active_listen)
        })
        .collect();

    if owners_for_dir.len() > 1 {
        let names = owners_for_dir
            .iter()
            .map(|o| o.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        checks.push(Check::warn(
            "owners",
            format!("More than one daemon owner is configured for this data folder ({names})"),
            Some("Pick one owner (wx daemon, systemd, desktop or OpenClaw) so they do not compete for the same data folder.".into()),
        ));
    } else if !same_port_others.is_empty() {
        let names = same_port_others
            .iter()
            .map(|d| format!("{} ({})", d.owner.as_str(), d.data_dir.display()))
            .collect::<Vec<_>>()
            .join(", ");
        checks.push(Check::warn(
            "owners",
            format!(
                "Other configured daemons share port {}: {names}",
                active_listen
            ),
            Some("Stop the other daemon or assign each daemon its own --listen port.".into()),
        ));
    } else {
        checks.push(Check::ok("owners", "No conflicting daemon owners detected"));
    }
}

fn evaluate_db_inspection(
    check_id: &str,
    redacted_role: &str,
    display_name: &str,
    db_path: &Path,
    inspect: &DatabaseInspection,
    daemon_running: bool,
) -> (Check, Option<RedactedDatabaseReport>, bool) {
    let files: Vec<RedactedDbFile> = inspect
        .members
        .iter()
        .map(|m| RedactedDbFile {
            suffix: m.suffix.clone(),
            bytes: m.bytes,
            private: m.private,
            is_link: m.is_link,
            header_hex: m.header_hex.clone(),
        })
        .collect();

    let wal_state = match &inspect.wal {
        WalInspection::Valid { retain_recovery } => Some(if *retain_recovery {
            "valid_with_recovery_backup".into()
        } else {
            "valid".into()
        }),
        WalInspection::Refused { reason } => Some(format!("refused: {reason}")),
        WalInspection::Skipped { reason } => Some(format!("skipped: {reason}")),
    };

    let report = RedactedDatabaseReport {
        role: redacted_role.to_owned(),
        valid_sqlite_header: inspect.valid_sqlite_header,
        schema_version: inspect.schema_version,
        supported_schema_version: inspect.supported_schema_version,
        settled: inspect.settled,
        files,
        wal_state,
        leftover_admission_dirs: inspect.leftover_admission_dirs,
        recovery_backup_dirs: inspect.recovery_backup_dirs,
    };

    if !inspect.exists {
        return (
            Check::info(check_id, format!("{display_name} does not exist yet"), None),
            None,
            false,
        );
    }

    // Check symlink or non-private permissions across family members.
    for member in &inspect.members {
        if member.is_link {
            return (
                Check::fail(
                    check_id,
                    format!(
                        "{display_name}{} is a symlink; daemon startup refuses symlinks",
                        member.suffix
                    ),
                    Some("Replace symlinks in the data folder with regular private files.".into()),
                ),
                Some(report),
                true,
            );
        }
        if !member.is_regular_file {
            return (
                Check::fail(
                    check_id,
                    format!("{display_name}{} is not a regular file", member.suffix),
                    Some(format!(
                        "Inspect {} before starting the daemon.",
                        db_path.display()
                    )),
                ),
                Some(report),
                true,
            );
        }
        if !member.private {
            return (
                Check::fail(
                    check_id,
                    format!(
                        "{display_name}{} does not have owner-only permissions",
                        member.suffix
                    ),
                    Some(format!(
                        "Fix ownership and run `chmod 600 {}*` while the daemon is stopped.",
                        db_path.display()
                    )),
                ),
                Some(report),
                false,
            );
        }
        if let Some(err) = &member.read_error {
            return (
                Check::fail(
                    check_id,
                    format!("Cannot read {display_name}{}: {err}", member.suffix),
                    Some("Check file permissions and filesystem health.".into()),
                ),
                Some(report),
                true,
            );
        }
    }

    // Main header check
    let main_bytes = inspect
        .members
        .iter()
        .find(|m| m.suffix.is_empty())
        .map_or(0, |m| m.bytes);
    if main_bytes > 0 && !inspect.valid_sqlite_header {
        return (
            Check::fail(
                check_id,
                format!("{display_name} does not have a valid SQLite 3 header ({main_bytes} bytes)"),
                Some(format!(
                    "Do not delete or overwrite the files. Restore from a backup or open an issue with the bug report below; see {TROUBLESHOOTING_DAEMON_URL}."
                )),
            ),
            Some(report),
            true,
        );
    }

    // Schema version check
    if let Some(version) = inspect.schema_version {
        if version > inspect.supported_schema_version {
            return (
                Check::fail(
                    check_id,
                    format!(
                        "{display_name} schema version is {version}, newer than this wx binary supports (max {})",
                        inspect.supported_schema_version
                    ),
                    Some("Upgrade wx to the newer release that created this database; downgrading is not supported.".into()),
                ),
                Some(report),
                false,
            );
        }
        if version < 0 {
            return (
                Check::fail(
                    check_id,
                    format!("{display_name} has an unrecognized negative schema version ({version})"),
                    Some(format!(
                        "Restore from backup or open an issue with the bug report below; see {TROUBLESHOOTING_DAEMON_URL}."
                    )),
                ),
                Some(report),
                true,
            );
        }
    }

    // WAL/SHM/journal admission check
    match &inspect.wal {
        WalInspection::Refused { reason } => {
            if daemon_running {
                // While the daemon holds an active WAL/SHM in memory, a snapshot read without
                // taking locks can observe transient writes; report as warning rather than hard fail.
                return (
                    Check::warn(
                        check_id,
                        format!(
                            "{display_name} WAL/SHM inspection noticed \"{reason}\" while daemon is running (live writes can look transient)"
                        ),
                        Some("Stop the daemon and re-run `wx daemon doctor` to verify offline database state.".into()),
                    ),
                    Some(report),
                    false,
                );
            }
            let next = wal_refusal_next_step(db_path, inspect, reason);
            (
                Check::fail(
                    check_id,
                    format!("{display_name} fails startup admission: {reason}"),
                    Some(next),
                ),
                Some(report),
                true,
            )
        }
        WalInspection::Skipped { reason } => (
            Check::warn(
                check_id,
                format!("{display_name} WAL admission check skipped: {reason}"),
                None,
            ),
            Some(report),
            true,
        ),
        WalInspection::Valid { retain_recovery } => {
            let ver_text = inspect
                .schema_version
                .map(|v| format!("schema v{v}/{}", inspect.supported_schema_version))
                .unwrap_or_else(|| "empty/uninitialized".into());
            let mut notes = Vec::new();
            if *retain_recovery && !daemon_running {
                let copy_mib = inspect.copy_validation_bytes.div_ceil(1 << 20).max(1);
                notes.push(format!(
                    "startup will validate on a copy (~{copy_mib} MiB free needed) and retain a .sqlite-recovery-* backup"
                ));
            } else if let Some(v) = inspect.schema_version {
                if v > 0 && v < inspect.supported_schema_version {
                    let copy_mib = inspect.copy_validation_bytes.div_ceil(1 << 20).max(1);
                    notes.push(format!(
                        "pending migration to v{} (~{copy_mib} MiB free needed)",
                        inspect.supported_schema_version
                    ));
                }
            }
            if inspect.leftover_admission_dirs > 0 {
                notes.push(format!(
                    "{} leftover .sqlite-admission-* scratch dir(s)",
                    inspect.leftover_admission_dirs
                ));
            }
            if inspect.recovery_backup_dirs > 0 {
                notes.push(format!(
                    "{} retained .sqlite-recovery-* backup dir(s)",
                    inspect.recovery_backup_dirs
                ));
            }

            if *retain_recovery && !daemon_running {
                (
                    Check::warn(
                        check_id,
                        format!(
                            "{display_name} ({ver_text}, {}): {}; existing files look admissible",
                            format_bytes(main_bytes),
                            notes.join("; ")
                        ),
                        Some("Ensure sufficient free disk space, then start the daemon; startup handles recovery automatically.".into()),
                    ),
                    Some(report),
                    false,
                )
            } else if !notes.is_empty() {
                (
                    Check::ok(
                        check_id,
                        format!(
                            "{display_name} OK ({ver_text}, {}; {})",
                            format_bytes(main_bytes),
                            notes.join("; ")
                        ),
                    ),
                    Some(report),
                    false,
                )
            } else {
                (
                    Check::ok(
                        check_id,
                        format!(
                            "{display_name} OK ({ver_text}, {})",
                            format_bytes(main_bytes)
                        ),
                    ),
                    Some(report),
                    false,
                )
            }
        }
    }
}

fn wal_refusal_next_step(db_path: &Path, inspect: &DatabaseInspection, reason: &str) -> String {
    let shm_bytes = inspect
        .members
        .iter()
        .find(|m| m.suffix == "-shm")
        .map(|m| m.bytes);
    let wal_bytes = inspect
        .members
        .iter()
        .find(|m| m.suffix == "-wal")
        .map(|m| m.bytes);
    if reason.contains("unrecognized database WAL index")
        && shm_bytes == Some(3)
        && wal_bytes.unwrap_or(0) == 0
    {
        return format!(
            "3-byte -shm beside an empty/missing -wal (known interrupted-open shape, #665). Keep the daemon stopped, back up {} with its -wal and -shm files, then upgrade wx or follow {TROUBLESHOOTING_DAEMON_URL}.",
            db_path.display()
        );
    }
    if reason.contains("commits from a newer log generation")
        || reason.contains("index exceeds log")
        || reason.contains("published frame is not a verified commit")
        || reason.contains("references a missing log")
        || reason.contains("WAL index")
    {
        return format!(
            "Stop the daemon and copy {} with its -wal and -shm files somewhere safe. Follow the WAL recovery steps at {TROUBLESHOOTING_DAEMON_URL} (never delete the main database file).",
            db_path.display()
        );
    }
    format!(
        "Stop the daemon, back up {} with its companion files, and restore from backup or open an issue with the redacted bug report below; see {TROUBLESHOOTING_DAEMON_URL}.",
        db_path.display()
    )
}

fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    while !cur.exists() {
        if !cur.pop() {
            return PathBuf::from("/");
        }
    }
    cur
}

fn inspect_filesystem(data_dir: &Path) -> FilesystemReport {
    let probe = nearest_existing_ancestor(data_dir);
    #[cfg(unix)]
    {
        use std::ffi::CString;
        let mut free_bytes = None;
        let mut total_bytes = None;
        let mut free_inodes = None;
        let mut total_inodes = None;
        if let Ok(cpath) = CString::new(wechat_platform::path_bytes(&probe)) {
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) } == 0 {
                #[allow(clippy::unnecessary_cast)]
                {
                    let unit = if stat.f_frsize > 0 {
                        stat.f_frsize as u64
                    } else {
                        stat.f_bsize as u64
                    };
                    free_bytes = Some((stat.f_bavail as u64).saturating_mul(unit));
                    total_bytes = Some((stat.f_blocks as u64).saturating_mul(unit));
                    if stat.f_files > 0 {
                        free_inodes = Some(stat.f_favail as u64);
                        total_inodes = Some(stat.f_files as u64);
                    }
                }
            }
        }
        let fs_type = unix_fs_type(&probe);
        FilesystemReport {
            fs_type,
            free_bytes,
            total_bytes,
            free_inodes,
            total_inodes,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = probe;
        FilesystemReport {
            fs_type: None,
            free_bytes: None,
            total_bytes: None,
            free_inodes: None,
            total_inodes: None,
        }
    }
}

#[cfg(target_os = "linux")]
fn unix_fs_type(path: &Path) -> Option<String> {
    let canonical = canonical_or_original(path);
    if let Ok(text) = fs::read_to_string("/proc/self/mountinfo") {
        if let Some(fs_type) = parse_linux_mountinfo(&text, &canonical) {
            return Some(fs_type);
        }
    }
    if let Ok(text) = fs::read_to_string("/proc/mounts") {
        if let Some(fs_type) = parse_linux_mounts(&text, &canonical) {
            return Some(fs_type);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn unix_fs_type(path: &Path) -> Option<String> {
    use std::ffi::{CStr, CString};
    let cpath = CString::new(wechat_platform::path_bytes(path)).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(cpath.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let name = unsafe { CStr::from_ptr(stat.f_fstypename.as_ptr()) };
    Some(name.to_string_lossy().into_owned())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unix_fs_type(_path: &Path) -> Option<String> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn unescape_mount_path(raw: &str) -> String {
    raw.replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn parse_linux_mountinfo(contents: &str, target: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in contents.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left_cols: Vec<_> = left.split_whitespace().collect();
        let right_cols: Vec<_> = right.split_whitespace().collect();
        if left_cols.len() < 5 || right_cols.is_empty() {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount_path(left_cols[4]));
        if target.starts_with(&mount_point) {
            let depth = mount_point.as_os_str().len();
            if best.as_ref().is_none_or(|(d, _)| depth >= *d) {
                best = Some((depth, right_cols[0].to_owned()));
            }
        }
    }
    best.map(|(_, fs_type)| fs_type)
}

#[cfg(target_os = "linux")]
fn parse_linux_mounts(contents: &str, target: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in contents.lines() {
        let cols: Vec<_> = line.split_whitespace().collect();
        if cols.len() < 3 {
            continue;
        }
        let mount_point = PathBuf::from(unescape_mount_path(cols[1]));
        if target.starts_with(&mount_point) {
            let depth = mount_point.as_os_str().len();
            if best.as_ref().is_none_or(|(d, _)| depth >= *d) {
                best = Some((depth, cols[2].to_owned()));
            }
        }
    }
    best.map(|(_, fs_type)| fs_type)
}

fn is_unsafe_sqlite_fs(fs_type: &str) -> Option<&'static str> {
    let lower = fs_type.to_ascii_lowercase();
    match lower.as_str() {
        "drvfs" | "9p" => Some("WSL/Windows mounted filesystem"),
        "nfs" | "nfs4" | "cifs" | "smb" | "smb3" | "smbfs" | "afpfs" | "webdav" | "fuse.sshfs"
        | "sshfs" | "fuse.rclone" | "glusterfs" | "ceph" | "lustre" | "afs" | "ncpfs"
        | "vboxsf" | "vmhgfs" | "fuse.gvfsd-fuse" => Some("network or shared-folder filesystem"),
        _ => None,
    }
}

fn check_filesystem(
    data_dir: &Path,
    fs_report: &FilesystemReport,
    copy_needed: u64,
    daemon_running: bool,
    checks: &mut Vec<Check>,
) {
    // Space & inodes check
    match (fs_report.free_bytes, fs_report.total_bytes) {
        (Some(free), Some(total)) => {
            let inode_part = match (fs_report.free_inodes, fs_report.total_inodes) {
                (Some(fi), Some(ti)) => format!(", {fi}/{ti} inodes free"),
                _ => String::new(),
            };
            let min_needed = if daemon_running {
                FAIL_FREE_BYTES
            } else {
                copy_needed.max(FAIL_FREE_BYTES)
            };
            let warn_needed = if daemon_running {
                WARN_FREE_BYTES
            } else {
                copy_needed
                    .saturating_add(64 * 1024 * 1024)
                    .max(WARN_FREE_BYTES)
            };
            if !daemon_running && free < min_needed {
                checks.push(Check::fail(
                    "disk.space",
                    format!(
                        "Low free space on {}: {} free of {}{inode_part} (startup needs at least {})",
                        data_dir.display(),
                        format_bytes(free),
                        format_bytes(total),
                        format_bytes(min_needed)
                    ),
                    Some("Free disk space before starting the daemon; SQLite validation copies need roughly 2x the database size.".into()),
                ));
            } else if !daemon_running && fs_report.free_inodes.is_some_and(|fi| fi < 100) {
                checks.push(Check::fail(
                    "disk.space",
                    format!(
                        "Critically low free inodes on {}: {} free{inode_part}",
                        data_dir.display(),
                        format_bytes(free)
                    ),
                    Some("Free inodes on this filesystem before starting the daemon.".into()),
                ));
            } else if free < warn_needed || fs_report.free_inodes.is_some_and(|fi| fi < 1000) {
                checks.push(Check::warn(
                    "disk.space",
                    format!(
                        "Limited disk headroom on {}: {} free of {}{inode_part}",
                        data_dir.display(),
                        format_bytes(free),
                        format_bytes(total)
                    ),
                    Some("Free disk space or move --data-dir to a larger volume.".into()),
                ));
            } else {
                checks.push(Check::ok(
                    "disk.space",
                    format!(
                        "{} free of {}{inode_part}",
                        format_bytes(free),
                        format_bytes(total)
                    ),
                ));
            }
        }
        _ => {
            checks.push(Check::info(
                "disk.space",
                "Disk space and inode counts are unavailable on this platform",
                None,
            ));
        }
    }

    // Filesystem type check
    if let Some(fs_type) = &fs_report.fs_type {
        if let Some(kind) = is_unsafe_sqlite_fs(fs_type) {
            checks.push(Check::warn(
                "disk.filesystem",
                format!(
                    "Data folder is on `{fs_type}` ({kind}); SQLite WAL locking and crash recovery are unreliable on this mount"
                ),
                Some("Move --data-dir to a local native filesystem (for example under your Linux home inside WSL instead of /mnt/c).".into()),
            ));
        } else {
            checks.push(Check::ok(
                "disk.filesystem",
                format!("Filesystem type: {fs_type}"),
            ));
        }
    }
}

/// Read the last error line from `daemon.log` (bounded to the trailing 64 KiB, read-only).
/// If `after_offset > 0`, only inspects bytes appended after that offset.
pub(crate) fn last_log_error(log_path: &Path, after_offset: u64) -> Option<LogErrorMatch> {
    let mut file = wechat_platform::OpenOptions::new()
        .read(true)
        .open(log_path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let len = meta.len();
    if len <= after_offset {
        return None;
    }
    const TAIL_LIMIT: u64 = 64 * 1024;
    let start = after_offset.max(len.saturating_sub(TAIL_LIMIT));
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    parse_last_log_error(&text)
}

pub(crate) fn parse_last_log_error(text: &str) -> Option<LogErrorMatch> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // If the most recent meaningful line is a successful startup readiness message,
    // check whether there is still an earlier error only when caller asks — wait:
    // we return the last error line after the last "Daemon ready" if one exists,
    // otherwise None (caller checks separately if needed).
    let last_ready_idx = lines
        .iter()
        .rposition(|l| l.starts_with("Daemon ready on ") || l.contains("\"state\":\"ready\""));
    let search_slice = match last_ready_idx {
        Some(idx) => &lines[idx + 1..],
        None => &lines[..],
    };
    let candidate = search_slice
        .iter()
        .rev()
        .find(|line| looks_like_error_line(line))?;
    Some(classify_log_error(candidate))
}

fn looks_like_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("error")
        || lower.contains("cannot ")
        || lower.contains("failed")
        || lower.contains("refused")
        || lower.contains("denied")
        || lower.contains("already in use")
        || lower.contains("already owned")
        || lower.contains("unsupported")
        || lower.contains("unrecognized")
        || lower.contains("invalid ")
        || lower.contains("no space left")
        || lower.contains("read-only file system")
        || lower.contains("must be")
        || lower.contains("panic")
        || lower.contains("existing files preserved")
}

pub(crate) fn classify_log_error(raw: &str) -> LogErrorMatch {
    let line = sanitize_log_line(raw);
    let lower = line.to_ascii_lowercase();
    let (cause, next) = if lower.contains("address already in use")
        || lower.contains("listener is occupied")
    {
        (
            Some("port already in use"),
            Some("Stop the daemon or program holding the port, or configure a different --listen port.".into()),
        )
    } else if lower.contains("storage is already owned") {
        (
            Some("data folder lock (owner.lock) held by another process"),
            Some("Another wx serve or wx store process is using this data folder. Stop it before starting a second daemon.".into()),
        )
    } else if lower.contains("database wal")
        || lower.contains("super-journal")
        || lower.contains("commits from a newer log generation")
        || lower.contains("index exceeds log")
    {
        (
            Some("SQLite WAL/SHM admission refusal"),
            Some(format!(
                "Back up the database with its -wal and -shm files and follow {TROUBLESHOOTING_DAEMON_URL}."
            )),
        )
    } else if lower.contains("unsupported registry schema version")
        || lower.contains("unsupported account schema version")
    {
        (
            Some("database schema is newer than this binary"),
            Some("Upgrade wx to the version that wrote this database.".into()),
        )
    } else if lower.contains("no space left")
        || lower.contains("storagefull")
        || lower.contains("quota")
        || lower.contains("cannot copy") && lower.contains("free beside it")
    {
        (
            Some("insufficient free disk space"),
            Some("Free disk space on the data folder's filesystem and retry.".into()),
        )
    } else if lower.contains("permission denied")
        || lower.contains("must be private")
        || lower.contains("owner-only")
    {
        (
            Some("ownership or permissions mismatch"),
            Some("Fix owner and mode (0700 for directories, 0600 for files) on the data folder and token file.".into()),
        )
    } else if lower.contains("api token") || lower.contains("tokenfile") {
        (
            Some("missing or invalid API token file"),
            Some("Check the token file path, permissions (0600) and format.".into()),
        )
    } else if lower.contains("serverurl") || lower.contains("backend") {
        (
            Some("legacy 0.x configuration"),
            Some(
                "Update your configuration for the native daemon; see the migration guide.".into(),
            ),
        )
    } else {
        (None, None)
    };
    LogErrorMatch {
        line,
        known_cause: cause,
        next_step: next,
    }
}

pub(crate) fn sanitize_log_line(raw: &str) -> String {
    let mut clean = String::new();
    for ch in raw.chars() {
        if ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}') {
            clean.push(' ');
        } else {
            clean.push(ch);
        }
        if clean.len() >= 240 {
            clean.push('…');
            break;
        }
    }
    clean.trim().to_owned()
}

fn check_daemon_log(
    log_path: &Path,
    data_dir: &Path,
    known_account_ids: &[String],
    daemon_running: bool,
    checks: &mut Vec<Check>,
    unrecognized_errors: &mut Vec<String>,
    need_bug_report: &mut bool,
) {
    if fs::symlink_metadata(log_path).is_err() {
        checks.push(Check::ok("daemon_log", "No daemon.log present yet"));
        return;
    }
    let perm = check_private_path(log_path, "daemon_log.permissions", "Daemon log", false);
    if perm.status != CheckStatus::Ok {
        checks.push(perm);
        return;
    }
    match last_log_error(log_path, 0) {
        Some(matched) => {
            if daemon_running {
                checks.push(Check::info(
                    "daemon_log",
                    format!(
                        "Log error after last start: {}{}",
                        matched.line,
                        matched
                            .known_cause
                            .map(|c| format!(" (matched: {c})"))
                            .unwrap_or_default()
                    ),
                    matched.next_step,
                ));
            } else {
                let status = if matched.known_cause.is_some() {
                    CheckStatus::Warn
                } else {
                    *need_bug_report = true;
                    unrecognized_errors.push(redact_for_bug_report(
                        &matched.line,
                        data_dir,
                        known_account_ids,
                    ));
                    CheckStatus::Warn
                };
                checks.push(Check {
                    id: "daemon_log".into(),
                    status,
                    message: format!(
                        "Last fatal error in daemon.log: {}{}",
                        matched.line,
                        matched
                            .known_cause
                            .map(|c| format!(" (cause: {c})"))
                            .unwrap_or_default()
                    ),
                    next: matched.next_step.or_else(|| {
                        Some("Inspect `wx daemon logs` or open an issue with the redacted report below.".into())
                    }),
                });
            }
        }
        None => {
            checks.push(Check::ok(
                "daemon_log",
                format!(
                    "Daemon log permissions OK; no fatal error after last ready entry ({})",
                    log_path.display()
                ),
            ));
        }
    }
}

/// Scrub paths (even quoted, inside `key=...`, or containing spaces), known account IDs,
/// WeChat IDs (`wxid_*`, `*@chatroom`), non-loopback URLs, and long opaque token-like runs
/// before placing an unrecognized log line into the redacted bug report.
fn redact_for_bug_report(text: &str, data_dir: &Path, known_account_ids: &[String]) -> String {
    let mut scrubbed = text.to_owned();
    if let Some(dir_str) = data_dir.to_str() {
        if !dir_str.is_empty() {
            scrubbed = scrubbed.replace(dir_str, "<path>");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && home != "/" {
            scrubbed = scrubbed.replace(&home, "<home>");
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            scrubbed = scrubbed.replace(&profile, "<home>");
        }
    }
    for id in known_account_ids {
        if id.len() >= 3 {
            scrubbed = scrubbed.replace(id, "<account>");
        }
    }
    // Redact Windows drive paths (including spaces up to common delimiters) and Unix paths/URLs/IDs.
    let mut out = Vec::new();
    for raw_tok in scrubbed.split_whitespace() {
        out.push(redact_token(raw_tok));
    }
    out.join(" ")
}

fn redact_token(tok: &str) -> String {
    let lower = tok.to_ascii_lowercase();
    if lower.contains("wxid_") || lower.contains("@chatroom") {
        return "<wechat-id>".into();
    }
    if (lower.contains("http://") || lower.contains("https://"))
        && !lower.contains("127.0.0.1")
        && !lower.contains("localhost")
        && !lower.contains("[::1]")
    {
        return "<url>".into();
    }
    // Strip leading key= or punctuation to detect embedded Unix/Windows paths or secrets.
    let (prefix, rest) = if let Some((k, v)) = tok.split_once('=') {
        (format!("{k}="), v)
    } else {
        (String::new(), tok)
    };
    let trimmed = rest.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    });
    if trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with(r"\\")
        || (trimmed.len() >= 3
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':'
            && matches!(trimmed.as_bytes()[2], b'\\' | b'/'))
    {
        return format!("{prefix}<path>");
    }
    // Scrub long opaque token/hex/base64-like strings (>= 24 chars with no spaces).
    let core = trimmed.trim_end_matches(['.', ':']);
    if core.len() >= 24
        && core
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'/' | b'='))
        && core.bytes().any(|b| b.is_ascii_digit())
    {
        return format!("{prefix}<redacted>");
    }
    tok.to_owned()
}

fn read_official_profiles_readonly(data_dir: &Path) -> Vec<PathBuf> {
    let registry = data_dir.join("registry.sqlite");
    if !registry.is_file() {
        return Vec::new();
    }
    let Ok(accounts) = wechat_store::read_accounts_immutable(&registry) else {
        return Vec::new();
    };
    accounts
        .into_iter()
        .filter(|a| matches!(a.storage_mode.as_str(), "official" | "direct"))
        .map(|a| canonical_or_original(&a.profile_path))
        .collect()
}

fn check_official_profiles(
    data_dir: &Path,
    accounts: &[wechat_store::Account],
    systemd_unit: Option<&SystemdUnitInfo>,
    other_uid_procs: &[RunningWxProcess],
    official_profiles_by_dir: &BTreeMap<PathBuf, Vec<PathBuf>>,
    checks: &mut Vec<Check>,
) {
    let official_accounts: Vec<&wechat_store::Account> = accounts
        .iter()
        .filter(|a| matches!(a.storage_mode.as_str(), "official" | "direct"))
        .collect();
    if official_accounts.is_empty() {
        return;
    }

    let unit_targets_this_dir = systemd_unit
        .and_then(|u| u.data_dir.as_deref())
        .is_some_and(|u| canonical_or_original(u) == data_dir);

    for account in official_accounts {
        let id = format!("official_profile.{}", account.id);
        let profile = &account.profile_path;
        let check_dir = if account.storage_mode == "official" {
            profile.join("xwechat_files")
        } else {
            profile.clone()
        };
        if !profile.is_dir() || !check_dir.is_dir() {
            checks.push(Check::fail(
                &id,
                format!(
                    "Registered {} profile path is missing ({})",
                    account.storage_mode,
                    profile.display()
                ),
                Some(
                    "Restore or mount the profile folder at that path before starting the daemon."
                        .into(),
                ),
            ));
            continue;
        }

        let canonical_profile = canonical_or_original(profile);
        let mut issues = Vec::new();
        let mut next_steps = Vec::new();
        let mut failed = false;

        // Warning only: official WeChat process appears to be running
        if doctor_discovery::official_wechat_running(Some(&canonical_profile)) {
            issues.push("official WeChat appears to be running (warning only)".to_owned());
            next_steps.push(
                "Quit the official WeChat app before running the daemon on this profile."
                    .to_owned(),
            );
        }

        // Writeability check (non-mutating `access(W_OK)` on Unix)
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::fs::MetadataExt;
            let writable = CString::new(wechat_platform::path_bytes(&check_dir))
                .ok()
                .is_some_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0);
            if !writable {
                failed = true;
                let my_uid = unsafe { libc::geteuid() };
                let dir_uid = fs::symlink_metadata(&check_dir).map(|m| m.uid()).ok();
                issues.push(format!(
                    "current user (UID {my_uid}) cannot write to {} (owned by UID {})",
                    check_dir.display(),
                    dir_uid
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "unknown".into())
                ));
                if my_uid == 10001 || Path::new("/.dockerenv").exists() {
                    next_steps.push(format!(
                        "Grant container user UID {my_uid} read/write access to {} and verify the Docker volume is not mounted read-only.",
                        profile.display()
                    ));
                } else {
                    next_steps.push(format!(
                        "Grant your daemon user read/write access to {}.",
                        check_dir.display()
                    ));
                }
            }
        }

        // Systemd sandbox `ReadWritePaths=` check
        if unit_targets_this_dir {
            if let Some(unit) = systemd_unit {
                // Only `ProtectSystem=strict` or `ProtectHome` (anything other than `no`/`false`)
                // makes user home / arbitrary paths read-only (`ProtectSystem=full|yes` only covers /usr, /boot, /etc).
                let sandboxed = unit
                    .protect_home
                    .as_deref()
                    .is_some_and(|v| v != "no" && v != "false")
                    || unit
                        .protect_system
                        .as_deref()
                        .is_some_and(|v| v == "strict");
                if sandboxed {
                    let mut required_dirs = vec![canonical_or_original(&check_dir)];
                    if account.storage_mode == "official" {
                        let dot_xwechat = profile.join(".xwechat");
                        if dot_xwechat.is_dir() {
                            required_dirs.push(canonical_or_original(&dot_xwechat));
                        }
                    }
                    let uncovered: Vec<PathBuf> = required_dirs
                        .into_iter()
                        .filter(|req| {
                            !unit.read_write_paths.iter().any(|allowed| {
                                let allowed = canonical_or_original(allowed);
                                req.starts_with(&allowed) || canonical_profile.starts_with(&allowed)
                            })
                        })
                        .collect();
                    if !uncovered.is_empty() {
                        let missing_str = uncovered
                            .iter()
                            .map(|p| p.display().to_string())
                            .collect::<Vec<_>>()
                            .join(" ");
                        failed = true;
                        issues.push(format!(
                            "systemd unit {} sets ProtectSystem=strict/ProtectHome without covering {} in ReadWritePaths=",
                            unit.path.display(),
                            missing_str
                        ));
                        next_steps.push(format!(
                            "Add `{missing_str}` to ReadWritePaths= in {}, then run `systemctl --user daemon-reload`.",
                            unit.path.display()
                        ));
                    }
                }
            }
        }

        // Cross-daemon & container/host overlap check
        let duplicate_dirs: Vec<String> = official_profiles_by_dir
            .iter()
            .filter(|(dir, profiles)| {
                dir.as_path() != data_dir && profiles.iter().any(|p| p == &canonical_profile)
            })
            .map(|(dir, _)| dir.display().to_string())
            .collect();
        if !duplicate_dirs.is_empty() {
            issues.push(format!(
                "another local daemon registers the same official profile path ({})",
                duplicate_dirs.join(", ")
            ));
            next_steps.push("Use only one daemon per official profile; never run two daemons on the same profile.".into());
        }

        if let Some(other_uid) = doctor_discovery::other_uid_holds_profile(&canonical_profile) {
            issues.push(format!(
                "a process under UID {other_uid} (such as a container or another user) has files open in this official profile; per-user profile locks in $XDG_RUNTIME_DIR do not cover different users"
            ));
            next_steps.push("Stop the container or other user's daemon before running a host daemon on this official profile.".into());
        } else if !other_uid_procs.is_empty() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let my_uid = unsafe { libc::geteuid() };
                if let Ok(meta) = fs::symlink_metadata(&check_dir) {
                    if meta.uid() != my_uid {
                        issues.push(format!(
                            "official profile is owned by UID {} while another UID's wx serve process is running; per-user profile locks in $XDG_RUNTIME_DIR do not cover host vs container daemons",
                            meta.uid()
                        ));
                        next_steps.push("Verify no container and host daemon both register this official profile.".into());
                    }
                }
            }
        }

        if failed {
            checks.push(Check::fail(
                &id,
                format!(
                    "Account {} ({} profile {}): {}",
                    account.id,
                    account.storage_mode,
                    profile.display(),
                    issues.join("; ")
                ),
                Some(next_steps.join(" ")),
            ));
        } else if !issues.is_empty() {
            checks.push(Check::warn(
                &id,
                format!(
                    "Account {} ({} profile {}): {}",
                    account.id,
                    account.storage_mode,
                    profile.display(),
                    issues.join("; ")
                ),
                Some(next_steps.join(" ")),
            ));
        } else {
            checks.push(Check::ok(
                &id,
                format!(
                    "Account {} ({} profile {}) exists and is writable",
                    account.id,
                    account.storage_mode,
                    profile.display()
                ),
            ));
        }
    }
}

fn probe_tool(binary: &str) -> Option<String> {
    let output = ProcessCommand::new(binary).arg("-version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    Some(if first_line.is_empty() {
        binary.to_owned()
    } else {
        first_line
    })
}

fn check_media_tools() -> Check {
    let ffmpeg = probe_tool("ffmpeg");
    let ffprobe = probe_tool("ffprobe");
    match (ffmpeg, ffprobe) {
        (Some(ff), Some(fp)) => Check::ok(
            "media_tools",
            format!(
                "FFmpeg and FFprobe available ({}; {})",
                short_tool_ver(&ff),
                short_tool_ver(&fp)
            ),
        ),
        (ff, fp) => {
            let mut missing = Vec::new();
            if ff.is_none() {
                missing.push("ffmpeg");
            }
            if fp.is_none() {
                missing.push("ffprobe");
            }
            Check::warn(
                "media_tools",
                format!(
                    "Missing {} in PATH; voice messages, video previews and animated stickers require FFmpeg",
                    missing.join(" and ")
                ),
                Some("Install ffmpeg (which includes ffprobe) and ensure it is on PATH.".into()),
            )
        }
    }
}

fn short_tool_ver(line: &str) -> String {
    // e.g. "ffmpeg version 6.1.1-3ubuntu5 Copyright ..." -> "ffmpeg 6.1.1-3ubuntu5"
    let parts: Vec<_> = line.split_whitespace().collect();
    if parts.len() >= 3 && parts[1] == "version" {
        format!("{} {}", parts[0], parts[2])
    } else {
        line.chars().take(48).collect()
    }
}

async fn check_running_daemon(
    data_dir: &Path,
    token_file: &Path,
    listen: SocketAddr,
    pid: Option<u32>,
    owner: Option<DaemonOwner>,
    uses_token_stdin: bool,
    checks: &mut Vec<Check>,
) {
    let client_cfg = config::ClientConfig {
        schema_version: 1,
        endpoint: format!("http://{listen}"),
        token_file: token_file.to_path_buf(),
        account: None,
    };
    let remote = match Remote::new(client_cfg) {
        Ok(r) => r,
        Err(_) => {
            let msg = if uses_token_stdin || owner == Some(DaemonOwner::Desktop) {
                format!(
                    "Daemon is running ({}{}) on http://{listen}, but its key is supplied over stdin so live account checks cannot authenticate",
                    owner.map_or("daemon", DaemonOwner::as_str),
                    pid.map_or(String::new(), |p| format!(", PID {p}"))
                )
            } else {
                format!(
                    "Daemon is running on http://{listen}, but token file {} cannot be read to query live status",
                    token_file.display()
                )
            };
            let next = if owner == Some(DaemonOwner::Desktop) {
                Some("In AgentWechat Desktop, use Settings -> Export key for CLI if you want CLI access.".into())
            } else {
                Some("Pass --token-file PATH pointing to this daemon's API token to run live checks.".into())
            };
            checks.push(Check::info("daemon.api", msg, next));
            return;
        }
    };

    let status = match tokio::time::timeout(Duration::from_secs(3), remote.get("v1/status")).await {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => {
            checks.push(Check::warn(
                "daemon.api",
                format!("Daemon on http://{listen} did not accept status request: {err}"),
                Some("Verify the token file matches this running daemon.".into()),
            ));
            return;
        }
        Err(_) => {
            checks.push(Check::warn(
                "daemon.api",
                format!("Timed out waiting for http://{listen}/v1/status"),
                Some("Check `wx daemon logs` to see if the daemon is busy or wedged.".into()),
            ));
            return;
        }
    };

    let version = status["version"]["version"].as_str().unwrap_or("unknown");
    let state = status["state"].as_str().unwrap_or("unknown");
    let api_pid = status["pid"].as_u64();
    checks.push(Check::ok(
        "daemon.api",
        format!(
            "Daemon is running and responding on http://{listen} (state: {state}, version: {version}{})",
            api_pid.map_or(String::new(), |p| format!(", PID {p}"))
        ),
    ));

    let accounts_val =
        match tokio::time::timeout(Duration::from_secs(3), remote.get("v1/accounts")).await {
            Ok(Ok(val)) => val,
            Ok(Err(err)) => {
                checks.push(Check::warn(
                    "daemon.accounts",
                    format!("Could not read accounts from running daemon: {err}"),
                    None,
                ));
                return;
            }
            Err(_) => {
                checks.push(Check::warn(
                    "daemon.accounts",
                    "Timed out reading /v1/accounts from running daemon",
                    None,
                ));
                return;
            }
        };

    let Some(items) = accounts_val["items"].as_array() else {
        return;
    };
    if items.is_empty() {
        checks.push(Check::info(
            "daemon.accounts",
            "No accounts are registered in this daemon yet",
            Some("Create an account with `wx accounts create --alias me` and sign in with `wx auth login`.".into()),
        ));
        return;
    }

    for item in items {
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        let auth = item["authenticated"].as_bool().unwrap_or(false);
        let conn = item["connection"].as_str().unwrap_or("unknown");
        let transport = item["incomingTransport"].as_str().unwrap_or("unknown");
        let recovery_pending = item["profileRecoveryPending"].as_bool().unwrap_or(false);
        let pending_batches = item["pendingProjectionBatches"].as_i64().unwrap_or(0);

        if !auth {
            checks.push(Check::warn(
                format!("account.{id}.login"),
                format!("Account {id} is not authenticated (connection: {conn})"),
                Some(format!("Run `wx --account {id} auth login` to sign in.")),
            ));
        } else if recovery_pending {
            checks.push(Check::warn(
                format!("account.{id}.login"),
                format!(
                    "Account {id} is authenticated ({conn}, {transport}) with profile recovery pending"
                ),
                Some(format!(
                    "Run `wx --account {id} store recover-projection` or inspect `wx --account {id} accounts status`."
                )),
            ));
        } else {
            checks.push(Check::ok(
                format!("account.{id}.login"),
                format!(
                    "Account {id} authenticated ({conn}, {transport}, pending sync batches: {pending_batches})"
                ),
            ));
        }

        // Query GET /v1/accounts/{id}/storage for control.sqlite size, blocked sync batches, and needs_review counts.
        let storage_path = format!("v1/accounts/{}/storage", crate::encode(id));
        match tokio::time::timeout(Duration::from_secs(3), remote.get(&storage_path)).await {
            Ok(Ok(usage)) => {
                evaluate_live_storage(id, &usage, checks);
            }
            Ok(Err(err)) => {
                let err_text = err.to_string();
                let control_path = data_dir
                    .join("accounts")
                    .join(id)
                    .join("agent/control.sqlite");
                let size_str = fs::symlink_metadata(&control_path)
                    .map(|m| format_bytes(m.len()))
                    .unwrap_or_else(|_| "unknown size".into());
                if err_text.contains("(404 ") {
                    // Graceful fallback for an older running daemon without /v1/accounts/{a}/storage.
                    checks.push(Check::info(
                        format!("account.{id}.storage"),
                        format!(
                            "Account {id} control.sqlite is {size_str} (running daemon does not expose /v1/accounts/{id}/storage)"
                        ),
                        None,
                    ));
                } else {
                    checks.push(Check::warn(
                        format!("account.{id}.storage"),
                        format!(
                            "Could not query live storage diagnostics for account {id} (control.sqlite {size_str}): {err_text}"
                        ),
                        Some("Check `wx daemon logs` for control.sqlite or token scope errors.".into()),
                    ));
                }
            }
            Err(_) => {
                checks.push(Check::warn(
                    format!("account.{id}.storage"),
                    format!("Timed out reading storage diagnostics for account {id}"),
                    None,
                ));
            }
        }
    }
}

fn evaluate_live_storage(account_id: &str, usage: &Value, checks: &mut Vec<Check>) {
    let file_bytes = usage["fileBytes"].as_i64().unwrap_or(0);
    let free_bytes = usage["freeBytes"].as_i64().unwrap_or(0);
    let unresolved = &usage["unresolved"];
    let blocked = unresolved["syncBatchesBlocked"].as_i64().unwrap_or(0);
    let sends_review = unresolved["sendsNeedReview"].as_i64().unwrap_or(0);
    let media_review = unresolved["mediaJobsNeedReview"].as_i64().unwrap_or(0);
    let group_review = unresolved["groupMutationsNeedReview"].as_i64().unwrap_or(0);
    let contact_review = unresolved["contactMutationsNeedReview"]
        .as_i64()
        .unwrap_or(0);
    let recall_review = unresolved["recallsNeedReview"].as_i64().unwrap_or(0);
    let total_review = sends_review + media_review + group_review + contact_review + recall_review;

    let mut warnings = Vec::new();
    let mut next = Vec::new();
    if file_bytes >= WARN_CONTROL_BYTES {
        warnings.push(format!(
            "control.sqlite is large ({}, {} reclaimable free pages)",
            format_bytes(file_bytes.max(0) as u64),
            format_bytes(free_bytes.max(0) as u64)
        ));
        next.push(format!(
            "Inspect table sizes with `wx --account {account_id} store usage` while stopped."
        ));
    }
    if blocked > 0 {
        warnings.push(format!("{blocked} blocked sync batch(es)"));
        next.push(format!(
            "Inspect blocked controls with `wx --account {account_id} store usage` or upgrade wx."
        ));
    }
    if total_review > 0 {
        warnings.push(format!(
            "{total_review} item(s) in needs_review (sends: {sends_review}, media jobs: {media_review}, groups: {group_review}, contacts: {contact_review}, recalls: {recall_review})"
        ));
        next.push("Check the chat on your phone before retrying any needs_review send.".into());
    }

    if !warnings.is_empty() {
        checks.push(Check::warn(
            format!("account.{account_id}.storage"),
            format!(
                "Account {account_id} control.sqlite ({}): {}",
                format_bytes(file_bytes.max(0) as u64),
                warnings.join("; ")
            ),
            Some(next.join(" ")),
        ));
    } else {
        checks.push(Check::ok(
            format!("account.{account_id}.storage"),
            format!(
                "Account {account_id} control.sqlite {}, 0 blocked sync batches, 0 needs_review items",
                format_bytes(file_bytes.max(0) as u64)
            ),
        ));
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn format_human(report: &DoctorReport) -> String {
    let mut out = String::new();
    use std::fmt::Write;

    let state_desc = if report.target.daemon_running {
        format!(
            "running ({}{})",
            report
                .target
                .daemon_owner
                .map_or("daemon", DaemonOwner::as_str),
            report
                .target
                .daemon_pid
                .map_or(String::new(), |p| format!(", PID {p}"))
        )
    } else {
        "stopped (read-only check)".into()
    };

    let _ = writeln!(
        out,
        "Target: {} (chosen from: {})",
        report.target.data_dir.display(),
        report.target.source.label()
    );
    let _ = writeln!(
        out,
        "Listen: http://{}  State: {state_desc}",
        report.target.listen
    );
    let _ = writeln!(out);

    render_checks_table(&mut out, &report.checks);

    if !report.others.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Other daemons on this computer:");
        render_table(
            &mut out,
            ["Owner", "Address", "Data folder", "State"],
            report.others.iter().map(|d| {
                [
                    d.owner.as_str().to_owned(),
                    d.address.clone(),
                    d.data_dir.display().to_string(),
                    d.state.clone(),
                ]
            }),
        );
        for other in &report.others {
            if let Some(checks) = &other.checks {
                let _ = writeln!(out);
                let _ = writeln!(
                    out,
                    "Checks for {} ({}):",
                    other.owner.as_str(),
                    other.data_dir.display()
                );
                render_checks_table(&mut out, checks);
            }
            if let Some(br) = &other.bug_report {
                render_bug_report(&mut out, br);
            }
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "{}", report.docker_hint);

    if let Some(br) = &report.bug_report {
        render_bug_report(&mut out, br);
    }

    out
}

fn render_checks_table(out: &mut String, checks: &[Check]) {
    use std::fmt::Write;
    render_table(
        out,
        ["Status", "Check", "Summary"],
        checks.iter().map(|c| {
            [
                c.status.label().to_owned(),
                c.id.clone(),
                one_line_clean(&c.message),
            ]
        }),
    );
    let next_steps: Vec<(&str, &str)> = checks
        .iter()
        .filter_map(|c| c.next.as_deref().map(|n| (c.id.as_str(), n)))
        .collect();
    if !next_steps.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Next steps:");
        for (id, next) in next_steps {
            let _ = writeln!(out, "  - [{id}] {}", one_line_clean(next));
        }
    }
}

fn render_bug_report(out: &mut String, report: &BugReport) {
    use std::fmt::Write;
    let _ = writeln!(out);
    let _ = writeln!(out, "--- Bug report (personal data removed) ---");
    if let Ok(pretty) = serde_json::to_string_pretty(report) {
        let _ = writeln!(out, "{pretty}");
    }
    let _ = writeln!(out, "--- End bug report ---");
}

fn render_table<const N: usize>(
    out: &mut String,
    headers: [&str; N],
    items: impl Iterator<Item = [String; N]>,
) {
    use std::fmt::Write;
    let mut rows = vec![headers.map(str::to_owned)];
    rows.extend(items);
    let mut widths = [0; N];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.width());
        }
    }
    for row in rows {
        let cells: [String; N] = std::array::from_fn(|index| {
            let cell = &row[index];
            let pad = widths[index].saturating_sub(cell.width());
            format!("{cell}{}", " ".repeat(pad))
        });
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
}

fn one_line_clean(text: &str) -> String {
    let mut safe = String::new();
    for c in text.chars() {
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            safe.push(' ');
        } else {
            safe.push(c);
        }
    }
    safe
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    /// Capture every path, permissions mode, and exact file bytes under `root`.
    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, (bool, Vec<u8>)> {
        fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (bool, Vec<u8>)>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    out.insert(rel, (true, Vec::new()));
                    walk(base, &path, out);
                } else {
                    let bytes = fs::read(&path).unwrap_or_default();
                    out.insert(rel, (false, bytes));
                }
            }
        }
        let mut map = BTreeMap::new();
        walk(root, root, &mut map);
        map
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            wechat_platform::private_dir(parent).unwrap();
        }
        wechat_platform::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    /// Build a minimal valid 4096-byte SQLite header with the given `user_version`.
    fn synthetic_sqlite_db(version: i32) -> Vec<u8> {
        let mut page = vec![0u8; 4096];
        page[..16].copy_from_slice(b"SQLite format 3\0");
        page[16..18].copy_from_slice(&4096u16.to_be_bytes());
        page[18] = 2; // WAL write version
        page[19] = 2; // WAL read version
        page[21] = 64;
        page[22] = 32;
        page[23] = 32;
        page[28..32].copy_from_slice(&1u32.to_be_bytes()); // db size in pages
        page[60..64].copy_from_slice(&version.to_be_bytes());
        page
    }

    fn wal_checksum(data: &[u8], big: bool, mut sum: [u32; 2]) -> [u32; 2] {
        for pair in data.chunks_exact(8) {
            let word = |slice: &[u8]| {
                if big {
                    u32::from_be_bytes(slice[..4].try_into().unwrap())
                } else {
                    u32::from_le_bytes(slice[..4].try_into().unwrap())
                }
            };
            sum[0] = sum[0].wrapping_add(word(pair)).wrapping_add(sum[1]);
            sum[1] = sum[1].wrapping_add(word(&pair[4..])).wrapping_add(sum[0]);
        }
        sum
    }

    /// Build a synthetic valid 1-frame WAL file with a committed frame.
    fn synthetic_valid_wal(salt1: u32, salt2: u32) -> Vec<u8> {
        let page_size: u32 = 4096;
        let mut wal = vec![0u8; 32 + 24 + page_size as usize];
        // Little-endian WAL magic 0x377f0682
        wal[0..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        wal[4..8].copy_from_slice(&3007000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&page_size.to_be_bytes());
        wal[12..16].copy_from_slice(&0u32.to_be_bytes());
        wal[16..20].copy_from_slice(&salt1.to_be_bytes());
        wal[20..24].copy_from_slice(&salt2.to_be_bytes());
        let hsum = wal_checksum(&wal[..24], false, [0, 0]);
        wal[24..28].copy_from_slice(&hsum[0].to_be_bytes());
        wal[28..32].copy_from_slice(&hsum[1].to_be_bytes());

        // Frame 1: page 1, db_size = 1 (commit frame)
        let f = &mut wal[32..];
        f[0..4].copy_from_slice(&1u32.to_be_bytes());
        f[4..8].copy_from_slice(&1u32.to_be_bytes());
        f[8..12].copy_from_slice(&salt1.to_be_bytes());
        f[12..16].copy_from_slice(&salt2.to_be_bytes());
        let sum = wal_checksum(&f[..8], false, hsum);
        let sum = wal_checksum(&f[24..], false, sum);
        f[16..20].copy_from_slice(&sum[0].to_be_bytes());
        f[20..24].copy_from_slice(&sum[1].to_be_bytes());
        wal
    }

    /// Build a synthetic 32 KiB `-shm` file with a valid published header for an earlier generation (#401).
    fn synthetic_shm(salt1: u32, salt2: u32, frame: u32) -> Vec<u8> {
        let mut shm = vec![0u8; 32768];
        let mut hdr = [0u8; 48];
        hdr[0..4].copy_from_slice(&3007000u32.to_ne_bytes());
        hdr[12] = 1; // isInit
        hdr[13] = 0; // little-endian WAL checksum
        hdr[14..16].copy_from_slice(&4096u16.to_ne_bytes());
        hdr[16..20].copy_from_slice(&frame.to_ne_bytes());
        hdr[20..24].copy_from_slice(&1u32.to_ne_bytes()); // nPage
        hdr[32..36].copy_from_slice(&salt1.to_be_bytes());
        hdr[36..40].copy_from_slice(&salt2.to_be_bytes());
        let sum = wal_checksum(&hdr[..40], cfg!(target_endian = "big"), [0, 0]);
        hdr[40..44].copy_from_slice(&sum[0].to_ne_bytes());
        hdr[44..48].copy_from_slice(&sum[1].to_ne_bytes());
        shm[0..48].copy_from_slice(&hdr);
        shm[48..96].copy_from_slice(&hdr);
        shm
    }

    #[tokio::test]
    async fn doctor_leaves_stopped_data_folder_byte_for_byte_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        wechat_platform::private_dir(&data_dir).unwrap();
        let store = wechat_store::Store::open(&data_dir).unwrap();
        store.create_account("alice").unwrap();
        drop(store);

        // Add synthetic daemon files, a 3-byte -shm (#665) on control.sqlite, stale process.json, and daemon.log.
        let control = data_dir.join("accounts/alice/agent/control.sqlite");
        write_private(&control.with_file_name("control.sqlite-shm"), &[0, 0, 0]);
        write_private(&control.with_file_name("control.sqlite-wal"), &[]);
        write_private(
            &data_dir.join("api-token"),
            b"0123456789abcdef0123456789abcdef\n",
        );
        write_private(
            &data_dir.join("daemon/daemon.log"),
            b"Error: cannot open control.sqlite: unrecognized database WAL index; existing files preserved\n",
        );
        write_private(
            &data_dir.join("daemon/process.json"),
            serde_json::to_vec(&json!({
                "pid": u32::MAX,
                "startTicks": "999999",
                "executable": "/nonexistent/wx",
                "dataDir": data_dir,
                "endpoint": "http://127.0.0.1:19999",
                "serverId": "srv-1",
                "ownershipGeneration": "gen-1"
            }))
            .unwrap()
            .as_slice(),
        );

        let before = snapshot_tree(&data_dir);
        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let overrides = Overrides {
            data_dir: Some(data_dir.clone()),
            listen: Some("127.0.0.1:19999".parse().unwrap()),
            ..Default::default()
        };

        let report = diagnose(
            &overrides,
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: false,
            },
        )
        .await
        .unwrap();

        let after = snapshot_tree(&data_dir);
        assert_eq!(
            before, after,
            "wx daemon doctor modified the stopped data folder!"
        );

        // Verify #665 check failed, stale process.json warned, and redacted bug report was generated without personal IDs.
        assert_eq!(report.target.source, DataDirSource::Flag);
        let ctrl_check = report
            .checks
            .iter()
            .find(|c| c.id == "sqlite.account.alice")
            .unwrap();
        assert_eq!(ctrl_check.status, CheckStatus::Fail);
        assert!(ctrl_check
            .message
            .contains("unrecognized database WAL index"));
        assert!(ctrl_check.next.as_deref().unwrap().contains("3-byte -shm"));

        let proc_check = report
            .checks
            .iter()
            .find(|c| c.id == "process_record")
            .unwrap();
        assert_eq!(proc_check.status, CheckStatus::Warn);

        let br = report.bug_report.expect("expected redacted bug report");
        let br_json = serde_json::to_string(&br).unwrap();
        assert!(
            !br_json.contains("alice"),
            "bug report leaked account ID: {br_json}"
        );
        assert!(
            br_json.contains("\"bytes\":3"),
            "bug report missing 3-byte shm size: {br_json}"
        );
        assert!(
            br_json.contains("000000"),
            "bug report missing shm header hex: {br_json}"
        );
    }

    #[tokio::test]
    async fn doctor_detects_401_stale_shm_and_newer_schema_version() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        wechat_platform::private_dir(&data_dir).unwrap();

        // Registry with schema newer than binary
        write_private(
            &data_dir.join("registry.sqlite"),
            &synthetic_sqlite_db(wechat_store::REGISTRY_SCHEMA_VERSION as i32 + 5),
        );
        // Account control.sqlite with #401 shape: stale -shm from older generation beside valid -wal
        let ctrl = data_dir.join("accounts/acct1/agent/control.sqlite");
        write_private(
            &ctrl,
            &synthetic_sqlite_db(wechat_store::ACCOUNT_SCHEMA_VERSION as i32),
        );
        write_private(
            &ctrl.with_file_name("control.sqlite-wal"),
            &synthetic_valid_wal(10, 20),
        );
        write_private(
            &ctrl.with_file_name("control.sqlite-shm"),
            &synthetic_shm(5, 99, 1),
        );

        let before = snapshot_tree(&data_dir);
        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let overrides = Overrides {
            data_dir: Some(data_dir.clone()),
            listen: Some("127.0.0.1:19998".parse().unwrap()),
            ..Default::default()
        };
        let report = diagnose(
            &overrides,
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(before, snapshot_tree(&data_dir));

        let reg = report
            .checks
            .iter()
            .find(|c| c.id == "sqlite.registry")
            .unwrap();
        assert_eq!(reg.status, CheckStatus::Fail);
        assert!(reg.message.contains("newer than this wx binary"));

        // #401 shape: older generation -shm beside valid -wal is admitted with retain_recovery = true
        let acct = report
            .checks
            .iter()
            .find(|c| c.id == "sqlite.account.acct1")
            .unwrap();
        assert_eq!(acct.status, CheckStatus::Warn);
        assert!(acct.message.contains(".sqlite-recovery-*"));
    }

    #[tokio::test]
    async fn doctor_fix_removes_only_stale_process_json_and_never_touches_databases() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        wechat_platform::private_dir(&data_dir).unwrap();
        let reg_bytes = synthetic_sqlite_db(wechat_store::REGISTRY_SCHEMA_VERSION as i32);
        write_private(&data_dir.join("registry.sqlite"), &reg_bytes);
        let shm_bytes = [0u8; 3];
        write_private(&data_dir.join("registry.sqlite-shm"), &shm_bytes);
        let proc_path = data_dir.join("daemon/process.json");
        write_private(
            &proc_path,
            serde_json::to_vec(&json!({
                "pid": u32::MAX,
                "startTicks": "1",
                "executable": "/nonexistent/wx",
                "dataDir": data_dir,
                "endpoint": "http://127.0.0.1:19997",
                "serverId": "srv-2",
                "ownershipGeneration": "gen-2"
            }))
            .unwrap()
            .as_slice(),
        );

        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let overrides = Overrides {
            data_dir: Some(data_dir.clone()),
            listen: Some("127.0.0.1:19997".parse().unwrap()),
            ..Default::default()
        };
        let report = diagnose(
            &overrides,
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: true,
            },
        )
        .await
        .unwrap();

        assert!(
            !proc_path.exists(),
            "--fix should remove stale process.json"
        );
        assert_eq!(
            fs::read(data_dir.join("registry.sqlite")).unwrap(),
            reg_bytes,
            "--fix must never modify database files"
        );
        assert_eq!(
            fs::read(data_dir.join("registry.sqlite-shm")).unwrap(),
            shm_bytes,
            "--fix must never remove or modify -shm files"
        );
        let proc_check = report
            .checks
            .iter()
            .find(|c| c.id == "process_record")
            .unwrap();
        assert_eq!(proc_check.status, CheckStatus::Ok);
        assert!(proc_check.message.contains("Removed stale"));
    }

    #[test]
    fn mountinfo_parsing_detects_wsl_drvfs_and_network_mounts() {
        let mountinfo = "\
22 1 0:21 / / rw,relatime - ext4 /dev/sda rw
35 22 0:30 / /mnt/c rw,noatime - drvfs C:\\ rw
40 22 0:35 / /mnt/nas rw - nfs4 10.0.0.2:/export rw
";
        assert_eq!(
            parse_linux_mountinfo(mountinfo, Path::new("/home/user/.local/share/agent-wechat"))
                .as_deref(),
            Some("ext4")
        );
        assert_eq!(
            parse_linux_mountinfo(mountinfo, Path::new("/mnt/c/Users/test/data")).as_deref(),
            Some("drvfs")
        );
        assert!(is_unsafe_sqlite_fs("drvfs").is_some());
        assert!(is_unsafe_sqlite_fs("nfs4").is_some());
        assert!(is_unsafe_sqlite_fs("ext4").is_none());
    }

    #[test]
    fn log_error_classification_matches_known_causes_and_ignores_older_errors_before_ready() {
        let recovered = "\
Error: Address already in use (os error 98)
Daemon ready on 127.0.0.1:6174 (server ID srv-1).
";
        assert!(parse_last_log_error(recovered).is_none());

        let failed = "\
Daemon ready on 127.0.0.1:6174 (server ID srv-1).
Error: storage is already owned by another operation
";
        let matched = parse_last_log_error(failed).unwrap();
        assert_eq!(
            matched.known_cause,
            Some("data folder lock (owner.lock) held by another process")
        );

        let wal = classify_log_error(
            "cannot open /home/alice/control.sqlite: database WAL index has commits from a newer log generation",
        );
        assert_eq!(wal.known_cause, Some("SQLite WAL/SHM admission refusal"));

        let sha = Sha256::digest(wal.line.as_bytes());
        assert_eq!(sha.len(), 32);
    }

    #[tokio::test]
    async fn doctor_detects_permissions_occupied_port_and_systemd_official_profile_sandbox() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let data_dir = home.join(".local/share/agent-wechat");
        let official_home = home.join("official-home");
        fs::create_dir_all(official_home.join("xwechat_files")).unwrap();
        fs::create_dir_all(official_home.join(".xwechat")).unwrap();

        let store = wechat_store::Store::open(&data_dir).unwrap();
        let _ = store.attach_official_home("main", &official_home).unwrap();
        drop(store);

        // Also create a second daemon folder registering the exact same official profile path
        let desktop_dir = home.join(".local/share/io.github.thisnick.agent-wechat/hub");
        let desktop_store = wechat_store::Store::open(&desktop_dir).unwrap();
        let _ = desktop_store
            .attach_official_home("dup", &official_home)
            .unwrap();
        drop(desktop_store);

        // Overly broad token file permissions
        let token_path = data_dir.join("api-token");
        write_private(&token_path, b"0123456789abcdef0123456789abcdef\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        // Systemd user unit with ProtectHome=read-only that omits official_home from ReadWritePaths=
        let unit_dir = home.join(".config/systemd/user");
        fs::create_dir_all(&unit_dir).unwrap();
        fs::write(
            unit_dir.join("agent-wechat-native.service"),
            "[Service]\nExecStart=%h/.local/bin/wx serve\nProtectSystem=strict\nProtectHome=read-only\nReadWritePaths=%h/.local/share/agent-wechat\n",
        )
        .unwrap();

        // Occupy a local loopback port to trigger the occupied-port check
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let occupied_addr = occupied.local_addr().unwrap();

        let env = Environment {
            home: Some(home.to_path_buf()),
            ..Default::default()
        };
        let overrides = Overrides {
            listen: Some(occupied_addr),
            ..Default::default()
        };

        let report = diagnose(
            &overrides,
            &env,
            home,
            &DoctorOptions {
                all: false,
                fix: false,
            },
        )
        .await
        .unwrap();
        drop(occupied);

        assert_eq!(report.target.source, DataDirSource::Default);
        let port_check = report.checks.iter().find(|c| c.id == "port").unwrap();
        assert_eq!(port_check.status, CheckStatus::Fail);

        #[cfg(unix)]
        {
            let tok_check = report.checks.iter().find(|c| c.id == "token_file").unwrap();
            assert_eq!(tok_check.status, CheckStatus::Fail);
            assert!(tok_check.message.contains("0644"));
        }

        let prof_check = report
            .checks
            .iter()
            .find(|c| c.id == "official_profile.main")
            .unwrap();
        assert_eq!(prof_check.status, CheckStatus::Fail);
        assert!(prof_check.message.contains("ReadWritePaths="));
        assert!(prof_check.message.contains("same official profile path"));

        // Desktop hub should show up in `others`
        assert!(
            report
                .others
                .iter()
                .any(|o| o.owner == DaemonOwner::Desktop),
            "expected desktop hub in others: {:?}",
            report.others
        );
    }

    #[tokio::test]
    async fn doctor_queries_running_daemon_for_login_storage_blocked_and_needs_review() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("live-data");
        let store = std::sync::Arc::new(wechat_store::Store::open(&data_dir).unwrap());
        let core = wechat_core::Core::new(store.clone(), None).unwrap();
        store.create_account("acct").unwrap();
        // Seed a blocked sync batch and a needs_review send intent
        store
            .stage_sync_if_new("acct", "batch-1", b"sealed", b"cursor-1")
            .unwrap();
        store.block_sync("acct", "batch-1").unwrap();
        let (intent, _) = store
            .create_send(
                "acct",
                "idem-1",
                "filehelper",
                "hash-1",
                b"payload",
                "cid-1",
            )
            .unwrap();
        store
            .update_send("acct", &intent.id, "needs_review", None, Some("uncertain"))
            .unwrap();

        let token = "0123456789abcdef0123456789abcdef";
        write_private(&data_dir.join("api-token"), format!("{token}\n").as_bytes());
        let router = wechat_api::router(core.clone(), token);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        // Write a live process.json matching this test process so doctor sees it as running
        let pid = std::process::id();
        let (ticks, exe) = wechat_platform::process_identity(pid).unwrap();
        write_private(
            &data_dir.join("daemon/process.json"),
            serde_json::to_vec(&json!({
                "pid": pid,
                "startTicks": ticks,
                "executable": exe,
                "dataDir": fs::canonicalize(&data_dir).unwrap(),
                "endpoint": format!("http://{addr}"),
                "serverId": store.server_id,
                "ownershipGeneration": core.ownership_generation
            }))
            .unwrap()
            .as_slice(),
        );

        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let overrides = Overrides {
            data_dir: Some(data_dir.clone()),
            listen: Some(addr),
            ..Default::default()
        };
        let report = diagnose(
            &overrides,
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: false,
            },
        )
        .await
        .unwrap();
        server_task.abort();

        assert!(report.target.daemon_running);
        let api_check = report.checks.iter().find(|c| c.id == "daemon.api").unwrap();
        assert_eq!(api_check.status, CheckStatus::Ok);
        let login_check = report
            .checks
            .iter()
            .find(|c| c.id == "account.acct.login")
            .unwrap();
        assert_eq!(login_check.status, CheckStatus::Warn);
        assert!(login_check.message.contains("not authenticated"));
        let storage_check = report
            .checks
            .iter()
            .find(|c| c.id == "account.acct.storage")
            .unwrap();
        assert_eq!(storage_check.status, CheckStatus::Warn);
        assert!(storage_check.message.contains("1 blocked sync batch"));
        assert!(storage_check.message.contains("1 item(s) in needs_review"));
    }

    #[test]
    fn redact_for_bug_report_scrubs_quoted_paths_ids_urls_and_tokens() {
        let dir = Path::new("/home/alice/my data dir");
        let raw = concat!(
            "Error opening \"/home/alice/my data dir/db\" path=/var/private/secret ",
            "for acct_personal wxid_secret123 998877@chatroom ",
            "https://private.example.com/hook token=abcdef0123456789abcdef0123456789"
        );
        let scrubbed = redact_for_bug_report(raw, dir, &["acct_personal".into()]);
        for forbidden in [
            "alice",
            "/var/private",
            "acct_personal",
            "wxid_secret123",
            "998877@chatroom",
            "private.example.com",
            "abcdef0123456789abcdef0123456789",
        ] {
            assert!(
                !scrubbed.contains(forbidden),
                "redacted output leaked {forbidden}: {scrubbed}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn doctor_reports_fail_for_valid_and_broken_symlink_data_dir() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let real_dir = temp.path().join("real-data");
        wechat_platform::private_dir(&real_dir).unwrap();
        let valid_link = temp.path().join("valid-link");
        symlink(&real_dir, &valid_link).unwrap();
        let broken_link = temp.path().join("broken-link");
        symlink(temp.path().join("missing-target"), &broken_link).unwrap();

        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        for link in [&valid_link, &broken_link] {
            let report = diagnose(
                &Overrides {
                    data_dir: Some(link.clone()),
                    listen: Some("127.0.0.1:19995".parse().unwrap()),
                    ..Default::default()
                },
                &env,
                temp.path(),
                &DoctorOptions {
                    all: false,
                    fix: false,
                },
            )
            .await
            .unwrap();
            let check = report.checks.iter().find(|c| c.id == "data_dir").unwrap();
            assert_eq!(check.status, CheckStatus::Fail, "link={}", link.display());
            assert!(check.message.contains("symlink"), "{}", check.message);
        }
    }

    #[tokio::test]
    async fn doctor_fix_preserves_process_json_when_live_pid_has_different_executable() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("upgraded-daemon");
        wechat_platform::private_dir(&data_dir).unwrap();
        let pid = std::process::id();
        let (ticks, _) = wechat_platform::process_identity(pid).unwrap();
        let record_path = data_dir.join("daemon/process.json");
        write_private(
            &record_path,
            serde_json::to_vec(&json!({
                "pid": pid,
                "startTicks": ticks,
                "executable": "/nonexistent/previous-wx (deleted)",
                "dataDir": fs::canonicalize(&data_dir).unwrap(),
                "endpoint": "http://127.0.0.1:19994",
                "serverId": "srv",
                "ownershipGeneration": "gen"
            }))
            .unwrap()
            .as_slice(),
        );

        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let report = diagnose(
            &Overrides {
                data_dir: Some(data_dir.clone()),
                listen: Some("127.0.0.1:19994".parse().unwrap()),
                ..Default::default()
            },
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: true,
            },
        )
        .await
        .unwrap();

        assert!(
            record_path.exists(),
            "--fix must not delete process.json when the recorded PID is still alive after an upgrade"
        );
        assert!(report.target.daemon_running);
        let proc_check = report
            .checks
            .iter()
            .find(|c| c.id == "process_record")
            .unwrap();
        assert_eq!(proc_check.status, CheckStatus::Warn);
        assert!(
            proc_check.message.contains("still running"),
            "{}",
            proc_check.message
        );
    }

    #[tokio::test]
    async fn doctor_bug_report_redacts_folder_only_account_ids_when_registry_is_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("corrupt-reg");
        wechat_platform::private_dir(&data_dir).unwrap();
        // Write an invalid registry.sqlite so immutable read cannot yield any account IDs.
        write_private(&data_dir.join("registry.sqlite"), b"not a sqlite file");
        // Create a folder-only account ID (e.g. only in WAL or mid-creation).
        wechat_platform::private_dir(&data_dir.join("accounts/team-secret-42")).unwrap();
        write_private(
            &data_dir.join("daemon/daemon.log"),
            b"Error: unexpected subsystem failure while touching team-secret-42\n",
        );

        let env = Environment {
            home: Some(temp.path().to_path_buf()),
            ..Default::default()
        };
        let report = diagnose(
            &Overrides {
                data_dir: Some(data_dir),
                listen: Some("127.0.0.1:19993".parse().unwrap()),
                ..Default::default()
            },
            &env,
            temp.path(),
            &DoctorOptions {
                all: false,
                fix: false,
            },
        )
        .await
        .unwrap();

        let bug = report.bug_report.expect("bug report should be present");
        assert!(
            !bug.unrecognized_errors.is_empty(),
            "expected unrecognized error in bug report"
        );
        let serialized = serde_json::to_string(&bug).unwrap();
        assert!(
            !serialized.contains("team-secret-42"),
            "bug report leaked folder-only account ID: {serialized}"
        );
    }

    #[test]
    fn check_filesystem_does_not_fail_running_daemon_on_copy_estimate() {
        let dir = Path::new("/tmp/data");
        let fs_report = FilesystemReport {
            fs_type: Some("ext4".into()),
            free_bytes: Some(500 * 1024 * 1024),
            total_bytes: Some(2 * 1024 * 1024 * 1024),
            free_inodes: Some(50_000),
            total_inodes: Some(100_000),
        };
        let large_copy = 1024 * 1024 * 1024; // 1 GiB > 500 MiB free

        let mut running_checks = Vec::new();
        check_filesystem(dir, &fs_report, 0, true, &mut running_checks);
        let running_space = running_checks
            .iter()
            .find(|c| c.id == "disk.space")
            .unwrap();
        assert_eq!(running_space.status, CheckStatus::Ok);

        let low_report = FilesystemReport {
            free_bytes: Some(10 * 1024 * 1024),
            ..fs_report.clone()
        };
        let mut running_low_checks = Vec::new();
        check_filesystem(dir, &low_report, 0, true, &mut running_low_checks);
        let running_low_space = running_low_checks
            .iter()
            .find(|c| c.id == "disk.space")
            .unwrap();
        assert_eq!(running_low_space.status, CheckStatus::Warn);

        let mut stopped_checks = Vec::new();
        check_filesystem(dir, &fs_report, large_copy, false, &mut stopped_checks);
        let stopped_space = stopped_checks
            .iter()
            .find(|c| c.id == "disk.space")
            .unwrap();
        assert_eq!(stopped_space.status, CheckStatus::Fail);
    }
}
