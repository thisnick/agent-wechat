//! Classify explicit message expiry without native hooks or inferred lifetimes.
use crate::ia::types::MediaResult;

/// Only the message's own extcommoninfo counts, not quoted/nested messages.
pub fn expiry_timestamp(content: &str, created_at: i64) -> Option<i64> {
    if content.len() > 1024 * 1024 || created_at <= 0 {
        return None;
    }
    let document = roxmltree::Document::parse_with_options(content, roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: 4096,
        ..Default::default()
    }).ok()?;
    let root = document.root_element();
    if !root.has_tag_name("msg") || root.tag_name().namespace().is_some() {
        return None;
    }
    let mut blocks = root.children().filter(|node| node.has_tag_name("extcommoninfo"));
    let block = blocks.next()?;
    if blocks.next().is_some() || block.tag_name().namespace().is_some() {
        return None;
    }
    let mut fields = block.children().filter(|node| node.has_tag_name("media_expire_at"));
    let field = fields.next()?;
    if fields.next().is_some() || field.tag_name().namespace().is_some()
        || field.children().any(|node| node.is_element()) {
        return None;
    }
    let text: String = field.children().filter(|node| node.is_text()).filter_map(|node| node.text()).collect();
    let value = text.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let expires_at = value.parse::<i64>().ok()?;
    // Valid Unix seconds through year 9999; reject zero, milliseconds and
    // timestamps preceding the message itself. Never invent a default TTL.
    (expires_at >= created_at && expires_at <= 253_402_300_799).then_some(expires_at)
}

/// True means the caller should return now, without submitting a CDN transfer.
/// An elapsed CDN lifetime never invalidates already verified cached bytes.
pub fn finish_expired(result: &mut MediaResult, expires_at: Option<i64>, now: i64) -> bool {
    let Some(expires_at) = expires_at.filter(|expires_at| now >= *expires_at) else {
        return false;
    };
    if result.data.is_none() {
        result.media_type = "expired".into();
        result.reason = Some("media_expiry_timestamp_elapsed".into());
        result.expires_at = Some(expires_at);
        result.retryable = Some(false);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xml(value: &str) -> String {
        format!("<msg><extcommoninfo><media_expire_at>{value}</media_expire_at></extcommoninfo></msg>")
    }

    #[test]
    fn parses_explicit_seconds_including_cdata_and_whitespace() {
        for value in ["1700000200", " 1700000200 ", "<![CDATA[1700000200]]>"] {
            assert_eq!(expiry_timestamp(&xml(value), 1_700_000_100), Some(1_700_000_200));
        }
    }

    #[test]
    fn invalid_or_ambiguous_metadata_never_proves_expiry() {
        for content in [
            xml("0"), xml("-1"), xml("+1700000200"), xml("1700000200x"),
            xml("1700000200000"), xml("99999999999999999999999999"), xml("1700000000"),
            "<msg/>".into(), "<msg><extcommoninfo>".into(),
            "<msg><appmsg><refermsg><extcommoninfo><media_expire_at>1700000200</media_expire_at></extcommoninfo></refermsg></appmsg></msg>".into(),
            "<msg><extcommoninfo><media_expire_at>1700000200</media_expire_at><media_expire_at>1700000300</media_expire_at></extcommoninfo></msg>".into(),
            format!("<msg><extcommoninfo/><extcommoninfo><media_expire_at>1700000200</media_expire_at></extcommoninfo></msg>"),
            xml("<value>1700000200</value>"),
            xml("170000<!--0200-->"),
            "<msg xmlns='untrusted'><extcommoninfo><media_expire_at>1700000200</media_expire_at></extcommoninfo></msg>".into(),
            "<!DOCTYPE msg [<!ENTITY exp '1700000200'>]><msg><extcommoninfo><media_expire_at>&exp;</media_expire_at></extcommoninfo></msg>".into(),
        ] {
            assert_eq!(expiry_timestamp(&content, 1_700_000_100), None);
        }
        assert_eq!(expiry_timestamp(&xml("1700000200"), 0), None);
    }

    #[test]
    fn unknown_and_future_expiry_leave_pending_unchanged() {
        let mut pending = MediaResult { media_type: "pending".into(), ..Default::default() };
        assert!(!finish_expired(&mut pending, None, 1_700_000_200));
        assert!(!finish_expired(&mut pending, Some(1_700_000_201), 1_700_000_200));
        assert_eq!(pending.media_type, "pending");
        assert!(pending.reason.is_none());
    }

    #[test]
    fn uncached_expired_result_is_terminal() {
        let mut result = MediaResult { media_type: "pending".into(), ..Default::default() };
        assert!(finish_expired(&mut result, Some(1_700_000_200), 1_700_000_200));
        let json = serde_json::to_value(result).unwrap();
        assert_eq!(json["type"], "expired");
        assert_eq!(json["reason"], "media_expiry_timestamp_elapsed");
        assert_eq!(json["expiresAt"], 1_700_000_200);
        assert_eq!(json["retryable"], false);
        assert!(json.get("data").is_none());
    }

    #[test]
    fn expiry_keeps_any_available_best_image_variant_and_file_bytes() {
        for (kind, quality) in [("file", None), ("image", Some("standard")), ("image", Some("thumbnail"))] {
            let mut cached = MediaResult { media_type: kind.into(), data: Some("cached".into()),
                quality: quality.map(str::to_string), ..Default::default() };
            assert!(finish_expired(&mut cached, Some(1_700_000_200), 1_700_000_300));
            assert_eq!(cached.media_type, kind);
            assert_eq!(cached.data.as_deref(), Some("cached"));
            assert_eq!(cached.quality.as_deref(), quality);
            assert!(cached.reason.is_none());
        }
    }
}
