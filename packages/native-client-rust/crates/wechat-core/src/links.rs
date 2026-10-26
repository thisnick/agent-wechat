//! Received shared cards keep their text independent of optional thumbnail bytes.
use crate::quotes::{child, leaf};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use wechat_protocol::media::{MediaDescriptor, MediaQuality};

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ShareSendRequest {
    Share {
        #[serde(rename = "threadId")]
        thread_id: String,
        title: String,
        url: String,
        #[serde(default)]
        #[ts(optional)]
        description: Option<String>,
        #[serde(default, rename = "thumbnailUrl")]
        #[ts(optional)]
        thumbnail_url: Option<String>,
    },
}
impl ShareSendRequest {
    pub fn destination(&self) -> &str {
        let Self::Share { thread_id, .. } = self;
        thread_id
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        let destination = self.destination();
        anyhow::ensure!(
            !destination.is_empty()
                && destination.len() <= 256
                && !destination
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
            "invalid_share_request"
        );
        self.xml("validation-only")?;
        Ok(())
    }
    pub(crate) fn xml(&self, sender: &str) -> anyhow::Result<String> {
        let Self::Share {
            title,
            url,
            description,
            thumbnail_url,
            ..
        } = self;
        wechat_protocol::media_send::share_xml(
            sender,
            title,
            url,
            description.as_deref(),
            thumbnail_url.as_deref(),
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LinkCard {
    pub title: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub source_name: Option<String>,
}
impl LinkCard {
    pub(crate) fn body(&self) -> String {
        let mut parts = vec![self
            .title
            .as_ref()
            .map(|s| format!("[Link] {s}"))
            .unwrap_or_else(|| "[Link]".into())];
        parts.extend(self.description.iter().cloned());
        parts.extend(self.url.iter().cloned());
        parts.join("\n")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum LinkThumbnail {
    Cdn(MediaDescriptor),
    Http(String),
}
pub(crate) struct ParsedLink {
    pub card: LinkCard,
    pub thumbnail: Option<LinkThumbnail>,
}
fn text(node: roxmltree::Node<'_, '_>, name: &str, max: usize) -> Option<String> {
    let value = leaf(child(node, name)?)?;
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= max
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t')))
    .then(|| value.to_owned())
}
pub(crate) fn parse(app: roxmltree::Node<'_, '_>, subtype: i32) -> Option<ParsedLink> {
    if !matches!(subtype, 3..=5) {
        return None;
    }
    let card = LinkCard {
        title: text(app, "title", 16384),
        description: text(app, "des", 16384),
        url: text(app, "url", 8192)
            .filter(|s| wechat_protocol::media::validate_shared_link_url(s).is_ok()),
        source_name: text(app, "sourcedisplayname", 1024),
    };
    if card.title.is_none() && card.description.is_none() && card.url.is_none() {
        return None;
    }
    Some(ParsedLink {
        card,
        thumbnail: thumbnail(app),
    })
}

pub(crate) fn thumbnail(app: roxmltree::Node<'_, '_>) -> Option<LinkThumbnail> {
    (|| {
        let attach = child(app, "appattach")?;
        let id = text(attach, "cdnthumburl", 8192)?;
        let key = text(attach, "cdnthumbaeskey", 32)?;
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let key = (0..32)
            .step_by(2)
            .map(|i| u8::from_str_radix(&key[i..i + 2], 16).ok())
            .collect::<Option<Vec<_>>>()?;
        let size = match text(attach, "cdnthumblength", 20) {
            Some(value) => {
                if !value.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let size = value.parse::<u64>().ok()?;
                if size > wechat_protocol::media::MAX_LINK_THUMBNAIL_BYTES as u64 {
                    return None;
                }
                (size > 0).then_some(size)
            }
            None => None,
        };
        let md5 = text(attach, "cdnthumbmd5", 32);
        if md5
            .as_ref()
            .is_some_and(|s| s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return None;
        }
        Some(LinkThumbnail::Cdn(MediaDescriptor {
            file_id: id,
            aes_key: key,
            quality: MediaQuality::Thumbnail,
            expected_bytes: size,
            expected_md5: md5,
            strict_size: true,
            expires_at: None,
        }))
    })()
    .or_else(|| {
        let url = text(app, "thumburl", 8192)?;
        wechat_protocol::media::validate_link_thumbnail_url(&url).ok()?;
        Some(LinkThumbnail::Http(url))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quotes::app_message;

    const CARD: &str = include_str!("../tests/fixtures/shared-link.xml");

    #[test]
    fn encrypted_profile_plain_and_compressed_group_cards_use_the_same_normalization() {
        use crate::Core;
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
        db.execute_batch("PRAGMA cipher_compatibility=4;PRAGMA cipher_page_size=4096;PRAGMA kdf_iter=256000; INSERT INTO Name2Id(rowid,user_name,is_session)VALUES(1,'room@chatroom',1),(2,'sender',0);").unwrap();
        let table = format!("Msg_{:x}", md5::Md5::digest(b"room@chatroom"));
        db.execute_batch(&format!("CREATE TABLE {table}(local_id INTEGER PRIMARY KEY,server_id INTEGER,local_type INTEGER,create_time INTEGER,real_sender_id INTEGER,message_content TEXT,source TEXT,WCDB_CT_message_content INTEGER,WCDB_CT_source INTEGER)")).unwrap();
        let xml = format!("sender:\n{CARD}");
        db.execute(
            &format!("INSERT INTO {table} VALUES(1,1,?1,1,2,?2,'',0,0)"),
            rusqlite::params![(5i64 << 32) | 49, &xml],
        )
        .unwrap();
        db.execute(
            &format!("INSERT INTO {table} VALUES(2,2,?1,2,2,?2,'',4,0)"),
            rusqlite::params![
                (5i64 << 32) | 49,
                zstd::stream::encode_all(xml.as_bytes(), 3).unwrap()
            ],
        )
        .unwrap();
        // Stored subtype disagreement must not turn a file into a card.
        db.execute(
            &format!("INSERT INTO {table} VALUES(3,3,?1,3,2,?2,'',0,0)"),
            rusqlite::params![(6i64 << 32) | 49, &xml],
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
        for local in [1, 2] {
            let message = core.message("a", &id(local)).unwrap();
            assert_eq!(message["body"], "[Link] Title & preview\nDescription <kept as text>\nhttps://example.invalid/article?a=1&b=2");
            assert_eq!(message["link"]["sourceName"], "Example publisher");
            assert_eq!(message["appMessageType"], 5);
            let assets = core.media_assets("a", &id(local)).unwrap();
            assert_eq!(assets.len(), 1);
            assert_eq!(assets[0].kind, crate::media_types::MediaKind::Image);
            assert_eq!(
                assets[0].variant,
                crate::media_types::MediaVariant::Thumbnail
            );
            assert_eq!(assets[0].message_id.as_deref(), Some(id(local).as_str()));
            assert_eq!(assets[0].official_writeback, "unsupported");
            let public = format!("{message}{}", serde_json::to_string(&assets).unwrap());
            for private in [
                "synthetic-cdn-id",
                "0101010101010101",
                "private-signature",
                "<appmsg",
            ] {
                assert!(!public.contains(private));
            }
            assert!(core.media_assets("b", &id(local)).is_err());
        }
        let mismatched = core.message("a", &id(3)).unwrap();
        assert!(mismatched["link"].is_null());
        assert!(mismatched["body"].is_null());
        assert!(core.media_assets("a", &id(3)).is_err());
    }

    #[test]
    fn decodes_shared_card_fields_without_exposing_transport_metadata() {
        for subtype in [3, 4, 5] {
            let app =
                app_message(&CARD.replace("<type>5</type>", &format!("<type>{subtype}</type>")))
                    .unwrap();
            let link = app.link.unwrap();
            assert_eq!(link.card.title.as_deref(), Some("Title & preview"));
            assert_eq!(
                link.card.description.as_deref(),
                Some("Description <kept as text>")
            );
            assert_eq!(
                link.card.url.as_deref(),
                Some("https://example.invalid/article?a=1&b=2")
            );
            assert_eq!(link.card.source_name.as_deref(), Some("Example publisher"));
            assert_eq!(link.card.body(), "[Link] Title & preview\nDescription <kept as text>\nhttps://example.invalid/article?a=1&b=2");
            let LinkThumbnail::Cdn(descriptor) = link.thumbnail.unwrap() else {
                panic!("expected CDN thumbnail");
            };
            assert_eq!(descriptor.aes_key, [1; 16]);
            assert_eq!(descriptor.expected_bytes, Some(123));
            assert!(descriptor.strict_size);
            assert!(matches!(descriptor.quality, MediaQuality::Thumbnail));
            let public = serde_json::to_string(&link.card).unwrap();
            for private in [
                "synthetic-cdn-id",
                "0101010101010101",
                "preview.jpg",
                "private-signature",
            ] {
                assert!(!public.contains(private));
            }
        }
    }

    #[test]
    fn malformed_or_missing_thumbnail_never_removes_the_card() {
        for xml in [
            CARD.replace("01010101010101010101010101010101", "bad-key"),
            CARD.replace(
                "<cdnthumblength>123</cdnthumblength>",
                "<cdnthumblength>8388609</cdnthumblength>",
            ),
            CARD.replace(
                "<cdnthumblength>123</cdnthumblength>",
                "<cdnthumblength>invalid</cdnthumblength>",
            ),
        ] {
            let link = app_message(&xml).unwrap().link.unwrap();
            assert!(matches!(link.thumbnail, Some(LinkThumbnail::Http(_))));
            assert!(link.card.body().contains("Title & preview"));
        }
        let minimal = "<msg><appmsg><type>5</type><title>Only title</title><url>javascript:alert(1)</url><thumburl>http://127.0.0.1/private</thumburl></appmsg></msg>";
        let link = app_message(minimal).unwrap().link.unwrap();
        assert_eq!(link.card.body(), "[Link] Only title");
        assert!(link.card.url.is_none());
        assert!(link.thumbnail.is_none());
        assert!(
            app_message(&minimal.replace("<title>Only title</title>", ""))
                .unwrap()
                .link
                .is_none()
        );
    }

    #[test]
    fn duplicate_nested_and_oversized_fields_are_not_metadata() {
        for title in [
            "<title>one</title><title>two</title>".into(),
            "<title><b>nested</b></title>".into(),
            format!("<title>{}</title>", "a".repeat(16385)),
        ] {
            let xml = format!("<msg><appmsg><type>5</type>{title}<url>https://example.invalid/</url></appmsg></msg>");
            assert!(app_message(&xml)
                .unwrap()
                .link
                .unwrap()
                .card
                .title
                .is_none());
        }
        for xml in [
            format!("<!DOCTYPE msg>{CARD}"),
            CARD.replace("<msg>", "<msg xmlns='foreign'>"),
            CARD.replace("<type>5</type>", "<type>6</type>"),
            CARD.replace("</appmsg>", "</broken>"),
        ] {
            assert!(app_message(&xml).and_then(|app| app.link).is_none());
        }
    }
}
