//! Quote references never grant lookup in another conversation. Embedded image
//! metadata uses the same validated, server-owned transfer path as ordinary media.
use crate::{source_message_id, Core};
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub(crate) struct AppMessage {
    pub subtype: i32,
    pub title: Option<String>,
    pub reference: Option<(String, i32)>,
    pub embedded: Option<String>,
    pub sender_id: Option<String>,
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    let mut matches = node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == name);
    let first = matches.next()?;
    (matches.next().is_none() && first.tag_name().namespace().is_none()).then_some(first)
}
fn leaf(node: roxmltree::Node<'_, '_>) -> Option<String> {
    if node.children().any(|n| n.is_element()) {
        return None;
    }
    Some(
        node.children()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect(),
    )
}
pub(crate) fn app_message(content: &str) -> Option<AppMessage> {
    if content.len() > 256 * 1024
        || content.to_ascii_uppercase().contains("<!DOCTYPE")
        || content.to_ascii_uppercase().contains("<!ENTITY")
    {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        content,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    if doc.descendants().count() > 4096
        || doc
            .descendants()
            .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    let root = doc.root_element();
    if root.tag_name().name() != "msg" || root.tag_name().namespace().is_some() {
        return None;
    }
    let app = child(root, "appmsg")?;
    let subtype = leaf(child(app, "type")?)?.trim().parse::<i32>().ok()?;
    if subtype <= 0 {
        return None;
    }
    let title = child(app, "title")
        .and_then(leaf)
        .filter(|s| s.len() <= 16384 && !s.contains('\0'));
    let reference = (|| {
        let reference = child(app, "refermsg")?;
        let id = leaf(child(reference, "svrid")?)?;
        let id = id.trim();
        let number = id.parse::<u64>().ok()?;
        if number == 0 || number.to_string() != id {
            return None;
        }
        let kind = leaf(child(reference, "type")?)?
            .trim()
            .parse::<i32>()
            .ok()?;
        if kind <= 0 {
            return None;
        }
        Some((id.into(), kind))
    })();
    Some(AppMessage {
        subtype,
        title,
        reference,
        embedded: child(app, "refermsg")
            .and_then(|r| child(r, "content"))
            .and_then(leaf)
            .filter(|s| s.len() <= 128 * 1024 && !s.contains('\0')),
        sender_id: child(app, "refermsg")
            .and_then(|r| {
                let from = child(r, "fromusr").and_then(leaf)?;
                if from.ends_with("@chatroom") {
                    child(r, "chatusr").and_then(leaf)
                } else {
                    Some(from)
                }
            })
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 256
                    && !s.chars().any(|c| c.is_control() || c.is_whitespace())
            }),
    })
}

pub(crate) fn quoted_image_content(msg: &wechat_profile::Message) -> Option<String> {
    let app = message_app(msg)?;
    if app.subtype != 57 || app.reference.as_ref()?.1 != 3 {
        return None;
    }
    let content = app.embedded?;
    let content = app
        .sender_id
        .as_ref()
        .and_then(|sender| content.strip_prefix(&format!("{sender}:\n")))
        .unwrap_or(&content);
    let doc = roxmltree::Document::parse_with_options(
        content,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "msg"
        || root.tag_name().namespace().is_some()
        || child(root, "img").is_none()
    {
        return None;
    }
    Some(content.into())
}

pub(crate) fn message_app(msg: &wechat_profile::Message) -> Option<AppMessage> {
    if msg.message_type != 49 {
        return None;
    }
    let mut text = msg.content.text.as_deref()?;
    if msg
        .peer
        .as_deref()
        .is_some_and(|s| s.ends_with("@chatroom"))
    {
        if let Some(sender) = &msg.sender {
            text = text.strip_prefix(&format!("{sender}:\n")).unwrap_or(text);
        }
    }
    let parsed = app_message(text)?;
    let stored = msg.local_type.parse::<u64>().ok()? >> 32;
    (stored == 0 || stored == parsed.subtype as u64).then_some(parsed)
}

pub(crate) fn forwarded_body(msg: &wechat_profile::Message) -> Option<String> {
    if msg.message_type != 49 {
        return None;
    }
    let content = msg.content.text.as_deref()?;
    let content = if msg
        .peer
        .as_deref()
        .is_some_and(|s| s.ends_with("@chatroom"))
    {
        msg.sender
            .as_ref()
            .and_then(|sender| content.strip_prefix(&format!("{sender}:\n")))
            .unwrap_or(content)
    } else {
        content
    };
    let stored = msg.local_type.parse::<u64>().ok()? >> 32;
    if stored != 0 && stored != 19 {
        return None;
    }
    crate::forwarded::render(content)
}

impl Core {
    /// The parent message fixes the account and conversation. Embedded fromusr,
    /// chatusr fields never broaden lookup. With no local original, bounded
    /// embedded text or image metadata stays attached to the parent quote.
    pub fn quoted_message(&self, account: &str, id: &str) -> Result<Value> {
        let reader = self.profile_reader(account)?;
        let own = self.profile_username(account)?;
        let parent = reader.message(&source_message_id(id)?, own.as_deref())?;
        let thread = parent.peer.as_deref().context("quote_thread_unavailable")?;
        let response = |state: &str, message: Value| json!({"accountId":account,"messageId":id,"threadId":thread,"state":state,"message":message});
        if self.observe_profile_recall(account, &parent)?.is_some() {
            return Ok(response("recalled", Value::Null));
        }
        let Some(app) = message_app(&parent).filter(|a| a.subtype == 57) else {
            return Ok(response("invalid", Value::Null));
        };
        let Some((server, kind)) = app.reference else {
            return Ok(response("invalid", Value::Null));
        };
        if self.quote_is_recalled(account, thread, &server, &reader, own.as_deref())? {
            return Ok(response("recalled", Value::Null));
        }
        if ![1, 3].contains(&kind) {
            return Ok(response("unsupported", Value::Null));
        }
        let mut matches = reader.messages_by_server_id(thread, &server, own.as_deref())?;
        if matches.len() > 1 {
            return Ok(response("ambiguous", Value::Null));
        }
        let Some(original) = matches.pop() else {
            let text = (kind == 1)
                .then_some(app.embedded.as_deref())
                .flatten()
                .filter(|s| !s.is_empty() && s.len() <= 16384);
            let image = kind == 3 && quoted_image_content(&parent).is_some();
            if text.is_some() || image {
                let mut result = response("embedded", Value::Null);
                result["embeddedText"] = json!(text);
                result["embeddedImage"] = json!(image);
                result["senderId"] = json!(app.sender_id);
                return Ok(result);
            }
            return Ok(response("missing", Value::Null));
        };
        if original.id == parent.id || original.message_type != i64::from(kind) {
            return Ok(response("invalid", Value::Null));
        }
        Ok(response(
            "resolved",
            self.normalize_recalled(account, original, own.as_deref(), reader.generation())?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_text_and_images_from_parent_chat_without_using_embedded_content() {
        use md5::Digest;
        use std::sync::Arc;
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let profile = temp.path().join("fixture");
        let password = [42u8; 32];
        let (_, keys, _) = wechat_profile::initialize_profile(&profile, &password).unwrap();
        let db =
            rusqlite::Connection::open(profile.join("db_storage/message/message_0.db")).unwrap();
        assert_eq!(
            unsafe { rusqlite::ffi::sqlite3_key(db.handle(), password.as_ptr().cast(), 32) },
            0
        );
        db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000; INSERT INTO Name2Id(rowid,user_name,is_session)VALUES(1,'room@chatroom',1),(2,'other',1),(3,'sender',0);").unwrap();
        let table = format!("Msg_{:x}", md5::Md5::digest(b"room@chatroom"));
        let other = format!("Msg_{:x}", md5::Md5::digest(b"other"));
        for table in [&table, &other] {
            db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER)")).unwrap();
        }
        db.execute_batch(&format!("INSERT INTO {table} VALUES(1,-1,1,1,3,'sender:'||char(10)||'original stored text','',0,0),(2,2,3,2,3,'private image transport','',0,0); INSERT INTO {other} VALUES(1,999,1,1,3,'foreign must not leak','',0,0)")).unwrap();
        for (index, server, kind) in [
            (10, "18446744073709551615", 1),
            (11, "2", 3),
            (12, "999", 1),
            (13, "0", 1),
            (14, "2", 1),
            (15, "2", 49),
        ] {
            let xml = format!("sender:\n<msg><appmsg><type>57</type><title>current comment</title><refermsg><type>{kind}</type><svrid>{server}</svrid><chatusr>other</chatusr><fromusr>foreign</fromusr><content>forged embedded text</content></refermsg></appmsg></msg>");
            db.execute(&format!("INSERT INTO {table} VALUES(?1,?1,?2,?1,3,?3,'<msgsource><quote><atuserlist>self</atuserlist></quote></msgsource>',0,0)"), rusqlite::params![index, (57i64 << 32) | 49, xml]).unwrap();
        }
        let embedded = "<msg><appmsg><type>57</type><title>current image comment</title><refermsg><type>3</type><svrid>998</svrid><fromusr>room@chatroom</fromusr><chatusr>sender</chatusr><content><![CDATA[sender:\n<msg><img aeskey='01010101010101010101010101010101' cdnmidimgurl='synthetic-cdn-id' length='123'/></msg>]]></content></refermsg></appmsg></msg>";
        db.execute(
            &format!("INSERT INTO {table} VALUES(16,16,?1,16,3,?2,'',0,0)"),
            rusqlite::params![(57i64 << 32) | 49, embedded],
        )
        .unwrap();
        db.execute(
            &format!("INSERT INTO {table} VALUES(17,17,?1,17,3,?2,'',0,0)"),
            rusqlite::params![
                (19i64 << 32) | 49,
                include_str!("../tests/fixtures/merged-forward.xml")
            ],
        )
        .unwrap();
        drop(db);
        let store = Arc::new(wechat_store::Store::open(&temp.path().join("data")).unwrap());
        store.attach("a", &profile, true).unwrap();
        let key_path = temp.path().join("keys.json");
        keys.write_new(&key_path).unwrap();
        store.set_keys_file("a", &key_path).unwrap();
        let core = Core::new(store, None).unwrap();
        core.create_account("b").unwrap();
        let id = |local: i32| {
            crate::message_id(&wechat_profile::SourceMessageId {
                database: "message/message_0.db".into(),
                table: table.clone(),
                local_id: local.to_string(),
            })
            .unwrap()
        };
        let parent = core.message("a", &id(10)).unwrap();
        assert_eq!(parent["body"], "current comment");
        assert_eq!(parent["appMessageType"], 57);
        assert_eq!(parent["quote"]["serverId"], u64::MAX.to_string());
        assert_eq!(parent["mentions"]["self"], Value::Null);
        let original = core.quoted_message("a", &id(10)).unwrap();
        assert_eq!(original["state"], "resolved");
        assert_eq!(original["message"]["body"], "original stored text");
        assert!(!original.to_string().contains("forged"));
        assert_eq!(
            core.quoted_message("a", &id(11)).unwrap()["message"]["type"],
            3
        );
        for (local, state) in [
            (12, "embedded"),
            (13, "invalid"),
            (14, "invalid"),
            (15, "unsupported"),
        ] {
            let result = core.quoted_message("a", &id(local)).unwrap();
            assert_eq!(result["state"], state);
            assert!(result["message"].is_null());
        }
        let embedded = core.quoted_message("a", &id(16)).unwrap();
        assert_eq!(embedded["state"], "embedded");
        assert_eq!(embedded["embeddedImage"], true);
        assert!(!embedded.to_string().contains("synthetic-cdn-id"));
        let assets = core.media_assets("a", &id(16)).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].message_id.as_deref(), Some(id(16).as_str()));
        assert_eq!(assets[0].official_writeback, "unsupported");
        assert!(!serde_json::to_string(&assets)
            .unwrap()
            .contains("0101010101010101"));
        let forwarded = core.message("a", &id(17)).unwrap();
        assert_eq!(forwarded["appMessageType"], 19);
        assert!(forwarded["body"]
            .as_str()
            .unwrap()
            .contains("[Chat History]"));
        assert!(core.quoted_message("b", &id(10)).is_err());
    }
    #[test]
    fn quote_fields_are_direct_bounded_and_unsigned() {
        let xml = "<msg><appmsg><type>57</type><title><![CDATA[look at this]]></title><refermsg><type>3</type><svrid>18446744073709551615</svrid><chatusr>foreign</chatusr><content>&lt;private-url&gt;</content></refermsg></appmsg></msg>";
        let parsed = app_message(xml).unwrap();
        assert_eq!(parsed.title.as_deref(), Some("look at this"));
        assert_eq!(parsed.reference, Some((u64::MAX.to_string(), 3)));
        for id in ["0", "01", "-1", "18446744073709551616", "1e3"] {
            assert!(app_message(&xml.replace("18446744073709551615", id))
                .unwrap()
                .reference
                .is_none());
        }
        assert!(
            app_message(&xml.replace("<svrid>", "<svrid>2</svrid><svrid>"))
                .unwrap()
                .reference
                .is_none()
        );
        assert!(
            app_message(&xml.replace("<type>57</type>", "<nested><type>57</type></nested>"))
                .is_none()
        );
        assert!(app_message(&format!("<!DOCTYPE msg>{xml}")).is_none());
        assert!(app_message(&xml.replace("<msg>", "<msg xmlns='foreign'>")).is_none());
        assert!(app_message(&format!("<msg>{}</msg>", "<n>".repeat(40))).is_none());
    }
}
