use super::*;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    process::{Command as ProcessCommand, Stdio},
    time::Instant,
};

#[derive(Subcommand)]
pub enum DaemonCommand {
    /// Start the local daemon in the background and wait until ready.
    Start,
    /// Show the local managed daemon's state and identity.
    Status,
    /// Stop the owned local daemon without logging out of WeChat.
    Stop {
        /// Seconds to wait for the daemon to exit (1 to 300); it is never force-killed.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Restart the local daemon managed by `wx daemon start` or the installed user service.
    Restart {
        /// Seconds to wait for a managed daemon to exit before starting again (1 to 300).
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Install and enable a Linux systemd user service for the local daemon.
    Install {
        /// Install and enable the user service without prompting for confirmation.
        #[arg(long)]
        yes: bool,
        /// Print the rendered systemd unit without writing or enabling it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Disable and remove the installed Linux systemd user service without deleting data.
    Uninstall,
    /// Read the local daemon log, optionally following new lines.
    Logs {
        /// Keep printing new log lines until interrupted.
        #[arg(long)]
        follow: bool,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnedProcess {
    pid: u32,
    start_ticks: String,
    executable: PathBuf,
    data_dir: PathBuf,
    endpoint: String,
    server_id: String,
    ownership_generation: String,
}

fn fingerprint(pid: u32) -> Option<String> {
    wechat_platform::process_identity(pid).map(|(created, _)| created)
}

fn executable_matches(actual: &Path, expected: &Path) -> bool {
    if actual == expected {
        return true;
    }
    // On Linux, replacing or unlinking an upgraded binary appends " (deleted)"
    // to /proc/<pid>/exe while the previous daemon process is still running.
    // During an npm upgrade, Arborist also renames the package folder to a
    // sibling temporary name (`.cli-<hash>`) before deleting it, so
    // `/proc/<pid>/exe` resolves through that sibling path with ` (deleted)`.
    // Full ownership still requires matching `start_ticks` plus authenticated
    // `/v1/status` verification (`pid`, `serverId`, `ownershipGeneration`, `dataDir`).
    #[cfg(target_os = "linux")]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        if let Some(stripped_bytes) = actual.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
            let stripped = Path::new(OsStr::from_bytes(stripped_bytes));
            if stripped == expected {
                return true;
            }
            if stripped.file_name() == expected.file_name() {
                let actual_tail: Vec<_> = stripped.components().rev().take(4).collect();
                let expected_tail: Vec<_> = expected.components().rev().take(4).collect();
                if actual_tail.len() == 4
                    && actual_tail == expected_tail
                    && matches!(expected_tail[3].as_os_str().to_str(), Some("native"))
                {
                    let actual_grandparent = stripped.ancestors().nth(5);
                    let expected_grandparent = expected.ancestors().nth(5);
                    if actual_grandparent.is_some() && actual_grandparent == expected_grandparent {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn process_matches(owned: &OwnedProcess) -> bool {
    wechat_platform::process_identity(owned.pid).is_some_and(|(created, executable)| {
        created == owned.start_ticks && executable_matches(&executable, &owned.executable)
    })
}

fn private_directory(path: &Path) -> Result<()> {
    Ok(wechat_platform::private_dir(path)?)
}

fn load(path: &Path) -> Result<Option<OwnedProcess>> {
    match private_input(path, 16384) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if !path.exists() => {
            let _ = error;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn live_managed_pid(record_path: &Path) -> Result<Option<u32>> {
    let Some(owned) = load(record_path)? else {
        return Ok(None);
    };
    if process_matches(&owned) {
        Ok(Some(owned.pid))
    } else {
        Ok(None)
    }
}

pub(crate) fn remote_for(config: &crate::config::ServerConfig) -> Result<Remote> {
    Remote::new(crate::config::ClientConfig {
        schema_version: 1,
        endpoint: format!("http://{}", config.listen),
        token_file: config.token_file.clone(),
        account: None,
    })
}

async fn identity(config: &crate::config::ServerConfig, owned: &OwnedProcess) -> Result<Value> {
    if !process_matches(owned)
        || owned.data_dir != std::fs::canonicalize(&config.data_dir)?
        || owned.endpoint != format!("http://{}", config.listen)
    {
        bail!("managed daemon ownership mismatch; no process was signalled");
    }
    let value = tokio::time::timeout(Duration::from_secs(3), remote_for(config)?.get("v1/status"))
        .await
        .context("managed daemon API is unavailable")??;
    if value["pid"].as_u64() != Some(owned.pid as u64)
        || value["serverId"] != owned.server_id
        || value["ownershipGeneration"] != owned.ownership_generation
        || value["dataDir"].as_str() != owned.data_dir.to_str()
        || value["version"]["backend"] != "native"
    {
        bail!("managed daemon API identity mismatch; no process was signalled");
    }
    Ok(value)
}

fn acquire_management_lock(directory: &Path) -> Result<std::fs::File> {
    let lock = wechat_platform::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(directory.join("management.lock"))?;
    if lock.try_lock_exclusive().is_err() {
        bail!("daemon lifecycle operation already in progress");
    }
    Ok(lock)
}

async fn stop_owned_locked(
    config: &crate::config::ServerConfig,
    record: &Path,
    timeout: u64,
) -> Result<Option<u32>> {
    let Some(owned) = load(record)? else {
        return Ok(None);
    };
    if fingerprint(owned.pid).is_none() {
        std::fs::remove_file(record)?;
        return Ok(None);
    }
    identity(config, &owned).await?;
    #[cfg(windows)]
    remote_for(config)?
        .post(
            "v1/daemon/shutdown",
            json!({
                "pid": owned.pid,
                "serverId": owned.server_id,
                "ownershipGeneration": owned.ownership_generation
            }),
        )
        .await?;
    #[cfg(unix)]
    if unsafe { libc::kill(owned.pid as i32, libc::SIGTERM) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot signal owned daemon");
    }
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while fingerprint(owned.pid).as_deref() == Some(&owned.start_ticks) {
        if Instant::now() >= deadline {
            bail!("owned daemon stop deadline reached; process was not force-killed");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::fs::remove_file(record)?;
    Ok(Some(owned.pid))
}

async fn start_locked(
    config: &crate::config::ServerConfig,
    record: &Path,
    log: &Path,
) -> Result<Value> {
    if let Some(owned) = load(record)? {
        if fingerprint(owned.pid).is_some() {
            return identity(config, &owned).await;
        }
        std::fs::remove_file(record)?;
    }
    let listener = std::net::TcpListener::bind(config.listen)
        .context("daemon listener is occupied; no owner was adopted or stopped")?;
    drop(listener);
    let _token = wechat_api::provision_token(&config.token_file, !config.token_explicit)?;
    let output_file = wechat_platform::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(wechat_platform::NOFOLLOW)
        .open(log)?;
    let metadata = output_file.metadata()?;
    if !metadata.is_file() || !wechat_platform::private_file(&output_file)? {
        bail!("daemon log must be a private owned regular file");
    }
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let mut command = ProcessCommand::new(&executable);
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(&config.data_dir)
        .arg("--listen")
        .arg(config.listen.to_string())
        .arg("--token-file")
        .arg(&config.token_file)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output_file.try_clone()?))
        .stderr(Stdio::from(output_file));
    if config.config_present {
        command.arg("--server-config").arg(&config.config_file);
    }
    if let Some(address) = config.network_listen {
        command
            .arg("--network-listen")
            .arg(address.to_string())
            .arg("--tls-cert-file")
            .arg(config.tls_cert_file.as_ref().unwrap())
            .arg("--tls-key-file")
            .arg(config.tls_key_file.as_ref().unwrap());
    }
    wechat_platform::detach(&mut command);
    let mut child = command.spawn().context("cannot launch native daemon")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let readiness = async {
        loop {
            if let Some(status) = child.try_wait()? {
                bail!("native daemon exited before readiness ({status}); inspect daemon logs");
            }
            if Instant::now() >= deadline {
                bail!("native daemon readiness deadline reached");
            }
            if let Ok(Ok(value)) = tokio::time::timeout(
                Duration::from_millis(500),
                remote_for(config)?.get("v1/status"),
            )
            .await
            {
                if value["pid"].as_u64() != Some(child.id() as u64)
                    || value["dataDir"].as_str() != config.data_dir.to_str()
                    || value["version"]["backend"] != "native"
                {
                    bail!("listener belongs to another process; no owner was adopted");
                }
                let owned = OwnedProcess {
                    pid: child.id(),
                    start_ticks: fingerprint(child.id())
                        .context("native daemon process identity disappeared")?,
                    executable: executable.clone(),
                    data_dir: std::fs::canonicalize(&config.data_dir)?,
                    endpoint: format!("http://{}", config.listen),
                    server_id: value["serverId"]
                        .as_str()
                        .context("missing daemon identity")?
                        .into(),
                    ownership_generation: value["ownershipGeneration"]
                        .as_str()
                        .context("missing daemon ownership generation")?
                        .into(),
                };
                private_output(record, &serde_json::to_vec(&owned)?)?;
                return Ok(value);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    .await;
    match readiness {
        Ok(value) => Ok(value),
        Err(error) => {
            if child.try_wait()?.is_some() {
                return Err(error);
            }
            #[cfg(unix)]
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            #[cfg(windows)]
            {
                let _ = child.kill();
            }
            for _ in 0..50 {
                if child.try_wait()?.is_some() {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            child.kill()?;
            let _ = child.wait();
            Err(error)
        }
    }
}

/// Start the local daemon in the background (or return the already-running
/// owned daemon's status) without printing to stdout.
pub async fn start_background(config: &crate::config::ServerConfig) -> Result<Value> {
    let mut config = config.clone();
    if config.data_dir.exists() {
        config.data_dir = std::fs::canonicalize(&config.data_dir)?;
    }
    if config.listen.port() == 0 {
        bail!("daemon start requires an explicit nonzero listen port");
    }
    private_directory(&config.data_dir)?;
    config.data_dir = std::fs::canonicalize(&config.data_dir)?;
    let directory = config.data_dir.join("daemon");
    let record = directory.join("process.json");
    let log = directory.join("daemon.log");
    private_directory(&directory)?;
    let _lock = acquire_management_lock(&directory)?;
    start_locked(&config, &record, &log).await
}

#[cfg(target_os = "linux")]
fn has_matching_systemd_service(
    config: &crate::config::ServerConfig,
    env: &crate::config::Environment,
) -> bool {
    crate::service::unit_path(env).is_ok_and(|path| {
        crate::service::installed_unit_matches_data_dir(&path, &config.data_dir, env)
    })
}

pub async fn run(
    config: crate::config::ServerConfig,
    overrides: &crate::config::Overrides,
    env: &crate::config::Environment,
    command: DaemonCommand,
    output: Output,
) -> Result<()> {
    let mut config = config;
    if config.data_dir.exists() {
        config.data_dir = std::fs::canonicalize(&config.data_dir)?;
    }
    let directory = config.data_dir.join("daemon");
    let record = directory.join("process.json");
    let log = directory.join("daemon.log");
    match command {
        DaemonCommand::Start => output.print(&start_background(&config).await?),
        DaemonCommand::Status => match load(&record)? {
            Some(owned) if fingerprint(owned.pid).is_some() => {
                output.print(&identity(&config, &owned).await?)
            }
            _ => {
                #[cfg(target_os = "linux")]
                if has_matching_systemd_service(&config, env) {
                    if crate::service::is_active() {
                        let probe_config = crate::service::installed_unit_server_config(env)
                            .unwrap_or_else(|| config.clone());
                        if let Some(mut value) =
                            crate::service::probe_service_status(&probe_config).await
                        {
                            if let Some(obj) = value.as_object_mut() {
                                obj.insert("manager".into(), json!("systemd"));
                                obj.insert("unitName".into(), json!(crate::service::UNIT_NAME));
                            }
                            return output.print(&value);
                        }
                        return output.print(&json!({
                            "state": "running",
                            "managed": true,
                            "manager": "systemd",
                            "unitName": crate::service::UNIT_NAME,
                            "dataDir": config.data_dir,
                        }));
                    }
                    return output.print(&json!({
                        "state": "stopped",
                        "managed": true,
                        "manager": "systemd",
                        "unitName": crate::service::UNIT_NAME,
                        "dataDir": config.data_dir,
                    }));
                }
                output.print(&json!({"state":"stopped","managed":true,"dataDir":config.data_dir}))
            }
        },
        DaemonCommand::Stop { timeout } => {
            if directory.exists() {
                private_directory(&directory)?;
            }
            let _lock = if directory.exists() {
                Some(acquire_management_lock(&directory)?)
            } else {
                None
            };
            if !(1..=300).contains(&timeout) {
                bail!("daemon stop timeout must be 1..300 seconds");
            }
            if let Some(pid) = stop_owned_locked(&config, &record, timeout).await? {
                return output.print(&json!({"state":"stopped","pid":pid,"logout":false}));
            }
            #[cfg(target_os = "linux")]
            if has_matching_systemd_service(&config, env) && crate::service::is_active() {
                bail!(
                    "daemon is managed by systemd service {}; stop it with `systemctl --user stop {}` or remove it with `wx daemon uninstall`",
                    crate::service::UNIT_NAME,
                    crate::service::UNIT_NAME
                );
            }
            output.print(&json!({"state":"stopped","managed":true}))
        }
        DaemonCommand::Restart { timeout } => {
            if !(1..=300).contains(&timeout) {
                bail!("daemon restart timeout must be 1..300 seconds");
            }
            #[cfg(target_os = "linux")]
            if has_matching_systemd_service(&config, env) {
                return crate::service::restart_service(&config, env, output).await;
            }
            if config.listen.port() == 0 {
                bail!("daemon restart requires an explicit nonzero listen port");
            }
            let has_owned = load(&record)?.is_some_and(|owned| process_matches(&owned));
            if !has_owned {
                crate::service::check_owner_lock_free(&config.data_dir)?;
                crate::service::check_ports_free(&config)?;
            }
            private_directory(&config.data_dir)?;
            config.data_dir = std::fs::canonicalize(&config.data_dir)?;
            let directory = config.data_dir.join("daemon");
            let record = directory.join("process.json");
            let log = directory.join("daemon.log");
            private_directory(&directory)?;
            let _lock = acquire_management_lock(&directory)?;
            let stopped = stop_owned_locked(&config, &record, timeout).await?;
            if stopped.is_none() {
                crate::service::check_owner_lock_free(&config.data_dir)?;
                crate::service::check_ports_free(&config)?;
            }
            let value = start_locked(&config, &record, &log).await?;
            output.print(&value)
        }
        DaemonCommand::Install { yes, dry_run } => {
            crate::service::install(config, overrides, env, yes, dry_run, output).await
        }
        DaemonCommand::Uninstall => crate::service::uninstall(&config, env, output),
        DaemonCommand::Logs { follow } => {
            let mut file = wechat_platform::OpenOptions::new()
                .read(true)
                .custom_flags(wechat_platform::NOFOLLOW)
                .open(&log)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || !wechat_platform::private_file(&file)? {
                bail!("daemon log must be a private owned regular file");
            }
            let length = metadata.len();
            if length > 4 * 1024 * 1024 {
                file.seek(SeekFrom::Start(length - 4 * 1024 * 1024))?;
            }
            let mut buffer = [0u8; 16384];
            loop {
                let n = file.read(&mut buffer)?;
                if n > 0 {
                    std::io::stdout().write_all(&buffer[..n])?;
                    std::io::stdout().flush()?;
                } else if !follow {
                    break;
                } else {
                    tokio::select! {_=tokio::signal::ctrl_c()=>break,_=tokio::time::sleep(Duration::from_millis(250))=>{}}
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_fingerprint_is_required_and_foreign_executable_is_rejected() {
        let pid = std::process::id();
        let ticks = fingerprint(pid).unwrap();
        let mut owned = OwnedProcess {
            pid,
            start_ticks: ticks,
            executable: std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
            data_dir: "/unused".into(),
            endpoint: "http://127.0.0.1:1".into(),
            server_id: "s".into(),
            ownership_generation: "g".into(),
        };
        assert!(process_matches(&owned));
        owned.start_ticks = "0".into();
        assert!(!process_matches(&owned));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn upgraded_linux_deleted_and_npm_renamed_executable_matches_recorded_path() {
        let expected =
            Path::new("/opt/node_modules/@agent-wechat/cli/native/3.2.0/linux-x64-gnu/wx");
        assert!(executable_matches(expected, expected));
        assert!(executable_matches(
            Path::new(
                "/opt/node_modules/@agent-wechat/cli/native/3.2.0/linux-x64-gnu/wx (deleted)"
            ),
            expected
        ));
        // npm Arborist renames the package folder to `.cli-<hash>` before deleting it.
        assert!(executable_matches(
            Path::new(
                "/opt/node_modules/@agent-wechat/.cli-AbCd1234/native/3.2.0/linux-x64-gnu/wx (deleted)"
            ),
            expected
        ));
        // Unrelated directory or different binary name is still rejected.
        assert!(!executable_matches(
            Path::new("/tmp/other/native/3.2.0/linux-x64-gnu/wx (deleted)"),
            expected
        ));
        assert!(!executable_matches(
            Path::new("/opt/other/wx (deleted)"),
            expected
        ));
        assert!(!executable_matches(
            Path::new("/opt/node_modules/@agent-wechat/cli/native/3.2.0/linux-x64-gnu/wx2"),
            expected
        ));
    }
}
