use crate::Core;
use anyhow::Result;
use serde_json::{json, Value};
use wechat_profile::ProfileReader;

impl Core {
    /// Enrich only the returned page, using the same profile snapshot as its
    /// catalog rows. No network refresh, media retrieval or new stored records.
    pub(crate) fn thread_summaries(
        &self,
        account: &str,
        rows: &mut [Value],
        reader: &ProfileReader,
        own: Option<&str>,
    ) -> Result<()> {
        self.classify_peers(rows, reader)?;
        let ids: Vec<_> = rows
            .iter()
            .filter(|r| r["deleted"] != true)
            .filter_map(|r| r["id"].as_str().map(str::to_owned))
            .collect();
        let muted = reader.thread_muted(&ids).unwrap_or_default();
        let pinned = reader.thread_pinned(&ids).unwrap_or_default();
        let unresolved = self.store.runtime_keys(
            account,
            &ids.iter()
                .flat_map(|id| {
                    [
                        format!("contact-notifications-unresolved:{id}"),
                        format!("contact-pinning-unresolved:{id}"),
                    ]
                })
                .collect::<Vec<_>>(),
        )?;
        let mut latest = reader.latest_messages(&ids, own).ok().and_then(|messages| {
            let (ids, messages): (Vec<_>, Vec<_>) = messages.into_iter().unzip();
            let senders: Vec<_> = messages
                .iter()
                .map(|message| message.sender.clone())
                .collect();
            self.normalize_thread_previews(account, messages, own, reader)
                .ok()
                .map(|normalized| {
                    ids.into_iter()
                        .zip(senders.into_iter().zip(normalized))
                        .collect::<std::collections::BTreeMap<_, _>>()
                })
        });
        let rooms: Vec<_> = ids
            .iter()
            .filter(|id| id.ends_with("@chatroom"))
            .cloned()
            .collect();
        let mut avatar_members = reader.group_avatar_members(&rooms).unwrap_or_default();
        let labels = reader.labels().unwrap_or_default();
        for row in rows.iter_mut().filter(|r| r["deleted"] != true) {
            let Some(id) = row["id"].as_str().map(str::to_owned) else {
                continue;
            };
            row["muted"] = if unresolved.contains(&format!("contact-notifications-unresolved:{id}"))
            {
                Value::Null
            } else {
                json!(muted.get(&id).copied().flatten())
            };
            row["pinned"] = if unresolved.contains(&format!("contact-pinning-unresolved:{id}")) {
                Value::Null
            } else {
                json!(pinned.get(&id).copied().flatten())
            };
            let members = avatar_members.remove(&id).unwrap_or_default();
            row["avatarMembers"] = json!(members
                .iter()
                .map(|m| json!({"id":m.username,"displayName":m.label}))
                .collect::<Vec<_>>());
            // Omitted means unavailable, null means the readable local history
            // has no messages. A cache gap must not masquerade as an empty chat.
            if let Some(latest) = &mut latest {
                row["lastMessage"] = if let Some((sender, message)) = latest.remove(&id) {
                    let sender_name = sender.as_deref().and_then(|sender| {
                        members
                            .iter()
                            .find(|m| m.username == sender)
                            .map(|m| m.label.clone())
                            .or_else(|| labels.get(sender).cloned())
                    });
                    match message {
                        Ok(message) => message_summary(&message, sender_name.as_deref()),
                        Err(_) => continue,
                    }
                } else {
                    Value::Null
                };
            }
        }
        Ok(())
    }
}

fn message_summary(message: &Value, sender_name: Option<&str>) -> Value {
    let kind = message["type"].as_i64().unwrap_or(0);
    let app = message["appMessageType"].as_i64();
    let text = if message["recall"].is_object() {
        message["recall"]["replacementText"]
            .as_str()
            .filter(|text| !text.is_empty())
            .unwrap_or("Message recalled")
    } else {
        match (kind, app) {
            (1, _) | (49, Some(57)) => message["body"].as_str().unwrap_or("Message"),
            (3, _) => "Photo",
            (34, _) => "Voice message",
            (43 | 62, _) => "Video",
            (47, _) | (49, Some(8)) => "Sticker",
            (48, _) => "Location",
            (49, Some(6)) => "File",
            (49, Some(19)) => "Chat history",
            (49, _) if message["link"].is_object() => message["link"]["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("Shared link"),
            (49, _) => "Shared content",
            (10000 | 10002, _) => "System message",
            _ => "Message",
        }
    };
    let mut text: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    if text.is_empty() {
        text = "Message".into();
    }
    json!({
        "id":message["id"],"senderId":message["senderId"],"senderName":sender_name,
        "direction":message["direction"],"type":message["type"],
        "text":text,"createdAt":message["createdAt"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_and_detail_derive_pin_state_and_unresolved_work_does_not_replace_saved_flags() {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = temp.path().join("account");
        let password = [42u8; 32];
        let (_, keys, _) = wechat_profile::initialize_profile(&profile, &password).unwrap();
        for (relative, sql) in [
            (
                "contact/contact.db",
                "INSERT INTO contact(id,username,flag,local_type,delete_flag,is_in_chat_room)VALUES(1,'peer',2561,1,0,0),(2,'other',1,1,0,0),(3,'room@chatroom',2050,2,0,1);",
            ),
            (
                "session/session.db",
                "INSERT INTO SessionTable(username,unread_count,is_hidden,sort_timestamp,last_timestamp)VALUES('peer',0,0,100,100),('other',0,0,101,101),('room@chatroom',0,0,102,102);",
            ),
        ] {
            let db = rusqlite::Connection::open(profile.join("db_storage").join(relative)).unwrap();
            assert_eq!(
                unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
                0
            );
            db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000;").unwrap();
            db.execute_batch(sql).unwrap();
        }
        let store = std::sync::Arc::new(crate::Store::open(&temp.path().join("data")).unwrap());
        store.attach("one", &profile, true).unwrap();
        let key_path = temp.path().join("keys.json");
        keys.write_new(&key_path).unwrap();
        store.set_keys_file("one", &key_path).unwrap();
        let core = Core::new(store, None).unwrap();
        let detail = |name| core.catalog_item("one", "threads", name).unwrap();
        let page = core
            .catalog_page("one", "threads", None, &Default::default())
            .unwrap();
        for (name, expected) in [("peer", true), ("other", false), ("room@chatroom", true)] {
            let listed = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|thread| thread["id"] == name)
                .unwrap();
            assert_eq!(listed["pinned"], expected);
            assert_eq!(detail(name)["pinned"], expected);
        }
        assert_eq!(detail("peer")["muted"], true);
        core.store
            .put_runtime("one", "contact-pinning-unresolved:peer", b"receipt")
            .unwrap();
        assert!(detail("peer")["pinned"].is_null());
        assert_eq!(detail("peer")["muted"], true);
        assert_eq!(detail("other")["pinned"], false);
        let reader = core.profile_reader("one").unwrap();
        assert_eq!(
            reader.thread_pinned(&["peer".into()]).unwrap()["peer"],
            Some(true)
        );
        let mut unknown = vec![
            json!({"id":"missing"}),
            json!({"id":"removed","deleted":true}),
        ];
        core.thread_summaries("one", &mut unknown, &reader, Some("synthetic_self"))
            .unwrap();
        assert_eq!(unknown[0].get("pinned"), Some(&Value::Null));
        assert!(unknown[1].get("pinned").is_none());
        core.store
            .clear_runtime("one", "contact-pinning-unresolved:peer")
            .unwrap();
        assert_eq!(detail("peer")["pinned"], true);
    }

    #[test]
    fn summary_is_bounded_plain_text_and_never_exposes_media_xml_or_recalled_text() {
        let base = json!({"id":"one","senderId":"peer","direction":"incoming","type":1,"body":"hello\n  world","createdAt":"now"});
        assert_eq!(message_summary(&base, Some("Peer"))["text"], "hello world");
        let mut message = base;
        message["body"] = json!("👋".repeat(200));
        assert_eq!(
            message_summary(&message, None)["text"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            160
        );
        message["type"] = json!(47);
        message["body"] = json!("<msg><emoji secret='not a preview'/></msg>");
        assert_eq!(message_summary(&message, None)["text"], "Sticker");
        message["type"] = json!(49);
        message["appMessageType"] = json!(8);
        assert_eq!(message_summary(&message, None)["text"], "Sticker");
        message["recall"] = json!({"observedAt":"now"});
        assert_eq!(message_summary(&message, None)["text"], "Message recalled");
        message["recall"]["replacementText"] = json!("You recalled a message");
        assert_eq!(
            message_summary(&message, None)["text"],
            "You recalled a message"
        );
        message["recall"]["replacementText"] = json!("");
        assert_eq!(message_summary(&message, None)["text"], "Message recalled");
        message["recall"] = Value::Null;
        for (kind, expected) in [
            (3, "Photo"),
            (34, "Voice message"),
            (43, "Video"),
            (10000, "System message"),
        ] {
            message["type"] = json!(kind);
            assert_eq!(message_summary(&message, None)["text"], expected);
        }
    }
}
