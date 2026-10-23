//! Read-only API attribution from existing original-message observations.
//! This cache stores no bodies, writes no profile/sidecar data, and expires on
//! a new retained sync receipt. Missing evidence is not an incoming author.
use crate::Core;
use anyhow::Result;
use std::collections::BTreeMap;

type Target = (String, String, i64);
pub(crate) struct RecallSyncCache {
    revision: i64,
    own: String,
    authors: BTreeMap<Target, Option<String>>,
}

fn valid_sender(sender: &str) -> bool {
    !sender.is_empty()
        && sender.len() <= 512
        && !sender.ends_with("@chatroom")
        && !sender.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn observed_author(message: &wechat_protocol::wire::AddMessage, own: &str) -> Option<String> {
    if !matches!(message.message_type, 1 | 3 | 34 | 43 | 47 | 49)
        || if message.outgoing {
            message.from != own || message.to != message.peer
        } else {
            message.to != own || message.from != message.peer
        }
    {
        return None;
    }
    let sender = if !message.outgoing && message.from.ends_with("@chatroom") {
        let end = message
            .content
            .windows(2)
            .take(513)
            .position(|bytes| bytes == b":\n")?;
        std::str::from_utf8(&message.content[..end]).ok()?
    } else {
        message.sender_id.as_deref().unwrap_or(&message.from)
    };
    valid_sender(sender).then(|| sender.into())
}

impl Core {
    pub(crate) fn infer_recalled_author(
        &self,
        account: &str,
        message: &wechat_profile::Message,
        own: Option<&str>,
    ) -> Result<Option<String>> {
        // While projection is pending the original official row is still the
        // strongest source. Settled system rows deliberately clear its sender.
        if (1..10000).contains(&message.message_type) {
            if let Some(sender) = message.sender.as_deref().filter(|s| valid_sender(s)) {
                return Ok(Some(sender.into()));
            }
        }
        let (Some(own), Some(thread), Some(server)) = (
            own,
            message.peer.as_deref(),
            message.server_message_id.as_deref(),
        ) else {
            return Ok(None);
        };
        if self.profile_username(account)?.as_deref() != Some(own) {
            return Ok(None);
        }
        use md5::Digest;
        let table = format!("Msg_{:x}", md5::Md5::digest(thread.as_bytes()));
        if !message.id.table.eq_ignore_ascii_case(&table) {
            return Ok(None);
        }
        let target = (thread.to_owned(), server.to_owned(), message.create_time);
        let revision = self.store.recall_sync_evidence_revision(account)?;
        let cached = self.recall_sync_cache.lock().ok().and_then(|cache| {
            cache
                .get(account)
                .filter(|entry| entry.revision == revision && entry.own == own)
                .map(|entry| entry.authors.get(&target).cloned())
        });
        let observed = if let Some(observed) = cached {
            observed
        } else {
            let mut authors = BTreeMap::new();
            for (id, sealed) in self.store.recall_sync_evidence(account)? {
                let Ok(plain) = self
                    .vault
                    .open_record(account, &format!("apply:{id}"), &sealed)
                else {
                    continue;
                };
                let Ok(batch) = serde_json::from_slice::<wechat_protocol::SyncBatch>(&plain) else {
                    continue;
                };
                for command in batch.commands.into_iter().filter(|c| c.id == 5) {
                    let Ok(original) =
                        wechat_protocol::wire::decode_add_message(&command.payload, own)
                    else {
                        continue;
                    };
                    let Some(author) = observed_author(&original, own) else {
                        continue;
                    };
                    let Some(server) = original.server_message_id else {
                        continue;
                    };
                    let key = (original.peer, server, i64::from(original.created_at));
                    if authors.len() >= 4096 && !authors.contains_key(&key) {
                        continue;
                    }
                    let value = authors.entry(key).or_insert_with(|| Some(author.clone()));
                    if value.as_deref() != Some(&author) {
                        *value = None;
                    }
                }
            }
            let observed = authors.get(&target).cloned();
            if let Ok(mut cache) = self.recall_sync_cache.lock() {
                if cache.len() >= 16 && !cache.contains_key(account) {
                    cache.pop_first();
                }
                cache.insert(
                    account.into(),
                    RecallSyncCache {
                        revision,
                        own: own.into(),
                        authors,
                    },
                );
            }
            observed
        };
        // Send acceptance may change without another sync receipt. Do not cache
        // the combined result or infer ownership from a generic recall success.
        let sent = self.recall_send_author(account, message, own)?;
        Ok(match observed {
            Some(Some(author)) if sent.as_ref().is_none_or(|sent| *sent == author) => Some(author),
            Some(_) => None,
            None => sent,
        })
    }
}
