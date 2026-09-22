use axum::{
    extract::{Path, Query},
    Json,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::context::create_context;
use crate::db::get_db;
use crate::execution::run_execution_loop;
use crate::ia::types::{MediaResult, Message, SendResult, SubscriptionEvent};
use crate::plans::send_message::{SendMessageParams, SendMessagePlan};
use crate::tools::wechat_db::{find_wechat_pid, list_account_dbs};
use crate::tools::wechat_keys::{extract_keys_async, get_stored_keys, get_image_keys, store_keys};
use crate::tools::wechat_media::{get_message_media, download_metadata, pending, ImageQuality};
use crate::tools::media_download::{ensure_queued, current_process};
use crate::tools::wechat_messages;
use crate::sessions::manager::get_session;

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    50
}

pub async fn list_messages(
    Path(chat_id): Path<String>,
    Query(params): Query<ListParams>,
) -> Json<Vec<Message>> {
    let session = match get_session("default") {
        Some(s) => s,
        None => return Json(Vec::new()),
    };
    let logged_in_user = match &session.logged_in_user {
        Some(u) => u.clone(),
        None => return Json(Vec::new()),
    };

    let mut keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &logged_in_user)
    };

    // Lazy key extraction: if message_*.db files exist on disk without stored keys, re-extract
    let on_disk = list_account_dbs(&logged_in_user);
    let has_missing_message_db = on_disk.iter().any(|name| {
        name.starts_with("message_")
            && name.ends_with(".db")
            && !name.contains("fts")
            && !name.contains("resource")
            && !keys.contains_key(name.as_str())
    });
    if has_missing_message_db {
        if let Some(pid) = find_wechat_pid() {
            let extracted = extract_keys_async(pid).await;
            if !extracted.is_empty() {
                let db = get_db();
                store_keys(&db, &session.id, &logged_in_user, &extracted);
                keys = get_stored_keys(&db, &session.id, &logged_in_user);
            }
        }
    }

    if !keys.keys().any(|k| k.starts_with("message_") && k.ends_with(".db") && !k.contains("fts") && !k.contains("resource")) {
        return Json(Vec::new());
    }

    Json(wechat_messages::list_messages(
        &logged_in_user,
        &keys,
        &chat_id,
        params.limit,
        params.offset,
    ))
}

#[derive(Deserialize, Default)]
pub struct MediaParams {
    #[serde(default)]
    quality: ImageQuality,
}

pub async fn get_media(
    Path((chat_id, local_id)): Path<(String, i64)>,
    Query(params): Query<MediaParams>,
) -> Json<MediaResult> {
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            return Json(MediaResult {
                media_type: "unsupported".to_string(),
                data: None,
                url: None,
                format: String::new(),
                filename: String::new(),
            })
        }
    };
    let logged_in_user = match &session.logged_in_user {
        Some(u) => u.clone(),
        None => {
            return Json(MediaResult {
                media_type: "unsupported".to_string(),
                data: None,
                url: None,
                format: String::new(),
                filename: String::new(),
            })
        }
    };

    let mut keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &logged_in_user)
    };

    // Lazy key extraction: if media_*.db files exist on disk without stored keys, extract them
    let on_disk = list_account_dbs(&logged_in_user);
    let has_missing_media = on_disk.iter().any(|name| {
        name.starts_with("media_") && name.ends_with(".db") && !keys.contains_key(name.as_str())
    });
    if has_missing_media {
        if let Some(pid) = find_wechat_pid() {
            let extracted = extract_keys_async(pid).await;
            if !extracted.is_empty() {
                let db = get_db();
                store_keys(&db, &session.id, &logged_in_user, &extracted);
                keys = get_stored_keys(&db, &session.id, &logged_in_user);
            }
        }
    }

    let image_keys = {
        let db = get_db();
        get_image_keys(&db, &session.id, &logged_in_user)
    };

    // All cache checks and native metadata reads happen outside the async runtime.
    let account = logged_in_user.clone();
    let request_keys = keys.clone();
    let request_chat = chat_id.clone();
    let request_image_keys = image_keys.clone();
    let result = tokio::task::spawn_blocking(move || {
        get_message_media(
            &logged_in_user,
            &keys,
            &chat_id,
            local_id,
            image_keys,
            params.quality,
        )
    })
    .await
    .unwrap_or_else(|_| MediaResult {
        media_type: "pending".into(),
        data: None,
        url: None,
        format: String::new(),
        filename: String::new(),
    });
    if result.data.is_some() || result.media_type == "unsupported" { return Json(result); }
    let metadata_account = account.clone();
    let metadata_keys = request_keys.clone();
    let metadata_chat = request_chat.clone();
    let metadata = tokio::task::spawn_blocking(move ||
        download_metadata(&metadata_account, &metadata_keys, &metadata_chat, local_id)
    ).await.ok().flatten();
    let Some(metadata) = metadata else { return Json(result); };
    let Some((_, identity)) = current_process(&account) else { return Json(pending()); };
    if !ensure_queued(account.clone(), metadata).await { return Json(pending()); }
    // A bounded HTTP wait; further GETs reuse the native submission for five minutes.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if current_process(&account).map(|(_, key)| key).as_ref() != Some(&identity) { return Json(pending()); }
        let a = account.clone(); let k = request_keys.clone(); let c = request_chat.clone();
        let i = request_image_keys.clone();
        let found = tokio::task::spawn_blocking(move || get_message_media(&a, &k, &c, local_id, i, params.quality))
            .await.unwrap_or_else(|_| pending());
        if found.data.is_some() {
            if current_process(&account).map(|(_, key)| key).as_ref() != Some(&identity) { return Json(pending()); }
            return Json(found);
        }
    }
    Json(pending())
}

#[derive(Deserialize)]
pub struct SendParams {
    #[serde(rename = "chatId")]
    chat_id: String,
    text: Option<String>,
    image: Option<ImageInput>,
    file: Option<FileInput>,
}

#[derive(Deserialize)]
pub struct ImageInput {
    data: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Deserialize)]
pub struct FileInput {
    data: String,
    filename: String,
}

pub async fn send_message(Json(input): Json<SendParams>) -> Json<SendResult> {
    if input.text.is_none() && input.image.is_none() && input.file.is_none() {
        return Json(SendResult {
            success: false,
            error: Some("No text, image, or file provided".to_string()),
        });
    }

    let session = match get_session("default") {
        Some(s) => s,
        None => {
            return Json(SendResult {
                success: false,
                error: Some("No session available".to_string()),
            })
        }
    };

    if session.logged_in_user.is_none() {
        return Json(SendResult {
            success: false,
            error: Some("NOT_LOGGED_IN".to_string()),
        });
    }

    // Decode base64 image to temp file
    let mut image_path: Option<String> = None;
    let mut image_mime: Option<String> = None;
    if let Some(ref img) = input.image {
        let ext = match img.mime_type.as_str() {
            "image/jpeg" => ".jpg",
            "image/gif" => ".gif",
            _ => ".png",
        };
        let path = format!("/tmp/send_image_{}{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(), ext);
        if img.data.len() > super::MAX_UPLOAD_BASE64_BYTES {
            return Json(SendResult {
                success: false,
                error: Some("Image exceeds the 128 MiB upload limit".to_string()),
            });
        }
        if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &img.data) {
            if bytes.len() > super::MAX_UPLOAD_BYTES {
                return Json(SendResult {
                    success: false,
                    error: Some("Image exceeds the 128 MiB upload limit".to_string()),
                });
            }
            if std::fs::write(&path, &bytes).is_ok() {
                image_mime = Some(img.mime_type.clone());
                image_path = Some(path);
            }
        }
    }

    // Decode base64 file to temp file
    let mut file_path: Option<String> = None;
    if let Some(ref f) = input.file {
        // Sanitize filename: keep ASCII alphanumerics, dot, hyphen, underscore;
        // replace everything else (including CJK) with underscore so the temp
        // path stays portable across locales.  The dot is preserved so that
        // file extensions survive (e.g. "遗憾.pdf" → "__.pdf"); the mangled
        // stem is acceptable since this is a transient temp path.
        let safe_name: String = f.filename.chars().map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        }).collect();
        let path = format!("/tmp/send_file_{}_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis(), safe_name);
        if f.data.len() > super::MAX_UPLOAD_BASE64_BYTES {
            return Json(SendResult {
                success: false,
                error: Some("File exceeds the 128 MiB upload limit".to_string()),
            });
        }
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &f.data) {
            Ok(bytes) => match std::fs::write(&path, &bytes) {
                Ok(_) if bytes.len() <= super::MAX_UPLOAD_BYTES => { file_path = Some(path); }
                Ok(_) => {
                    let _ = std::fs::remove_file(&path);
                    return Json(SendResult {
                        success: false,
                        error: Some("File exceeds the 128 MiB upload limit".to_string()),
                    });
                }
                Err(e) => {
                    return Json(SendResult {
                        success: false,
                        error: Some(format!("Failed to write temp file: {e}")),
                    });
                }
            },
            Err(e) => {
                return Json(SendResult {
                    success: false,
                    error: Some(format!("Failed to decode base64 file data: {e}")),
                });
            }
        }
    }

    let mut context = {
        let db = get_db();
        create_context(session, &db)
    };

    let plan = SendMessagePlan;
    let params = SendMessageParams {
        chat_id: input.chat_id,
        message: input.text,
        image_path: image_path.clone(),
        image_mime,
        file_path: file_path.clone(),
    };
    let cancel = CancellationToken::new();
    let noop_emit = |_: SubscriptionEvent| {};

    let (result, _plan_state) =
        run_execution_loop(&plan, &params, &mut context, &noop_emit, cancel).await;

    // Clean up temp files
    if let Some(p) = &image_path {
        let _ = std::fs::remove_file(p);
    }
    if let Some(p) = &file_path {
        let _ = std::fs::remove_file(p);
    }

    Json(SendResult {
        success: result.success,
        error: result.error,
    })
}
