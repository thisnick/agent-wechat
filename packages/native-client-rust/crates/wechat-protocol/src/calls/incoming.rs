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

/// Room-entry context carried by a group invitation. It is pending call data,
/// not a joined roster or account authority. The embedding actor binds it to
/// the authenticated message envelope and a current account generation.
#[derive(Clone)]
pub struct ConferenceJoinContext {
    pub group_id: String,
    pub context_id: String,
    pub room_id: u64,
    pub room_key: u64,
    pub own_member: u16,
    pub caller_username: String,
    pub callee_username: String,
}

impl NotificationCarrier {
    pub fn conference_join_context(&self) -> Result<ConferenceJoinContext> {
        ensure!(
            self.kind == CarrierKind::Invite,
            "Conference entry needs an invitation"
        );
        ensure!(
            self.fields
                .iter()
                .all(|field| (1..=10).contains(&field.number)),
            "Unsupported conference carrier fields"
        );
        let conference = self.conference_notification()?;
        ensure!(
            conference.unknown_fields.is_empty(),
            "Unsupported conference envelope fields"
        );
        let NotificationPayload::Invite(invite) = conference.payload()? else {
            bail!("Conference invitation missing");
        };
        ensure!(
            invite.unknown_fields.is_empty()
                && invite.room_type == Some(1)
                && invite.app_id.as_deref() == Some("wechat")
                && invite.sub_app_id.as_deref() == Some("voip-mt"),
            "Unsupported conference invitation"
        );
        let group_id =
            optional_string(&self.fields, 1)?.context("Conference chat identity missing")?;
        let context_id =
            optional_string(&self.fields, 3)?.context("Conference entry context missing")?;
        ensure!(
            group_id.ends_with("@chatroom")
                && group_id.len() <= 128
                && !group_id.chars().any(char::is_control)
                && invite.wechat_group_id.as_deref() == Some(group_id.as_str()),
            "Conference chat identities disagree"
        );
        ensure!(
            !context_id.is_empty()
                && context_id.len() <= 1024
                && !context_id.chars().any(char::is_control),
            "Invalid conference entry context"
        );
        let room_id = invite
            .room_id
            .filter(|id| *id != 0)
            .context("Conference room missing")?;
        let room_key = invite
            .room_key
            .filter(|key| *key != 0)
            .context("Conference room key missing")?;
        ensure!(
            optional_u64(&self.fields, 2)? == Some(room_id),
            "Conference room identities disagree"
        );
        for identity in [&invite.caller_openid, &invite.callee_openid] {
            ensure!(
                identity.as_deref().is_some_and(|value| !value.is_empty()
                    && value.len() <= 128
                    && !value.chars().any(char::is_control)),
                "Conference invitation participant binding missing"
            );
        }
        ensure!(
            optional_string(&self.fields, 4)? == invite.caller_openid
                && optional_string(&self.fields, 5)? == invite.callee_openid,
            "Conference invitation bindings disagree"
        );
        let caller_username = invite
            .caller_username
            .context("Conference caller missing")?;
        let callee_username = invite
            .callee_username
            .context("Conference recipient missing")?;
        ensure!(
            optional_string(&self.fields, 8)?.as_deref() == Some(caller_username.as_str()),
            "Conference caller identities disagree"
        );
        for identity in [&caller_username, &callee_username] {
            ensure!(
                !identity.is_empty()
                    && identity.len() <= 128
                    && !identity.ends_with("@chatroom")
                    && !identity.chars().any(char::is_control),
                "Invalid conference participant"
            );
        }
        optional_u64(&self.fields, 9)?;
        optional_u32(&self.fields, 10)?;
        Ok(ConferenceJoinContext {
            group_id,
            context_id,
            room_id,
            room_key,
            own_member: u16::try_from(invite.member_id.context("Conference member missing")?)?,
            caller_username,
            callee_username,
        })
    }
}

#[cfg(test)]
mod conference_context_tests {
    use super::*;

    fn carrier(chat: &str, room: u64, context: &str, caller: &str) -> NotificationCarrier {
        let inner = p::message(&[
            p::uint(1, 42),
            p::uint(2, 43),
            p::uint(3, 2),
            p::uint(4, 1),
            p::bytes(5, b"caller-open"),
            p::bytes(6, b"callee-open"),
            p::bytes(7, b"caller"),
            p::bytes(8, b"callee"),
            p::bytes(10, b"wechat"),
            p::bytes(11, b"voip-mt"),
            p::bytes(13, b"test@chatroom"),
        ])
        .unwrap();
        let envelope = p::message(&[p::uint(1, 2), p::bytes(2, &inner)]).unwrap();
        NotificationCarrier::decode(
            CarrierKind::Invite,
            &p::message(&[
                p::bytes(1, chat.as_bytes()),
                p::uint(2, room),
                p::bytes(3, context.as_bytes()),
                p::bytes(4, b"caller-open"),
                p::bytes(5, b"callee-open"),
                p::bytes(6, &envelope),
                p::bytes(8, caller.as_bytes()),
                p::uint(9, 123),
                p::uint(10, 0),
            ])
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn group_context_binds_each_distinct_invitation_identity() {
        let source = carrier("test@chatroom", 42, "opaque-test-context", "caller");
        let context = source.conference_join_context().unwrap();
        assert_eq!(context.group_id, "test@chatroom");
        assert_eq!(context.context_id, "opaque-test-context");
        assert_eq!(
            (context.room_id, context.room_key, context.own_member),
            (42, 43, 2)
        );
        assert_eq!(
            (
                context.caller_username.as_str(),
                context.callee_username.as_str()
            ),
            ("caller", "callee")
        );
    }
    #[test]
    fn mixed_or_missing_group_context_never_admits_entry() {
        for source in [
            carrier("different@chatroom", 42, "opaque-test-context", "caller"),
            carrier("test@chatroom", 44, "opaque-test-context", "caller"),
            carrier("test@chatroom", 42, "", "caller"),
            carrier("test@chatroom", 42, "opaque-test-context", "other-caller"),
        ] {
            assert!(source.conference_join_context().is_err());
        }
    }
}
