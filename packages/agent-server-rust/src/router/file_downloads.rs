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

const ACTIVE_TIMEOUT: &str = "-10 minutes";
const SUPPORTED_EXTENSIONS: &[&str] = &["pdf", "docx", "xlsx", "zip"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    idempotency_key: String,
    chat_id: String,
    chat_name: String,
    local_id: i64,
    server_id: Option<String>,
    expected_filename: String,
    sent_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    id: String,
    idempotency_key: String,
    chat_id: String,
    chat_name: String,
    local_id: i64,
    server_id: Option<String>,
    expected_filename: String,
    sent_at: Option<String>,
    state: String,
    attempts: i64,
    error_code: Option<String>,
    size_bytes: Option<i64>,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

fn validate(input: &CreateRequest) -> Result<String, &'static str> {
    if input.idempotency_key.is_empty() || input.idempotency_key.len() > 200 {
        return Err("FILE_DOWNLOAD_IDEMPOTENCY_KEY_INVALID");
    }
    if input.chat_id.is_empty()
        || input.chat_id.len() > 200
        || input.chat_name.trim().is_empty()
        || input.chat_name.len() > 200
        || input.chat_name.chars().any(char::is_control)
        || input.local_id < 0
    {
        return Err("FILE_DOWNLOAD_MESSAGE_ID_INVALID");
    }
    let filename = std::path::Path::new(&input.expected_filename)
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or("FILE_DOWNLOAD_FILENAME_INVALID")?;
    if filename != input.expected_filename
        || filename.is_empty()
        || filename.len() > 240
        || filename.chars().any(char::is_control)
    {
        return Err("FILE_DOWNLOAD_FILENAME_INVALID");
    }
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !SUPPORTED_EXTENSIONS.contains(&extension.as_str()) {
        return Err("FILE_TYPE_UNSUPPORTED");
    }
    Ok(extension)
}

fn same_identity(input: &CreateRequest, operation: &Operation) -> bool {
    input.chat_id == operation.chat_id
        && input.chat_name == operation.chat_name
        && input.local_id == operation.local_id
        && input.server_id == operation.server_id
        && input.expected_filename == operation.expected_filename
}

fn load_operation(id: &str) -> Option<Operation> {
    let db = get_db();
    db.query_row(
        "SELECT id, idempotency_key, chat_id, COALESCE(chat_name, ''), local_id, server_id,
                expected_filename, sent_at, state, attempts, error_code,
                size_bytes, created_at, updated_at, completed_at
         FROM file_download_operations WHERE id=?1",
        [id],
        |row| {
            Ok(Operation {
                id: row.get(0)?,
                idempotency_key: row.get(1)?,
                chat_id: row.get(2)?,
                chat_name: row.get(3)?,
                local_id: row.get(4)?,
                server_id: row.get(5)?,
                expected_filename: row.get(6)?,
                sent_at: row.get(7)?,
                state: row.get(8)?,
                attempts: row.get(9)?,
                error_code: row.get(10)?,
                size_bytes: row.get(11)?,
                created_at: row.get(12)?,
                updated_at: row.get(13)?,
                completed_at: row.get(14)?,
            })
        },
    )
    .ok()
}

fn update_state(id: &str, state: &str, error_code: Option<&str>) {
    let db = get_db();
    if let Err(error) = db.execute(
        "UPDATE file_download_operations
         SET state=?2, error_code=?3, updated_at=datetime('now') WHERE id=?1",
        rusqlite::params![id, state, error_code],
    ) {
        tracing::error!("[file-download:{id}] state update failed: {error}");
    }
}

fn finish_ready(id: &str, output_path: Option<&str>, size_bytes: i64) {
    let db = get_db();
    if let Err(error) = db.execute(
        "UPDATE file_download_operations
         SET state='ready', error_code=NULL, output_path=?2, size_bytes=?3,
             completed_at=datetime('now'), updated_at=datetime('now') WHERE id=?1",
        rusqlite::params![id, output_path, size_bytes],
    ) {
        tracing::error!("[file-download:{id}] completion update failed: {error}");
    }
}

fn schedule(id: String, extension: String) {
    tokio::spawn(async move {
        run(id, extension).await;
    });
}

async fn run(id: String, extension: String) {
    let _guard = super::ui_lock::UI_OPERATION_LOCK.lock().await;
    let claimed = {
        let db = get_db();
        db.execute(
            "UPDATE file_download_operations
             SET state='dispatching', attempts=attempts+1, error_code=NULL,
                 updated_at=datetime('now')
             WHERE id=?1 AND (
                 state IN ('queued','failed')
                 OR (state IN ('dispatching','locating','triggering','waiting_cache','validating')
                     AND updated_at <= datetime('now', ?2))
             )",
            rusqlite::params![id, ACTIVE_TIMEOUT],
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
    if cached.media_type == "file" {
        if let Some(data) = cached.data {
            let decoded_len = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map(|bytes| bytes.len() as i64)
                .unwrap_or(0);
            if decoded_len > 0 {
                finish_ready(&id, None, decoded_len);
                return;
            }
        }
    }

    let Some(session) = get_session("default") else {
        update_state(&id, "failed", Some("SOURCE_SESSION_UNAVAILABLE"));
        return;
    };
    if session.logged_in_user.is_none() {
        update_state(&id, "failed", Some("SOURCE_LOGIN_REQUIRED"));
        return;
    }

    update_state(&id, "triggering", None);
    let output_path = format!(
        "/home/{}/Downloads/agent-file-{}.{}",
        session.linux_user, id, extension
    );
    let local_id = operation.local_id.to_string();
    let options = ExecOptions {
        session: Some(session),
        timeout_ms: 300_000,
    };
    let result = exec_command(
        "/opt/tools/file-download",
        &[
            "--chat-id",
            &operation.chat_id,
            "--chat-name",
            &operation.chat_name,
            "--local-id",
            &local_id,
            "--filename",
            &operation.expected_filename,
            "--output",
            &output_path,
        ],
        &options,
    )
    .await;
    if result.exit_code != 0 {
        let code = serde_json::from_str::<serde_json::Value>(&result.stdout)
            .ok()
            .and_then(|value| value.get("errorCode")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "FILE_DOWNLOAD_UI_FAILED".to_string());
        update_state(&id, "failed", Some(&code));
        return;
    }

    update_state(&id, "validating", None);
    match std::fs::metadata(&output_path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {
            finish_ready(&id, Some(&output_path), metadata.len() as i64);
        }
        _ => update_state(&id, "failed", Some("FILE_DOWNLOAD_OUTPUT_INVALID")),
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
            "INSERT INTO file_download_operations(
                 id, idempotency_key, chat_id, chat_name, local_id, server_id,
                 expected_filename, sent_at, state
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'queued')
             ON CONFLICT(idempotency_key) DO UPDATE SET
                 chat_name=excluded.chat_name,
                 state=CASE
                     WHEN file_download_operations.state IN ('ready','completed')
                         THEN file_download_operations.state
                     WHEN file_download_operations.updated_at <= datetime('now', '-10 minutes')
                         THEN 'queued'
                     ELSE file_download_operations.state
                 END,
                 updated_at=CASE
                     WHEN file_download_operations.updated_at <= datetime('now', '-10 minutes')
                         THEN datetime('now')
                     ELSE file_download_operations.updated_at
                 END",
            rusqlite::params![
                id,
                input.idempotency_key,
                input.chat_id,
                input.chat_name,
                input.local_id,
                input.server_id,
                input.expected_filename,
                input.sent_at,
            ],
        ) {
            tracing::error!("file download insert failed: {error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        db.query_row(
            "SELECT id FROM file_download_operations WHERE idempotency_key=?1",
            [&input.idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_default()
    };
    if operation_id.is_empty() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let Some(operation) = load_operation(&operation_id) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if !same_identity(&input, &operation) {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "FILE_DOWNLOAD_IDEMPOTENCY_CONFLICT"})),
        )
            .into_response();
    }
    let extension = std::path::Path::new(&operation.expected_filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    schedule(operation_id.clone(), extension);
    Json(operation).into_response()
}

pub async fn get(Path(id): Path<String>) -> Response {
    match load_operation(&id) {
        Some(operation) => Json(operation).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn content(Path(id): Path<String>) -> Response {
    let (operation, output_path) = {
        let Some(operation) = load_operation(&id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        if operation.state != "ready" {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "FILE_DOWNLOAD_NOT_READY"})),
            )
                .into_response();
        }
        let db = get_db();
        let path = db
            .query_row(
                "SELECT output_path FROM file_download_operations WHERE id=?1",
                [&id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap_or(None);
        (operation, path)
    };

    if let Some(path) = output_path {
        let data = match std::fs::read(&path) {
            Ok(value) if !value.is_empty() => value,
            _ => {
                return (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "FILE_DOWNLOAD_OUTPUT_MISSING"})),
                )
                    .into_response()
            }
        };
        let format = std::path::Path::new(&operation.expected_filename)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        return Json(crate::ia::types::MediaResult {
            media_type: "file".to_string(),
            data: Some(base64::engine::general_purpose::STANDARD.encode(data)),
            url: None,
            format: format.to_string(),
            filename: operation.expected_filename,
        })
        .into_response();
    }

    Json(super::messages::resolve_media(&operation.chat_id, operation.local_id).await)
        .into_response()
}

pub async fn complete(Path(id): Path<String>) -> Response {
    let output_path = {
        let db = get_db();
        db.query_row(
            "SELECT output_path FROM file_download_operations WHERE id=?1 AND state='ready'",
            [&id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    };
    if let Some(path) = output_path {
        let allowed_prefix = format!(
            "/home/{}/Downloads/agent-file-",
            get_session("default")
                .map(|s| s.linux_user)
                .unwrap_or_default()
        );
        if path.starts_with(&allowed_prefix) {
            let _ = std::fs::remove_file(path);
        }
    }
    let updated = {
        let db = get_db();
        db.execute(
            "UPDATE file_download_operations
             SET state='completed', output_path=NULL, updated_at=datetime('now')
             WHERE id=?1 AND state='ready'",
            [&id],
        )
        .unwrap_or(0)
    };
    if updated == 1 {
        Json(serde_json::json!({"id": id, "state": "completed"})).into_response()
    } else {
        StatusCode::CONFLICT.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(filename: &str) -> CreateRequest {
        CreateRequest {
            idempotency_key: "groupbot-media-1".into(),
            chat_id: "123@chatroom".into(),
            chat_name: "测试群".into(),
            local_id: 12,
            server_id: Some("9001".into()),
            expected_filename: filename.into(),
            sent_at: None,
        }
    }

    #[test]
    fn validates_supported_file_names() {
        assert_eq!(validate(&request("资料.zip")), Ok("zip".into()));
        assert_eq!(validate(&request("报告.DOCX")), Ok("docx".into()));
        assert_eq!(
            validate(&request("../资料.zip")),
            Err("FILE_DOWNLOAD_FILENAME_INVALID")
        );
        assert_eq!(validate(&request("程序.exe")), Err("FILE_TYPE_UNSUPPORTED"));
    }

    #[test]
    fn idempotency_identity_includes_message_and_filename() {
        let input = request("资料.zip");
        let operation = Operation {
            id: "operation-1".into(),
            idempotency_key: input.idempotency_key.clone(),
            chat_id: input.chat_id.clone(),
            chat_name: input.chat_name.clone(),
            local_id: input.local_id,
            server_id: input.server_id.clone(),
            expected_filename: input.expected_filename.clone(),
            sent_at: None,
            state: "queued".into(),
            attempts: 0,
            error_code: None,
            size_bytes: None,
            created_at: "2026-09-08 00:00:00".into(),
            updated_at: "2026-09-08 00:00:00".into(),
            completed_at: None,
        };
        assert!(same_identity(&input, &operation));
        assert!(!same_identity(&request("另一个.zip"), &operation));
    }
}
