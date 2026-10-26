//! Receive-only shared stickers. Transport descriptors stay inside the hub.
use crate::quotes::{child, leaf};
use base64::Engine;
use wechat_protocol::{media::StickerDescriptor, protobuf as pb};

pub(crate) struct SharedSticker {
    pub original: Option<StickerDescriptor>,
    pub thumbnail: Option<crate::links::LinkThumbnail>,
}

pub(crate) fn parse(app: roxmltree::Node<'_, '_>, subtype: i32) -> Option<SharedSticker> {
    if subtype != 8 {
        return None;
    }
    let thumbnail = crate::links::thumbnail(app);
    let original = (|| {
        let attach = child(app, "appattach")?;
        let value = |name| leaf(child(attach, name)?);
        let size = value("totallen")?.parse::<u64>().ok()?;
        let md5 = value("emoticonmd5")?;
        let encoded = value("emojiinfo")?;
        if encoded.len() > 32 * 1024 || !valid_identity(size, &md5) {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let fields = pb::decode(&bytes).ok()?;
        // The received EmojiInfo carries the original MD5 in field 1 and its
        // direct source in field 2. Alternate/encrypted representations aren't
        // interchangeable with that source. Require agreement with appattach.
        let identity = std::str::from_utf8(pb::data(&fields, 1).ok()?).ok()?;
        if !identity.eq_ignore_ascii_case(&md5) {
            return None;
        }
        let url = std::str::from_utf8(pb::data(&fields, 2).ok()?).ok()?;
        wechat_protocol::media::validate_sticker_url(url).ok()?;
        Some(StickerDescriptor {
            url: url.into(),
            expected_bytes: size,
            expected_md5: md5,
            expires_at: None,
        })
    })();
    (original.is_some() || thumbnail.is_some()).then_some(SharedSticker {
        original,
        thumbnail,
    })
}

fn valid_identity(size: u64, md5: &str) -> bool {
    size > 0
        && size <= 16 * 1024 * 1024
        && md5.len() == 32
        && md5.bytes().all(|b| b.is_ascii_hexdigit())
}

pub(crate) fn ordinary(content: &str) -> Option<StickerDescriptor> {
    if content.len() > 128 * 1024
        || content.to_ascii_uppercase().contains("<!DOCTYPE")
        || content.to_ascii_uppercase().contains("<!ENTITY")
    {
        return None;
    }
    let doc = roxmltree::Document::parse_with_options(
        content,
        roxmltree::ParsingOptions {
            nodes_limit: 4096,
            ..Default::default()
        },
    )
    .ok()?;
    let root = doc.root_element();
    if root.tag_name().name() != "msg"
        || root.tag_name().namespace().is_some()
        || doc
            .descendants()
            .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    let emoji = child(root, "emoji")?;
    let size = emoji.attribute("len")?.parse::<u64>().ok()?;
    let md5 = emoji.attribute("md5")?;
    if !valid_identity(size, md5) {
        return None;
    }
    let url = emoji.attribute("cdnurl")?;
    wechat_protocol::media::validate_sticker_url(url).ok()?;
    let expires_at = match emoji.attribute("expiretime") {
        None | Some("") => None,
        Some(value) => Some(value.parse().ok()?),
    };
    Some(StickerDescriptor {
        url: url.into(),
        expected_bytes: size,
        expected_md5: md5.into(),
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const XML: &str = include_str!("../tests/fixtures/shared-sticker.xml");
    fn original(xml: &str) -> Option<StickerDescriptor> {
        crate::quotes::app_message(xml)?.sticker?.original
    }
    #[test]
    fn shared_original_requires_matching_identity_and_one_allowed_source() {
        let descriptor = original(XML).unwrap();
        assert_eq!(descriptor.expected_bytes, 42);
        assert_eq!(descriptor.expected_md5, "0123456789abcdef0123456789abcdef");
        assert!(descriptor.url.ends_with("/synthetic-sticker"));
        for xml in [
            XML.replace("<totallen>42", "<totallen>0"),
            XML.replace("<totallen>42", "<totallen>16777217"),
            XML.replace(
                "<emoticonmd5>0123456789abcdef0123456789abcdef",
                "<emoticonmd5>00000000000000000000000000000000",
            ),
            XML.replace("<emojiinfo>", "<emojiinfo>not-base64:"),
            XML.replace("<type>8", "<type>6"),
            XML.replace(
                "</appattach>",
                "<emojiinfo>duplicate</emojiinfo></appattach>",
            ),
        ] {
            assert!(original(&xml).is_none());
        }
        for bytes in [
            pb::message(&[
                pb::bytes(1, b"0123456789abcdef0123456789abcdef"),
                pb::bytes(2, b"https://127.0.0.1/private"),
            ])
            .unwrap(),
            pb::message(&[
                pb::bytes(1, b"0123456789abcdef0123456789abcdef"),
                pb::bytes(2, b"https://snsvideo.c2c.wechat.com/a"),
                pb::bytes(2, b"https://snsvideo.c2c.wechat.com/b"),
            ])
            .unwrap(),
            vec![10, 255],
        ] {
            let doc = roxmltree::Document::parse(XML).unwrap();
            let old = doc
                .descendants()
                .find(|n| n.has_tag_name("emojiinfo"))
                .unwrap()
                .text()
                .unwrap();
            let xml = XML.replace(
                old,
                &base64::engine::general_purpose::STANDARD.encode(bytes),
            );
            assert!(original(&xml).is_none());
        }
    }
    #[test]
    fn unsupported_original_can_keep_a_valid_thumbnail_without_exposing_transport() {
        let xml = XML.replace("<emojiinfo>", "<emojiinfo>invalid:");
        let sticker = crate::quotes::app_message(&xml).unwrap().sticker.unwrap();
        assert!(sticker.original.is_none());
        assert!(sticker.thumbnail.is_some());
        assert!(crate::quotes::app_message(
            &xml.replace("01010101010101010101010101010101", "bad-key")
        )
        .unwrap()
        .sticker
        .is_none());
    }
}
