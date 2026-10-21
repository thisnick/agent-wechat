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
    json!({"state":"known","userIds":ids,"self":own.map(|own|ids.contains(&own)),"all":null,"source":"msgsource.atuserlist"})
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
    pub fn read_state_version(
        &self,
        account: &str,
        thread: &str,
        generation: &str,
    ) -> Result<String> {
        use sha2::{Digest, Sha256};
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                generation,
                thread,
                self.store.read_version(account, thread)?
            ))?)
        ))
    }
    pub fn profile_schema(&self, account: &str) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        Ok(
            json!({"accountId":account,"sourceGeneration":reader.generation(),"schemas":reader.schemas(),"coverage":reader.coverage()}),
        )
    }
    pub fn threads(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let page = reader.threads(q)?;
        let items:Vec<_>=page.items.into_iter().map(|t|Ok(json!({"id":t.username,"peerId":t.username,"accountId":account,"kind":if t.username.ends_with("@chatroom") {"group"}else{"direct"},"displayName":t.label,"unreadCount":t.unread_count,"readStateVersion":self.read_state_version(account,&t.username,reader.generation())?,"history":{"coverage":"partial","localCount":null},"version":reader.generation()}))).collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn contacts(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let page = reader.contacts(q)?;
        let items:Vec<_>=page.items.into_iter().filter(|c|!c.is_group).map(|c|json!({"id":c.username,"username":c.username,"displayName":c.label,"nickname":c.nickname,"remark":c.remark,"alias":c.alias,"relation":"unknown"})).collect();
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn groups(&self, account: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let page = reader.groups(q)?;
        let items:Vec<_>=page.items.into_iter().map(|g|json!({"id":g.username,"roomId":g.username,"displayName":g.label,"ownerId":g.owner,"announcement":g.announcement})).collect();
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn group_members(&self, account: &str, room: &str, q: &ListQuery) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let page = reader.group_members(room, q)?;
        let items:Vec<_>=page.items.into_iter().map(|m|json!({"id":m.username,"username":m.username,"displayName":m.label,"roomDisplayName":m.room_display_name})).collect();
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
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        let page = reader.messages(&MessageQuery {
            chat: Some(thread.into()),
            own_username: own.clone(),
            limit,
            cursor,
            ..Default::default()
        })?;
        let items = page
            .items
            .into_iter()
            .map(|msg| normalized_message(account, msg, own.as_deref(), reader.generation()))
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"items":items,"nextCursor":page.next_cursor,"sourceGeneration":page.source_generation,"coverage":page.coverage}),
        )
    }
    pub fn message(&self, account: &str, id: &str) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        normalized_message(
            account,
            reader.message(&source_message_id(id)?, own.as_deref())?,
            own.as_deref(),
            reader.generation(),
        )
    }
    pub fn image(
        &self,
        account: &str,
        id: &str,
        quality: wechat_profile::ImageQuality,
    ) -> Result<wechat_profile::CachedImage> {
        Ok(self
            .profile_reader(account)?
            .cached_image(&source_message_id(id)?, quality)?)
    }
    pub fn avatar(&self, account: &str, username: &str) -> Result<wechat_profile::CachedBytes> {
        Ok(self.profile_reader(account)?.avatar(username)?)
    }
}

pub(crate) fn normalized_message(
    account: &str,
    msg: wechat_profile::Message,
    own: Option<&str>,
    generation: &str,
) -> Result<Value> {
    let thread = msg.peer.clone();
    let mut text = if msg.message_type == 1 {
        msg.content.text.clone()
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
        .and_then(|a| a.reference.as_ref())
        .map(|(id, kind)| json!({"serverId":id,"type":kind}));
    if let Some(app) = app.filter(|a| a.subtype == 57) {
        text = app.title;
    }
    if forwarded.is_some() {
        text = forwarded;
    }
    Ok(
        json!({"id":message_id(&msg.id)?,"accountId":account,"threadId":thread,"peerId":thread,"serverId":msg.server_message_id,"senderId":msg.sender,"direction":msg.outgoing.map(|out|if out{"outgoing"}else{"incoming"}),"type":msg.message_type,"appMessageType":app_type,"quote":quote,"body":text,"createdAt":created,"mentions":mentions(msg.source.text.as_deref(),own),"media":[],"version":generation,"projection":"official-profile"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(mentions(Some("<msgsource/>"), Some("self"))["all"].is_null());
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
