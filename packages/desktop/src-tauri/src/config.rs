use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub local: bool,
    pub server_id: Option<String>,
    pub certificate: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub installation_id: String,
    pub active: Option<String>,
    pub connections: Vec<Connection>,
    pub local_port: u16,
    pub network_port: u16,
    pub network_enabled: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            installation_id: uuid::Uuid::new_v4().to_string(),
            active: None,
            connections: Vec::new(),
            local_port: 6174,
            network_port: 6175,
            network_enabled: false,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let file = wechat_platform::OpenOptions::new().read(true).open(path)?;
        if !file.metadata()?.is_file() || !wechat_platform::private_file(&file)? {
            bail!("Desktop settings must be private to this user");
        }
        let mut bytes = Vec::new();
        file.take(128 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 128 * 1024 {
            bail!("Desktop settings are too large");
        }
        let config: Self = serde_json::from_slice(&bytes).map_err(|_| {
            anyhow::anyhow!("Cannot read desktop settings; existing data was preserved")
        })?;
        if config.schema_version != 1 {
            bail!("This desktop settings version needs a newer application");
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.installation_id).context("Invalid installation identity")?;
        if self.local_port < 1024
            || self.network_port < 1024
            || self.local_port == self.network_port
        {
            bail!("Choose different local and network ports between 1024 and 65535");
        }
        if self.connections.len() > 32 {
            bail!("Too many saved hubs");
        }
        let mut ids = std::collections::HashSet::new();
        for connection in &self.connections {
            if !ids.insert(&connection.id) || connection.id.is_empty() || connection.id.len() > 64 {
                bail!("Invalid saved hub identity");
            }
            endpoint(&connection.endpoint)?;
            if connection.name.is_empty() || connection.name.len() > 128 {
                bail!("Enter a hub name of at most 128 bytes");
            }
            if connection
                .certificate
                .as_ref()
                .is_some_and(|value| value.len() > 64 * 1024)
            {
                bail!("Certificate is too large");
            }
        }
        Ok(())
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        private_write(path, &serde_json::to_vec_pretty(self)?)
    }
}
pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Missing settings folder")?;
    if parent.exists() {
        if !wechat_platform::owned_path(parent)? || !parent.is_dir() {
            bail!("Desktop settings folder must belong to this user");
        }
        wechat_platform::protect_path(parent, true)?;
    }
    wechat_platform::private_dir(parent)?;
    if path.exists() && !wechat_platform::private_path(path)? {
        bail!("Existing file must be private to this user");
    }
    let mut file = wechat_platform::private_tempfile_in(parent, ".agent-wechat-")?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    wechat_platform::sync_dir(parent)?;
    Ok(())
}
pub fn endpoint(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).context("Enter a valid hub address")?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || !(url.scheme() == "https" || url.scheme() == "http" && loopback)
    {
        bail!("Use an HTTPS hub address, or HTTP on localhost, without a path or credentials");
    }
    Ok(url)
}
pub fn token(value: &str) -> Result<()> {
    if !(32..=1024).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!("Enter a valid access key");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_contain_references_and_survive_reload() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.json");
        let config = Config::load(&path).unwrap();
        config.save(&path).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().installation_id,
            config.installation_id
        );
        assert!(wechat_platform::private_path(&path).unwrap());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("token"));
    }
    #[test]
    fn endpoints_require_verified_transport_without_embedded_keys() {
        for input in [
            "http://192.168.1.2:6175",
            "https://user:key@example.com",
            "https://example.com/v1",
            "https://example.com?key=x",
        ] {
            assert!(endpoint(input).is_err());
        }
        for input in [
            "http://127.0.0.1:6174",
            "http://[::1]:6174",
            "https://example.com",
        ] {
            assert!(endpoint(input).is_ok());
        }
    }
    #[test]
    fn invalid_settings_never_overwrite_existing_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.json");
        let mut config = Config::default();
        config.save(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        config.network_port = config.local_port;
        assert!(config.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}
