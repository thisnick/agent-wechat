//! Received shared cards keep their text independent of optional thumbnail bytes.
use crate::quotes::{child, leaf};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use wechat_protocol::media::{MediaDescriptor, MediaQuality};

/// A share card to send: a link with a title, an optional description and an optional thumbnail
/// address. Nothing is fetched from the linked page.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum ShareSendRequest {
    /// Send a link card to a chat.
    Share {
        /// ID of the chat to send to.
        #[serde(rename = "threadId")]
        thread_id: String,
        /// Card title. It must not be blank.
        title: String,
        /// Address the card opens. It must be an http or https address.
        url: String,
        /// Text shown under the title. Optional.
        #[serde(default)]
        #[ts(optional)]
        description: Option<String>,
        /// Address of a preview image for the card. It must be an http or https address without a
        /// port or fragment.
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

/// Details of a shared link, article, music or video card, or a Channels post.
#[derive(Clone, Debug, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LinkCard {
    /// Card title. Null when there is none.
    pub title: Option<String>,
    /// Card description or summary. Null when there is none.
    pub description: Option<String>,
    /// Address the card opens. Null when the card has none or it is not a valid http or https
    /// address.
    pub url: Option<String>,
    /// Name of the site or account that published the card, such as a Channels account. Null when
    /// there is none.
    pub source_name: Option<String>,
    /// Length of a Channels video in seconds. Omitted for other cards and when the card does not
    /// say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub duration_seconds: Option<u32>,
}
impl LinkCard {
    fn link_body(&self) -> String {
        let mut parts = vec![self
            .title
            .as_ref()
            .map(|s| format!("[Link] {s}"))
            .unwrap_or_else(|| "[Link]".into())];
        parts.extend(self.description.iter().cloned());
        parts.extend(self.url.iter().cloned());
        parts.join("\n")
    }
    fn channels_body(&self, video: bool) -> String {
        let mut body = String::from(if video {
            "[Channels video]"
        } else {
            "[Channels]"
        });
        if let Some(name) = &self.source_name {
            body.push(' ');
            body.push_str(name);
        }
        if let Some(description) = &self.description {
            body.push_str(if self.source_name.is_some() {
                ": "
            } else {
                " "
            });
            body.push_str(description);
        }
        if let Some(seconds) = self.duration_seconds {
            body.push_str(&format!(" ({}:{:02})", seconds / 60, seconds % 60));
        }
        body
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum LinkThumbnail {
    Cdn(MediaDescriptor),
    Http(String),
}
/// A Channels post that carries exactly one video this client can retrieve on request.
///
/// The identifiers locate the post through the authenticated detail request. They are not
/// public card fields.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ChannelsVideo {
    pub object_id: u64,
    pub nonce: String,
}
impl std::fmt::Debug for ChannelsVideo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelsVideo(<redacted>)")
    }
}
pub(crate) struct ParsedLink {
    pub card: LinkCard,
    pub thumbnail: Option<LinkThumbnail>,
    /// Set for a subtype-51 Channels card, with the retrievable video when there is one.
    pub channels: Option<Option<ChannelsVideo>>,
}
impl ParsedLink {
    /// Plain-text rendering used as the message body and quote text.
    pub(crate) fn body(&self) -> String {
        match &self.channels {
            Some(video) => self.card.channels_body(video.is_some()),
            None => self.card.link_body(),
        }
    }
}
pub(crate) fn text(node: roxmltree::Node<'_, '_>, name: &str, max: usize) -> Option<String> {
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
    match subtype {
        3..=5 => {}
        51 => return parse_channels(app),
        _ => return None,
    }
    let card = LinkCard {
        title: text(app, "title", 16384),
        description: text(app, "des", 16384),
        url: text(app, "url", 8192)
            .filter(|s| wechat_protocol::media::validate_shared_link_url(s).is_ok()),
        source_name: text(app, "sourcedisplayname", 1024),
        duration_seconds: None,
    };
    if card.title.is_none() && card.description.is_none() && card.url.is_none() {
        return None;
    }
    Some(ParsedLink {
        card,
        thumbnail: thumbnail(app),
        channels: None,
    })
}

fn canonical_u64(value: &str) -> Option<u64> {
    let number = value.parse::<u64>().ok()?;
    (number != 0 && number.to_string() == value).then_some(number)
}

/// A Channels card's own `title` and `url` hold WeChat's upgrade notice for clients that cannot
/// render the post, so the card is built from `finderFeed` instead.
fn parse_channels(app: roxmltree::Node<'_, '_>) -> Option<ParsedLink> {
    let finder = child(app, "finderFeed")?;
    let media = child(finder, "mediaList").and_then(|list| child(list, "media"));
    let number = |node: roxmltree::Node<'_, '_>, name: &str| {
        text(node, name, 20).and_then(|value| value.parse::<u64>().ok())
    };
    let duration_seconds = media
        .and_then(|media| number(media, "videoPlayDuration"))
        .filter(|seconds| *seconds > 0)
        .and_then(|seconds| u32::try_from(seconds).ok());
    let card = LinkCard {
        title: None,
        description: text(finder, "desc", 16384),
        url: None,
        source_name: text(finder, "nickname", 1024),
        duration_seconds,
    };
    let video = (|| {
        let object_id = canonical_u64(&text(finder, "objectId", 20)?)?;
        let nonce = text(finder, "objectNonceId", 256)?;
        let prefix = nonce.split('_').next()?;
        if prefix.is_empty() || !prefix.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        prefix.parse::<u64>().ok()?;
        let media = media?;
        let video_shape = number(finder, "feedType") == Some(4)
            && number(finder, "mediaCount") == Some(1)
            && number(media, "mediaType") == Some(4);
        video_shape.then_some(ChannelsVideo { object_id, nonce })
    })();
    if card.description.is_none() && card.source_name.is_none() && video.is_none() {
        return None;
    }
    let cover = media.and_then(|media| {
        ["coverUrl", "thumbUrl"].into_iter().find_map(|name| {
            text(media, name, 8192)
                .filter(|url| wechat_protocol::media::validate_link_thumbnail_url(url).is_ok())
        })
    });
    Some(ParsedLink {
        card,
        thumbnail: cover.map(LinkThumbnail::Http).or_else(|| thumbnail(app)),
        channels: Some(video),
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
    fn channels_cards_use_the_post_and_keep_transport_private() {
        // WeChat fills a Channels card's own title and url with an upgrade notice for clients
        // that cannot render the post; the card comes from finderFeed instead.
        let xml = "<msg><appmsg><type>51</type><title>Upgrade notice</title><url>https://example.invalid/update/</url><finderFeed><objectId>18446744073709551000</objectId><feedType>4</feedType><nickname>Publisher</nickname><desc>Post &amp; description</desc><mediaCount>1</mediaCount><objectNonceId>12345678901234567890_0_38_12_4_private-context</objectNonceId><mediaList><media><thumbUrl>https://example.invalid/private-thumb</thumbUrl><videoPlayDuration>96</videoPlayDuration><url>https://example.invalid/private-video</url><coverUrl>https://example.invalid/private-cover</coverUrl><mediaType>4</mediaType></media></mediaList></finderFeed></appmsg></msg>";
        let link = app_message(xml).unwrap().link.unwrap();
        assert_eq!(link.card.title, None);
        assert_eq!(link.card.url, None);
        assert_eq!(link.card.description.as_deref(), Some("Post & description"));
        assert_eq!(link.card.source_name.as_deref(), Some("Publisher"));
        assert_eq!(link.card.duration_seconds, Some(96));
        assert_eq!(
            link.body(),
            "[Channels video] Publisher: Post & description (1:36)"
        );
        let video = link.channels.clone().unwrap().unwrap();
        assert_eq!(video.object_id, 18_446_744_073_709_551_000);
        assert_eq!(
            video.nonce,
            "12345678901234567890_0_38_12_4_private-context"
        );
        assert_eq!(format!("{video:?}"), "ChannelsVideo(<redacted>)");
        assert!(matches!(
            link.thumbnail,
            Some(LinkThumbnail::Http(ref url)) if url == "https://example.invalid/private-cover"
        ));
        let public = serde_json::to_string(&link.card).unwrap();
        assert!(public.contains("\"durationSeconds\":96"));
        for private in [
            "Upgrade notice",
            "update/",
            "18446744073709551000",
            "private-context",
            "private-video",
            "private-cover",
            "private-thumb",
        ] {
            assert!(!public.contains(private), "{private}");
        }

        // A missing or unsafe cover falls back to the thumbnail URL.
        let thumb = app_message(&xml.replace(
            "https://example.invalid/private-cover",
            "https://example.invalid:8443/cover",
        ))
        .unwrap()
        .link
        .unwrap();
        assert!(matches!(
            thumb.thumbnail,
            Some(LinkThumbnail::Http(ref url)) if url == "https://example.invalid/private-thumb"
        ));

        // Shapes this client cannot play stay text-only cards.
        for unsupported in [
            xml.replace("<feedType>4</feedType>", "<feedType>2</feedType>"),
            xml.replace("<mediaCount>1</mediaCount>", "<mediaCount>3</mediaCount>"),
            xml.replace("<mediaType>4</mediaType>", "<mediaType>2</mediaType>"),
            xml.replace("18446744073709551000", "018446744073709551000"),
            xml.replace("18446744073709551000", "18446744073709551616"),
            xml.replace("12345678901234567890_", "not-a-number_"),
            xml.replace("<objectId>18446744073709551000</objectId>", ""),
        ] {
            let link = app_message(&unsupported).unwrap().link.unwrap();
            assert_eq!(link.channels, Some(None), "{unsupported}");
            assert!(link.body().starts_with("[Channels] Publisher: "));
        }
        let silent = app_message(&xml.replace("<videoPlayDuration>96</videoPlayDuration>", ""))
            .unwrap()
            .link
            .unwrap();
        assert_eq!(silent.card.duration_seconds, None);
        assert_eq!(
            silent.body(),
            "[Channels video] Publisher: Post & description"
        );

        for bad in [
            xml.replace("<finderFeed>", "<finderFeed/><finderFeed>"),
            xml.replace("finderFeed", "unknownFeed"),
        ] {
            assert!(app_message(&bad).unwrap().link.is_none());
        }
    }

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
            assert_eq!(link.body(), "[Link] Title & preview\nDescription <kept as text>\nhttps://example.invalid/article?a=1&b=2");
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
            assert!(link.body().contains("Title & preview"));
        }
        let minimal = "<msg><appmsg><type>5</type><title>Only title</title><url>javascript:alert(1)</url><thumburl>http://127.0.0.1/private</thumburl></appmsg></msg>";
        let link = app_message(minimal).unwrap().link.unwrap();
        assert_eq!(link.body(), "[Link] Only title");
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
