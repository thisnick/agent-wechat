//! Voice conference creation and additional invitation codecs.
//! These builders perform no I/O, allocate no task/intent IDs and never retry.
use super::{
    personal::{HostOperation, HostRequest, VoiceInputs, VoiceRoom},
    signaling,
};
use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Context, Result};

const MAX_BODY: usize = 64 * 1024;
const MAX_INVITEES: usize = 100;

/// Voice-only microphone state, distinct from membership and local playback.
/// Video fields remain absent. Timestamp is caller-owned and must increase for
/// each actual change; a response retry hint never permits automatic resending.
pub fn switch_microphone(
    inputs: &VoiceInputs<'_>,
    room: VoiceRoom,
    enabled: bool,
    timestamp: u64,
    sdk_task: u32,
) -> Result<signaling::CloudRequest> {
    inputs.validate_conference_context()?;
    ensure!(
        room.room != 0 && room.room_key != 0 && timestamp != 0 && sdk_task != 0,
        "Invalid conference microphone context"
    );
    let body = p::message(&[
        p::bytes(1, inputs.sdk),
        p::bytes(2, inputs.group),
        p::uint(3, room.room),
        p::uint(4, room.room_key),
        p::uint(5, u64::from(room.own_member)),
        p::uint(6, u64::from(enabled)),
        p::uint(7, timestamp),
        p::bytes(8, &[]),
    ])?;
    Ok(signaling::CloudRequest::for_operation(
        sdk_task,
        signaling::CallOperation::SwitchAv,
        body,
    ))
}

/// Prepare one voice conference creation request. Application metadata must
/// select conference mode and the same group; direct-call inputs stay invalid.
/// Absent custom authorization remains absent on the wire.
pub fn create_voice(
    inputs: &VoiceInputs<'_>,
    group_id: &str,
    invitees: &[String],
    invite_id: u64,
    sdk_task: u32,
    custom_auth: Option<&[u8]>,
) -> Result<HostRequest> {
    group(group_id)?;
    contacts(invitees)?;
    ensure!(
        invite_id != 0 && sdk_task != 0,
        "Missing conference intent or task"
    );
    let client = inputs.conference_client_info(group_id)?;
    let mut fields = invitees
        .iter()
        .map(|name| p::bytes(1, name.as_bytes()))
        .collect::<Vec<_>>();
    fields.extend([
        p::uint(2, 1),
        p::bytes(3, group_id.as_bytes()),
        p::bytes(4, &client),
        p::uint(5, invite_id),
    ]);
    if let Some(auth) = custom_auth {
        ensure!(
            !auth.is_empty() && auth.len() <= 8192,
            "Invalid conference custom authorization"
        );
        fields.push(p::bytes(6, auth));
    }
    fields.push(p::bytes(7, b"voip-mt"));
    request(HostOperation::Invite, sdk_task, fields)
}

/// Submit additional invitations for a known conference. Invitation submission
/// does not add membership or audio tracks. The runtime must check current room
/// authority and record the original intent before submitting this request.
pub fn add_members(
    room: VoiceRoom,
    group_id: &str,
    invitees: &[String],
    invite_id: u64,
    sdk_task: u32,
) -> Result<HostRequest> {
    group(group_id)?;
    contacts(invitees)?;
    ensure!(
        room.room != 0 && room.room_key != 0 && invite_id != 0 && sdk_task != 0,
        "Missing conference room, intent or task"
    );
    let mut fields = vec![
        p::uint(1, room.room),
        p::uint(2, room.room_key),
        p::uint(3, u64::from(room.own_member)),
        p::uint(4, invite_id),
        p::bytes(5, group_id.as_bytes()),
    ];
    fields.extend(invitees.iter().map(|name| p::bytes(6, name.as_bytes())));
    fields.extend([p::bytes(7, b"voip-mt"), p::uint(8, 1)]);
    request(HostOperation::ConferenceAddMembers, sdk_task, fields)
}

/// Cancel the creator's original unanswered invitation. The runtime must first
/// verify that its invitation timer is still active. Pending recipients are
/// supplied in source traversal order, including duplicates; an empty list does
/// not by itself prove the invitation timer has stopped.
pub fn cancel_creation(
    inputs: &VoiceInputs<'_>,
    room: VoiceRoom,
    invite_id: u64,
    pending_recipients: &[String],
    reason: super::personal::CancelReason,
    sdk_task: u32,
) -> Result<HostRequest> {
    inputs.validate_conference_context()?;
    ensure!(
        invite_id != 0
            && sdk_task != 0
            && (room.room == 0) == (room.room_key == 0)
            && pending_recipients.len() <= 228,
        "Invalid conference cancellation context"
    );
    for name in pending_recipients {
        text(name.as_bytes(), 128, true)?;
    }
    let reason = match reason {
        super::personal::CancelReason::Local => 1,
        super::personal::CancelReason::Timeout => 4,
        super::personal::CancelReason::Other => 0,
        super::personal::CancelReason::NoResponse => {
            bail!("Direct no-response cancellation is not a conference operation")
        }
    };
    let inner = p::message(&[
        p::bytes(1, inputs.sdk),
        p::bytes(2, inputs.group),
        p::uint(3, room.room),
        p::uint(4, room.room_key),
        p::uint(5, invite_id),
        p::uint(6, reason),
    ])?;
    let mut outer = vec![
        p::bytes(1, &inner),
        p::uint(2, invite_id),
        p::uint(3, room.room),
    ];
    outer.extend(
        pending_recipients
            .iter()
            .map(|name| p::bytes(4, name.as_bytes())),
    );
    outer.push(p::bytes(5, b"voip-mt"));
    request(HostOperation::Cancel, sdk_task, outer)
}

fn request(operation: HostOperation, sdk_task: u32, fields: Vec<Vec<u8>>) -> Result<HostRequest> {
    let body = p::message(&fields)?;
    ensure!(body.len() <= MAX_BODY, "Conference request too large");
    Ok(HostRequest {
        operation,
        sdk_task,
        body,
        longpoll_seconds: None,
    })
}

fn contacts(invitees: &[String]) -> Result<()> {
    ensure!(
        !invitees.is_empty() && invitees.len() <= MAX_INVITEES,
        "Invalid conference invitation count"
    );
    for contact in invitees {
        text(contact.as_bytes(), 128, true)?;
    }
    Ok(())
}

fn group(group_id: &str) -> Result<()> {
    text(group_id.as_bytes(), 256, true)?;
    ensure!(group_id.ends_with("@chatroom"), "Invalid conference group");
    Ok(())
}

fn text(value: &[u8], maximum: usize, nonempty: bool) -> Result<()> {
    let text = std::str::from_utf8(value)?;
    ensure!(
        value.len() <= maximum
            && (!nonempty || !value.is_empty())
            && !text.chars().any(char::is_control),
        "Invalid conference text"
    );
    Ok(())
}

fn fields(raw: &[u8]) -> Result<Vec<Field>> {
    ensure!(raw.len() <= MAX_BODY, "Conference response too large");
    signaling::decode_fields(raw)
}
fn optional_u64(fields: &[Field], number: u32) -> Result<Option<u64>> {
    match p::optional(fields, number)? {
        Some(Value::Varint(value)) => Ok(Some(*value)),
        None => Ok(None),
        _ => bail!("Invalid conference integer field {number}"),
    }
}
fn bytes(fields: &[Field], number: u32) -> Result<&[u8]> {
    p::optional_data(fields, number)?.context("Missing conference bytes field")
}
fn optional_text(fields: &[Field], number: u32, maximum: usize) -> Result<Option<String>> {
    p::optional_data(fields, number)?
        .map(|raw| {
            text(raw, maximum, false)?;
            Ok(std::str::from_utf8(raw)?.to_owned())
        })
        .transpose()
}

/// One server invitation outcome. On a successful outer response, code zero
/// means submission, not acceptance by a participant. Missing and present-zero codes retain distinct presence.
pub struct InvitationResult {
    pub contact_id: String,
    pub service_code: Option<u32>,
    pub unknown_fields: Vec<Field>,
}
fn invitation_results(values: &[Field], number: u32) -> Result<Vec<InvitationResult>> {
    let mut results = Vec::new();
    for field in values.iter().filter(|field| field.number == number) {
        ensure!(
            results.len() < MAX_INVITEES,
            "Too many conference invitation outcomes"
        );
        let Value::Bytes(raw) = &field.value else {
            bail!("Invalid conference invitation outcome");
        };
        let fields = fields(raw)?;
        let contact = bytes(&fields, 1)?;
        text(contact, 128, true)?;
        results.push(InvitationResult {
            contact_id: std::str::from_utf8(contact)?.to_owned(),
            service_code: signaling::optional_u32(&fields, 2)?,
            unknown_fields: fields
                .into_iter()
                .filter(|field| ![1, 2].contains(&field.number))
                .collect(),
        });
    }
    Ok(results)
}

/// Raw authenticated creation result. Allocation, invitation outcomes and the
/// nested client response remain separate. This type never invents participants
/// from invitees and deliberately does not implement Debug or serialization.
pub struct CreateResponse {
    pub room_id: Option<u64>,
    pub room_key: Option<u64>,
    pub own_member: Option<u32>,
    pub cloud_group_id: Option<String>,
    pub client_response: Option<Vec<u8>>,
    pub user_mappings: Vec<Vec<u8>>,
    pub custom_auth: Option<Vec<u8>>,
    pub invite_results: Vec<InvitationResult>,
    pub verification_url: Option<String>,
    pub ui_flags: Option<u32>,
    pub original: Vec<u8>,
    pub unknown_fields: Vec<Field>,
}
impl CreateResponse {
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let values = fields(raw)?;
        let room_id = optional_u64(&values, 1)?;
        let room_key = optional_u64(&values, 2)?;
        let own_member = signaling::optional_u32(&values, 3)?;
        let cloud_group_id = optional_text(&values, 4, 1024)?;
        let client_response = p::optional_data(&values, 5)?.map(Vec::from);
        if let Some(client) = &client_response {
            fields(client)?;
        }
        let mut users = Vec::new();
        for entry in values.iter().filter(|field| field.number == 6) {
            ensure!(users.len() < 128, "Too many conference user mappings");
            let Value::Bytes(raw) = &entry.value else {
                bail!("Invalid conference user mapping");
            };
            fields(raw)?;
            users.push(raw.clone());
        }
        Ok(Self {
            room_id,
            room_key,
            own_member,
            cloud_group_id,
            client_response,
            user_mappings: users,
            custom_auth: p::optional_data(&values, 7)?.map(Vec::from),
            invite_results: invitation_results(&values, 8)?,
            verification_url: optional_text(&values, 9, 8192)?,
            ui_flags: signaling::optional_u32(&values, 10)?,
            original: raw.to_vec(),
            unknown_fields: values
                .into_iter()
                .filter(|field| !(1..=10).contains(&field.number))
                .collect(),
        })
    }
    /// Identify a complete allocation without claiming membership or media.
    /// Missing/zero room identifiers can belong to a rejection; partial nonzero
    /// identity needs review and is never filled from a previous call.
    pub fn allocated_room(&self) -> Result<Option<VoiceRoom>> {
        if self.room_id.unwrap_or(0) == 0 && self.room_key.unwrap_or(0) == 0 {
            return Ok(None);
        }
        match (self.room_id, self.room_key, self.own_member) {
            (Some(room), Some(room_key), Some(own_member)) if room != 0 && room_key != 0 => {
                Ok(Some(VoiceRoom {
                    room,
                    room_key,
                    own_member,
                }))
            }
            _ => bail!("Incomplete conference allocation"),
        }
    }
}

/// Adding contacts cannot allocate or replace the current room. Optional reply
/// identity fields are retained and checked when present; omitted fields are
/// not silently replaced with zero-valued identities.
pub struct AddMembersResponse {
    pub room_id: Option<u64>,
    pub room_key: Option<u64>,
    pub own_member: Option<u32>,
    pub invite_results: Vec<InvitationResult>,
    pub verification_url: Option<String>,
    pub original: Vec<u8>,
    pub unknown_fields: Vec<Field>,
}
impl AddMembersResponse {
    pub fn decode(raw: &[u8]) -> Result<Self> {
        let values = fields(raw)?;
        let room_id = optional_u64(&values, 1)?;
        Ok(Self {
            room_id,
            room_key: optional_u64(&values, 2)?,
            own_member: signaling::optional_u32(&values, 3)?,
            invite_results: invitation_results(&values, 4)?,
            verification_url: optional_text(&values, 5, 8192)?,
            original: raw.to_vec(),
            unknown_fields: values
                .into_iter()
                .filter(|field| !(1..=5).contains(&field.number))
                .collect(),
        })
    }
    pub fn check_room(&self, expected: VoiceRoom) -> Result<()> {
        ensure!(
            expected.room != 0
                && expected.room_key != 0
                && self.room_id == Some(expected.room)
                && self.room_key.is_none_or(|key| key == expected.room_key)
                && self
                    .own_member
                    .is_none_or(|member| member == expected.own_member),
            "Conference invitation response context mismatch"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
