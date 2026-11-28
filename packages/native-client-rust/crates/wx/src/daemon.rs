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
    /// Check a local daemon's data folder, databases and runtime health without modifying databases.
    Doctor {
        /// Run the full check suite on every discovered local daemon data folder.
        #[arg(long)]
        all: bool,
        /// Apply only clearly safe fixes, such as removing a stale process.json for a dead process.
        #[arg(long)]
        fix: bool,
    },
    /// Stop the owned local daemon without logging out of WeChat.
    Stop {
        /// Seconds to wait for the daemon to exit (1 to 300); it is never force-killed.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
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
fn process_matches(owned: &OwnedProcess) -> bool {
    wechat_platform::process_identity(owned.pid).is_some_and(|(created, executable)| {
        created == owned.start_ticks && executable == owned.executable
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
fn remote(config: &crate::config::ServerConfig) -> Result<Remote> {
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
    let value = tokio::time::timeout(Duration::from_secs(3), remote(config)?.get("v1/status"))
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
#[derive(Debug)]
pub(crate) struct HintedError {
    pub(crate) source: anyhow::Error,
    pub(crate) last_log_error: Option<String>,
}

impl std::fmt::Display for HintedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.source, f)
    }
}

impl std::error::Error for HintedError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

fn with_log_hint(error: anyhow::Error, log: &Path, after_offset: Option<u64>) -> anyhow::Error {
    let last_log_error = after_offset
        .and_then(|offset| crate::doctor::last_log_error(log, offset))
        .map(|matched| matched.line);
    anyhow::Error::new(HintedError {
        source: error,
        last_log_error,
    })
}

pub async fn run(
    config: crate::config::ServerConfig,
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
        DaemonCommand::Doctor { .. } => {
            unreachable!("wx daemon doctor is handled before config::resolve in main")
        }
        DaemonCommand::Start => {
            if config.listen.port() == 0 {
                bail!("daemon start requires an explicit nonzero listen port");
            }
            private_directory(&config.data_dir)?;
            config.data_dir = std::fs::canonicalize(&config.data_dir)?;
            private_directory(&directory)?;
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
            if let Some(owned) = load(&record)? {
                if fingerprint(owned.pid).is_some() {
                    return output.print(
                        &identity(&config, &owned)
                            .await
                            .map_err(|e| with_log_hint(e, &log, None))?,
                    );
                }
                std::fs::remove_file(&record)?;
            }
            let listener = std::net::TcpListener::bind(config.listen).map_err(|e| {
                with_log_hint(
                    anyhow::Error::new(e)
                        .context("daemon listener is occupied; no owner was adopted or stopped"),
                    &log,
                    None,
                )
            })?;
            drop(listener);
            let _token = wechat_api::provision_token(&config.token_file, !config.token_explicit)
                .map_err(|e| with_log_hint(e, &log, None))?;
            let output_file = wechat_platform::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(wechat_platform::NOFOLLOW)
                .open(&log)?;
            let metadata = output_file.metadata()?;
            if !metadata.is_file() || !wechat_platform::private_file(&output_file)? {
                bail!("daemon log must be a private owned regular file; run `wx daemon doctor` to diagnose");
            }
            let log_start_offset = metadata.len();
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
                        return Err(with_log_hint(
                            anyhow::anyhow!("native daemon exited before readiness ({status})"),
                            &log,
                            Some(log_start_offset),
                        ));
                    }
                    if Instant::now() >= deadline {
                        return Err(with_log_hint(
                            anyhow::anyhow!("native daemon readiness deadline reached"),
                            &log,
                            Some(log_start_offset),
                        ));
                    }
                    if let Ok(Ok(value)) = tokio::time::timeout(
                        Duration::from_millis(500),
                        remote(&config)?.get("v1/status"),
                    )
                    .await
                    {
                        if value["pid"].as_u64() != Some(child.id() as u64)
                            || value["dataDir"].as_str() != config.data_dir.to_str()
                            || value["version"]["backend"] != "native"
                        {
                            return Err(with_log_hint(
                                anyhow::anyhow!(
                                    "listener belongs to another process; no owner was adopted"
                                ),
                                &log,
                                Some(log_start_offset),
                            ));
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
                        private_output(&record, &serde_json::to_vec(&owned)?)?;
                        return Ok(value);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            .await;
            match readiness {
                Ok(value) => output.print(&value),
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
        DaemonCommand::Status => match load(&record).map_err(|e| with_log_hint(e, &log, None))? {
            Some(owned) if fingerprint(owned.pid).is_some() => output.print(
                &identity(&config, &owned)
                    .await
                    .map_err(|e| with_log_hint(e, &log, None))?,
            ),
            _ => output.print(&json!({"state":"stopped","managed":true,"dataDir":config.data_dir})),
        },
        DaemonCommand::Stop { timeout } => {
            if directory.exists() {
                private_directory(&directory)?;
            }
            let _lock = if directory.exists() {
                let file = wechat_platform::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .mode(0o600)
                    .custom_flags(wechat_platform::NOFOLLOW)
                    .open(directory.join("management.lock"))?;
                if file.try_lock_exclusive().is_err() {
                    bail!("daemon lifecycle operation already in progress");
                }
                Some(file)
            } else {
                None
            };
            if !(1..=300).contains(&timeout) {
                bail!("daemon stop timeout must be1..300seconds");
            }
            let Some(owned) = load(&record)? else {
                return output.print(&json!({"state":"stopped","managed":true}));
            };
            if fingerprint(owned.pid).is_none() {
                std::fs::remove_file(&record)?;
                return output.print(&json!({"state":"stopped","managed":true}));
            }
            identity(&config, &owned).await?;
            #[cfg(windows)]
            remote(&config)?.post("v1/daemon/shutdown", json!({"pid":owned.pid,"serverId":owned.server_id,"ownershipGeneration":owned.ownership_generation})).await?;
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
            std::fs::remove_file(&record)?;
            output.print(&json!({"state":"stopped","pid":owned.pid,"logout":false}))
        }
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

    #[test]
    fn with_log_hint_preserves_json_message_exit_classification_and_log_offset() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("daemon.log");
        let stale = "Error: stale problem from earlier run\n";
        std::fs::write(&log, stale).unwrap();
        let offset = stale.len() as u64;

        let inner_io = std::io::Error::new(std::io::ErrorKind::AddrInUse, "Address in use");
        let err_none = with_log_hint(
            anyhow::Error::new(inner_io)
                .context("daemon listener is occupied; no owner was adopted or stopped"),
            &log,
            None,
        );
        // JSON error message (`to_string()`) keeps the real cause, never the hint text.
        assert_eq!(
            err_none.to_string(),
            "daemon listener is occupied; no owner was adopted or stopped"
        );
        // Human `{:#}` preserves the cause chain without duplicating the message.
        let formatted_none = format!("{err_none:#}");
        assert_eq!(
            formatted_none
                .matches("daemon listener is occupied")
                .count(),
            1
        );
        assert!(formatted_none.contains("Address in use"));
        let hint_none = err_none.downcast_ref::<HintedError>().unwrap();
        assert_eq!(hint_none.last_log_error, None);

        // Fresh log line containing `invalid ` and `config` must not flip exit 1 to exit 2.
        let fresh_line = "Error: invalid config value in fresh startup failure\n";
        std::fs::write(&log, format!("{stale}{fresh_line}")).unwrap();
        let err_fresh = with_log_hint(
            anyhow::anyhow!("native daemon exited before readiness"),
            &log,
            Some(offset),
        );
        assert_eq!(
            err_fresh.to_string(),
            "native daemon exited before readiness"
        );
        assert_eq!(outcome::classify(&err_fresh).0, 1);
        let hint_fresh = err_fresh.downcast_ref::<HintedError>().unwrap();
        assert_eq!(
            hint_fresh.last_log_error.as_deref(),
            Some("Error: invalid config value in fresh startup failure")
        );

        // A real config/invalid error still classifies as exit 2, and typed outcome errors keep their code.
        let err_load = with_log_hint(anyhow::anyhow!("invalid daemon/process.json"), &log, None);
        assert_eq!(outcome::classify(&err_load).0, 2);
        let err_auth = with_log_hint(outcome::failure(4, "unauthorized", "denied"), &log, None);
        assert_eq!(outcome::classify(&err_auth), (4, "unauthorized".into()));
    }
}
