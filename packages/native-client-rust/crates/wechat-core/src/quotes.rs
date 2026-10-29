//! Quote references never grant lookup in another conversation. Embedded image
//! metadata uses the same validated, server-owned transfer path as ordinary media.
use serde_json::{json, Value};

pub(crate) struct AppMessage {
    pub subtype: i32,
    pub title: Option<String>,
    pub text: Option<String>,
    pub reference: Option<(String, i32)>,
    pub embedded: Option<String>,
    pub sender_id: Option<String>,
    pub link: Option<crate::links::ParsedLink>,
    pub sticker: Option<crate::stickers::SharedSticker>,
}

pub(crate) fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    let mut matches = node
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == name);
    let first = matches.next()?;
    (matches.next().is_none() && first.tag_name().namespace().is_none()).then_some(first)
}
pub(crate) fn leaf(node: roxmltree::Node<'_, '_>) -> Option<String> {
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

/// A small readable view of the supported attribute-only shared-contact form.
pub(crate) fn contact_card_text(content: &str) -> Option<String> {
    if content.len() > 128 * 1024
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
    let root = doc.root_element();
    if root.tag_name().name() != "msg"
        || root.tag_name().namespace().is_some()
        || root
            .children()
            .any(|n| n.is_element() || n.text().is_some_and(|s| !s.trim().is_empty()))
        || root.attributes().any(|a| a.namespace().is_some())
    {
        return None;
    }
    let username = root.attribute("username")?;
    if username.is_empty()
        || username.len() > 256
        || username
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return None;
    }
    let nickname = root.attribute("nickname").unwrap_or("");
    if nickname.len() > 4096 || nickname.chars().any(char::is_control) {
        return None;
    }
    Some(format!(
        "[Shared contact]\n{}",
        if nickname.trim().is_empty() {
            username
        } else {
            nickname
        }
    ))
}

fn forwarded_summary(app: roxmltree::Node<'_, '_>) -> Option<String> {
    fn summary(record: roxmltree::Node<'_, '_>) -> Option<String> {
        if record.tag_name().name() != "recordinfo" || record.tag_name().namespace().is_some() {
            return None;
        }
        let values: Vec<_> = ["title", "desc"]
            .into_iter()
            .filter_map(|name| child(record, name).and_then(leaf))
            .filter(|value| {
                !value.trim().is_empty() && value.len() <= 16384 && !value.contains('\0')
            })
            .collect();
        let text = values.join("\n");
        (!text.is_empty() && text.len() <= 16384).then_some(text)
    }
    if let Some(title) = crate::links::text(app, "title", 16384) {
        return Some(title);
    }
    let record = child(app, "recorditem")?;
    if let Some(record) = child(record, "recordinfo") {
        return summary(record).or_else(|| crate::links::text(app, "des", 16384));
    }
    let xml = leaf(record)?;
    if xml.len() > 128 * 1024
        || xml.to_ascii_uppercase().contains("<!DOCTYPE")
        || xml.to_ascii_uppercase().contains("<!ENTITY")
    {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        &xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    if doc
        .descendants()
        .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    summary(doc.root_element()).or_else(|| crate::links::text(app, "des", 16384))
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
    let text = match subtype {
        1 | 57 => title.clone(),
        19 => forwarded_summary(app),
        // Supplied notice text only; this does not join live-location sharing.
        17 => crate::links::text(app, "title", 16384)
            .map(|title| format!("[Location sharing notice]\n{title}")),
        // A retained notice does not establish the current transfer state.
        74 => title
            .as_ref()
            .filter(|title| !title.trim().is_empty())
            .map(|title| format!("[File upload notice]\n{title}")),
        // The companion announcement field contains editor HTML. Read only
        // the plain-text field rather than exposing that document as markup.
        87 => crate::links::text(app, "textannouncement", 16384)
            .map(|text| format!("[Group announcement]\n{text}")),
        _ => None,
    };
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
        link: crate::links::parse(app, subtype),
        sticker: crate::stickers::parse(app, subtype),
        title,
        text,
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

fn embedded_content(app: &AppMessage) -> Option<&str> {
    let content = app.embedded.as_deref()?;
    Some(
        app.sender_id
            .as_ref()
            .and_then(|sender| content.strip_prefix(&format!("{sender}:\n")))
            .unwrap_or(content),
    )
}

fn embedded_image_content(app: &AppMessage) -> Option<&str> {
    if app.subtype != 57 || app.reference.as_ref()?.1 != 3 {
        return None;
    }
    let content = embedded_content(app)?;
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
    Some(content)
}

pub(crate) fn quoted_image_content(msg: &wechat_profile::Message) -> Option<String> {
    embedded_image_content(&message_app(msg)?).map(str::to_owned)
}

pub(crate) fn quoted_link(app: &AppMessage) -> Option<crate::links::ParsedLink> {
    if app.subtype != 57 || app.reference.as_ref()?.1 != 49 {
        return None;
    }
    app_message(embedded_content(app)?)?.link
}

pub(crate) fn quoted_sticker_content(msg: &wechat_profile::Message) -> Option<(i32, String)> {
    let app = message_app(msg)?;
    if app.subtype != 57 {
        return None;
    }
    let kind = app.reference.as_ref()?.1;
    let content = embedded_content(&app)?;
    let valid = match kind {
        47 => crate::stickers::ordinary(content).is_some(),
        49 => app_message(content)?.sticker.is_some(),
        _ => false,
    };
    valid.then(|| (kind, content.into()))
}

/// Public quote content is derived only from the received parent payload.
/// Original rows may supply recall evidence, never replacement quote content.
pub(crate) fn embedded_quote(app: &AppMessage, own: Option<&str>) -> Option<Value> {
    if app.subtype != 57 {
        return None;
    }
    let (server, kind) = app.reference.as_ref()?;
    let card = (*kind == 49)
        .then(|| app_message(embedded_content(app)?))
        .flatten();
    let text = if *kind == 42 {
        embedded_content(app).and_then(contact_card_text)
    } else if *kind == 48 {
        embedded_content(app)
            .and_then(crate::locations::parse)
            .map(|location| location.body())
    } else if *kind == 1 {
        app.embedded.clone()
    } else {
        card.as_ref().and_then(|card| match card.subtype {
            1 | 74 | 87 => card.text.clone(),
            3..=5 | 51 => card.link.as_ref().map(|link| link.card.body()),
            6 | 57 => card.title.clone(),
            19 => card.text.clone(),
            _ => None,
        })
    }
    .filter(|s| !s.is_empty() && s.len() <= 16384);
    let image = embedded_image_content(app).is_some();
    let sticker = (*kind == 47
        && embedded_content(app)
            .and_then(crate::stickers::ordinary)
            .is_some())
        || card.as_ref().is_some_and(|c| c.sticker.is_some());
    let supported = matches!(*kind, 1 | 3 | 42 | 47 | 48)
        || sticker
        || card
            .as_ref()
            .is_some_and(|c| matches!(c.subtype, 1 | 3..=5 | 19 | 51 | 74 | 87));
    let state = if !supported {
        "unsupported"
    } else if text.is_some() || image || sticker {
        "embedded"
    } else {
        "missing"
    };
    let mut quote = json!({"serverId":server,"type":kind,"state":state,
        "senderId":app.sender_id,"direction":app.sender_id.as_deref().zip(own)
            .map(|(sender, own)| if sender == own { "outgoing" } else { "incoming" }),
        "appMessageType":card.as_ref().map(|c| c.subtype),
        "embeddedText":text,"embeddedImage":image});
    if sticker {
        quote["embeddedSticker"] = json!(true);
    }
    if let Some(link) = card.as_ref().and_then(|c| c.link.as_ref()) {
        quote["embeddedLink"] = json!(&link.card);
    }
    Some(quote)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Core;
    const LINK: &str = "<msg><appmsg><type>5</type><title><![CDATA[Article <title>]]></title><des>A &amp; B</des><url>https://example.com/article?a=1&amp;b=2</url><appattach><cdnattachurl>private-transport</cdnattachurl></appattach></appmsg></msg>";

    #[test]
    fn forwarded_summary_uses_outer_description_when_nested_record_has_only_private_items() {
        let content = "<msg><appmsg><type>19</type><title/><des>Outer summary only</des><recorditem><recordinfo><datalist><dataitem><datadesc>Private nested text</datadesc></dataitem></datalist></recordinfo></recorditem></appmsg></msg>";
        let app = app_message(content).unwrap();
        assert_eq!(app.text.as_deref(), Some("Outer summary only"));
        let escaped = content
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let parent = app_message(&format!("<msg><appmsg><type>57</type><title>Reply</title><refermsg><type>49</type><svrid>123</svrid><content>{escaped}</content></refermsg></appmsg></msg>")).unwrap();
        let quote = embedded_quote(&parent, None).unwrap();
        assert_eq!(quote["state"], "embedded");
        assert_eq!(quote["embeddedText"], "Outer summary only");
        assert!(!quote["embeddedText"].as_str().unwrap().contains("Private"));
    }

    #[test]
    fn shared_contact_and_location_quotes_use_only_validated_embedded_fields() {
        let contact =
            "<msg username='shared_user' nickname='Shared &amp; contact' unknown='kept'/>";
        assert_eq!(
            contact_card_text(contact).as_deref(),
            Some("[Shared contact]\nShared & contact")
        );
        assert_eq!(
            contact_card_text("<msg username='shared_user'/>").as_deref(),
            Some("[Shared contact]\nshared_user")
        );
        for malformed in [
            "<msg/>",
            "<msg username='two words'/>",
            "<msg username='user' xmlns='foreign'/>",
            "<msg username='user'><appmsg/></msg>",
            "<!DOCTYPE msg><msg username='user'/>",
        ] {
            assert!(contact_card_text(malformed).is_none());
        }
        for (kind, content, excerpt) in [
            (42, contact, "[Shared contact]\nShared & contact"),
            (
                48,
                "<msg><location x='12.5' y='-45.25' poiname='Example park'/></msg>",
                "Example park",
            ),
        ] {
            let escaped = content
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            let parent = app_message(&format!("<msg><appmsg><type>57</type><title>Reply</title><refermsg><type>{kind}</type><svrid>123</svrid><content>{escaped}</content></refermsg></appmsg></msg>")).unwrap();
            let quote = embedded_quote(&parent, None).unwrap();
            assert_eq!(quote["state"], "embedded");
            assert!(quote["embeddedText"].as_str().unwrap().contains(excerpt));
            assert!(!quote["embeddedText"].as_str().unwrap().contains("<msg"));
        }
    }

    #[test]
    fn location_sharing_notice_exposes_only_the_supplied_title() {
        let xml = "<msg><appmsg><type>17</type><title>Example &amp; location sharing</title><fromusername>private-sender</fromusername></appmsg></msg>";
        let parsed = app_message(xml).unwrap();
        assert_eq!(parsed.subtype, 17);
        assert_eq!(
            parsed.text.as_deref(),
            Some("[Location sharing notice]\nExample & location sharing")
        );
        assert!(parsed.link.is_none() && parsed.reference.is_none());
        for bad in [
            xml.replace("<title>", "<title/><title>"),
            xml.replace("Example &amp; location sharing", ""),
            xml.replace("Example &amp; location sharing", "<nested>Example</nested>"),
        ] {
            assert!(app_message(&bad).unwrap().text.is_none());
        }
    }

    #[test]
    fn upload_notice_and_announcement_use_plain_fields_and_embedded_quotes() {
        for (subtype, xml, expected) in [
            (74, "<msg><appmsg><type>74</type><title>report.pdf</title><appattach><status>0</status><fileuploadtoken>private-token</fileuploadtoken></appattach></appmsg></msg>", "[File upload notice]\nreport.pdf"),
            (87, "<msg><appmsg><type>87</type><announcement><![CDATA[<script>private editor document</script>]]></announcement><textannouncement>Group &amp; announcement</textannouncement><xmlpuretext>1</xmlpuretext></appmsg></msg>", "[Group announcement]\nGroup & announcement"),
        ] {
            let app = app_message(xml).unwrap();
            assert_eq!(app.text.as_deref(), Some(expected));
            assert!(app.link.is_none());
            let content = xml.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let parent = app_message(&format!("<msg><appmsg><type>57</type><title>Comment</title><refermsg><type>49</type><svrid>42</svrid><content>{content}</content></refermsg></appmsg></msg>")).unwrap();
            let quote = embedded_quote(&parent, None).unwrap();
            assert_eq!(quote["appMessageType"], subtype);
            assert_eq!(quote["state"], "embedded");
            assert_eq!(quote["embeddedText"], expected);
            assert!(!quote.to_string().contains("private-"));
        }
        for field in [
            "<textannouncement>one</textannouncement><textannouncement>two</textannouncement>"
                .into(),
            "<textannouncement><b>nested</b></textannouncement>".into(),
            format!("<textannouncement>{}</textannouncement>", "x".repeat(16385)),
            "<announcement>HTML only has no plain fallback</announcement>".into(),
        ] {
            assert!(app_message(&format!(
                "<msg><appmsg><type>87</type>{field}</appmsg></msg>"
            ))
            .unwrap()
            .text
            .is_none());
        }
    }

    #[test]
    fn quotes_use_embedded_content_even_when_an_original_is_stored() {
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
        let insert_app = |local: i64, subtype: i64, content: &str| {
            db.execute(
                &format!("INSERT INTO {table} VALUES(?1,?1,?2,?1,3,?3,'',0,0)"),
                rusqlite::params![local, (subtype << 32) | 49, content],
            )
            .unwrap();
        };
        insert_app(18, 5, &format!("sender:\n{LINK}"));
        for (local, server, content) in [
            (19, 18, LINK.replace("Article <title>", "Quoted excerpt")),
            (20, 997, format!("sender:\n{LINK}")),
            (
                23,
                996,
                "<msg><appmsg><type>6</type><title>file</title></appmsg></msg>".to_owned(),
            ),
        ] {
            let escaped = content
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            insert_app(local, 57, &format!("<msg><appmsg><type>57</type><title>Explain this link</title><refermsg><type>49</type><svrid>{server}</svrid><fromusr>room@chatroom</fromusr><chatusr>sender</chatusr><content>{escaped}</content></refermsg></appmsg></msg>"));
        }
        insert_app(25, 57, "<msg><appmsg><type>57</type><title>Plain URL</title><refermsg><type>1</type><svrid>995</svrid><fromusr>sender</fromusr><content>https://example.com/?a=1&amp;b=2</content></refermsg></appmsg></msg>");
        let sticker = include_str!("../tests/fixtures/shared-sticker.xml");
        insert_app(26, 8, &format!("sender:\n{sticker}"));
        for (local, server, kind, embedded) in [
            (27, 991, 49, sticker.to_owned()),
            (28, 992, 47, "<msg><emoji md5='0123456789abcdef0123456789abcdef' len='42' cdnurl='https://snsvideo.c2c.wechat.com/synthetic-sticker'/></msg>".into()),
            (29, 993, 49, include_str!("../tests/fixtures/shared-link.xml").replace("<type>5", "<type>4")),
        ] {
            let embedded = embedded.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            insert_app(local, 57, &format!("<msg><appmsg><type>57</type><title>Comment</title><refermsg><type>{kind}</type><svrid>{server}</svrid><fromusr>sender</fromusr><content>{embedded}</content></refermsg></appmsg></msg>"));
        }
        for (index, kind, subtype, content) in [
            (0, 34, 0, "<msg><voicemsg voiceformat='4'/></msg>"),
            (1, 43, 0, "<msg><videomsg playlength='3'/></msg>"),
            (
                2,
                49,
                5,
                "<msg><appmsg><type>5</type><title>Link</title></appmsg></msg>",
            ),
            (
                3,
                49,
                6,
                "<msg><appmsg><type>6</type><title>File</title></appmsg></msg>",
            ),
            (
                4,
                49,
                57,
                "<msg><appmsg><type>57</type><title>Reply</title></appmsg></msg>",
            ),
        ] {
            let original_id = 30 + index;
            let server = 300 + index;
            db.execute(
                &format!("INSERT INTO {table} VALUES(?1,?2,?3,30,3,?4,'',0,0)"),
                rusqlite::params![
                    original_id,
                    server,
                    (subtype << 32) | i64::from(kind),
                    format!("sender:\n{content}")
                ],
            )
            .unwrap();
            let xml = format!("<msg><appmsg><type>57</type><title>Quote</title><refermsg><type>{kind}</type><svrid>{server}</svrid></refermsg></appmsg></msg>");
            db.execute(
                &format!("INSERT INTO {table} VALUES(?1,?1,?2,60,3,?3,'',0,0)"),
                rusqlite::params![60 + index, (57i64 << 32) | 49, xml],
            )
            .unwrap();
        }
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
        let original = &parent["quote"];
        assert_eq!(original["state"], "embedded");
        assert_eq!(original["embeddedText"], "forged embedded text");
        assert!(original.get("id").is_none());
        assert!(!original.to_string().contains("original stored text"));
        assert_eq!(
            core.message("a", &id(11)).unwrap()["quote"]["state"],
            "missing"
        );
        for (local, state) in [
            (12, Some("embedded")),
            (13, None),
            (14, Some("embedded")),
            (15, Some("unsupported")),
            (23, Some("unsupported")),
        ] {
            let result = core.message("a", &id(local)).unwrap();
            assert_eq!(result["quote"]["state"].as_str(), state);
            if state.is_none() {
                assert!(result["quote"].is_null());
            }
        }
        for index in 0..5 {
            let resolved = core.message("a", &id(60 + index)).unwrap()["quote"].clone();
            assert_eq!(resolved["state"], "unsupported");
            assert!(resolved["embeddedText"].is_null());
            assert!(!resolved.to_string().contains("<msg>"));
        }
        let embedded = core.message("a", &id(16)).unwrap()["quote"].clone();
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
        let link = core.message("a", &id(18)).unwrap();
        assert_eq!(link["appMessageType"], 5);
        assert_eq!(
            link["body"],
            "[Link] Article <title>\nA & B\nhttps://example.com/article?a=1&b=2"
        );
        let stored_link = core.message("a", &id(19)).unwrap()["quote"].clone();
        assert_eq!(stored_link["state"], "embedded");
        assert!(stored_link.get("id").is_none());
        assert!(stored_link["embeddedText"]
            .as_str()
            .unwrap()
            .contains("Quoted excerpt"));
        assert!(!stored_link["embeddedText"]
            .as_str()
            .unwrap()
            .contains("Article <title>"));
        let embedded_link = core.message("a", &id(20)).unwrap()["quote"].clone();
        assert_eq!(embedded_link["state"], "embedded");
        assert_eq!(embedded_link["embeddedText"], link["body"]);
        assert_eq!(embedded_link["senderId"], "sender");
        assert_eq!(embedded_link["embeddedImage"], false);
        assert!(!embedded_link.to_string().contains("private-transport"));
        assert_eq!(embedded_link["embeddedLink"]["title"], "Article <title>");
        for local in [26, 27, 28, 29] {
            let value = core.message("a", &id(local)).unwrap();
            if local != 26 {
                assert_eq!(value["quote"]["state"], "embedded");
                if local != 29 {
                    assert_eq!(value["quote"]["embeddedSticker"], true);
                }
            }
            if local == 29 {
                assert_eq!(value["quote"]["appMessageType"], 4);
                assert_eq!(value["quote"]["embeddedLink"]["title"], "Title & preview");
            }
            let assets = core.media_assets("a", &id(local)).unwrap();
            assert!(!assets.is_empty());
            for asset in &assets {
                assert_eq!(asset.message_id.as_deref(), Some(id(local).as_str()));
                assert_eq!(asset.official_writeback, "unsupported");
                assert!(core.media_asset("b", &asset.id).is_err());
            }
            let public = format!("{value}{}", serde_json::to_string(&assets).unwrap());
            assert!(!public.contains("snsvideo.c2c.wechat.com"));
            assert!(!public.contains("synthetic-sticker-thumbnail"));
            assert!(!public.contains("01010101010101010101010101010101"));
        }
        for (server, local) in [("991", 27), ("992", 28), ("993", 29)] {
            core.store
                .record_recall("a", "room@chatroom", server, None, None, None, "live")
                .unwrap();
            let quote = core.message("a", &id(local)).unwrap()["quote"].clone();
            assert_eq!(quote["state"], "recalled");
            assert!(quote["embeddedLink"].is_null());
            assert!(quote["embeddedSticker"].is_null());
            assert!(core.media_assets("a", &id(local)).is_err());
        }
        assert_eq!(
            core.message("a", &id(25)).unwrap()["quote"]["embeddedText"],
            "https://example.com/?a=1&b=2"
        );
        for (server, parent) in [("18", 19), ("997", 20)] {
            core.store
                .record_recall("a", "room@chatroom", server, None, None, None, "live")
                .unwrap();
            let recalled = core.message("a", &id(parent)).unwrap()["quote"].clone();
            assert_eq!(recalled["state"], "recalled");
            assert!(recalled["embeddedText"].is_null());
        }
        assert!(core.message("b", &id(10)).is_err());
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
    #[test]
    fn shared_text_quotes_use_only_the_embedded_title_and_keep_parser_bounds() {
        for (content, expected) in [
            ("<msg><appmsg><type>1</type><title>Shared &amp; text</title><des>Not the title</des><url/></appmsg></msg>".to_owned(), Some("Shared & text")),
            ("<msg><appmsg><type>1</type><title/><des>No fallback</des></appmsg></msg>".to_owned(), None),
            ("<msg><appmsg><type>1</type><title>one</title><title>two</title></appmsg></msg>".to_owned(), None),
            ("<msg><appmsg><type>1</type><title><nested>Not text</nested></title></appmsg></msg>".to_owned(), None),
            (format!("<msg><appmsg><type>1</type><title>{}</title></appmsg></msg>", "x".repeat(16385)), None),
        ] {
            let content = content.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
            let parent = app_message(&format!("<msg><appmsg><type>57</type><title>Comment</title><refermsg><type>49</type><svrid>1</svrid><content>{content}</content></refermsg></appmsg></msg>")).unwrap();
            let quote = embedded_quote(&parent, None).unwrap();
            assert_eq!(quote["embeddedText"].as_str(), expected);
            assert_eq!(quote["state"], if expected.is_some() { "embedded" } else { "missing" });
            assert!(quote["embeddedLink"].is_null());
        }
    }
}
