use anyhow::{bail, Result};
use wechat_profile::BusinessContactProjection;
use wechat_protocol::{protobuf as p, wire::ContactRecord};

const MAX_BUSINESS_TEXT: usize = 256 * 1024;

fn string(fields: &[p::Field], number: u32) -> Result<String> {
    let Some(bytes) = p::optional_data(fields, number)? else {
        return Ok(String::new());
    };
    if bytes.len() > MAX_BUSINESS_TEXT {
        bail!("business_metadata_capacity_exceeded");
    }
    Ok(std::str::from_utf8(bytes)?.to_owned())
}

/// Ordinary contact updates carry a complete business record when admitted by
/// the incoming verification flag. Missing optional metadata uses its defaults.
pub(crate) fn business_contact_patch(
    record: &ContactRecord,
) -> Result<Option<BusinessContactProjection>> {
    let fields = p::decode(&record.raw)?;
    let verify = match p::optional(&fields, 24)? {
        Some(p::Value::Varint(value)) => u32::try_from(*value)?,
        Some(_) => bail!("invalid_business_metadata"),
        None => return Ok(None),
    };
    if verify & 8 == 0 {
        return Ok(None);
    }
    let mut patch = BusinessContactProjection {
        service_type: 0,
        accept_type: 0,
        child_type: 0,
        brand_flag: 0,
        home_url: String::new(),
        external_info: String::new(),
        brand_info: String::new(),
        brand_icon_url: String::new(),
        brand_list: string(&fields, 41)?,
        belong: String::new(),
    };
    if let Some(bytes) = p::optional_data(&fields, 42)? {
        let customized = p::decode(bytes)?;
        patch.brand_flag = match p::optional(&customized, 1)? {
            Some(p::Value::Varint(value)) => u32::try_from(*value)?,
            Some(_) => bail!("invalid_business_metadata"),
            None => 0,
        };
        patch.external_info = string(&customized, 2)?;
        patch.brand_info = string(&customized, 3)?;
        patch.brand_icon_url = string(&customized, 4)?;
    }
    // The complete external document is retained in the official business row.
    // Failed/non-object JSON leaves the initialized scalar/string defaults.
    if let Ok(serde_json::Value::Object(object)) = serde_json::from_str(&patch.external_info) {
        let integer = |object: &serde_json::Map<String, serde_json::Value>, key| {
            object
                .get(key)
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0)
        };
        patch.service_type = integer(&object, "ServiceType");
        patch.accept_type = integer(&object, "ConnectorMsgType");
        if let Some(serde_json::Value::Object(enterprise)) = object.get("EnterpriseBizInfo") {
            patch.child_type = integer(enterprise, "child_type");
            patch.belong = enterprise
                .get("belong")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into();
            patch.home_url = enterprise
                .get("home_url")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .into();
        }
    }
    Ok(Some(patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(number: u32, value: &str) -> Vec<u8> {
        p::bytes(number, value.as_bytes())
    }

    fn record(raw: Vec<u8>) -> ContactRecord {
        let mut fields = p::bytes(1, &text(1, "gh_synthetic"));
        fields.extend(raw);
        wechat_protocol::wire::decode_contact_record(&fields).unwrap()
    }

    #[test]
    fn business_presence_and_defaults_follow_the_incoming_verification_flag() {
        assert!(business_contact_patch(&record(vec![])).unwrap().is_none());
        assert!(business_contact_patch(&record(p::uint(24, 1)))
            .unwrap()
            .is_none());
        let patch = business_contact_patch(&record(p::uint(24, 8)))
            .unwrap()
            .unwrap();
        assert_eq!(patch.service_type, 0);
        assert!(patch.external_info.is_empty());
        assert!(patch.brand_info.is_empty());
    }

    #[test]
    fn independent_nested_wire_fixture_keeps_the_three_brand_fields_separate() {
        // CustomizedInfo: f1 scalar, f2 external JSON, f3 brand info, f4 icon.
        let mut customized = p::uint(1, 17);
        customized.extend(text(2, r#"{"ServiceType":1,"ConnectorMsgType":2,"EnterpriseBizInfo":{"child_type":3,"belong":"parent","home_url":"https://example.com"}}"#));
        customized.extend(text(3, "brand-info"));
        customized.extend(text(4, "https://example.com/icon"));
        let mut raw = p::uint(24, 24);
        raw.extend(text(41, "brand-list"));
        raw.extend(p::bytes(42, &customized));
        let patch = business_contact_patch(&record(raw)).unwrap().unwrap();
        assert_eq!(patch.service_type, 1);
        assert_eq!(patch.accept_type, 2);
        assert_eq!(patch.child_type, 3);
        assert_eq!(patch.brand_flag, 17);
        assert_eq!(patch.brand_list, "brand-list");
        assert_eq!(patch.brand_info, "brand-info");
        assert_eq!(patch.brand_icon_url, "https://example.com/icon");
        assert_eq!(patch.belong, "parent");
    }

    #[test]
    fn non_integer_or_invalid_json_uses_defaults_and_bad_wire_is_retained_as_an_error() {
        for external in [
            "invalid",
            "[]",
            r#"{"ServiceType":true}"#,
            r#"{"ServiceType":"1"}"#,
            r#"{"ServiceType":1.0}"#,
            r#"{"ServiceType":1e0}"#,
            r#"{"ServiceType":2147483648}"#,
            r#"{"ServiceType":4294967295}"#,
        ] {
            let mut raw = p::uint(24, 8);
            raw.extend(p::bytes(42, &text(2, external)));
            let patch = business_contact_patch(&record(raw)).unwrap().unwrap();
            assert_eq!(patch.service_type, 0);
            assert_eq!(patch.external_info, external);
        }
        let mut raw = p::uint(24, 8);
        raw.extend(p::uint(42, 1));
        assert!(business_contact_patch(&record(raw)).is_err());
    }
}
