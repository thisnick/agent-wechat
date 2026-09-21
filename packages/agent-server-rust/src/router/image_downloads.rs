use axum::{
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::get_db;
use crate::sessions::manager::get_session;
use crate::tools::exec::{exec_command, ExecOptions};
use crate::tools::wechat_chats;
use crate::tools::wechat_keys::get_stored_keys;
use crate::tools::wechat_media::get_image_download_cache_target;
use crate::tools::wechat_messages::latest_image_local_id;

const ACTIVE_TIMEOUT: &str = "-10 minutes";
const MAX_ATTEMPTS: i64 = 2;
const RESETTABLE_ERRORS: [&str; 2] = ["IMAGE_NOT_LATEST_MESSAGE", "IMAGE_DOWNLOAD_UI_FAILED"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    idempotency_key: String,
    chat_id: String,
    local_id: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    id: String,
    idempotency_key: String,
    chat_id: String,
    local_id: i64,
    state: String,
    attempts: i64,
    error_code: Option<String>,
    size_bytes: Option<i64>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

fn validate(input: &CreateRequest) -> Result<(), &'static str> {
    if input.idempotency_key.is_empty() || input.idempotency_key.len() > 200 {
        return Err("IMAGE_DOWNLOAD_IDEMPOTENCY_KEY_INVALID");
    }
    if input.chat_id.is_empty()
        || input.chat_id.len() > 200
        || input.chat_id.chars().any(char::is_control)
        || input.local_id < 0
    {
        return Err("IMAGE_DOWNLOAD_MESSAGE_ID_INVALID");
    }
    Ok(())
}

fn same_identity(input: &CreateRequest, operation: &Operation) -> bool {
    input.chat_id == operation.chat_id && input.local_id == operation.local_id
}

fn is_latest_image(local_id: i64, latest_image_local_id: Option<i64>) -> bool {
    latest_image_local_id == Some(local_id)
}

#[cfg(test)]
fn is_resettable_error(error_code: &str) -> bool {
    RESETTABLE_ERRORS.contains(&error_code)
}

fn load_operation(id: &str) -> Option<Operation> {
    let db = get_db();
    db.query_row(
        "SELECT id, idempotency_key, chat_id, local_id, state, attempts,
                error_code, size_bytes, created_at, updated_at, completed_at
         FROM image_download_operations WHERE id=?1",
        [id],
        |row| {
            Ok(Operation {
                id: row.get(0)?,
                idempotency_key: row.get(1)?,
                chat_id: row.get(2)?,
                local_id: row.get(3)?,
                state: row.get(4)?,
                attempts: row.get(5)?,
                error_code: row.get(6)?,
                size_bytes: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                completed_at: row.get(10)?,
            })
        },
    )
    .ok()
}

fn update_state(id: &str, state: &str, error_code: Option<&str>) {
    let db = get_db();
    if let Err(error) = db.execute(
        "UPDATE image_download_operations
         SET state=?2, error_code=?3, updated_at=datetime('now') WHERE id=?1",
        rusqlite::params![id, state, error_code],
    ) {
        tracing::error!("[image-download:{id}] state update failed: {error}");
    }
}

fn finish_ready(id: &str, size_bytes: i64) {
    let db = get_db();
    if let Err(error) = db.execute(
        "UPDATE image_download_operations
         SET state='ready', error_code=NULL, size_bytes=?2,
             completed_at=datetime('now'), updated_at=datetime('now') WHERE id=?1",
        rusqlite::params![id, size_bytes],
    ) {
        tracing::error!("[image-download:{id}] completion update failed: {error}");
    }
}

fn schedule(id: String) {
    tokio::spawn(async move { run(id).await });
}

async fn run(id: String) {
    let _guard = super::ui_lock::UI_OPERATION_LOCK.lock().await;
    let claimed = {
        let db = get_db();
        db.execute(
            "UPDATE image_download_operations
             SET state='dispatching', attempts=attempts+1, error_code=NULL,
                 updated_at=datetime('now')
             WHERE id=?1 AND attempts < ?3 AND (
                 state IN ('queued','failed')
                 OR (state IN ('dispatching','locating','triggering','waiting_cache','validating')
                     AND updated_at <= datetime('now', ?2))
             )",
            rusqlite::params![id, ACTIVE_TIMEOUT, MAX_ATTEMPTS],
        )
        .unwrap_or(0)
    };
    if claimed != 1 {
        return;
    }
    let Some(operation) = load_operation(&id) else {
        return;
    };

    update_state(&id, "locating", None);
    let cached = super::messages::resolve_media(&operation.chat_id, operation.local_id).await;
    if cached.media_type == "image" && !cached.filename.contains("_thumb.") {
        let size = cached
            .data
            .as_deref()
            .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .map(|data| data.len() as i64)
            .unwrap_or(0);
        if size > 0 {
            finish_ready(&id, size);
            return;
        }
    }

    let Some(session) = get_session("default") else {
        update_state(&id, "failed", Some("SOURCE_SESSION_UNAVAILABLE"));
        return;
    };
    let Some(account_dir) = session.logged_in_user.clone() else {
        update_state(&id, "failed", Some("SOURCE_LOGIN_REQUIRED"));
        return;
    };
    let keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &account_dir)
    };
    let Some(chat) = wechat_chats::get_chat_by_username(&account_dir, &keys, &operation.chat_id)
    else {
        update_state(&id, "failed", Some("IMAGE_CHAT_NOT_FOUND"));
        return;
    };
    if !is_latest_image(
        operation.local_id,
        latest_image_local_id(&account_dir, &keys, &operation.chat_id),
    ) {
        update_state(&id, "failed", Some("IMAGE_NOT_LATEST_IMAGE"));
        return;
    }
    let Some(target) = get_image_download_cache_target(
        &account_dir,
        &keys,
        &operation.chat_id,
        operation.local_id,
    ) else {
        update_state(&id, "failed", Some("IMAGE_CACHE_IDENTITY_UNAVAILABLE"));
        return;
    };
    let Some(thumbnail_data) = cached.data.as_deref() else {
        update_state(&id, "failed", Some("IMAGE_THUMBNAIL_UNAVAILABLE"));
        return;
    };
    let Ok(thumbnail_bytes) = base64::engine::general_purpose::STANDARD.decode(thumbnail_data)
    else {
        update_state(&id, "failed", Some("IMAGE_THUMBNAIL_INVALID"));
        return;
    };
    let thumbnail_path = format!("/tmp/agent-image-{id}.jpg");
    if std::fs::write(&thumbnail_path, thumbnail_bytes).is_err() {
        update_state(&id, "failed", Some("IMAGE_THUMBNAIL_WRITE_FAILED"));
        return;
    }

    update_state(&id, "triggering", None);
    let local_id = operation.local_id.to_string();
    let image_dir = target.image_dir.to_string_lossy().to_string();
    let options = ExecOptions {
        session: Some(session),
        timeout_ms: 300_000,
    };
    let result = exec_command(
        "/opt/tools/image-download",
        &[
            "--chat-id",
            &operation.chat_id,
            "--chat-name",
            &chat.name,
            "--local-id",
            &local_id,
            "--thumbnail",
            &thumbnail_path,
            "--image-dir",
            &image_dir,
            "--stem",
            &target.stem,
        ],
        &options,
    )
    .await;
    let _ = std::fs::remove_file(&thumbnail_path);
    if result.exit_code != 0 {
        let code = serde_json::from_str::<serde_json::Value>(&result.stdout)
            .ok()
            .and_then(|value| value.get("errorCode")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "IMAGE_DOWNLOAD_UI_FAILED".to_string());
        update_state(&id, "failed", Some(&code));
        return;
    }

    update_state(&id, "validating", None);
    let media = super::messages::resolve_media(&operation.chat_id, operation.local_id).await;
    let size = if media.media_type == "image" && !media.filename.contains("_thumb.") {
        media
            .data
            .as_deref()
            .and_then(|data| base64::engine::general_purpose::STANDARD.decode(data).ok())
            .map(|data| data.len() as i64)
            .unwrap_or(0)
    } else {
        0
    };
    if size > 0 {
        finish_ready(&id, size);
    } else {
        update_state(&id, "failed", Some("IMAGE_DOWNLOAD_OUTPUT_INVALID"));
    }
}

pub async fn create(Json(input): Json<CreateRequest>) -> Response {
    if let Err(code) = validate(&input) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": code})),
        )
            .into_response();
    }
    let id = Uuid::new_v4().to_string();
    let operation_id = {
        let db = get_db();
        if let Err(error) = db.execute(
            "INSERT INTO image_download_operations(id, idempotency_key, chat_id, local_id, state)
             VALUES (?1,?2,?3,?4,'queued')
             ON CONFLICT(idempotency_key) DO UPDATE SET
                 state=CASE
                     WHEN image_download_operations.state='ready' THEN 'ready'
                     WHEN image_download_operations.state='failed'
                          AND image_download_operations.error_code IN (?6, ?7)
                         THEN 'queued'
                     WHEN image_download_operations.state='failed'
                          AND image_download_operations.attempts < ?5 THEN 'queued'
                     WHEN image_download_operations.updated_at <= datetime('now', '-10 minutes')
                          AND image_download_operations.attempts < ?5 THEN 'queued'
                     ELSE image_download_operations.state
                 END,
                 attempts=CASE
                     WHEN image_download_operations.state='failed'
                          AND image_download_operations.error_code IN (?6, ?7)
                         THEN 0
                     ELSE image_download_operations.attempts
                 END,
                 error_code=CASE
                     WHEN image_download_operations.state='failed'
                          AND image_download_operations.error_code IN (?6, ?7)
                         THEN NULL
                     ELSE image_download_operations.error_code
                 END,
                 updated_at=datetime('now')",
            rusqlite::params![
                id,
                input.idempotency_key,
                input.chat_id,
                input.local_id,
                MAX_ATTEMPTS,
                RESETTABLE_ERRORS[0],
                RESETTABLE_ERRORS[1]
            ],
        ) {
            tracing::error!("image download insert failed: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        db.query_row(
            "SELECT id FROM image_download_operations WHERE idempotency_key=?1",
            [&input.idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default()
    };
    let Some(operation) = load_operation(&operation_id) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if !same_identity(&input, &operation) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "IMAGE_DOWNLOAD_IDEMPOTENCY_CONFLICT"})),
        )
            .into_response();
    }
    schedule(operation_id);
    Json(operation).into_response()
}

pub async fn get(Path(id): Path<String>) -> Response {
    match load_operation(&id) {
        Some(operation) => Json(operation).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CreateRequest {
        CreateRequest {
            idempotency_key: "groupbot-image-1".into(),
            chat_id: "123@chatroom".into(),
            local_id: 42,
        }
    }

    #[test]
    fn validates_message_identity() {
        assert_eq!(validate(&request()), Ok(()));
        let mut invalid = request();
        invalid.chat_id = "bad\nchat".into();
        assert_eq!(validate(&invalid), Err("IMAGE_DOWNLOAD_MESSAGE_ID_INVALID"));
    }

    #[test]
    fn allows_latest_image_when_a_newer_non_image_message_exists() {
        assert!(is_latest_image(42, Some(42)));
    }

    #[test]
    fn rejects_an_older_image() {
        assert!(!is_latest_image(41, Some(42)));
        assert!(!is_latest_image(41, None));
    }

    #[test]
    fn resets_operations_failed_by_fixed_runtime_errors() {
        assert!(is_resettable_error("IMAGE_NOT_LATEST_MESSAGE"));
        assert!(is_resettable_error("IMAGE_DOWNLOAD_UI_FAILED"));
        assert!(!is_resettable_error("IMAGE_CARD_NOT_FOUND"));
    }
}
