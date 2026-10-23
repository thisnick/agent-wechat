use crate::Core;
use anyhow::Result;
use serde_json::{json, Value};
use wechat_profile::{ListQuery, ProfileReader};

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
        let ids: Vec<_> = rows
            .iter()
            .filter(|r| r["deleted"] != true)
            .filter_map(|r| r["id"].as_str().map(str::to_owned))
            .collect();
        let mut latest = reader.latest_messages(&ids, own).ok();
        let labels = reader.labels().unwrap_or_default();
        for row in rows.iter_mut().filter(|r| r["deleted"] != true) {
            let Some(id) = row["id"].as_str().map(str::to_owned) else {
                continue;
            };
            let members = if id.ends_with("@chatroom") {
                reader
                    .group_members(
                        &id,
                        &ListQuery {
                            limit: Some(4),
                            ..Default::default()
                        },
                    )
                    .ok()
                    .map(|p| p.items)
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            row["avatarMembers"] = json!(members
                .iter()
                .map(|m| json!({"id":m.username,"displayName":m.label}))
                .collect::<Vec<_>>());
            // Omitted means unavailable, null means the readable local history
            // has no messages. A cache gap must not masquerade as an empty chat.
            if let Some(latest) = &mut latest {
                row["lastMessage"] = if let Some(message) = latest.remove(&id) {
                    let sender_name = message.sender.as_deref().and_then(|sender| {
                        members
                            .iter()
                            .find(|m| m.username == sender)
                            .map(|m| m.label.clone())
                            .or_else(|| labels.get(sender).cloned())
                    });
                    match self.normalize_recalled(account, message, own, reader.generation()) {
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
        "Message recalled"
    } else {
        match (kind, app) {
            (1, _) | (49, Some(57)) => message["body"].as_str().unwrap_or("Message"),
            (3, _) => "Photo",
            (34, _) => "Voice message",
            (43 | 62, _) => "Video",
            (47, _) => "Sticker",
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
        message["recall"] = json!({"observedAt":"now"});
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
