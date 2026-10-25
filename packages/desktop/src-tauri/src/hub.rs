use crate::config::{self, Config, Connection};
use anyhow::{bail, Context, Result};
use rand::RngCore;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
};
use zeroize::Zeroizing;

pub struct OwnedHub {
    pub child: Child,
    pub connection: Connection,
    pub key: Zeroizing<String>,
    pub identity: Value,
}
pub fn http(connection: &Connection) -> Result<reqwest::Client> {
    config::endpoint(&connection.endpoint)?;
    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if let Some(pem) = &connection.certificate {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_pem(pem.as_bytes())
                .context("Invalid trusted certificate")?,
        );
    }
    Ok(builder.build()?)
}
pub async fn identity(connection: &Connection, key: &str) -> Result<Value> {
    let mut response = http(connection)?.get(format!("{}/v1/status", connection.endpoint.trim_end_matches('/')))
        .bearer_auth(key).timeout(Duration::from_secs(5)).send().await
        .map_err(|_| anyhow::anyhow!("Cannot connect to the hub; check its address, certificate and whether it is running"))?;
    if !response.status().is_success() {
        bail!("The hub did not accept this access key");
    }
    if response
        .content_length()
        .is_some_and(|length| length > 128 * 1024)
    {
        bail!("Invalid hub response");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.context("Invalid hub response")? {
        if body.len() + chunk.len() > 128 * 1024 {
            bail!("Invalid hub response");
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value = serde_json::from_slice(&body).context("Invalid hub response")?;
    if value["version"]["backend"] != "native" || !value["serverId"].is_string() {
        bail!("This address is not a native agent-wechat hub");
    }
    if connection
        .server_id
        .as_ref()
        .is_some_and(|expected| value["serverId"].as_str() != Some(expected))
    {
        bail!("The hub identity changed; reconnect to confirm the new hub");
    }
    Ok(value)
}
pub fn local_connection(config: &Config) -> Connection {
    Connection {
        id: "local".into(),
        name: "This device".into(),
        endpoint: format!("http://127.0.0.1:{}", config.local_port),
        local: true,
        server_id: config
            .connections
            .iter()
            .find(|value| value.local)
            .and_then(|value| value.server_id.clone()),
        certificate: None,
    }
}
pub fn executable(resource: &Path, name: &str) -> Result<PathBuf> {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let path = resource.join(format!("{name}{suffix}"));
    if !path.is_file() {
        bail!("A bundled executable is missing; reinstall AgentWechat");
    }
    Ok(path)
}
pub async fn start(
    config: &Config,
    data: &Path,
    binaries: &Path,
    key: Zeroizing<String>,
) -> Result<OwnedHub> {
    config::token(&key)?;
    wechat_platform::private_dir(data)?;
    let data = std::fs::canonicalize(data)?;
    let server_config = data.join("desktop-server.toml");
    if !server_config.exists() {
        config::private_write(&server_config, b"schemaVersion = 1\n")?;
    }
    // An occupied listener is never adopted, and an existing process is never stopped.
    drop(
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, config.local_port))
            .context("The local port is already in use; choose another port in desktop settings")?,
    );
    let connection = local_connection(config);
    let mut command = Command::new(executable(binaries, "wx")?);
    command
        .arg("serve")
        .arg("--data-dir")
        .arg(&data)
        .arg("--server-config")
        .arg(&server_config)
        .arg("--listen")
        .arg(format!("127.0.0.1:{}", config.local_port))
        .arg("--token-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // Use only this package's media helpers before inherited system tools.
    let mut paths = vec![binaries.to_owned()];
    if let Some(path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&path));
    }
    command.env("PATH", std::env::join_paths(paths)?);
    for variable in [
        "AGENT_WECHAT_TOKEN",
        "AGENT_WECHAT_TOKEN_FILE",
        "AGENT_WECHAT_SERVER_CONFIG",
        "AGENT_WECHAT_CONFIG",
        "AGENT_WECHAT_DATA_DIR",
    ] {
        command.env_remove(variable);
    }
    if config.network_enabled {
        let (cert, private) = tls_files(&data)?;
        command
            .arg("--network-listen")
            .arg(format!("0.0.0.0:{}", config.network_port))
            .arg("--tls-cert-file")
            .arg(cert)
            .arg("--tls-key-file")
            .arg(private);
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW, independent of signature status.
    let mut child = command.spawn().context("Cannot start the bundled hub")?;
    let mut input = child
        .stdin
        .take()
        .context("Cannot open the hub credential pipe")?;
    input.write_all(key.as_bytes()).await?;
    input.shutdown().await?;
    drop(input);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait()?.is_some() {
            bail!("The local hub stopped during startup; existing profiles were preserved");
        }
        if let Ok(value) = identity(&connection, &key).await {
            if value["pid"].as_u64() != child.id().map(u64::from)
                || value["dataDir"].as_str() != data.to_str()
            {
                bail!("The listener belongs to a different hub; it was not adopted");
            }
            return Ok(OwnedHub {
                child,
                connection,
                key,
                identity: value,
            });
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("The local hub did not become ready in time");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
pub async fn stop(hub: &mut OwnedHub) -> Result<()> {
    if hub.child.try_wait()?.is_some() {
        return Ok(());
    }
    let response = http(&hub.connection)?.post(format!("{}/v1/daemon/shutdown", hub.connection.endpoint))
        .bearer_auth(hub.key.as_str()).json(&json!({"pid":hub.identity["pid"],"serverId":hub.identity["serverId"],"ownershipGeneration":hub.identity["ownershipGeneration"]}))
        .timeout(Duration::from_secs(5)).send().await?;
    if !response.status().is_success() {
        bail!("The local hub refused the shutdown request");
    }
    tokio::time::timeout(Duration::from_secs(35), hub.child.wait())
        .await
        .context("The hub is still stopping; wait before trying again")??;
    Ok(())
}
pub fn fresh_key() -> Zeroizing<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    Zeroizing::new(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub fn tls_files(data: &Path) -> Result<(PathBuf, PathBuf)> {
    let cert = data.join("network/certificate.pem");
    let key = data.join("network/private-key.pem");
    if cert.exists() && key.exists() {
        return Ok((cert, key));
    }
    if cert.exists() || key.exists() {
        bail!("The network certificate is incomplete; existing files were preserved");
    }
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    names.extend(
        wechat_platform::local_ipv4_addresses()?
            .iter()
            .map(ToString::to_string),
    );
    let rcgen::CertifiedKey {
        cert: certificate,
        signing_key,
    } = rcgen::generate_simple_self_signed(names)?;
    config::private_write(&key, signing_key.serialize_pem().as_bytes())?;
    config::private_write(&cert, certificate.pem().as_bytes())?;
    Ok((cert, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
    #[tokio::test]
    #[ignore = "requires the freshly built wx executable through WX_BINARY"]
    async fn private_pipe_tls_shutdown_and_independent_client_keys() {
        let binary = PathBuf::from(std::env::var_os("WX_BINARY").expect("WX_BINARY is required"));
        let binaries = binary.parent().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("hub");
        let mut config = Config {
            local_port: port(),
            network_port: port(),
            network_enabled: true,
            ..Config::default()
        };
        while config.network_port == config.local_port {
            config.network_port = port();
        }
        let original_key = fresh_key();
        let mut hub = start(
            &config,
            &data,
            binaries,
            Zeroizing::new(original_key.to_string()),
        )
        .await
        .unwrap();
        assert!(!data.join("api-token").exists());
        assert!(data.join("storage-key").is_file());
        let certificate = std::fs::read_to_string(data.join("network/certificate.pem")).unwrap();
        let secure = Connection {
            endpoint: format!("https://localhost:{}", config.network_port),
            certificate: Some(certificate),
            ..hub.connection.clone()
        };
        assert_eq!(
            identity(&secure, &original_key).await.unwrap()["serverId"],
            hub.identity["serverId"]
        );
        let mut client = Command::new(&binary)
            .args(["--json", "--server", &secure.endpoint, "--ca-cert-file"])
            .arg(data.join("network/certificate.pem"))
            .args(["--token-stdin", "status"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = client.stdin.take().unwrap();
        input.write_all(original_key.as_bytes()).await.unwrap();
        input.shutdown().await.unwrap();
        drop(input);
        let output = client.wait_with_output().await.unwrap();
        assert!(output.status.success(), "Client pipe authentication failed");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["serverId"],
            hub.identity["serverId"]
        );
        let untrusted = Connection {
            certificate: None,
            ..secure.clone()
        };
        assert!(identity(&untrusted, &original_key).await.is_err());
        let issue = http(&secure)
            .unwrap()
            .post(format!("{}/v1/tokens", secure.endpoint))
            .bearer_auth(original_key.as_str())
            .json(&json!({"name":"fixture","accounts":["*"],"scopes":["read"]}))
            .send()
            .await
            .unwrap();
        assert!(issue.status().is_success());
        let issued: Value = issue.json().await.unwrap();
        let client_key = issued["token"].as_str().unwrap();
        assert!(identity(&secure, client_key).await.is_ok());
        let before = std::fs::read(data.join("storage-key")).unwrap();
        stop(&mut hub).await.unwrap();
        let replacement = fresh_key();
        let mut restarted = start(&config, &data, binaries, replacement).await.unwrap();
        assert!(identity(&secure, &original_key).await.is_err());
        assert!(identity(&secure, client_key).await.is_ok());
        assert_eq!(std::fs::read(data.join("storage-key")).unwrap(), before);
        stop(&mut restarted).await.unwrap();
    }
    #[tokio::test]
    async fn occupied_listener_is_never_adopted() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            local_port: listener.local_addr().unwrap().port(),
            ..Config::default()
        };
        assert!(
            start(&config, directory.path(), directory.path(), fresh_key())
                .await
                .is_err()
        );
        assert!(listener.local_addr().is_ok());
    }
}
