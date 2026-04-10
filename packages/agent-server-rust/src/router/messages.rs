use axum::{
    extract::{Path, Query},
    Json,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::context::create_context;
use crate::db::get_db;
use crate::db::queries::{get_payment_receipts, mark_payment_received};
use crate::execution::run_execution_loop;
use crate::ia::types::{
    MediaResult, Message, PaymentInfo, ReceivePaymentResult, SendResult, SubscriptionEvent,
};
use crate::plans::receive_transfer::{ReceiveTransferParams, ReceiveTransferPlan};
use crate::plans::send_message::{SendMessageParams, SendMessagePlan};
use crate::sessions::manager::get_session;
use crate::tools::wechat_db::{find_wechat_pid, list_account_dbs};
use crate::tools::wechat_keys::{extract_keys_async, get_image_keys, get_stored_keys, store_keys};
use crate::tools::wechat_media::get_message_media;
use crate::tools::wechat_messages;

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

    if !keys.keys().any(|k| {
        k.starts_with("message_")
            && k.ends_with(".db")
            && !k.contains("fts")
            && !k.contains("resource")
    }) {
        return Json(Vec::new());
    }

    let mut messages = wechat_messages::list_messages(
        &logged_in_user,
        &keys,
        &chat_id,
        params.limit,
        params.offset,
    );
    apply_payment_receipt_state(&session.id, &chat_id, &mut messages);

    Json(messages)
}

pub async fn get_media(Path((chat_id, local_id)): Path<(String, i64)>) -> Json<MediaResult> {
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

    Json(get_message_media(
        &logged_in_user,
        &keys,
        &chat_id,
        local_id,
        image_keys,
    ))
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
        let path = format!(
            "/tmp/send_image_{}{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            ext
        );
        if let Ok(bytes) =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &img.data)
        {
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
        let safe_name: String = f
            .filename
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let path = format!(
            "/tmp/send_file_{}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            safe_name
        );
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &f.data) {
            Ok(bytes) => match std::fs::write(&path, &bytes) {
                Ok(_) => {
                    file_path = Some(path);
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

#[derive(Deserialize)]
pub struct ReceiveTransferInput {
    #[serde(rename = "transactionId")]
    transaction_id: Option<String>,
    #[serde(rename = "localId")]
    local_id: Option<i64>,
}

#[derive(Deserialize)]
pub struct ReceiveRedPacketInput {
    #[serde(rename = "localId")]
    local_id: Option<i64>,
    #[serde(rename = "sendId")]
    send_id: Option<String>,
    #[serde(rename = "payMsgId")]
    pay_msg_id: Option<String>,
}

fn payment_result(
    kind: &str,
    success: bool,
    error: Option<String>,
    local_id: Option<i64>,
    is_received: Option<bool>,
    received_at: Option<String>,
    payment: Option<&PaymentInfo>,
) -> ReceivePaymentResult {
    ReceivePaymentResult {
        success,
        kind: kind.to_string(),
        error,
        local_id,
        is_received,
        received_at,
        amount_text: payment.and_then(|p| p.amount_text.clone()),
        amount_cents: payment.and_then(|p| p.amount_cents),
        currency: payment.and_then(|p| p.currency.clone()),
        transaction_id: payment.and_then(|p| p.transaction_id.clone()),
        transfer_id: payment.and_then(|p| p.transfer_id.clone()),
        send_id: payment.and_then(|p| p.send_id.clone()),
        pay_msg_id: payment.and_then(|p| p.pay_msg_id.clone()),
    }
}

fn apply_payment_receipt_state(session_id: &str, chat_id: &str, messages: &mut [Message]) {
    let receipts = {
        let db = get_db();
        get_payment_receipts(&db, session_id, chat_id)
    };

    for message in messages {
        if message.payment.is_none() {
            continue;
        }

        if let Some(received_at) = receipts.get(&message.local_id) {
            message.is_received = Some(true);
            message.received_at = Some(received_at.clone());
        } else {
            message.is_received = Some(false);
            message.received_at = None;
        }
    }
}

async fn load_logged_in_session_and_keys() -> Result<
    (
        crate::ia::types::Session,
        String,
        std::collections::HashMap<String, String>,
    ),
    ReceivePaymentResult,
> {
    let session = get_session("default").ok_or_else(|| {
        payment_result(
            "unknown",
            false,
            Some("No session available".to_string()),
            None,
            None,
            None,
            None,
        )
    })?;

    let logged_in_user = session.logged_in_user.clone().ok_or_else(|| {
        payment_result(
            "unknown",
            false,
            Some("NOT_LOGGED_IN".to_string()),
            None,
            None,
            None,
            None,
        )
    })?;

    let mut keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &logged_in_user)
    };

    let on_disk = list_account_dbs(&logged_in_user);
    let has_missing_db = on_disk.iter().any(|name| {
        (name.starts_with("message_")
            && name.ends_with(".db")
            && !name.contains("fts")
            && !name.contains("resource"))
            && !keys.contains_key(name.as_str())
    });

    if has_missing_db {
        if let Some(pid) = find_wechat_pid() {
            let extracted = extract_keys_async(pid).await;
            if !extracted.is_empty() {
                let db = get_db();
                store_keys(&db, &session.id, &logged_in_user, &extracted);
                keys = get_stored_keys(&db, &session.id, &logged_in_user);
            }
        }
    }

    if !keys.keys().any(|k| {
        k.starts_with("message_")
            && k.ends_with(".db")
            && !k.contains("fts")
            && !k.contains("resource")
    }) {
        return Err(payment_result(
            "unknown",
            false,
            Some("MESSAGE_DB_UNAVAILABLE".to_string()),
            None,
            None,
            None,
            None,
        ));
    }

    Ok((session, logged_in_user, keys))
}

fn find_payment_message<'a>(
    messages: &'a [Message],
    kind: &str,
    local_id: Option<i64>,
    transaction_id: Option<&str>,
    send_id: Option<&str>,
    pay_msg_id: Option<&str>,
) -> Option<&'a Message> {
    messages.iter().find(|message| {
        let payment = match &message.payment {
            Some(payment) if payment.kind == kind => payment,
            _ => return false,
        };

        if let Some(local_id) = local_id {
            return message.local_id == local_id;
        }

        if let Some(transaction_id) = transaction_id {
            return payment.transaction_id.as_deref() == Some(transaction_id);
        }

        if let Some(send_id) = send_id {
            return payment.send_id.as_deref() == Some(send_id);
        }

        if let Some(pay_msg_id) = pay_msg_id {
            return payment.pay_msg_id.as_deref() == Some(pay_msg_id);
        }

        true
    })
}

pub async fn receive_transfer(
    Path(chat_id): Path<String>,
    Json(input): Json<ReceiveTransferInput>,
) -> Json<ReceivePaymentResult> {
    let (session, logged_in_user, keys) = match load_logged_in_session_and_keys().await {
        Ok(value) => value,
        Err(result) => return Json(result),
    };

    let mut messages = wechat_messages::list_messages(&logged_in_user, &keys, &chat_id, 200, 0);
    apply_payment_receipt_state(&session.id, &chat_id, &mut messages);
    let message = match find_payment_message(
        &messages,
        "transfer",
        input.local_id,
        input.transaction_id.as_deref(),
        None,
        None,
    ) {
        Some(message) => message.clone(),
        None => {
            return Json(payment_result(
                "transfer",
                false,
                Some("TRANSFER_NOT_FOUND".to_string()),
                input.local_id,
                None,
                None,
                None,
            ))
        }
    };

    let payment = match message.payment.as_ref() {
        Some(payment) => payment,
        None => {
            return Json(payment_result(
                "transfer",
                false,
                Some("MESSAGE_IS_NOT_TRANSFER".to_string()),
                Some(message.local_id),
                None,
                None,
                None,
            ))
        }
    };

    if message.is_received == Some(true) {
        return Json(payment_result(
            "transfer",
            true,
            None,
            Some(message.local_id),
            Some(true),
            message.received_at.clone(),
            Some(payment),
        ));
    }

    let session_id = session.id.clone();
    let mut context = {
        let db = get_db();
        create_context(session, &db)
    };

    let plan = ReceiveTransferPlan;
    let params = ReceiveTransferParams {
        chat_id: chat_id.clone(),
        transaction_id: payment.transaction_id.clone(),
        amount_text: payment.amount_text.clone(),
    };
    let cancel = CancellationToken::new();
    let noop_emit = |_: SubscriptionEvent| {};

    let (result, _plan_state) =
        run_execution_loop(&plan, &params, &mut context, &noop_emit, cancel).await;

    let (is_received, received_at) = if result.success {
        let received_at = {
            let db = get_db();
            mark_payment_received(&db, &session_id, &chat_id, message.local_id)
        };
        (Some(true), Some(received_at))
    } else {
        (Some(false), None)
    };

    Json(payment_result(
        "transfer",
        result.success,
        result.error,
        Some(message.local_id),
        is_received,
        received_at,
        Some(payment),
    ))
}

pub async fn receive_red_packet(
    Path(chat_id): Path<String>,
    Json(input): Json<ReceiveRedPacketInput>,
) -> Json<ReceivePaymentResult> {
    let (session, logged_in_user, keys) = match load_logged_in_session_and_keys().await {
        Ok(value) => value,
        Err(result) => return Json(result),
    };

    let mut messages = wechat_messages::list_messages(&logged_in_user, &keys, &chat_id, 200, 0);
    apply_payment_receipt_state(&session.id, &chat_id, &mut messages);
    let message = match find_payment_message(
        &messages,
        "red_packet",
        input.local_id,
        None,
        input.send_id.as_deref(),
        input.pay_msg_id.as_deref(),
    ) {
        Some(message) => message,
        None => {
            return Json(payment_result(
                "red_packet",
                false,
                Some("RED_PACKET_NOT_FOUND".to_string()),
                input.local_id,
                None,
                None,
                None,
            ))
        }
    };

    Json(payment_result(
        "red_packet",
        false,
        Some("UNSUPPORTED_ON_LINUX_WECHAT_CLIENT".to_string()),
        Some(message.local_id),
        message.is_received,
        message.received_at.clone(),
        message.payment.as_ref(),
    ))
}
