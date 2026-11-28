//! End-to-end tests for `wx daemon start`, `restart`, `stop`, `install` and `uninstall`.
use serde_json::Value;
use std::{
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct DaemonEnv {
    home: PathBuf,
    config_home: PathBuf,
    data_dir: PathBuf,
    listen: String,
    started: bool,
}

impl DaemonEnv {
    fn new(root: &Path) -> Self {
        let home = root.join("home");
        let config_home = home.join(".config");
        let data_dir = root.join("data");
        std::fs::create_dir_all(&config_home).unwrap();
        Self {
            home,
            config_home,
            data_dir,
            listen: format!("127.0.0.1:{}", free_port()),
            started: false,
        }
    }

    fn cmd(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wx"));
        command
            .arg("--json")
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--listen")
            .arg(&self.listen)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.config_home)
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env_remove("AGENT_WECHAT_TOKEN")
            .env_remove("AGENT_WECHAT_TOKEN_FILE")
            .env_remove("AGENT_WECHAT_SERVER_CONFIG")
            .env_remove("AGENT_WECHAT_DATA_DIR")
            .env_remove("AGENT_WECHAT_CONFIG")
            .env_remove("AGENT_WECHAT_URL")
            .env_remove("AGENT_WECHAT_SERVER");
        command.output().unwrap()
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let out = self.cmd(args);
        assert!(
            out.status.success(),
            "wx {:?} failed ({}): stdout={} stderr={}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for DaemonEnv {
    fn drop(&mut self) {
        if self.started {
            let _ = self.cmd(&["daemon", "stop"]);
        }
    }
}

#[test]
fn daemon_restart_install_refusal_and_uninstall_preserve_ownership_and_data() {
    let root = wechat_platform::private_tempdir("wx-daemon-lifecycle-").unwrap();
    let mut env = DaemonEnv::new(root.path());

    #[cfg(target_os = "linux")]
    {
        let preview = env.run_json(&["daemon", "install", "--dry-run"]);
        assert_eq!(preview["dryRun"], true);
        assert_eq!(preview["unitName"], "agent-wechat-native.service");
        let unit = preview["unit"].as_str().unwrap();
        assert!(unit.contains("ExecStart="));
        assert!(unit.contains("ReadWritePaths="));
        assert!(unit.contains("ProtectHome=read-only"));
    }

    let first = env.run_json(&["daemon", "start"]);
    env.started = true;
    let pid1 = first["pid"].as_u64().unwrap();
    let server_id = first["serverId"].as_str().unwrap().to_owned();

    // Installing while `wx daemon start` owns the data folder must refuse.
    #[cfg(target_os = "linux")]
    {
        let refused = env.cmd(&["daemon", "install", "--yes"]);
        assert!(!refused.status.success());
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&refused.stdout),
            String::from_utf8_lossy(&refused.stderr)
        );
        assert!(
            combined.contains("wx daemon stop"),
            "expected stop guidance in: {combined}"
        );
    }

    // Even if a stopped unit file exists for this data folder, `wx daemon restart`
    // must restart the live `process.json`-owned daemon rather than failing.
    #[cfg(target_os = "linux")]
    let unit_file = {
        let preview = env.run_json(&["daemon", "install", "--dry-run"]);
        let unit_path = PathBuf::from(preview["unitPath"].as_str().unwrap());
        std::fs::create_dir_all(unit_path.parent().unwrap()).unwrap();
        std::fs::write(&unit_path, preview["unit"].as_str().unwrap()).unwrap();
        unit_path
    };

    let restarted = env.run_json(&["daemon", "restart"]);
    let pid2 = restarted["pid"].as_u64().unwrap();
    assert_ne!(pid1, pid2, "restart must launch a new daemon process");
    assert_eq!(restarted["serverId"].as_str().unwrap(), server_id);
    assert_eq!(restarted["state"], "ready");

    let stopped = env.run_json(&["daemon", "stop"]);
    env.started = false;
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["logout"], false);

    // Uninstall removes the stopped unit file while preserving registry data,
    // and is idempotent when already absent.
    #[cfg(target_os = "linux")]
    {
        assert!(unit_file.is_file());
        let removed = env.run_json(&["daemon", "uninstall"]);
        assert_eq!(removed["state"], "uninstalled");
        assert_eq!(removed["dataPreserved"], true);
        assert!(!unit_file.exists());
        assert!(env.data_dir.join("registry.sqlite").is_file());

        let again = env.run_json(&["daemon", "uninstall"]);
        assert_eq!(again["state"], "not_installed");
        assert_eq!(again["dataPreserved"], true);
    }
}
