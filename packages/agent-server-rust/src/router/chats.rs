use axum::{extract::{Path, Query}, Json};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::context::create_context;
use crate::db::get_db;
use crate::execution::run_execution_loop;
use crate::ia::types::{Chat, SubscriptionEvent};
use crate::plans::chat_open::{ChatOpenParams, ChatOpenPlan};
use crate::sessions::manager::get_session;
use crate::tools::wechat_chats;
use crate::tools::wechat_db::{find_wechat_pid, list_account_dbs};
use crate::tools::wechat_keys::{extract_keys_async, get_stored_keys, store_keys};

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

pub async fn list_chats(Query(params): Query<ListParams>) -> Json<Vec<Chat>> {
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

    // Lazy key extraction: if session.db or contact.db exist on disk without stored keys, re-extract
    if !keys.contains_key("session.db") || !keys.contains_key("contact.db") {
        let on_disk = list_account_dbs(&logged_in_user);
        let has_missing = on_disk.iter().any(|name| {
            (name == "session.db" || name == "contact.db") && !keys.contains_key(name.as_str())
        });
        if has_missing {
            if let Some(pid) = find_wechat_pid() {
                let extracted = extract_keys_async(pid).await;
                if !extracted.is_empty() {
                    let db = get_db();
                    store_keys(&db, &session.id, &logged_in_user, &extracted);
                    keys = get_stored_keys(&db, &session.id, &logged_in_user);
                }
            }
        }
    }

    if !keys.contains_key("session.db") || !keys.contains_key("contact.db") {
        return Json(Vec::new());
    }

    Json(wechat_chats::list_chats(
        &logged_in_user,
        &keys,
        params.limit,
        params.offset,
    ))
}

pub async fn get_chat(Path(id): Path<String>) -> Json<Option<Chat>> {
    let session = match get_session("default") {
        Some(s) => s,
        None => return Json(None),
    };
    let logged_in_user = match &session.logged_in_user {
        Some(u) => u.clone(),
        None => return Json(None),
    };

    let mut keys = {
        let db = get_db();
        get_stored_keys(&db, &session.id, &logged_in_user)
    };

    // Lazy key extraction: if session.db or contact.db exist on disk without stored keys, re-extract
    if !keys.contains_key("session.db") || !keys.contains_key("contact.db") {
        let on_disk = list_account_dbs(&logged_in_user);
        let has_missing = on_disk.iter().any(|name| {
            (name == "session.db" || name == "contact.db") && !keys.contains_key(name.as_str())
        });
        if has_missing {
            if let Some(pid) = find_wechat_pid() {
                let extracted = extract_keys_async(pid).await;
                if !extracted.is_empty() {
                    let db = get_db();
                    store_keys(&db, &session.id, &logged_in_user, &extracted);
                    keys = get_stored_keys(&db, &session.id, &logged_in_user);
                }
            }
        }
    }

    Json(wechat_chats::get_chat_by_username(&logged_in_user, &keys, &id))
}

#[derive(Deserialize)]
pub struct FindParams {
    name: String,
}

pub async fn find_chats(Query(params): Query<FindParams>) -> Json<Vec<Chat>> {
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

    // Lazy key extraction: if session.db or contact.db exist on disk without stored keys, re-extract
    if !keys.contains_key("session.db") || !keys.contains_key("contact.db") {
        let on_disk = list_account_dbs(&logged_in_user);
        let has_missing = on_disk.iter().any(|name| {
            (name == "session.db" || name == "contact.db") && !keys.contains_key(name.as_str())
        });
        if has_missing {
            if let Some(pid) = find_wechat_pid() {
                let extracted = extract_keys_async(pid).await;
                if !extracted.is_empty() {
                    let db = get_db();
                    store_keys(&db, &session.id, &logged_in_user, &extracted);
                    keys = get_stored_keys(&db, &session.id, &logged_in_user);
                }
            }
        }
    }

    Json(wechat_chats::find_chats_by_name(
        &logged_in_user,
        &keys,
        &params.name,
    ))
}

#[derive(Deserialize)]
pub struct OpenChatParams {
    #[serde(default, rename = "clearUnreads")]
    clear_unreads: bool,
}

pub async fn open_chat(
    Path(chat_id): Path<String>,
    Query(params): Query<OpenChatParams>,
) -> Json<serde_json::Value> {
    let clear_unreads = params.clear_unreads;
    let session = match get_session("default") {
        Some(s) => s,
        None => {
            return Json(serde_json::json!({
                "ok": false,
                "error": "No session available"
            }))
        }
    };

    if session.logged_in_user.is_none() {
        return Json(serde_json::json!({
            "ok": false,
            "error": "NOT_LOGGED_IN"
        }));
    }

    if chat_id.starts_with("gh_") {
        return Json(serde_json::json!({
            "ok": false,
            "error": "Opening official accounts is not supported"
        }));
    }

    let mut context = {
        let db = get_db();
        create_context(session, &db)
    };

    let plan = ChatOpenPlan;
    let params = ChatOpenParams { chat_id, clear_unreads };
    let cancel = CancellationToken::new();
    let noop_emit = |_: SubscriptionEvent| {};

    let (result, plan_state) =
        run_execution_loop(&plan, &params, &mut context, &noop_emit, cancel).await;

    if result.success {
        if let Some(open_result) = plan_state.result {
            Json(serde_json::to_value(open_result).unwrap_or_else(|_| serde_json::json!({"ok": true})))
        } else {
            Json(serde_json::json!({ "ok": true }))
        }
    } else {
        Json(serde_json::json!({
            "ok": false,
            "error": result.error.unwrap_or_else(|| "Chat open failed".to_string())
        }))
    }
}
