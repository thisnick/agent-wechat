use super::*;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Write};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Server {
    pub url: String,
    pub token_file: PathBuf,
    pub default_account_id: Option<String>,
    pub local_profile: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalProfile {
    pub format: String,
    pub path: PathBuf,
    pub account_dir: Option<String>,
    pub keys_file: Option<PathBuf>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub active_server: Option<String>,
    pub local_profile: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, Server>,
    #[serde(default)]
    pub local_profiles: BTreeMap<String, LocalProfile>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            active_server: None,
            local_profile: None,
            servers: BTreeMap::new(),
            local_profiles: BTreeMap::new(),
        }
    }
}
pub struct File {
    pub path: PathBuf,
    pub settings: Settings,
}
fn absolute(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        base.join(path)
    }
}
pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
    {
        bail!("invalid configuration name");
    }
    Ok(())
}
impl File {
    pub fn load(
        explicit: Option<&Path>,
        env: &wechat_config::Environment,
        cwd: &Path,
        create: bool,
    ) -> Result<Self> {
        let chosen = explicit
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("AGENT_WECHAT_CONFIG").map(PathBuf::from));
        let path = match chosen.as_ref() {
            Some(path) => absolute(path, cwd),
            None => env
                .config_home
                .as_ref()
                .filter(|p| p.is_absolute())
                .cloned()
                .or_else(|| env.home.as_ref().map(|p| p.join(".config")))
                .context("client configuration location unavailable")?
                .join("agent-wechat/client.toml"),
        };
        let settings = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file()
                    || !wechat_platform::private_path(&path)?
                    || metadata.len() > 65536
                {
                    bail!("client config must be a bounded owner-only regular file");
                }
                let bytes = private_input(&path, 65536)?;
                let text = std::str::from_utf8(&bytes).context("invalid client configuration")?;
                let mut parsed = match toml::from_str::<Settings>(text) {
                    Ok(value) => value,
                    Err(_) => {
                        let legacy: wechat_config::ClientConfig = toml::from_str(text)
                            .map_err(|_| anyhow::anyhow!("invalid client configuration"))?;
                        if legacy.schema_version != 1 {
                            bail!("unsupported client configuration schemaVersion");
                        }
                        let mut value = Settings {
                            active_server: Some("legacy".into()),
                            ..Default::default()
                        };
                        value.servers.insert(
                            "legacy".into(),
                            Server {
                                url: legacy.endpoint,
                                token_file: legacy.token_file,
                                default_account_id: legacy.account,
                                local_profile: None,
                            },
                        );
                        value
                    }
                };
                if parsed.schema_version != 1 {
                    bail!("unsupported client configuration schemaVersion");
                }
                let parent = path.parent().context("invalid client config path")?;
                for server in parsed.servers.values_mut() {
                    server.token_file = absolute(&server.token_file, parent);
                }
                for profile in parsed.local_profiles.values_mut() {
                    profile.path = absolute(&profile.path, parent);
                    profile.keys_file = profile.keys_file.as_ref().map(|p| absolute(p, parent));
                }
                parsed
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && (chosen.is_none() || create) => {
                Settings::default()
            }
            Err(e) => return Err(e).context("cannot read selected client configuration"),
        };
        Ok(Self { path, settings })
    }
    pub fn save(&self) -> Result<()> {
        let parent = self.path.parent().context("invalid client config path")?;
        if !parent.exists() {
            wechat_platform::private_dir(parent)?;
        }
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || !wechat_platform::owned_path(parent)? {
            bail!("client config directory must be owned by this user");
        }
        wechat_platform::protect_path(parent, true)?;
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if !metadata.is_file() || !wechat_platform::private_path(&self.path)? {
                bail!("client config must be an owner-only regular file");
            }
        }
        let mut file = wechat_platform::private_tempfile_in(parent, ".wx-config-")?;
        file.write_all(toml::to_string_pretty(&self.settings)?.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        wechat_platform::sync_dir(parent)?;
        Ok(())
    }
    pub fn server_name(&self, name: Option<&str>, url: Option<&str>) -> Result<Option<String>> {
        if name.is_some() && url.is_some() {
            bail!("server name and URL conflict");
        }
        if let Some(name) = name {
            if !self.settings.servers.contains_key(name) {
                bail!("saved server not found");
            }
            return Ok(Some(name.into()));
        }
        if let Some(url) = url {
            let matches: Vec<_> = self
                .settings
                .servers
                .iter()
                .filter(|(_, s)| s.url.trim_end_matches('/') == url.trim_end_matches('/'))
                .map(|(n, _)| n.clone())
                .collect();
            if matches.len() > 1 {
                bail!("ambiguous saved endpoint; select --server-name");
            }
            return Ok(matches.into_iter().next());
        }
        if let Some(name) = &self.settings.active_server {
            if !self.settings.servers.contains_key(name) {
                bail!("saved active server not found");
            }
            return Ok(Some(name.clone()));
        }
        Ok(None)
    }
    pub fn profile(
        &self,
        selected: Option<&str>,
        server: Option<&str>,
    ) -> Result<(String, LocalProfile)> {
        let name = selected
            .map(str::to_owned)
            .or_else(|| std::env::var("AGENT_WECHAT_LOCAL_PROFILE").ok())
            .or_else(|| {
                server
                    .and_then(|s| self.settings.servers.get(s))
                    .and_then(|s| s.local_profile.clone())
            })
            .or_else(|| self.settings.local_profile.clone())
            .context("select --local-profile or register a default profile")?;
        let profile = self
            .settings
            .local_profiles
            .get(&name)
            .context("local profile not found")?
            .clone();
        Ok((name, profile))
    }
    pub fn client(
        &self,
        selected: Option<&str>,
        url: Option<&str>,
        server: &wechat_config::ServerConfig,
    ) -> Result<(wechat_config::ClientConfig, Option<String>)> {
        let environment_url = std::env::var("AGENT_WECHAT_URL").ok();
        let environment_name = std::env::var("AGENT_WECHAT_SERVER").ok();
        let direct = url.or_else(|| {
            if selected.is_none() {
                environment_url.as_deref()
            } else {
                None
            }
        });
        let name = selected.or_else(|| {
            if direct.is_none() {
                environment_name.as_deref()
            } else {
                None
            }
        });
        let name = self.server_name(name, direct)?;
        let saved = name.as_ref().and_then(|n| self.settings.servers.get(n));
        Ok((
            wechat_config::ClientConfig {
                schema_version: 1,
                endpoint: direct
                    .map(str::to_owned)
                    .or_else(|| saved.map(|s| s.url.clone()))
                    .unwrap_or_else(|| format!("http://{}", server.listen)),
                token_file: std::env::var_os("AGENT_WECHAT_TOKEN_FILE")
                    .map(PathBuf::from)
                    .or_else(|| saved.map(|s| s.token_file.clone()))
                    .unwrap_or_else(|| server.token_file.clone()),
                account: std::env::var("AGENT_WECHAT_ACCOUNT")
                    .ok()
                    .or_else(|| saved.and_then(|s| s.default_account_id.clone())),
            },
            name,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_defaults_never_leak_between_named_or_ambiguous_servers() {
        let mut file = File {
            path: "/unused".into(),
            settings: Settings::default(),
        };
        for name in ["a", "b"] {
            file.settings.servers.insert(
                name.into(),
                Server {
                    url: "https://example.invalid".into(),
                    token_file: format!("/{name}").into(),
                    default_account_id: Some(name.into()),
                    local_profile: None,
                },
            );
        }
        assert!(file
            .server_name(None, Some("https://example.invalid"))
            .is_err());
        assert_eq!(
            file.server_name(Some("b"), None).unwrap().as_deref(),
            Some("b")
        );
        assert_eq!(
            file.server_name(None, Some("https://other.invalid"))
                .unwrap(),
            None
        );
    }
    #[test]
    fn protected_config_roundtrip_keeps_server_and_local_profile_scopes_separate() {
        let temp = tempfile::tempdir().unwrap();
        let mut file = File {
            path: temp.path().join("client.toml"),
            settings: Settings::default(),
        };
        file.settings.local_profiles.insert(
            "archive".into(),
            LocalProfile {
                format: "official-linux".into(),
                path: "/stopped/copy".into(),
                account_dir: None,
                keys_file: Some("/private/missing.keys.json".into()),
            },
        );
        file.settings.local_profile = Some("archive".into());
        file.save().unwrap();
        let loaded = File::load(
            Some(&file.path),
            &wechat_config::Environment::default(),
            temp.path(),
            false,
        )
        .unwrap();
        assert_eq!(loaded.profile(None, None).unwrap().0, "archive");
        assert!(loaded.settings.servers.is_empty());
        assert_eq!(std::fs::metadata(&file.path).unwrap().mode() & 0o777, 0o600);
    }
}
