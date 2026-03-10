use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::ia::identify_states;
use crate::sessions::manager::get_session;
use crate::tools::a11y::get_a11y_desktop;
use crate::tools::exec::ExecOptions;
use crate::tools::screenshot::capture_screenshot;
use crate::tools::wechat_db::find_wechat_pid;

/// How often to run the health scan (in seconds).
const SCAN_INTERVAL_SECS: u64 = 1;

/// Kill WeChat if no IA state has been identified for this long (in seconds).
const UNRESPONSIVE_TIMEOUT_SECS: u64 = 60;

/// Global flag to pause health monitoring during active execution loops.
static MONITORING_PAUSED: AtomicBool = AtomicBool::new(false);

/// Pause health monitoring (call when an execution loop starts).
pub fn pause_monitoring() {
    MONITORING_PAUSED.store(true, Ordering::Relaxed);
}

/// Resume health monitoring (call when an execution loop ends).
pub fn resume_monitoring() {
    MONITORING_PAUSED.store(false, Ordering::Relaxed);
}

/// Spawn the background health monitor task.
///
/// Every second, it checks the default session's WeChat process by running
/// a11y → identify. If no IA state has been identified for more than 60 seconds,
/// it kills the WeChat process so the entrypoint restart loop can relaunch it.
pub fn spawn_health_monitor() {
    tokio::spawn(async move {
        tracing::info!("[health] WeChat health monitor started");

        let mut last_identified = Instant::now();
        let mut was_running = false;

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(SCAN_INTERVAL_SECS)).await;

            // Skip if monitoring is paused (an execution loop is active)
            if MONITORING_PAUSED.load(Ordering::Relaxed) {
                last_identified = Instant::now();
                continue;
            }

            // Only monitor the default session
            let session = match get_session("default") {
                Some(s) if s.status == "running" => s,
                _ => {
                    last_identified = Instant::now();
                    continue;
                }
            };

            // Check if WeChat process is even running
            let wechat_pid = match find_wechat_pid() {
                Some(pid) => {
                    if !was_running {
                        tracing::info!("[health] WeChat process found (pid={})", pid);
                        was_running = true;
                    }
                    pid
                }
                None => {
                    if was_running {
                        tracing::warn!(
                            "[health] WeChat process disappeared (likely crashed), waiting for entrypoint to restart it"
                        );
                        was_running = false;
                    }
                    last_identified = Instant::now();
                    continue;
                }
            };

            // Run a11y + identify to see if we can detect any state
            let exec_options = ExecOptions {
                session: Some(session),
                timeout_ms: 10_000,
            };

            let a11y = match get_a11y_desktop(&exec_options).await {
                Ok(tree) => tree,
                Err(_) => {
                    // a11y failed — count as unresponsive, don't reset timer
                    check_and_kill(wechat_pid, &last_identified);
                    continue;
                }
            };

            let screenshot = capture_screenshot(&exec_options)
                .await
                .unwrap_or_default();
            let identified = identify_states(&a11y, &screenshot);

            if identified.main_window.is_some() {
                // State identified — WeChat is responsive
                last_identified = Instant::now();
            } else {
                // No state identified — check timeout
                check_and_kill(wechat_pid, &last_identified);
            }
        }
    });
}

/// If time since last identified state exceeds the timeout, kill the WeChat process.
fn check_and_kill(wechat_pid: i64, last_identified: &Instant) {
    let elapsed = last_identified.elapsed();
    if elapsed.as_secs() >= UNRESPONSIVE_TIMEOUT_SECS {
        tracing::warn!(
            "[health] WeChat (pid={}) unresponsive for {}s, killing process",
            wechat_pid,
            elapsed.as_secs()
        );

        let result = std::process::Command::new("kill")
            .args(["-9", &wechat_pid.to_string()])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                tracing::info!(
                    "[health] Killed WeChat pid={}, entrypoint will restart it",
                    wechat_pid
                );
            }
            Ok(output) => {
                tracing::warn!(
                    "[health] kill returned non-zero for pid={}: {}",
                    wechat_pid,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(e) => {
                tracing::error!("[health] Failed to kill WeChat pid={}: {}", wechat_pid, e);
            }
        }
    } else {
        tracing::debug!(
            "[health] WeChat unresponsive for {}s (threshold: {}s)",
            elapsed.as_secs(),
            UNRESPONSIVE_TIMEOUT_SECS
        );
    }
}
