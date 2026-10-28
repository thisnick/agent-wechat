//! Static pins are parsed from canonical message content, without map requests
//! or joining the separate live-location sharing protocol.
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LocationCoordinateSystem {
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LocationCard {
    pub latitude: f64,
    pub longitude: f64,
    pub name: Option<String>,
    pub address: Option<String>,
    pub scale: Option<i32>,
    pub map_type: Option<String>,
    pub poi_id: Option<String>,
    pub coordinate_system: LocationCoordinateSystem,
}
impl LocationCard {
    pub(crate) fn body(&self) -> String {
        let mut lines = vec!["[Location]".to_owned()];
        lines.extend(self.name.iter().cloned());
        lines.extend(
            self.address
                .iter()
                .filter(|s| Some(*s) != self.name.as_ref())
                .cloned(),
        );
        lines.push(format!(
            "Latitude: {}\nLongitude: {}",
            self.latitude, self.longitude
        ));
        lines.push("Coordinate system: unknown".into());
        lines.join("\n")
    }
}

pub(crate) fn parse(content: &str) -> Option<LocationCard> {
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
    if doc
        .descendants()
        .any(|n| n.ancestors().take(34).count() > 32)
    {
        return None;
    }
    let root = doc.root_element();
    if root.tag_name().name() != "msg" || root.tag_name().namespace().is_some() {
        return None;
    }
    let pin = crate::quotes::child(root, "location")?;
    if pin.children().any(|n| n.is_element()) || pin.attributes().any(|a| a.namespace().is_some()) {
        return None;
    }
    let latitude = pin.attribute("x")?.parse::<f64>().ok()?;
    let longitude = pin.attribute("y")?.parse::<f64>().ok()?;
    if !latitude.is_finite()
        || !longitude.is_finite()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return None;
    }
    let text = |key, max| -> Option<Option<String>> {
        let value = pin.attribute(key).unwrap_or("").trim();
        if value.len() > max || value.chars().any(char::is_control) {
            return None;
        }
        Some((!value.is_empty()).then(|| value.to_owned()))
    };
    Some(LocationCard {
        latitude,
        longitude,
        name: text("poiname", 4096)?,
        address: text("label", 4096)?,
        scale: pin
            .attribute("scale")
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .transpose()
            .ok()?,
        map_type: text("maptype", 1024)?,
        poi_id: text("poiid", 1024)?,
        coordinate_system: LocationCoordinateSystem::Unknown,
    })
}

pub(crate) fn message_location(msg: &wechat_profile::Message) -> Option<LocationCard> {
    if msg.message_type != 48 {
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
    parse(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    const PIN: &str = "<msg><location x=\"12.5\" y=\"-45.25\" scale=\"-2\" label=\"Example &amp; address\" poiname=\"Example park\" maptype=\"supplied-map\" poiid=\"example-poi\"/></msg>";

    #[test]
    fn preserves_static_pin_fields_without_assuming_a_coordinate_system() {
        let pin = parse(PIN).unwrap();
        assert_eq!(pin.latitude, 12.5);
        assert_eq!(pin.longitude, -45.25);
        assert_eq!(pin.scale, Some(-2));
        assert_eq!(pin.map_type.as_deref(), Some("supplied-map"));
        assert_eq!(pin.address.as_deref(), Some("Example & address"));
        assert_eq!(pin.coordinate_system, LocationCoordinateSystem::Unknown);
        assert!(pin
            .body()
            .contains("Example park\nExample & address\nLatitude: 12.5"));
        let no_address = parse(&PIN.replace("Example &amp; address", "")).unwrap();
        assert_eq!(no_address.address, None);
        assert_eq!(no_address.name.as_deref(), Some("Example park"));
        let minimal = parse("<msg><location x='0' y='0'/></msg>").unwrap();
        assert!(minimal.name.is_none() && minimal.map_type.is_none() && minimal.scale.is_none());
    }

    #[test]
    fn rejects_ambiguous_xml_and_invalid_coordinates_without_returning_raw_content() {
        for xml in [
            PIN.replace("12.5", "NaN"),
            PIN.replace("12.5", "91"),
            PIN.replace("-45.25", "inf"),
            PIN.replace("-45.25", "-181"),
            PIN.replace("x=\"12.5\"", ""),
            PIN.replace("scale=\"-2\"", "scale=\"1.5\""),
            PIN.replace("<msg>", "<msg xmlns='foreign'>"),
            PIN.replace("<location", "<location xmlns='foreign'"),
            PIN.replace("</msg>", "<location x='1' y='2'/></msg>"),
            PIN.replace("<msg>", "<!DOCTYPE msg><msg>"),
            PIN.replace("/>", "><location/></location>"),
            PIN.replace("Example park", "Example&#10;park"),
            "<sysmsg type='trackmsg'><trackmsg/></sysmsg>".into(),
            "<msg><appmsg><type>17</type><title>Location sharing</title></appmsg></msg>".into(),
        ] {
            assert!(parse(&xml).is_none(), "accepted malformed location");
        }
    }

    #[test]
    fn normalization_accepts_exact_group_sender_prefix_only() {
        use wechat_profile::{DecodeState, DecodedField, Message, SourceMessageId};
        let field = |text| DecodedField {
            text,
            state: DecodeState::Utf8,
            compression_flag: None,
        };
        let mut msg = Message {
            id: SourceMessageId {
                database: "message/message_0.db".into(),
                table: "Msg_example".into(),
                local_id: "1".into(),
            },
            server_message_id: Some("42".into()),
            server_id_signed: Some("42".into()),
            peer: Some("example@chatroom".into()),
            sender: Some("member".into()),
            outgoing: Some(false),
            message_type: 48,
            local_type: "48".into(),
            create_time: 100,
            sort_seq: None,
            content: field(Some(format!("member:\n{PIN}"))),
            source: field(None),
            raw_fields: Default::default(),
        };
        let value =
            crate::profile::normalized_message("example", msg.clone(), Some("self"), "1").unwrap();
        assert_eq!(value["location"]["latitude"], 12.5);
        assert_eq!(value["location"]["coordinateSystem"], "unknown");
        assert!(!value["body"].as_str().unwrap().contains("<msg>"));
        msg.sender = Some("different".into());
        assert!(message_location(&msg).is_none());
        msg.message_type = 49;
        assert!(message_location(&msg).is_none());
    }
}
