use crate::db::get_db;
use crate::ia::types::Session;
use rusqlite::params;

/// Convert a DB row to a Session.
fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get("id")?,
        name: row.get("name")?,
        linux_user: row.get("linux_user")?,
        display: row.get("display")?,
        dbus_address: row.get("dbus_address")?,
        vnc_port: row.get("vnc_port")?,
        status: row.get("status")?,
        login_state: row.get("login_state")?,
        logged_in_user: row.get("logged_in_user")?,
        wechat_pid: row.get("wechat_pid")?,
        xvfb_pid: row.get("xvfb_pid")?,
        dbus_pid: row.get("dbus_pid")?,
        error_message: row.get("error_message")?,
        created_at: row.get::<_, Option<String>>("created_at")?
            .unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>("updated_at")?
            .unwrap_or_default(),
    })
}

/// Get a session by ID or name.
pub fn get_session(id_or_name: &str) -> Option<Session> {
    let db = get_db();
    db.query_row(
        "SELECT * FROM sessions WHERE id = ?1 OR name = ?1",
        params![id_or_name],
        row_to_session,
    )
    .ok()
}

/// List all sessions.
pub fn list_sessions() -> Vec<Session> {
    let db = get_db();
    let mut stmt = db
        .prepare("SELECT * FROM sessions ORDER BY created_at")
        .unwrap();
    let rows = stmt.query_map([], row_to_session).unwrap();
    rows.flatten().collect()
}

/// Create a new session.
pub async fn create_session(name: &str) -> Result<Session, String> {
    let id = {
        let db = get_db();

        // Check if name already exists
        let exists: bool = db
            .query_row(
                "SELECT COUNT(*) > 0 FROM sessions WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if exists {
            return Err(format!("Session with name \"{name}\" already exists"));
        }

        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        // Get next display number
        let max_display: Option<i32> = db
            .query_row(
                "SELECT MAX(CAST(SUBSTR(display, 2) AS INTEGER)) FROM sessions",
                [],
                |row| row.get(0),
            )
            .ok();
        let display_num = max_display.unwrap_or(99) + 1;
        let display = format!(":{display_num}");
        let linux_user = format!("wechat-{display_num}");

        // Get next VNC port
        let max_port: Option<i32> = db
            .query_row(
                "SELECT MAX(vnc_port) FROM sessions",
                [],
                |row| row.get(0),
            )
            .ok();
        let vnc_port = max_port.unwrap_or(5900) + 1;

        // Create Linux user
        let _ = std::process::Command::new("useradd")
            .args(["-m", "-s", "/bin/bash", &linux_user])
            .output();

        db.execute(
            "INSERT INTO sessions (id, name, linux_user, display, vnc_port, status, login_state, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'stopped', 'logged_out', ?6, ?6)",
            params![id, name, linux_user, display, vnc_port, now],
        )
        .map_err(|e| format!("Failed to create session: {e}"))?;

        id
    };

    get_session(&id).ok_or_else(|| "Failed to read created session".to_string())
}

/// Get or create default session.
pub async fn get_or_create_default_session() -> Result<Session, String> {
    if let Some(session) = get_session("default") {
        if session.display == ":99" {
            return Ok(session);
        }
        // Wrong display — delete and recreate
        let db = get_db();
        db.execute("DELETE FROM sessions WHERE name = 'default'", [])
            .ok();
    }

    // Create default session matching entrypoint.sh setup
    let id = {
        let db = get_db();
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let dbus_address = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();

        db.execute(
            "INSERT INTO sessions (id, name, linux_user, display, dbus_address, vnc_port, status, login_state, created_at, updated_at)
             VALUES (?1, 'default', 'wechat', ':99', ?2, 5900, 'running', 'logged_out', ?3, ?3)",
            params![id, dbus_address, now],
        )
        .map_err(|e| format!("Failed to create default session: {e}"))?;

        id
    };

    get_session(&id).ok_or_else(|| "Failed to read default session".to_string())
}

/// Start a session (launches Xvfb, D-Bus, AT-SPI, WeChat).
pub async fn start_session(id_or_name: &str) -> Result<Session, String> {
    let session = get_session(id_or_name)
        .ok_or_else(|| format!("Session not found: {id_or_name}"))?;

    if session.status == "running" {
        return Ok(session);
    }

    // Update status to 'starting' — scoped so MutexGuard drops before await
    {
        let db = get_db();
        let now = chrono::Utc::now().to_rfc3339();
        db.execute(
            "UPDATE sessions SET status = 'starting', updated_at = ?1 WHERE id = ?2",
            params![now, session.id],
        )
        .ok();
    }

    let display = &session.display;
    let linux_user = &session.linux_user;
    let home_dir = format!("/home/{linux_user}");

    // 1. Start Xvfb
    let display_num = display.trim_start_matches(':');
    let _ = std::fs::remove_file(format!("/tmp/.X{display_num}-lock"));
    let _ = std::process::Command::new("Xvfb")
        .args([display.as_str(), "-screen", "0", "1280x800x24"])
        .spawn();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 2. D-Bus
    let dbus_output = std::process::Command::new("su")
        .args(["-s", "/bin/bash", "-c", "dbus-launch --sh-syntax", linux_user.as_str()])
        .output();

    let dbus_address = dbus_output
        .ok()
        .and_then(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let re = regex::Regex::new(r"DBUS_SESSION_BUS_ADDRESS='([^']+)'").ok()?;
            re.captures(&stdout).map(|c| c[1].to_string())
        })
        .unwrap_or_default();

    // 3. Fluxbox
    let _ = std::process::Command::new("su")
        .args(["-s", "/bin/bash", "-c",
            &format!("DISPLAY={display} DBUS_SESSION_BUS_ADDRESS={dbus_address} HOME={home_dir} fluxbox &"),
            linux_user.as_str()])
        .spawn();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 4. AT-SPI
    let _ = std::process::Command::new("su")
        .args(["-s", "/bin/bash", "-c",
            &format!("DISPLAY={display} DBUS_SESSION_BUS_ADDRESS={dbus_address} HOME={home_dir} /usr/libexec/at-spi-bus-launcher &"),
            linux_user.as_str()])
        .spawn();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // 5. VNC (localhost only — auth enforced by agent-server proxy)
    let vnc_port = session.vnc_port.to_string();
    let _ = std::process::Command::new("x11vnc")
        .args(["-display", display.as_str(), "-forever", "-nopw", "-shared", "-viewonly", "-xkb", "-rfbport", &vnc_port, "-listen", "127.0.0.1"])
        .spawn();

    // 5b. noVNC (websockify on localhost only — proxied via agent-server with auth)
    if std::path::Path::new("/opt/novnc").exists() {
        let websockify_port = session.vnc_port + 180; // VNC 5900 -> websockify 6080
        let novnc_bind = format!("127.0.0.1:{}", websockify_port);
        let vnc_target = format!("localhost:{}", session.vnc_port);
        let _ = std::process::Command::new("websockify")
            .args(["--web", "/opt/novnc", &novnc_bind, &vnc_target])
            .spawn();
    }

    // 6. WeChat
    let _ = std::process::Command::new("su")
        .args(["-s", "/bin/bash", "-c",
            &format!(
                "DISPLAY={display} DBUS_SESSION_BUS_ADDRESS={dbus_address} QT_ACCESSIBILITY=1 QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1 GTK_MODULES=gail:atk-bridge HOME={home_dir} /usr/bin/wechat &"
            ),
            linux_user.as_str()])
        .spawn();

    // Update status to 'running' — scoped so MutexGuard drops
    {
        let db = get_db();
        let now = chrono::Utc::now().to_rfc3339();
        db.execute(
            "UPDATE sessions SET status = 'running', dbus_address = ?1, updated_at = ?2 WHERE id = ?3",
            params![dbus_address, now, session.id],
        )
        .ok();
    }

    Ok(get_session(&session.id).unwrap())
}

/// Stop a session.
pub async fn stop_session(id_or_name: &str) -> Result<Session, String> {
    let session = get_session(id_or_name)
        .ok_or_else(|| format!("Session not found: {id_or_name}"))?;

    if session.status == "stopped" {
        return Ok(session);
    }

    // Kill processes
    let _ = std::process::Command::new("pkill")
        .args(["-u", &session.linux_user])
        .output();
    let _ = std::process::Command::new("pkill")
        .args(["-f", &format!("Xvfb {}", session.display)])
        .output();

    let display_num = session.display.trim_start_matches(':');
    let _ = std::fs::remove_file(format!("/tmp/.X{display_num}-lock"));

    {
        let db = get_db();
        let now = chrono::Utc::now().to_rfc3339();
        db.execute(
            "UPDATE sessions SET status = 'stopped', dbus_address = NULL, xvfb_pid = NULL, wechat_pid = NULL, dbus_pid = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, session.id],
        ).ok();
    }

    Ok(get_session(&session.id).unwrap())
}

/// Delete a session.
pub async fn delete_session(id_or_name: &str) -> Result<(), String> {
    let session = get_session(id_or_name)
        .ok_or_else(|| format!("Session not found: {id_or_name}"))?;

    if session.status == "running" || session.status == "starting" {
        stop_session(&session.id).await?;
    }

    {
        let db = get_db();
        db.execute("DELETE FROM sync_state WHERE session_id = ?1", params![session.id]).ok();
        db.execute("DELETE FROM wechat_keys WHERE session_id = ?1", params![session.id]).ok();
        db.execute("DELETE FROM context WHERE session_id = ?1", params![session.id]).ok();
        db.execute("DELETE FROM sessions WHERE id = ?1", params![session.id]).ok();
    }

    let _ = std::process::Command::new("userdel")
        .args(["-r", &session.linux_user])
        .output();

    Ok(())
}

/// Initialize sessions on startup.
pub async fn initialize_sessions() -> Result<(), String> {
    // Ensure default session exists
    get_or_create_default_session().await?;
    tracing::info!("[SessionManager] Default session ready");
    Ok(())
}
