use axum::{extract::Query, Json};
use serde::Deserialize;

use crate::db::get_db;
use crate::ia::types::Contact;
use crate::sessions::manager::get_session;
use crate::tools::wechat_contacts;
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
    200
}

pub async fn list_contacts(Query(params): Query<ListParams>) -> Json<Vec<Contact>> {
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

    // Lazy key extraction: if contact.db exists on disk without stored key, re-extract
    if !keys.contains_key("contact.db") {
        let on_disk = list_account_dbs(&logged_in_user);
        if on_disk.iter().any(|name| name == "contact.db") {
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

    if !keys.contains_key("contact.db") {
        return Json(Vec::new());
    }

    Json(wechat_contacts::list_contacts(
        &logged_in_user,
        &keys,
        params.limit,
        params.offset,
    ))
}

#[derive(Deserialize)]
pub struct FindParams {
    name: String,
}

pub async fn find_contacts(Query(params): Query<FindParams>) -> Json<Vec<Contact>> {
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

    // Lazy key extraction: if contact.db exists on disk without stored key, re-extract
    if !keys.contains_key("contact.db") {
        let on_disk = list_account_dbs(&logged_in_user);
        if on_disk.iter().any(|name| name == "contact.db") {
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

    Json(wechat_contacts::find_contacts(
        &logged_in_user,
        &keys,
        &params.name,
    ))
}
