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
    Start,
    Status,
    Stop {
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    Logs {
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
fn remote(config: &wechat_config::ServerConfig) -> Result<Remote> {
    Remote::new(wechat_config::ClientConfig {
        schema_version: 1,
        endpoint: format!("http://{}", config.listen),
        token_file: config.token_file.clone(),
        account: None,
    })
}
async fn identity(config: &wechat_config::ServerConfig, owned: &OwnedProcess) -> Result<Value> {
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
pub async fn run(config: wechat_config::ServerConfig, command: DaemonCommand) -> Result<()> {
    let mut config = config;
    if config.data_dir.exists() {
        config.data_dir = std::fs::canonicalize(&config.data_dir)?;
    }
    let directory = config.data_dir.join("daemon");
    let record = directory.join("process.json");
    let log = directory.join("daemon.log");
    match command {
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
                    return output(&identity(&config, &owned).await?);
                }
                std::fs::remove_file(&record)?;
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
                .open(&log)?;
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
            wechat_platform::detach(&mut command);
            let mut child = command.spawn().context("cannot launch native daemon")?;
            let deadline = Instant::now() + Duration::from_secs(20);
            let readiness = async {
                loop {
                    if let Some(status) = child.try_wait()? {
                        bail!(
                            "native daemon exited before readiness ({status}); inspect daemon logs"
                        );
                    }
                    if Instant::now() >= deadline {
                        bail!("native daemon readiness deadline reached");
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
                        private_output(&record, &serde_json::to_vec(&owned)?)?;
                        return Ok(value);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            .await;
            match readiness {
                Ok(value) => output(&value),
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
        DaemonCommand::Status => match load(&record)? {
            Some(owned) if fingerprint(owned.pid).is_some() => {
                output(&identity(&config, &owned).await?)
            }
            _ => output(&json!({"state":"stopped","managed":true,"dataDir":config.data_dir})),
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
                return output(&json!({"state":"stopped","managed":true}));
            };
            if fingerprint(owned.pid).is_none() {
                std::fs::remove_file(&record)?;
                return output(&json!({"state":"stopped","managed":true}));
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
            output(&json!({"state":"stopped","pid":owned.pid,"logout":false}))
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
}
