//! Startup configuration for an isolated native call test. Constructing the
//! backend never places a call; an authenticated WebSocket owner must request it.
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wechat_core::{
    calls::native::{
        independent_voice_parameters, NativeCallBackend, NativeCallConfig, NativeTransmitScope,
    },
    Core,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TestConfig {
    schema_version: u32,
    data_dir: PathBuf,
    account_id: String,
    peer_id: String,
    intent_id: String,
    expires_at_unix_ms: u64,
    allow_known_audio_overlap: bool,
    #[serde(default = "default_duration")]
    max_duration_seconds: u64,
}
fn default_duration() -> u64 {
    20
}
fn intent(value: &str) -> Result<[u8; 32]> {
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid call test intent"
    );
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)?;
    }
    ensure!(bytes != [0; 32], "Empty call test intent");
    Ok(bytes)
}
fn check_window(expires: u64, now: u64, duration: u64) -> Result<()> {
    ensure!(
        expires > now && expires - now <= 300_000 && (1..=30).contains(&duration),
        "Call test must expire within five minutes and last at most 30 seconds"
    );
    Ok(())
}
pub(super) fn configure(
    core: &Arc<Core>,
    server: &wechat_config::ServerConfig,
    path: &Path,
) -> Result<()> {
    ensure!(
        server.listen.ip().is_loopback() && server.network_listen.is_none(),
        "Call tests require a loopback-only listener"
    );
    let config: TestConfig = serde_json::from_slice(&super::private_input(path, 8192)?)?;
    ensure!(
        config.schema_version == 1,
        "Unsupported call test configuration"
    );
    let now = u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())?;
    check_window(config.expires_at_unix_ms, now, config.max_duration_seconds)?;
    let test_intent = intent(&config.intent_id)?;
    ensure!(
        std::fs::canonicalize(&config.data_dir)? == core.store.root,
        "Call test data directory does not match this server"
    );
    let account = core.store.account(&config.account_id)?;
    ensure!(
        core.store.accounts()?.len() == 1
            && std::fs::canonicalize(&account.profile_path)?.starts_with(&core.store.root),
        "Call tests require a single managed isolated profile"
    );
    let mut native =
        NativeCallConfig::receive_only(config.account_id, independent_voice_parameters()?);
    native.transmit_scope = NativeTransmitScope::IsolatedTest {
        intent_id: test_intent,
        peer_id: config.peer_id,
        expires_at_unix_ms: config.expires_at_unix_ms,
        allow_known_audio_overlap: config.allow_known_audio_overlap,
    };
    native.max_duration = Duration::from_secs(config.max_duration_seconds);
    let backend = NativeCallBackend::new(Arc::downgrade(core), native)
        .context("Invalid isolated call test configuration")?;
    Core::configure_calls(core, Arc::new(backend))?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_intent_and_expiry_are_finite() {
        assert!(intent(&"01".repeat(32)).is_ok());
        for invalid in ["00".repeat(32), "é".repeat(32), "01".repeat(33)] {
            assert!(intent(&invalid).is_err());
        }
        assert!(check_window(301_000, 1_000, 20).is_ok());
        for (expires, now, duration) in [
            (1_000, 1_000, 20),
            (301_001, 1_000, 20),
            (2_000, 1_000, 31),
            (2_000, 1_000, 0),
        ] {
            assert!(check_window(expires, now, duration).is_err());
        }
    }
}
