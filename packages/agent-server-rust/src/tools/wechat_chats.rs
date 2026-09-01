use super::wechat_db::{get_db_path, query_wechat_db};
use crate::ia::types::Chat;
use std::collections::HashMap;

/// List chats by querying WeChat's session.db and contact.db.
pub fn list_chats(
    account_dir: &str,
    keys: &HashMap<String, String>,
    limit: i64,
    offset: i64,
) -> Vec<Chat> {
    let session_key = match keys.get("session.db") {
        Some(k) => k,
        None => return Vec::new(),
    };
    let contact_key = match keys.get("contact.db") {
        Some(k) => k,
        None => return Vec::new(),
    };

    let session_db = get_db_path(account_dir, "session.db");
    let contact_db = get_db_path(account_dir, "contact.db");

    let sessions = query_wechat_db(
        &session_db,
        session_key,
        &format!(
            "SELECT username, type, unread_count, summary, draft, last_timestamp,
                    sort_timestamp, last_msg_sender, last_sender_display_name, is_hidden,
                    last_msg_locald_id
             FROM SessionTable
             WHERE is_hidden = 0
             ORDER BY sort_timestamp DESC
             LIMIT {limit} OFFSET {offset};"
        ),
    );

    if sessions.is_empty() {
        return Vec::new();
    }

    // Batch lookup contacts
    let usernames: Vec<String> = sessions
        .iter()
        .filter_map(|s| s.get("username").and_then(|v| v.as_str()).map(String::from))
        .collect();

    let mut contact_map: HashMap<String, serde_json::Value> = HashMap::new();
    for chunk in usernames.chunks(50) {
        let placeholders: String = chunk
            .iter()
            .map(|u| format!("'{}'", u.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");

        let contacts = query_wechat_db(
            &contact_db,
            contact_key,
            &format!(
                "SELECT username, nick_name, remark, alias, small_head_url, local_type
                 FROM contact
                 WHERE username IN ({placeholders});"
            ),
        );

        for c in contacts {
            if let Some(u) = c.get("username").and_then(|v| v.as_str()) {
                contact_map.insert(u.to_string(), c);
            }
        }
    }

    sessions
        .iter()
        .filter_map(|session| {
            let username = session.get("username")?.as_str()?.to_string();
            let contact = contact_map.get(&username);
            let is_group = username.contains("@chatroom");

            let name = contact
                .and_then(|c| {
                    c.get("remark")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                })
                .or_else(|| {
                    contact.and_then(|c| {
                        c.get("nick_name")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                    })
                })
                .unwrap_or(&username)
                .to_string();

            let remark = contact
                .and_then(|c| {
                    c.get("remark")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                })
                .map(String::from);

            let unread_count = session
                .get("unread_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32;

            let last_message_preview = session
                .get("summary")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from);

            let last_message_sender = session
                .get("last_sender_display_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    session
                        .get("last_msg_sender")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                })
                .map(String::from);

            let last_activity_at = session
                .get("last_timestamp")
                .and_then(|v| v.as_i64())
                .filter(|&t| t > 0)
                .map(|t| {
                    chrono::DateTime::from_timestamp(t, 0)
                        .map(|dt| dt.to_rfc3339())
                        .unwrap_or_default()
                });

            let last_msg_local_id = session.get("last_msg_locald_id").and_then(|v| v.as_i64());

            Some(Chat {
                id: username.clone(),
                username,
                name,
                remark,
                unread_count,
                is_group,
                last_message_preview,
                last_message_sender,
                last_activity_at,
                last_msg_local_id,
            })
        })
        .collect()
}

/// Find a chat by WeChat username (exact match).
pub fn get_chat_by_username(
    account_dir: &str,
    keys: &HashMap<String, String>,
    username: &str,
) -> Option<Chat> {
    let session_key = keys.get("session.db")?;
    let contact_key = keys.get("contact.db")?;

    let session_db = get_db_path(account_dir, "session.db");
    let contact_db = get_db_path(account_dir, "contact.db");

    let escaped = username.replace('\'', "''");

    let sessions = query_wechat_db(
        &session_db,
        session_key,
        &format!(
            "SELECT username, type, unread_count, summary, draft, last_timestamp,
                    sort_timestamp, last_msg_sender, last_sender_display_name, is_hidden,
                    last_msg_locald_id
             FROM SessionTable
             WHERE username = '{escaped}';"
        ),
    );

    let session = sessions.first()?;
    let is_group = username.contains("@chatroom");

    let contacts = query_wechat_db(
        &contact_db,
        contact_key,
        &format!(
            "SELECT username, nick_name, remark, alias, small_head_url, local_type
             FROM contact
             WHERE username = '{escaped}';"
        ),
    );
    let contact = contacts.first();

    let name = contact
        .and_then(|c| {
            c.get("remark")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            contact.and_then(|c| {
                c.get("nick_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
        })
        .unwrap_or(username)
        .to_string();

    let remark = contact
        .and_then(|c| {
            c.get("remark")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .map(String::from);

    let unread_count = session
        .get("unread_count")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    Some(Chat {
        id: username.to_string(),
        username: username.to_string(),
        name,
        remark,
        unread_count,
        is_group,
        last_message_preview: session
            .get("summary")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        last_message_sender: session
            .get("last_sender_display_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from),
        last_activity_at: session
            .get("last_timestamp")
            .and_then(|v| v.as_i64())
            .filter(|&t| t > 0)
            .map(|t| {
                chrono::DateTime::from_timestamp(t, 0)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            }),
        last_msg_local_id: session.get("last_msg_locald_id").and_then(|v| v.as_i64()),
    })
}

/// Find chats by name (partial match).
pub fn find_chats_by_name(
    account_dir: &str,
    keys: &HashMap<String, String>,
    query: &str,
) -> Vec<Chat> {
    let contact_key = match keys.get("contact.db") {
        Some(k) => k,
        None => return Vec::new(),
    };
    let session_key = match keys.get("session.db") {
        Some(k) => k,
        None => return Vec::new(),
    };

    let contact_db = get_db_path(account_dir, "contact.db");
    let session_db = get_db_path(account_dir, "session.db");
    let escaped = query.replace('\'', "''");

    let contacts = query_wechat_db(
        &contact_db,
        contact_key,
        &format!(
            "SELECT username, nick_name, remark, alias, small_head_url, local_type
             FROM contact
             WHERE nick_name LIKE '%{escaped}%'
                OR remark LIKE '%{escaped}%'
                OR username LIKE '%{escaped}%'
             LIMIT 20;"
        ),
    );

    if contacts.is_empty() {
        return Vec::new();
    }

    let usernames: String = contacts
        .iter()
        .filter_map(|c| c.get("username").and_then(|v| v.as_str()))
        .map(|u| format!("'{}'", u.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");

    let sessions = query_wechat_db(
        &session_db,
        session_key,
        &format!(
            "SELECT username, type, unread_count, summary, draft, last_timestamp,
                    sort_timestamp, last_msg_sender, last_sender_display_name, is_hidden,
                    last_msg_locald_id
             FROM SessionTable
             WHERE username IN ({usernames})
             ORDER BY sort_timestamp DESC;"
        ),
    );

    let session_map: HashMap<String, &serde_json::Value> = sessions
        .iter()
        .filter_map(|s| {
            s.get("username")
                .and_then(|v| v.as_str())
                .map(|u| (u.to_string(), s))
        })
        .collect();

    contacts
        .iter()
        .filter_map(|contact| {
            let username = contact.get("username")?.as_str()?;
            let session = session_map.get(username)?;
            let is_group = username.contains("@chatroom");

            let name = contact
                .get("remark")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    contact
                        .get("nick_name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or(username)
                .to_string();

            Some(Chat {
                id: username.to_string(),
                username: username.to_string(),
                name,
                remark: contact
                    .get("remark")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                unread_count: session
                    .get("unread_count")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as i32,
                is_group,
                last_message_preview: session
                    .get("summary")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                last_message_sender: session
                    .get("last_sender_display_name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from),
                last_activity_at: session
                    .get("last_timestamp")
                    .and_then(|v| v.as_i64())
                    .filter(|&t| t > 0)
                    .map(|t| {
                        chrono::DateTime::from_timestamp(t, 0)
                            .map(|dt| dt.to_rfc3339())
                            .unwrap_or_default()
                    }),
                last_msg_local_id: session.get("last_msg_locald_id").and_then(|v| v.as_i64()),
            })
        })
        .collect()
}
