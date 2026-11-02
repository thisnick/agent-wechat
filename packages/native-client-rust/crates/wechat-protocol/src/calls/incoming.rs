//! Offline incoming call carriers and conference notification envelopes.
//!
//! Decoding does not verify freshness, account ownership or room admission. The
//! caller must retain the original notification and its projection obligations.
//! The outer carrier and inner conference envelope are different wire messages;
//! only the selected carrier field is forwarded to the conference parser.
//! Room kinds and cancellation reasons remain numbers, without guessed names.

use super::signaling::{decode_fields, optional_u32, unknown_fields};
use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarrierKind {
    Invite,
    Cancel,
}

/// Opaque outer fields survive without assigning semantics to unnamed slots.
/// These fields and the forwarded bytes can contain private call information;
/// none of the data-bearing notification types implement `Debug`.
pub struct NotificationCarrier {
    kind: CarrierKind,
    fields: Vec<Field>,
}

impl NotificationCarrier {
    pub fn decode(kind: CarrierKind, input: &[u8]) -> Result<Self> {
        let fields = decode_fields(input)?;
        match kind {
            CarrierKind::Invite => {
                for number in [1, 3, 4, 5, 6] {
                    p::optional_data(&fields, number)?;
                }
                optional_u64(&fields, 2)?;
                for field in fields.iter().filter(|field| field.number == 7) {
                    ensure!(
                        matches!(field.value, Value::Bytes(_)),
                        "Invalid invite member field"
                    );
                }
            }
            CarrierKind::Cancel => {
                optional_u64(&fields, 1)?;
                p::optional_data(&fields, 2)?;
            }
        }
        Ok(Self { kind, fields })
    }

    pub fn kind(&self) -> CarrierKind {
        self.kind
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Invite forwards outer field 6; cancellation forwards outer field 2.
    /// Returning these bytes is not permission to submit or consume a control.
    pub fn conference_bytes(&self) -> Result<&[u8]> {
        let number = match self.kind {
            CarrierKind::Invite => 6,
            CarrierKind::Cancel => 2,
        };
        p::optional_data(&self.fields, number)?.context("Missing conference notification")
    }

    pub fn conference_notification(&self) -> Result<ConferenceNotification> {
        let notification = ConferenceNotification::decode(self.conference_bytes()?)?;
        ensure!(
            matches!(
                (self.kind, notification.kind),
                (CarrierKind::Invite, Some(2)) | (CarrierKind::Cancel, Some(3))
            ),
            "Outer and conference notification kinds disagree"
        );
        Ok(notification)
    }
}

/// Conference envelope. Unsupported kinds remain readable as raw numbers and
/// bytes; [`Self::payload`] refuses to invent a supported control for them.
pub struct ConferenceNotification {
    pub kind: Option<u32>,
    pub data: Option<Vec<u8>>,
    pub unknown_fields: Vec<Field>,
}

impl ConferenceNotification {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let fields = decode_fields(input)?;
        Ok(Self {
            kind: optional_u32(&fields, 1)?,
            data: p::optional_data(&fields, 2)?.map(Vec::from),
            unknown_fields: unknown_fields(fields, 2),
        })
    }

    pub fn payload(&self) -> Result<NotificationPayload> {
        let data = self.data.as_deref().context("Missing conference payload")?;
        match self.kind {
            Some(2) => Ok(NotificationPayload::Invite(InviteNotification::decode(
                data,
            )?)),
            Some(3) => Ok(NotificationPayload::Cancel(CancelNotification::decode(
                data,
            )?)),
            _ => bail!("Unsupported conference notification kind"),
        }
    }
}

pub enum NotificationPayload {
    Invite(InviteNotification),
    Cancel(CancelNotification),
}

/// Presence and full integer widths are preserved. This is not an admitted
/// invitation: missing identities, unknown enums and peer/account bindings
/// require independent validation before a controller can ring.
pub struct InviteNotification {
    pub room_id: Option<u64>,
    pub room_key: Option<u64>,
    pub member_id: Option<u32>,
    pub room_type: Option<u32>,
    pub caller_openid: Option<String>,
    pub callee_openid: Option<String>,
    pub caller_username: Option<String>,
    pub callee_username: Option<String>,
    pub usernames: Vec<String>,
    pub app_id: Option<String>,
    pub sub_app_id: Option<String>,
    pub caller_member_id: Option<u32>,
    pub wechat_group_id: Option<String>,
    pub unknown_fields: Vec<Field>,
}

impl InviteNotification {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let fields = decode_fields(input)?;
        let usernames = fields
            .iter()
            .filter(|field| field.number == 9)
            .map(|field| {
                let Value::Bytes(value) = &field.value else {
                    bail!("Invalid invite username field")
                };
                Ok(std::str::from_utf8(value)?.to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            room_id: optional_u64(&fields, 1)?,
            room_key: optional_u64(&fields, 2)?,
            member_id: optional_u32(&fields, 3)?,
            room_type: optional_u32(&fields, 4)?,
            caller_openid: optional_string(&fields, 5)?,
            callee_openid: optional_string(&fields, 6)?,
            caller_username: optional_string(&fields, 7)?,
            callee_username: optional_string(&fields, 8)?,
            usernames,
            app_id: optional_string(&fields, 10)?,
            sub_app_id: optional_string(&fields, 11)?,
            caller_member_id: optional_u32(&fields, 12)?,
            wechat_group_id: optional_string(&fields, 13)?,
            unknown_fields: unknown_fields(fields, 13),
        })
    }
}

pub struct CancelNotification {
    pub room_id: Option<u64>,
    pub room_key: Option<u64>,
    pub member_id: Option<u32>,
    pub reason: Option<u32>,
    pub unknown_fields: Vec<Field>,
}

impl CancelNotification {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let fields = decode_fields(input)?;
        Ok(Self {
            room_id: optional_u64(&fields, 1)?,
            room_key: optional_u64(&fields, 2)?,
            member_id: optional_u32(&fields, 3)?,
            reason: optional_u32(&fields, 4)?,
            unknown_fields: unknown_fields(fields, 4),
        })
    }
}

fn optional_u64(fields: &[Field], number: u32) -> Result<Option<u64>> {
    match p::optional(fields, number)? {
        None => Ok(None),
        Some(Value::Varint(value)) => Ok(Some(*value)),
        _ => bail!("Invalid notification scalar field {number}"),
    }
}

fn optional_string(fields: &[Field], number: u32) -> Result<Option<String>> {
    p::optional_data(fields, number)?
        .map(|value| Ok(std::str::from_utf8(value)?.to_owned()))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_literal_uses_each_distinct_envelope() {
        // Outer f1=99, f2=ThirdNotifyData(type=3, data=(42,43,7,4)).
        let raw = [
            0x08, 0x63, 0x12, 0x0c, 0x08, 0x03, 0x12, 0x08, 0x08, 0x2a, 0x10, 0x2b, 0x18, 0x07,
            0x20, 0x04,
        ];
        let outer = NotificationCarrier::decode(CarrierKind::Cancel, &raw).unwrap();
        assert_eq!(outer.kind(), CarrierKind::Cancel);
        assert_eq!(outer.conference_bytes().unwrap(), &raw[4..]);
        let third = outer.conference_notification().unwrap();
        let NotificationPayload::Cancel(cancel) = third.payload().unwrap() else {
            panic!("wrong kind")
        };
        assert_eq!(cancel.room_id, Some(42));
        assert_eq!(cancel.room_key, Some(43));
        assert_eq!(cancel.member_id, Some(7));
        assert_eq!(cancel.reason, Some(4));
        // The complete outer carrier is not itself the SDK envelope.
        assert!(ConferenceNotification::decode(&raw)
            .unwrap()
            .payload()
            .is_err());
    }

    #[test]
    fn invite_preserves_widths_names_presence_and_unknowns() {
        let raw = p::message(&[
            p::uint(1, u64::MAX),
            p::uint(2, u64::MAX - 1),
            p::uint(3, u32::MAX.into()),
            p::uint(4, 2),
            p::bytes(5, b"caller-open"),
            p::bytes(6, b"callee-open"),
            p::bytes(7, b"synthetic-caller"),
            p::bytes(8, b"synthetic-callee"),
            p::bytes(9, b"one"),
            p::bytes(9, b"two"),
            p::bytes(10, b""),
            p::uint(12, 0),
            p::bytes(13, b"synthetic-group"),
            p::bytes(99, b"unknown"),
        ])
        .unwrap();
        let invite = InviteNotification::decode(&raw).unwrap();
        assert_eq!(invite.room_id, Some(u64::MAX));
        assert_eq!(invite.room_key, Some(u64::MAX - 1));
        assert_eq!(invite.member_id, Some(u32::MAX));
        assert_eq!(invite.room_type, Some(2));
        assert_eq!(invite.caller_username.as_deref(), Some("synthetic-caller"));
        assert_eq!(invite.callee_username.as_deref(), Some("synthetic-callee"));
        assert_eq!(invite.usernames, vec!["one", "two"]);
        assert_eq!(invite.app_id.as_deref(), Some(""));
        assert!(invite.sub_app_id.is_none());
        assert_eq!(invite.caller_member_id, Some(0));
        assert_eq!(invite.unknown_fields.len(), 1);
        assert_eq!(p::data(&invite.unknown_fields, 99).unwrap(), b"unknown");
        let third = p::message(&[p::uint(1, 2), p::bytes(2, &raw), p::uint(99, 7)]).unwrap();
        let outer = p::message(&[
            p::bytes(1, b"opaque"),
            p::uint(2, 9),
            p::bytes(6, &third),
            p::bytes(7, &p::uint(1, 7)),
            p::uint(88, 3),
        ])
        .unwrap();
        let outer = NotificationCarrier::decode(CarrierKind::Invite, &outer).unwrap();
        assert_eq!(p::scalar(outer.fields(), 88).unwrap(), 3);
        let third = outer.conference_notification().unwrap();
        assert_eq!(p::scalar(&third.unknown_fields, 99).unwrap(), 7);
        assert!(matches!(
            third.payload().unwrap(),
            NotificationPayload::Invite(_)
        ));
    }
}
