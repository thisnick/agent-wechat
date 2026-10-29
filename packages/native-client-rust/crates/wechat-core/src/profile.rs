use crate::Core;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use wechat_profile::{
    KeyFile, ListQuery, MessageQuery, ProfileLocation, ProfileReader, ReadOptions, SourceMessageId,
};

pub fn message_id(id: &SourceMessageId) -> Result<String> {
    Ok(format!(
        "msg_{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(id)?)
    ))
}
pub fn source_message_id(id: &str) -> Result<SourceMessageId> {
    if id.len() > 8192 {
        bail!("invalid message ID");
    }
    let raw = URL_SAFE_NO_PAD.decode(id.strip_prefix("msg_").context("invalid message ID")?)?;
    serde_json::from_slice(&raw).map_err(|_| anyhow::anyhow!("invalid message ID"))
}
pub fn mentions(source: Option<&str>, own: Option<&str>) -> Value {
    let unknown = || json!({"state":"unknown","userIds":[],"self":null,"all":null,"source":null});
    let Some(source) = source else {
        return unknown();
    };
    if source.len() > 256 * 1024
        || source.to_ascii_uppercase().contains("<!DOCTYPE")
        || source.to_ascii_uppercase().contains("<!ENTITY")
    {
        return unknown();
    }
    let Ok(doc) = roxmltree::Document::parse(source) else {
        return unknown();
    };
    let root = doc.root_element();
    if root.tag_name().name() != "msgsource"
        || root.tag_name().namespace().is_some()
        || doc.descendants().count() > 4096
    {
        return unknown();
    }
    let lists: Vec<_> = root
        .children()
        .filter(|n| {
            n.is_element()
                && n.tag_name().name() == "atuserlist"
                && n.tag_name().namespace().is_none()
        })
        .collect();
    if lists.len() > 1 {
        return unknown();
    }
    if lists
        .iter()
        .any(|n| n.children().any(|child| child.is_element()))
        || doc
            .descendants()
            .any(|n| n.ancestors().take(65).count() > 64)
    {
        return unknown();
    }
    let raw = lists.first().and_then(|n| n.text()).unwrap_or("");
    let ids: Vec<_> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if ids.len() > 128
        || ids
            .iter()
            .any(|s| s.len() > 256 || s.contains(['\0', '\n', '\r']))
    {
        return unknown();
    }
    let all = ids.contains(&"notify@all");
    let users: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| *id != "notify@all")
        .collect();
    json!({"state":"known","userIds":users,"self":own.map(|own|users.contains(&own)),"all":all,"source":"msgsource.atuserlist"})
}
impl Core {
    pub fn profile_location_keys(&self, account: &str) -> Result<(ProfileLocation, KeyFile)> {
        let record = self.store.account(account)?;
        let keys_path = self
            .store
            .keys_file(account)?
            .context("profile_keys_missing")?;
        let keys = KeyFile::load(keys_path)?;
        let path = if record.profile_path.join("db_storage").is_dir() {
            record.profile_path
        } else {
            record
                .profile_path
                .join("xwechat_files")
                .join(&keys.account_binding)
        };
        let location = ProfileLocation::new(path)?;
        Ok((location, keys))
    }
    pub fn profile_reader(&self, account: &str) -> Result<ProfileReader> {
        let (location, keys) = self.profile_location_keys(account)?;
        Ok(ProfileReader::open(
            location,
            keys,
            ReadOptions {
                stopped_or_consistent_snapshot: true,
                ..Default::default()
            },
        )?)
    }
    /// Use the request's pinned reader, or retain the account gate for an ordinary view.
    pub(crate) fn profile_reader_owned(
        &self,
        account: &str,
    ) -> Result<std::sync::Arc<ProfileReader>> {
        if let Some((bound, reader)) = &self.snapshot_reader {
            anyhow::ensure!(bound == account, "read snapshot account mismatch");
            return Ok(reader.clone());
        }
        let (location, keys) = self.profile_location_keys(account)?;
        let owned = self.owns_profile(account)?;
        let options = ReadOptions {
            stopped_or_consistent_snapshot: true,
            ..Default::default()
        };
        if owned {
            let mut readers = self
                .profile_readers
                .lock()
                .map_err(|_| anyhow::anyhow!("profile reader unavailable"))?;
            // Bound idle descriptors across accounts as well as within a profile.
            if !readers.contains_key(account) && readers.len() >= 4 {
                if let Some(oldest) = readers
                    .iter()
                    .min_by_key(|(_, (used, _))| *used)
                    .map(|(account, _)| account.clone())
                {
                    readers.remove(&oldest);
                }
            }
            let (used, reader) = readers
                .entry(account.into())
                .or_insert_with(|| (std::time::Instant::now(), Default::default()));
            *used = std::time::Instant::now();
            return Ok(std::sync::Arc::new(reader.read(location, keys, options)?));
        }
        Ok(std::sync::Arc::new(ProfileReader::open(
            location, keys, options,
        )?))
    }
    pub fn read_state_version(
        &self,
        account: &str,
        thread: &str,
        generation: &str,
    ) -> Result<String> {
        Self::read_state_hash(
            thread,
            generation,
            &self.store.read_version(account, thread)?,
        )
    }
    pub(crate) fn read_state_hash(thread: &str, generation: &str, version: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(generation, thread, version))?)
        ))
    }
    pub fn profile_schema(&self, account: &str) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        Ok(
            json!({"accountId":account,"sourceGeneration":reader.generation(),"schemas":reader.schemas(),"coverage":reader.coverage()}),
        )
    }
    pub fn threads(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let page = reader.threads(q)?;
        let ids: Vec<_> = page
            .items
            .iter()
            .map(|thread| thread.username.clone())
            .collect();
        let versions = self.store.read_versions(account, &ids)?;
        let mut items:Vec<_>=page.items.into_iter().map(|t|Ok(json!({"id":t.username,"peerId":t.username,"accountId":account,"kind":if t.username.ends_with("@chatroom") {"group"}else{"direct"},"displayName":t.label,"unreadCount":t.unread_count,"readStateVersion":Self::read_state_hash(&t.username,reader.generation(),versions.get(&t.username).map(String::as_str).unwrap_or("0"))?,"history":{"coverage":"partial","localCount":null},"version":reader.generation()}))).collect::<Result<Vec<_>>>()?;
        self.thread_summaries(
            account,
            &mut items,
            &reader,
            self.profile_username(account)?.as_deref(),
        )?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn contacts(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let page = reader.contacts(q)?;
        let mut items:Vec<_>=page.items.into_iter().filter(|c|!c.is_group).map(|c|json!({"id":c.username,"username":c.username,"displayName":c.label,"nickname":c.nickname,"remark":c.remark,"alias":c.alias,"relation":"unknown"})).collect();
        self.classify_peers(&mut items, &reader)?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn groups(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let page = reader.groups(q)?;
        let mut items:Vec<_>=page.items.into_iter().map(|g|json!({"id":g.username,"roomId":g.username,"displayName":g.label,"ownerId":g.owner,"announcement":g.announcement})).collect();
        self.classify_peers(&mut items, &reader)?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn group_members(&self, account: &str, room: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let page = reader.group_members(room, q)?;
        let mut items:Vec<_>=page.items.into_iter().map(|m|json!({"id":m.username,"username":m.username,"displayName":m.label,"roomDisplayName":m.room_display_name,"mentionName":crate::mention_send::mention_name(&m)})).collect();
        self.classify_peers(&mut items, &reader)?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn messages(
        &self,
        account: &str,
        thread: &str,
        limit: Option<usize>,
        cursor: Option<String>,
    ) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let own = self.profile_username(account)?;
        let page = reader.messages(&MessageQuery {
            chat: Some(thread.into()),
            own_username: own.clone(),
            limit,
            cursor,
            ..Default::default()
        })?;
        let items = self
            .normalize_recalled_batch(account, page.items, own.as_deref(), &reader)?
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn message(&self, account: &str, id: &str) -> Result<Value> {
        let reader = self.profile_reader_owned(account)?;
        let own = self.profile_username(account)?;
        self.normalize_recalled(
            account,
            reader.message(&source_message_id(id)?, own.as_deref())?,
            own.as_deref(),
            &reader,
        )
    }
    pub fn image(
        &self,
        account: &str,
        id: &str,
        quality: wechat_profile::ImageQuality,
    ) -> Result<wechat_profile::CachedImage> {
        self.ensure_message_not_recalled(account, id)?;
        let result = self
            .profile_reader_owned(account)?
            .cached_image(&source_message_id(id)?, quality)?;
        self.ensure_message_not_recalled(account, id)?;
        Ok(result)
    }
    pub fn avatar(&self, account: &str, username: &str) -> Result<wechat_profile::CachedBytes> {
        Ok(self.profile_reader_owned(account)?.avatar(username)?)
    }
}

pub(crate) fn plain_system_notice(text: &str) -> Option<&str> {
    (!text.trim().is_empty()
        && text.len() <= 16 * 1024
        && !text.contains(['<', '>'])
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')))
    .then_some(text)
}

pub(crate) fn normalized_message(
    account: &str,
    msg: wechat_profile::Message,
    own: Option<&str>,
    generation: &str,
) -> Result<Value> {
    let thread = msg.peer.clone();
    let system = matches!(msg.message_type, 10000 | 10002);
    let mut text = if msg.message_type == 1 {
        msg.content.text.clone()
    } else if msg.message_type == 10000 {
        msg.content
            .text
            .as_deref()
            .and_then(plain_system_notice)
            .map(str::to_owned)
    } else {
        None
    };
    if thread.as_ref().is_some_and(|s| s.ends_with("@chatroom")) {
        if let (Some(sender), Some(body)) = (&msg.sender, &text) {
            if let Some(rest) = body.strip_prefix(&format!("{sender}:\n")) {
                text = Some(rest.to_owned());
            }
        }
    }
    let created = chrono::DateTime::from_timestamp(msg.create_time, 0).map(|d| d.to_rfc3339());
    let app = crate::quotes::message_app(&msg);
    let location = crate::locations::message_location(&msg);
    if let Some(location) = &location {
        text = Some(location.body());
    }
    let forwarded = crate::quotes::forwarded_body(&msg);
    let app_type = (msg.message_type == 49)
        .then(|| {
            let stored = msg.local_type.parse::<u64>().unwrap_or(0) >> 32;
            if stored > 0 {
                Some(stored)
            } else if forwarded.is_some() {
                Some(19)
            } else {
                app.as_ref().map(|a| a.subtype as u64)
            }
        })
        .flatten();
    let quote = app
        .as_ref()
        .filter(|a| a.subtype == 57)
        .and_then(|a| crate::quotes::embedded_quote(a, own));
    let link = app.as_ref().and_then(|a| a.link.as_ref()).map(|l| &l.card);
    if let Some(link) = link {
        text = Some(link.body());
    }
    if let Some(body) = app.as_ref().and_then(|a| a.text.as_ref()) {
        text = Some(body.clone());
    }
    if forwarded.is_some() {
        text = forwarded;
    }
    // System notices may carry a transport peer in the official row. That
    // peer is not the author of the notice. Recall normalization separately
    // restores an original author only when its own evidence establishes one.
    let sender = if system { None } else { msg.sender };
    let direction = if system {
        None
    } else {
        msg.outgoing
            .map(|out| if out { "outgoing" } else { "incoming" })
    };
    let mut value = json!({"id":message_id(&msg.id)?,"accountId":account,"threadId":thread,"peerId":thread,"serverId":msg.server_message_id,"senderId":sender,"direction":direction,"type":msg.message_type,"appMessageType":app_type,"quote":quote,"link":link,"body":text,"createdAt":created,"mentions":mentions(msg.source.text.as_deref(),own),"media":[],"version":generation,"projection":"official-profile"});
    if let Some(location) = location {
        value["location"] = serde_json::to_value(location)?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_system_notices_keep_their_text_without_attributing_a_transport_peer() {
        use wechat_profile::{DecodeState, DecodedField, Message};
        let field = |text| DecodedField {
            text,
            state: DecodeState::Utf8,
            compression_flag: None,
        };
        let mut message = Message {
            id: SourceMessageId {
                database: "message/message_0.db".into(),
                table: "Msg_example".into(),
                local_id: "1".into(),
            },
            server_message_id: Some("42".into()),
            server_id_signed: Some("42".into()),
            peer: Some("peer".into()),
            sender: Some("peer".into()),
            outgoing: Some(false),
            message_type: 10000,
            local_type: "10000".into(),
            create_time: 100,
            sort_seq: None,
            content: field(Some("Location sharing has ended.".into())),
            source: field(None),
            raw_fields: Default::default(),
        };
        let value = normalized_message("example", message.clone(), Some("self"), "1").unwrap();
        assert_eq!(value["body"], "Location sharing has ended.");
        assert!(value["senderId"].is_null() && value["direction"].is_null());
        for text in [
            "<sysmsg><private>transport</private></sysmsg>".into(),
            "invalid\0notice".into(),
            "x".repeat(16385),
        ] {
            message.content.text = Some(text);
            assert!(
                normalized_message("example", message.clone(), Some("self"), "1").unwrap()["body"]
                    .is_null()
            );
        }
        message.content.text = Some("Ordinary text".into());
        message.message_type = 1;
        message.local_type = "1".into();
        let ordinary = normalized_message("example", message.clone(), Some("self"), "1").unwrap();
        assert_eq!(ordinary["senderId"], "peer");
        assert_eq!(ordinary["direction"], "incoming");
        message.message_type = 10002;
        message.local_type = "10002".into();
        let control = normalized_message("example", message, Some("self"), "1").unwrap();
        assert!(
            control["body"].is_null()
                && control["senderId"].is_null()
                && control["direction"].is_null()
        );
    }
    #[test]
    fn mentions_use_exact_direct_source_ids() {
        assert_eq!(
            mentions(
                Some("<msgsource><atuserlist><![CDATA[self,other]]></atuserlist></msgsource>"),
                Some("self")
            )["self"],
            true
        );
        assert_eq!(
            mentions(
                Some("<msgsource><atuserlist>self-more</atuserlist></msgsource>"),
                Some("self")
            )["self"],
            false
        );
        assert_eq!(
            mentions(
                Some("<msgsource><quote><atuserlist>self</atuserlist></quote></msgsource>"),
                Some("self")
            )["self"],
            false
        );
        assert_eq!(mentions(Some("@self"), Some("self"))["state"], "unknown");
        assert_eq!(
            mentions(Some("<!DOCTYPE x><msgsource/>"), Some("self"))["state"],
            "unknown"
        );
        assert_eq!(mentions(Some("<msgsource/>"), Some("self"))["all"], false);
        let all = mentions(
            Some("<msgsource><atuserlist>notify@all,other</atuserlist></msgsource>"),
            Some("self"),
        );
        assert_eq!(all["all"], true);
        assert_eq!(all["self"], false);
        assert_eq!(all["userIds"], json!(["other"]));
        for invalid in ["<msgsource><atuserlist>self<x/></atuserlist></msgsource>", "<msgsource><atuserlist>self</atuserlist><atuserlist>notify@all</atuserlist></msgsource>"] {
            assert_eq!(mentions(Some(invalid), Some("self"))["state"], "unknown");
        }
    }
    #[test]
    fn message_ids_preserve_full_scoped_decimal_values() {
        let id = SourceMessageId {
            database: "message/message_0.db".into(),
            table: "Msg_synthetic".into(),
            local_id: "18446744073709551615".into(),
        };
        assert_eq!(source_message_id(&message_id(&id).unwrap()).unwrap(), id);
    }
}
