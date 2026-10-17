use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::ia::identify_states;
use crate::sessions::manager::get_session;
use crate::tools::a11y::get_a11y_desktop;
use crate::tools::exec::ExecOptions;
use crate::tools::screenshot::capture_screenshot;
use crate::tools::wechat_db::find_wechat_pid;

/// Process liveness is cheap enough to check every second. This catches a
/// crashed WeChat process quickly without repeatedly traversing the full
/// AT-SPI tree.
const PROCESS_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The full UI observation (a11y tree + screenshot + state identification) is
/// expensive. Keep the default conservative enough for reliable recovery while
/// avoiding a continuous per-second scan on an idle container.
const DEFAULT_UI_SCAN_INTERVAL_SECS: u64 = 30;
const MIN_UI_SCAN_INTERVAL_SECS: u64 = 1;
const UI_SCAN_INTERVAL_ENV: &str = "AGENT_WECHAT_HEALTH_UI_SCAN_INTERVAL_SECS";

/// Kill WeChat if no IA state has been identified for this long (in seconds).
const DEFAULT_UNRESPONSIVE_TIMEOUT_SECS: u64 = 60;
const UNRESPONSIVE_TIMEOUT_ENV: &str = "AGENT_WECHAT_HEALTH_UNRESPONSIVE_TIMEOUT_SECS";

/// Delay before restarting WeChat after a crash (in seconds).
const RESTART_DELAY_SECS: u64 = 3;

/// If WeChat crashes this many times within RAPID_WINDOW_SECS, back off.
const MAX_RAPID_RESTARTS: u32 = 5;
const RAPID_WINDOW_SECS: u64 = 60;
const BACKOFF_DELAY_SECS: u64 = 30;

/// Global flag to pause health monitoring during active execution loops.
static MONITORING_PAUSED: AtomicBool = AtomicBool::new(false);

fn parse_positive_seconds(raw: Option<&str>) -> Option<u64> {
    let value = raw?.trim().parse::<u64>().ok()?;
    (value >= MIN_UI_SCAN_INTERVAL_SECS).then_some(value)
}

/// Resolve a positive duration from an environment variable. Invalid values
/// fall back to the safe default. There is intentionally no upper bound.
fn seconds_from_env(name: &str, default_secs: u64) -> Duration {
    let raw = std::env::var(name).ok();
    let seconds = parse_positive_seconds(raw.as_deref()).unwrap_or_else(|| {
        if raw.is_some() {
            tracing::warn!(
                "[health] Ignoring invalid {}; expected an integer >= {}",
                name,
                MIN_UI_SCAN_INTERVAL_SECS,
            );
        }
        default_secs
    });
    Duration::from_secs(seconds)
}

/// Pause health monitoring (call when an execution loop starts).
pub fn pause_monitoring() {
    MONITORING_PAUSED.store(true, Ordering::Relaxed);
}

/// Resume health monitoring (call when an execution loop ends).
pub fn resume_monitoring() {
    MONITORING_PAUSED.store(false, Ordering::Relaxed);
}

/// Spawn WeChat process for the given session using the shared launch script.
fn spawn_wechat(session: &crate::ia::types::Session) {
    // Use DBUS_SESSION_BUS_ADDRESS from our own environment (inherited from
    // entrypoint.sh) rather than the DB value. The entrypoint's D-Bus session
    // is the one AT-SPI is connected to, so WeChat must use it for a11y to work.
    let result = std::process::Command::new("/opt/tools/launch-wechat")
        .env("DISPLAY", &session.display)
        .env("WECHAT_HOME", format!("/home/{}", session.linux_user))
        .env("WECHAT_USER", &session.linux_user)
        .spawn();

    match result {
        Ok(_) => tracing::info!("[health] Spawned WeChat for session '{}'", session.name),
        Err(e) => tracing::error!("[health] Failed to spawn WeChat: {}", e),
    }
}

/// Spawn the background health monitor task.
///
/// Every second, it checks whether the default session's WeChat process exists.
/// The expensive a11y → screenshot → identify observation runs at the
/// configurable UI interval (30 seconds by default). If WeChat has crashed, it
/// restarts it. If no IA state has been identified for longer than the
/// configurable timeout (60 seconds by default), it kills and restarts it.
pub fn spawn_health_monitor() {
    tokio::spawn(async move {
        let ui_scan_interval =
            seconds_from_env(UI_SCAN_INTERVAL_ENV, DEFAULT_UI_SCAN_INTERVAL_SECS);
        let unresponsive_timeout =
            seconds_from_env(UNRESPONSIVE_TIMEOUT_ENV, DEFAULT_UNRESPONSIVE_TIMEOUT_SECS);
        if ui_scan_interval > unresponsive_timeout {
            tracing::warn!(
                "[health] {} {} exceeds {} {}; one failed UI scan will trigger recovery",
                UI_SCAN_INTERVAL_ENV,
                ui_scan_interval.as_secs(),
                UNRESPONSIVE_TIMEOUT_ENV,
                unresponsive_timeout.as_secs(),
            );
        }
        tracing::info!(
            "[health] WeChat health monitor started (process check={}s, ui scan={}s, unresponsive timeout={}s)",
            PROCESS_CHECK_INTERVAL.as_secs(),
            ui_scan_interval.as_secs(),
            unresponsive_timeout.as_secs(),
        );

        let mut last_identified = Instant::now();
        let mut last_ui_scan: Option<Instant> = None;
        let mut was_running = false;
        let mut restart_count: u32 = 0;
        let mut window_start = Instant::now();
        let mut waiting_restart_since: Option<Instant> = None;

        loop {
            tokio::time::sleep(PROCESS_CHECK_INTERVAL).await;

            // Skip if monitoring is paused (an execution loop is active)
            if MONITORING_PAUSED.load(Ordering::Relaxed) {
                last_identified = Instant::now();
                last_ui_scan = None;
                continue;
            }

            // Only monitor the default session
            let session = match get_session("default") {
                Some(s) if s.status == "running" => s,
                _ => {
                    last_identified = Instant::now();
                    last_ui_scan = None;
                    continue;
                }
            };

            // Check if WeChat process is even running
            let wechat_pid = match find_wechat_pid() {
                Some(pid) => {
                    if !was_running {
                        tracing::info!("[health] WeChat process found (pid={})", pid);
                        was_running = true;
                        waiting_restart_since = None;
                    }
                    pid
                }
                None => {
                    if was_running {
                        tracing::warn!(
                            "[health] WeChat process disappeared (likely crashed), restarting"
                        );
                        was_running = false;
                        waiting_restart_since = Some(Instant::now());
                    }

                    // Handle restart with crash loop protection
                    if let Some(since) = waiting_restart_since {
                        // Check crash loop
                        if window_start.elapsed().as_secs() > RAPID_WINDOW_SECS {
                            restart_count = 0;
                            window_start = Instant::now();
                        }

                        let delay = if restart_count >= MAX_RAPID_RESTARTS {
                            if since.elapsed().as_secs() == RESTART_DELAY_SECS {
                                tracing::warn!(
                                    "[health] Crash loop detected ({} restarts in {}s), backing off to {}s",
                                    restart_count, RAPID_WINDOW_SECS, BACKOFF_DELAY_SECS
                                );
                            }
                            BACKOFF_DELAY_SECS
                        } else {
                            RESTART_DELAY_SECS
                        };

                        if since.elapsed().as_secs() >= delay {
                            spawn_wechat(&session);
                            restart_count += 1;
                            waiting_restart_since = None;
                        }
                    }

                    last_identified = Instant::now();
                    last_ui_scan = None;
                    continue;
                }
            };

            let ui_scan_due = last_ui_scan
                .map(|last| last.elapsed() >= ui_scan_interval)
                .unwrap_or(true);
            if !ui_scan_due {
                continue;
            }
            last_ui_scan = Some(Instant::now());

            // Run a11y + identify to see if we can detect any state
            let exec_options = ExecOptions {
                session: Some(session.clone()),
                timeout_ms: 10_000,
            };

            let a11y = match get_a11y_desktop(&exec_options).await {
                Ok(tree) => tree,
                Err(_) => {
                    // a11y failed — count as unresponsive, don't reset timer
                    check_and_kill(wechat_pid, &last_identified, unresponsive_timeout);
                    continue;
                }
            };

            let screenshot = capture_screenshot(&exec_options).await.unwrap_or_default();
            let identified = identify_states(&a11y, &screenshot);

            if identified.main_window.is_some() {
                // State identified — WeChat is responsive
                last_identified = Instant::now();
            } else {
                // No state identified — check timeout
                check_and_kill(wechat_pid, &last_identified, unresponsive_timeout);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_scan_interval_uses_default_when_unset_or_invalid() {
        assert_eq!(parse_positive_seconds(None), None);
        assert_eq!(parse_positive_seconds(Some("")), None);
        assert_eq!(parse_positive_seconds(Some("not-a-number")), None);
        assert_eq!(parse_positive_seconds(Some("0")), None);
    }

    #[test]
    fn ui_scan_interval_accepts_any_positive_integer() {
        assert_eq!(parse_positive_seconds(Some("1")), Some(1));
        assert_eq!(parse_positive_seconds(Some(" 10 ")), Some(10));
        assert_eq!(parse_positive_seconds(Some("30")), Some(30));
        assert_eq!(parse_positive_seconds(Some("300")), Some(300));
    }
}

/// If time since last identified state exceeds the timeout, kill the WeChat process.
fn check_and_kill(wechat_pid: i64, last_identified: &Instant, timeout: Duration) {
    let elapsed = last_identified.elapsed();
    if elapsed >= timeout {
        tracing::warn!(
            "[health] WeChat (pid={}) unresponsive for {}s (threshold={}s), killing process",
            wechat_pid,
            elapsed.as_secs(),
            timeout.as_secs(),
        );

        let result = std::process::Command::new("kill")
            .args(["-9", &wechat_pid.to_string()])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                tracing::info!(
                    "[health] Killed WeChat pid={}, will restart automatically",
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
            timeout.as_secs()
        );
    }
}
