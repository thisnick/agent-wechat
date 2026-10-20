//! Server settings are independent of HTTP-client selections and profile contents.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Default, Clone)]
pub struct Overrides {
    pub server_config: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub token_file: Option<PathBuf>,
    pub listen: Option<SocketAddr>,
}

/// Captured once by the executable; pure resolution avoids process-global test state.
#[derive(Default, Clone)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub data_home: Option<PathBuf>,
    pub server_config: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

impl Environment {
    pub fn capture() -> Self {
        fn value(name: &str) -> Option<PathBuf> {
            std::env::var_os(name)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        }
        Self {
            home: value("HOME"),
            config_home: value("XDG_CONFIG_HOME"),
            data_home: value("XDG_DATA_HOME"),
            server_config: value("AGENT_WECHAT_SERVER_CONFIG"),
            data_dir: value("AGENT_WECHAT_DATA_DIR"),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileConfig {
    schema_version: Option<u32>,
    data_dir: Option<PathBuf>,
    token_file: Option<PathBuf>,
    listen: Option<SocketAddr>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub schema_version: u32,
    pub config_file: PathBuf,
    pub config_present: bool,
    pub data_dir: PathBuf,
    pub token_file: PathBuf,
    pub token_explicit: bool,
    pub listen: SocketAddr,
}

fn absolute(path: &Path, base: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.starts_with("~") {
        bail!("invalid path; expand the home directory explicitly");
    }
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        base.join(path)
    })
}

pub fn resolve(flags: &Overrides, env: &Environment, cwd: &Path) -> Result<ServerConfig> {
    if !cwd.is_absolute() {
        bail!("working directory must be absolute");
    }
    let home = env.home.as_ref().filter(|p| p.is_absolute());
    let config_root = env
        .config_home
        .as_ref()
        .filter(|p| p.is_absolute())
        .cloned()
        .or_else(|| home.map(|p| p.join(".config")));
    let explicit = flags.server_config.as_ref().or(env.server_config.as_ref());
    let config_file = match explicit {
        Some(p) => absolute(p, cwd)?,
        None => config_root
            .context("HOME or absolute XDG_CONFIG_HOME is required")?
            .join("agent-wechat/server/config.toml"),
    };
    let (file, present) = match std::fs::File::open(&config_file) {
        Ok(input) => {
            let mut text = String::new();
            input
                .take(64 * 1024 + 1)
                .read_to_string(&mut text)
                .context("cannot read server configuration")?;
            // TOML errors can quote secret-bearing source lines; do not include them.
            if text.len() > 64 * 1024 {
                bail!("server configuration is too large");
            }
            let file: FileConfig = toml::from_str(&text)
                .map_err(|_| anyhow::anyhow!("invalid server configuration"))?;
            if file.schema_version != Some(1) {
                bail!("unsupported server configuration schemaVersion");
            }
            (file, true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
            (FileConfig::default(), false)
        }
        Err(e) => return Err(e).context("cannot read selected server configuration"),
    };
    let base = config_file
        .parent()
        .context("configuration path has no parent")?;
    let data_dir = if let Some(p) = flags.data_dir.as_ref().or(env.data_dir.as_ref()) {
        absolute(p, cwd)?
    } else if let Some(p) = &file.data_dir {
        absolute(p, base)?
    } else {
        env.data_home
            .as_ref()
            .filter(|p| p.is_absolute())
            .cloned()
            .or_else(|| home.map(|p| p.join(".local/share")))
            .context("HOME or absolute XDG_DATA_HOME is required")?
            .join("agent-wechat")
    };
    let token_explicit = flags.token_file.is_some() || file.token_file.is_some();
    let token_file = match (&flags.token_file, &file.token_file) {
        (Some(p), _) => absolute(p, cwd)?,
        (_, Some(p)) => absolute(p, base)?,
        _ => data_dir.join("api-token"),
    };
    let listen = flags
        .listen
        .or(file.listen)
        .unwrap_or_else(|| "127.0.0.1:6174".parse().unwrap());
    if !listen.ip().is_loopback() {
        bail!("this initial server requires a loopback listener");
    }
    Ok(ServerConfig {
        schema_version: 1,
        config_file,
        config_present: present,
        data_dir,
        token_file,
        token_explicit,
        listen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_pure_and_separate_config_from_data() {
        let t = tempfile::tempdir().unwrap();
        let env = Environment {
            home: Some(t.path().to_owned()),
            ..Default::default()
        };
        let c = resolve(&Overrides::default(), &env, t.path()).unwrap();
        assert_eq!(
            c.config_file,
            t.path().join(".config/agent-wechat/server/config.toml")
        );
        assert_eq!(c.data_dir, t.path().join(".local/share/agent-wechat"));
        assert!(!c.data_dir.exists());
    }
    #[test]
    fn flags_env_file_and_relative_paths_have_defined_precedence() {
        let t = tempfile::tempdir().unwrap();
        let config = t.path().join("server.toml");
        std::fs::write(
            &config,
            "schemaVersion=1\ndataDir='file-data'\ntokenFile='secret'\n",
        )
        .unwrap();
        let env = Environment {
            server_config: Some(config),
            data_dir: Some(t.path().join("env-data")),
            ..Default::default()
        };
        let c = resolve(&Overrides::default(), &env, t.path()).unwrap();
        assert_eq!(c.data_dir, t.path().join("env-data"));
        assert_eq!(c.token_file, t.path().join("secret"));
        let flags = Overrides {
            data_dir: Some(t.path().join("flag-data")),
            ..Default::default()
        };
        assert_eq!(
            resolve(&flags, &env, t.path()).unwrap().data_dir,
            t.path().join("flag-data")
        );
    }
    #[test]
    fn explicit_missing_unknown_and_newer_configs_fail_before_mutation() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("server.toml");
        let flags = Overrides {
            server_config: Some(p.clone()),
            ..Default::default()
        };
        assert!(resolve(&flags, &Environment::default(), t.path()).is_err());
        for text in [
            "schemaVersion=2",
            "schemaVersion=1\nunknown='secret'",
            "bad = [",
        ] {
            std::fs::write(&p, text).unwrap();
            assert!(resolve(&flags, &Environment::default(), t.path()).is_err());
        }
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
    }
    #[test]
    fn relative_xdg_is_ignored_and_remote_listener_rejected() {
        let t = tempfile::tempdir().unwrap();
        let env = Environment {
            home: Some(t.path().into()),
            config_home: Some("relative".into()),
            ..Default::default()
        };
        assert!(resolve(&Overrides::default(), &env, t.path())
            .unwrap()
            .config_file
            .starts_with(t.path()));
        assert!(resolve(
            &Overrides {
                listen: Some("0.0.0.0:6174".parse().unwrap()),
                ..Default::default()
            },
            &env,
            t.path()
        )
        .is_err());
    }
}
