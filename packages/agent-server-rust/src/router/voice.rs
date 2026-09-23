use std::path::{Path as FilePath, PathBuf};

use axum::{
    extract::{Multipart, Path},
    http::{HeaderMap, StatusCode},
    Json,
};
use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::get_db;
use crate::execution::acquire_gui_guard;
use crate::ia::selectors::query_selector;
use crate::sessions::manager::get_session;
use crate::tools::a11y::get_a11y_desktop;
use crate::tools::chat_select::open_chat;
use crate::tools::exec::ExecOptions;

const JOBS_DIR: &str = "/data/voice-jobs";
const WORKER: &str = "/opt/tools/voice-send-worker";

fn response(status: StatusCode, body: Value) -> (StatusCode, Json<Value>) {
    (status, Json(body))
}

fn job_dir(job_id: &str) -> Result<PathBuf, String> {
    Uuid::parse_str(job_id).map_err(|_| "Invalid voice job ID".to_string())?;
    Ok(FilePath::new(JOBS_DIR).join(job_id))
}

fn read_status(job_id: &str) -> Result<Value, String> {
    let path = job_dir(job_id)?.join("status.json");
    let bytes = std::fs::read(path).map_err(|_| "Voice job was not found".to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| format!("Voice job status is unavailable: {e}"))
}

fn current_account_owns(status: &Value) -> bool {
    get_session("default")
        .and_then(|session| session.logged_in_user)
        .as_deref()
        == status["accountId"].as_str()
}

fn write_status(job_id: &str, status: &Value) -> Result<(), String> {
    let dir = job_dir(job_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let temporary = dir.join("status.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(status).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(temporary, dir.join("status.json")).map_err(|e| e.to_string())
}

pub async fn create_job(headers: HeaderMap, mut multipart: Multipart) -> (StatusCode, Json<Value>) {
    let Some(account) = get_session("default").and_then(|s| s.logged_in_user) else {
        return response(StatusCode::CONFLICT, json!({"error": "NOT_LOGGED_IN"}));
    };
    let key = headers
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key.is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"error": "A valid Idempotency-Key is required"}),
        );
    }

    let mut chat_id: Option<String> = None;
    let mut audio: Option<Vec<u8>> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => return response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()})),
        };
        match field.name().unwrap_or("") {
            "chatId" if chat_id.is_none() => match field.text().await {
                Ok(value) => chat_id = Some(value),
                Err(e) => {
                    return response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()}))
                }
            },
            "audio" if audio.is_none() => match field.bytes().await {
                Ok(value) if !value.is_empty() && value.len() <= super::MAX_UPLOAD_BYTES => {
                    audio = Some(value.to_vec())
                }
                Ok(_) => {
                    return response(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        json!({"error": "Audio is empty or exceeds 128 MiB"}),
                    )
                }
                Err(e) => {
                    return response(StatusCode::BAD_REQUEST, json!({"error": e.to_string()}))
                }
            },
            _ => {
                return response(
                    StatusCode::BAD_REQUEST,
                    json!({"error": "Unexpected or duplicate form field"}),
                )
            }
        }
    }
    let Some(chat_id) =
        chat_id.filter(|id| !id.is_empty() && id.len() <= 255 && !id.starts_with("gh_"))
    else {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"error": "Invalid destination chat"}),
        );
    };
    let Some(audio) = audio else {
        return response(
            StatusCode::BAD_REQUEST,
            json!({"error": "Audio file is required"}),
        );
    };
    let mut hasher = Sha256::new();
    hasher.update(chat_id.as_bytes());
    hasher.update([0]);
    hasher.update(&audio);
    let request_hash = format!("{:x}", hasher.finalize());

    let job_id = Uuid::new_v4().to_string();
    let existing = {
        let db = get_db();
        db.query_row(
            "SELECT job_id, request_hash FROM voice_jobs WHERE account_id=?1 AND idempotency_key=?2",
            rusqlite::params![account, key],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()
    };
    match existing {
        Ok(Some((id, hash))) => {
            if hash != request_hash {
                return response(
                    StatusCode::CONFLICT,
                    json!({"error": "Idempotency-Key was used with different audio or destination"}),
                );
            }
            return match read_status(&id) {
                Ok(status) => response(StatusCode::OK, status),
                Err(error) => response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": error})),
            };
        }
        Err(e) => {
            return response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            )
        }
        Ok(None) => {}
    }
    let dir = match job_dir(&job_id) {
        Ok(dir) => dir,
        Err(error) => return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": error})),
    };
    if let Err(e) =
        std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(dir.join("input"), audio))
    {
        return response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": e.to_string()}),
        );
    }
    let status = json!({"jobId": job_id, "chatId": chat_id, "accountId": account,
                        "status": "queued", "sourceDurationSeconds": null, "chunks": [], "error": null});
    if let Err(error) = write_status(&job_id, &status) {
        let _ = std::fs::remove_dir_all(&dir);
        return response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": error}));
    }
    let inserted = {
        let db = get_db();
        db.execute("INSERT OR IGNORE INTO voice_jobs (job_id, account_id, idempotency_key, request_hash) VALUES (?1, ?2, ?3, ?4)",
                   rusqlite::params![job_id, account, key, request_hash])
    };
    match inserted {
        Ok(1) => {
            tokio::spawn(run_job(job_id.clone(), chat_id, account));
            response(StatusCode::ACCEPTED, status)
        }
        Ok(_) => {
            let _ = std::fs::remove_dir_all(&dir);
            let db = get_db();
            let result = db.query_row(
                "SELECT job_id, request_hash FROM voice_jobs WHERE account_id=?1 AND idempotency_key=?2",
                rusqlite::params![account, key],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            );
            match result {
                Ok((id, hash)) if hash == request_hash => response(
                    StatusCode::OK,
                    read_status(&id).unwrap_or(json!({"error": "Voice job status unavailable"})),
                ),
                Ok(_) => response(
                    StatusCode::CONFLICT,
                    json!({"error": "Idempotency-Key was used with different audio or destination"}),
                ),
                Err(e) => response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({"error": e.to_string()}),
                ),
            }
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": e.to_string()}),
            )
        }
    }
}

async fn run_worker(mode: &str, dir: &FilePath) -> Result<(), String> {
    let output = tokio::process::Command::new("python3")
        .arg(WORKER)
        .arg(mode)
        .arg(dir)
        .env("AGENT_SERVER_PID", std::process::id().to_string())
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
}

async fn run_job(job_id: String, chat_id: String, account: String) {
    let result = run_job_inner(&job_id, &chat_id, &account).await;
    if let Err(error) = result {
        tracing::error!(job_id, "Voice job stopped: {error}");
        if let Ok(mut status) = read_status(&job_id) {
            let current = status["status"].as_str().unwrap_or("");
            if !["completed", "failed", "needs_review", "cancelled"].contains(&current) {
                let uncertain = status["chunks"].as_array().is_some_and(|chunks| {
                    chunks.iter().any(|part| {
                        matches!(
                            part["status"].as_str(),
                            Some("send_clicked" | "unknown" | "verified")
                        )
                    })
                });
                status["status"] = json!(if uncertain {
                    "needs_review"
                } else if job_dir(&job_id).is_ok_and(|d| d.join("cancel").exists()) {
                    "cancelled"
                } else {
                    "failed"
                });
                status["error"] = json!(error);
                let _ = write_status(&job_id, &status);
            }
        }
    }
}

async fn run_job_inner(job_id: &str, chat_id: &str, account: &str) -> Result<(), String> {
    let dir = job_dir(job_id)?;
    run_worker("prepare", &dir).await?;
    let _gui = acquire_gui_guard().await;
    if dir.join("cancel").exists() {
        return Err("Voice job cancelled before recording".into());
    }
    if get_session("default")
        .and_then(|s| s.logged_in_user)
        .as_deref()
        != Some(account)
    {
        return Err("The logged-in account changed before recording".into());
    }
    let tree = get_a11y_desktop(&ExecOptions::default()).await?;
    let first = query_selector(&tree, r#"list[name="Chats"] > list-item"#)
        .and_then(|item| item.bounds.as_ref())
        .ok_or("Chat list click target is unavailable")?;
    let center = (
        (first.x + first.width / 2.0).round(),
        (first.y + first.height / 2.0).round(),
    );
    let opened = open_chat(chat_id, true, Some(center)).await;
    if !opened.ok {
        return Err(opened.error.unwrap_or("Chat selection failed".into()));
    }
    if opened.username.as_deref() != Some(chat_id) {
        return Err("Chat selection returned a different destination".into());
    }
    run_worker("send", &dir).await?;
    if let Ok(status) = read_status(job_id) {
        cleanup_completed_files(&dir, &status);
    }
    Ok(())
}

fn cleanup_completed_files(dir: &FilePath, status: &Value) {
    if status["status"] != "completed" {
        return;
    }
    let _ = std::fs::remove_file(dir.join("input"));
    let _ = std::fs::remove_file(dir.join("source.wav"));
    if let Some(chunks) = status["chunks"].as_array() {
        for chunk in chunks {
            if let Some(name) = chunk["path"].as_str() {
                if name.starts_with("chunk-")
                    && name.ends_with(".wav")
                    && FilePath::new(name)
                        .file_name()
                        .is_some_and(|base| base == name)
                {
                    let _ = std::fs::remove_file(dir.join(name));
                }
            }
        }
    }
}

pub async fn get_job(Path(job_id): Path<String>) -> (StatusCode, Json<Value>) {
    match read_status(&job_id) {
        Ok(status) if current_account_owns(&status) => response(StatusCode::OK, status),
        Ok(_) => response(
            StatusCode::NOT_FOUND,
            json!({"error": "Voice job was not found"}),
        ),
        Err(error) => response(StatusCode::NOT_FOUND, json!({"error": error})),
    }
}

pub async fn cancel_job(Path(job_id): Path<String>) -> (StatusCode, Json<Value>) {
    let status = match read_status(&job_id) {
        Ok(status) if current_account_owns(&status) => status,
        Err(error) => return response(StatusCode::NOT_FOUND, json!({"error": error})),
        Ok(_) => {
            return response(
                StatusCode::NOT_FOUND,
                json!({"error": "Voice job was not found"}),
            )
        }
    };
    if ["completed", "failed", "needs_review", "cancelled"]
        .contains(&status["status"].as_str().unwrap_or(""))
    {
        return response(
            StatusCode::CONFLICT,
            json!({"error": "Voice job is already terminal", "job": status}),
        );
    }
    if let Err(e) = std::fs::write(job_dir(&job_id).unwrap().join("cancel"), b"") {
        return response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": e.to_string()}),
        );
    }
    response(StatusCode::ACCEPTED, status)
}

pub fn recover_interrupted_jobs() {
    let Ok(entries) = std::fs::read_dir(JOBS_DIR) else {
        return;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let Ok(mut status) = read_status(&id) else {
            continue;
        };
        let current = status["status"].as_str().unwrap_or("");
        if current == "completed" {
            if let Ok(dir) = job_dir(&id) {
                cleanup_completed_files(&dir, &status);
            }
            continue;
        }
        if ["failed", "needs_review", "cancelled"].contains(&current) {
            continue;
        }
        let uncertain = status["chunks"].as_array().is_some_and(|chunks| {
            chunks.iter().any(|part| {
                matches!(
                    part["status"].as_str(),
                    Some("send_clicked" | "unknown" | "verified")
                )
            })
        });
        status["status"] = json!(if uncertain { "needs_review" } else { "failed" });
        status["error"] = json!("Server restarted before the voice job completed");
        let _ = write_status(&id, &status);
    }
}
