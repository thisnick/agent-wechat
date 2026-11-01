//! Personal-call request codecs. Preparation performs no transport or account I/O.
//!
//! Host requests and Cloud requests use different envelopes. SDK task identifiers
//! are not host BaseRequest identifiers. The caller supplies current authenticated
//! context, selected capabilities and device metadata; this module invents none.

use super::signaling::{self, CallOperation, CloudRequest, CommonRequest};
use crate::protobuf::{self as p, Field, Value};
use anyhow::{bail, ensure, Result};

const MAX_BODY: usize = 64 * 1024;
const MAX_NESTED: usize = 16 * 1024;
const MAX_MEMBERS: usize = 16;

/// Host operation identity. Inner Cloud command IDs are a different namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostOperation {
    Invite,
    Cancel,
    LiveSync,
}

impl HostOperation {
    pub const fn command(self) -> u16 {
        match self {
            Self::Invite => 10342,
            Self::Cancel => 9332,
            Self::LiveSync => 9262,
        }
    }

    pub const fn path(self) -> &'static str {
        match self {
            Self::Invite => "/cgi-bin/micromsg-bin/voipilinkinvite",
            Self::Cancel => "/cgi-bin/micromsg-bin/voipilinkcancel",
            Self::LiveSync => "/cgi-bin/micromsg-bin/voipilinklivesync",
        }
    }
}

/// Encoded inner host operation. It does not imply authorization to dispatch.
/// Bodies may contain private identities and are deliberately not Debug.
pub struct HostRequest {
    pub operation: HostOperation,
    pub sdk_task: u32,
    body: Vec<u8>,
    pub longpoll_seconds: Option<u32>,
}

impl HostRequest {
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Wrap with a separately prepared host BaseRequest. This never rewrites its
    /// task field using `sdk_task`.
    pub fn common(&self, host_base: Vec<u8>) -> Result<Vec<u8>> {
        CommonRequest {
            base_request: Some(host_base),
            body: Some(self.body.clone()),
            unknown_fields: vec![],
        }
        .encode()
    }
}

/// Caller-supplied, selected direct-voice request settings. Nested protobufs are
/// checked for their known wire types while unknown fields remain byte-identical.
pub struct VoiceInputs<'a> {
    pub sdk: &'a [u8],
    pub group: &'a [u8],
    pub device: &'a [u8],
    pub engine_capability: &'a [u8],
    pub channel_capability: &'a [u8],
    pub network_type: u32,
    pub sim_type: u32,
    pub video_length: u32,
    pub video_ratio: u32,
    pub nics: &'a [Vec<u8>],
}

impl VoiceInputs<'_> {
    fn checked(&self) -> Result<(u32, Vec<u8>, Vec<u8>)> {
        ensure!(
            self.network_type < 8 && self.nics.len() <= 8,
            "Unsupported voice input configuration"
        );
        let sdk = fields(self.sdk)?;
        let version = required_u32(&sdk, 1)?;
        let owner_version = required_bytes(&sdk, 2)?;
        text(owner_version, 128, true)?;
        let group = fields(self.group)?;
        ensure!(
            required_bytes(&group, 1)? == b"wechat",
            "Unsupported call application"
        );
        required_u64(&group, 2)?;
        ensure!(
            required_bytes(&group, 3)? == b"voip-2p",
            "Unsupported call target kind"
        );
        text(required_bytes(&group, 4)?, 1024, false)?;
        ensure!(
            required_u32(&group, 5)? <= 1,
            "Unsupported personal call mode"
        );
        let owner_group = required_bytes(&group, 6)?;
        text(owner_group, 1024, false)?;
        required_u32(&group, 7)?;
        let device = fields(self.device)?;
        for n in [1, 2, 3, 4, 5, 9] {
            if let Some(v) = p::optional_data(&device, n)? {
                text(v, 1024, false)?;
            }
        }
        for n in [7, 8] {
            signaling::optional_u32(&device, n)?;
        }
        p::optional_data(&device, 6)?;
        for value in [self.engine_capability, self.channel_capability] {
            ensure!(
                !value.is_empty() && value.len() <= MAX_NESTED,
                "Invalid call capability size"
            );
        }
        for nic in self.nics {
            ensure!(nic.len() <= 2048, "Call NIC description too large");
            fields(nic)?;
        }
        Ok((version, owner_version.to_vec(), owner_group.to_vec()))
    }

    fn client_info(&self) -> Result<Vec<u8>> {
        let (version, owner_version, _) = self.checked()?;
        let capabilities = message(&[
            p::bytes(1, self.engine_capability),
            p::bytes(2, self.channel_capability),
        ])?;
        let mut values = vec![
            p::uint(1, u64::from(version)),
            p::bytes(2, &owner_version),
            p::uint(3, u64::from(self.network_type)),
            p::uint(4, 1),
            p::bytes(5, &capabilities),
            p::bytes(6, self.device),
            p::uint(7, u64::from(self.video_length)),
            p::uint(8, u64::from(self.video_ratio)),
        ];
        values.extend(self.nics.iter().map(|nic| p::bytes(9, nic)));
        values.push(p::uint(10, u64::from(self.sim_type)));
        message(&values)
    }
}

/// Prepare one direct voice invitation. `invite_id` identifies the original
/// intent and is never allocated from a wall clock by this codec. An absent
/// custom auth value stays absent, distinct from an invalid empty value.
pub fn invite(
    inputs: &VoiceInputs<'_>,
    peer: &str,
    invite_id: u64,
    sdk_task: u32,
    custom_auth: Option<&[u8]>,
) -> Result<HostRequest> {
    text(peer.as_bytes(), 128, true)?;
    ensure!(
        invite_id != 0 && sdk_task != 0,
        "Missing call intent or task"
    );
    let (_, _, owner_group) = inputs.checked()?;
    let mut values = vec![
        p::bytes(1, peer.as_bytes()),
        p::uint(2, 2),
        p::bytes(3, &owner_group),
        p::bytes(4, &inputs.client_info()?),
        p::uint(5, invite_id),
    ];
    if let Some(auth) = custom_auth {
        ensure!(
            !auth.is_empty() && auth.len() <= 8192,
            "Invalid custom call authorization"
        );
        values.push(p::bytes(6, auth));
    }
    values.push(p::bytes(7, b"voip-2p"));
    Ok(HostRequest {
        operation: HostOperation::Invite,
        sdk_task,
        body: message(&values)?,
        longpoll_seconds: None,
    })
}

/// Cancel only the still-pending recipients of this original invitation.
/// Preserve source traversal order and duplicates; membership/role/timer checks
/// are the session owner's responsibility before constructing this request.
pub fn cancel(
    inputs: &VoiceInputs<'_>,
    room: u64,
    room_key: u64,
    invite_id: u64,
    pending_recipients: &[String],
    reason: CancelReason,
    sdk_task: u32,
) -> Result<HostRequest> {
    inputs.checked()?;
    ensure!(
        invite_id != 0 && sdk_task != 0 && (room == 0) == (room_key == 0),
        "Invalid cancellation context"
    );
    ensure!(
        !pending_recipients.is_empty() && pending_recipients.len() <= 8,
        "Invalid pending recipient count"
    );
    for peer in pending_recipients {
        text(peer.as_bytes(), 128, true)?;
    }
    let inner = message(&[
        p::bytes(1, inputs.sdk),
        p::bytes(2, inputs.group),
        p::uint(3, room),
        p::uint(4, room_key),
        p::uint(5, invite_id),
        p::uint(6, reason.wire()),
    ])?;
    let mut outer = vec![p::bytes(1, &inner), p::uint(2, invite_id), p::uint(3, room)];
    outer.extend(
        pending_recipients
            .iter()
            .map(|name| p::bytes(4, name.as_bytes())),
    );
    outer.push(p::bytes(5, b"voip-2p"));
    Ok(HostRequest {
        operation: HostOperation::Cancel,
        sdk_task,
        body: message(&outer)?,
        longpoll_seconds: None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Local,
    Timeout,
    Other,
}

impl CancelReason {
    const fn wire(self) -> u64 {
        match self {
            Self::Local => 1,
            Self::Timeout => 4,
            Self::Other => 0,
        }
    }
}

/// Prepare one caller LiveSync. `timeout_seconds` is source-selected (3..50),
/// distinct from the outer request deadline. Cookies remain uncommitted until
/// the owner has handled the entire response batch.
pub fn live_sync(
    live_id: u64,
    cookies: &[u8],
    timeout_seconds: u32,
    sdk_task: u32,
) -> Result<HostRequest> {
    ensure!(
        live_id != 0 && sdk_task != 0 && (3..=50).contains(&timeout_seconds),
        "Invalid call sync context"
    );
    ensure!(cookies.len() <= MAX_NESTED, "Call sync cookie too large");
    Ok(HostRequest {
        operation: HostOperation::LiveSync,
        sdk_task,
        body: message(&[
            p::uint(1, live_id),
            p::bytes(2, cookies),
            p::bytes(4, b"voip-2p"),
            p::uint(5, u64::from(timeout_seconds)),
        ])?,
        longpoll_seconds: Some(timeout_seconds + 5),
    })
}

/// Owned room correlation supplied only after the matching Join/Invite reply.
/// Member zero is valid. Room/key are private and deliberately not Debug.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VoiceRoom {
    pub room: u64,
    pub room_key: u64,
    pub own_member: u32,
}
impl VoiceRoom {
    fn validate(self) -> Result<()> {
        ensure!(
            self.room != 0 && self.room_key != 0,
            "Unallocated call room"
        );
        Ok(())
    }
}

/// Prepare callee Cloud Join3361. The caller obtains its context from Invite and
/// must not manufacture a callee Join. Timestamp is an independently selected
/// wire value; this codec does not assume its clock or unit.
pub fn join(
    inputs: &VoiceInputs<'_>,
    room: VoiceRoom,
    timestamp: u64,
    sdk_task: u32,
) -> Result<CloudRequest> {
    inputs.checked()?;
    room.validate()?;
    ensure!(sdk_task != 0, "Missing call task");
    let capabilities = message(&[
        p::bytes(1, inputs.engine_capability),
        p::bytes(2, inputs.channel_capability),
    ])?;
    let mut values = vec![
        p::bytes(1, inputs.sdk),
        p::bytes(2, inputs.group),
        p::uint(3, u64::from(inputs.network_type)),
        p::bytes(4, &capabilities),
        p::uint(5, 2),
        p::uint(6, 1),
        p::bytes(7, inputs.device),
        p::uint(8, timestamp),
        p::uint(9, u64::from(inputs.video_length)),
        p::uint(10, u64::from(inputs.video_ratio)),
        p::uint(11, room.room),
        p::uint(12, room.room_key),
    ];
    values.extend(inputs.nics.iter().map(|nic| p::bytes(13, nic)));
    values.push(p::uint(14, u64::from(inputs.sim_type)));
    Ok(CloudRequest::for_operation(
        sdk_task,
        CallOperation::Join,
        message(&values)?,
    ))
}

/// Prepare joined Cloud Exit3380. Source reason numbers belong to the selected
/// SDK, not local HTTP/CLI enums. The caller must establish native talk-success
/// state separately; a joined roster or decoded frame is insufficient evidence.
/// Required reliable peer notices, when selected, are a separate operation.
pub fn exit(
    inputs: &VoiceInputs<'_>,
    room: VoiceRoom,
    source_reason: u32,
    talk_success: bool,
    sdk_task: u32,
) -> Result<CloudRequest> {
    inputs.checked()?;
    room.validate()?;
    ensure!(sdk_task != 0, "Missing call task");
    let mapped = [1, 0, 0, 0, 0, 8, 9, 10, 0, 5]
        .get(source_reason.wrapping_sub(1) as usize)
        .copied()
        .unwrap_or(0);
    let body = message(&[
        p::bytes(1, inputs.sdk),
        p::bytes(2, inputs.group),
        p::uint(3, room.room),
        p::uint(4, room.room_key),
        p::uint(5, u64::from(room.own_member)),
        p::uint(6, mapped),
        p::uint(7, u64::from(inputs.network_type)),
        p::uint(8, u64::from(!talk_success)),
    ])?;
    Ok(CloudRequest::for_operation(
        sdk_task,
        CallOperation::Exit,
        body,
    ))
}

/// Wrap an inner Cloud request using its separate app BaseRequest. This returns
/// IlinkRequestInfo bytes, not host CommonRequest or a packed app-task frame.
pub fn cloud_request_info(request: &CloudRequest, cloud_base: &[u8]) -> Result<Vec<u8>> {
    let operation = request.operation()?;
    let base = fields(cloud_base)?;
    required_u64(&base, 1)?;
    ensure!(
        required_bytes(&base, 2)?.len() <= 128,
        "Cloud device identity too large"
    );
    let signed = required_u64(&base, 3)? as i64;
    i32::try_from(signed)?;
    let body = request
        .body
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Missing Cloud call body"))?;
    let cloud = message(&[p::bytes(1, cloud_base), p::bytes(2, body)])?;
    message(&[
        p::uint(1, u64::from(operation.command_id())),
        p::bytes(2, operation.path().as_bytes()),
        p::bytes(3, &cloud),
        p::uint(5, 1),
        p::uint(6, 1),
        p::uint(7, 1),
    ])
}

/// Raw caller response, distinct from the callee Join response. Decode after
/// host CommonResponse authentication and success; it does not install a room.
/// Secret room values and original fields deliberately have no Debug output.
pub struct InviteResponse {
    pub room: u64,
    pub room_key: u64,
    pub own_member: u32,
    pub group_id: String,
    pub client_response: Vec<u8>,
    pub custom_auth: Option<Vec<u8>>,
    pub invite_results: Vec<(String, u32)>,
    pub original: Vec<u8>,
    pub unknown_fields: Vec<Field>,
}

impl InviteResponse {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let values = fields(input)?;
        let room = required_u64(&values, 1)?;
        let room_key = required_u64(&values, 2)?;
        ensure!(room != 0 && room_key != 0, "Unallocated caller room");
        let group = required_bytes(&values, 4)?;
        text(group, 1024, true)?;
        let client_response = required_bytes(&values, 5)?;
        ensure!(
            !client_response.is_empty(),
            "Missing caller client response"
        );
        JoinClientResponse::decode(client_response)?;
        let mut results = Vec::new();
        for value in values.iter().filter(|value| value.number == 8) {
            ensure!(
                results.len() < MAX_MEMBERS,
                "Too many call invitation results"
            );
            let nested = fields(field_bytes(value)?)?;
            let name = required_bytes(&nested, 1)?;
            text(name, 128, true)?;
            results.push((String::from_utf8(name.to_vec())?, required_u32(&nested, 2)?));
        }
        let users = values
            .iter()
            .filter(|value| value.number == 6)
            .collect::<Vec<_>>();
        ensure!(users.len() <= MAX_MEMBERS, "Too many call user mappings");
        for user in users {
            fields(field_bytes(user)?)?;
        }
        if let Some(url) = p::optional_data(&values, 9)? {
            text(url, 8192, false)?;
        }
        signaling::optional_u32(&values, 10)?;
        Ok(Self {
            room,
            room_key,
            own_member: required_u32(&values, 3)?,
            group_id: String::from_utf8(group.to_vec())?,
            client_response: client_response.to_vec(),
            custom_auth: p::optional_data(&values, 7)?.map(Vec::from),
            invite_results: results,
            original: input.to_vec(),
            unknown_fields: signaling::unknown_fields(values, 10),
        })
    }
}

/// A complete caller-sync reply. A decoder preserves unfamiliar items; the
/// account owner must handle all supported effects before committing `cookies`.
/// An unknown item is not evidence that advancing the cursor is safe.
pub struct LiveSyncResponse {
    pub items: Vec<LiveSyncItem>,
    pub cookies: Option<Vec<u8>>,
    pub original: Vec<u8>,
    pub unknown_fields: Vec<Field>,
}

pub struct LiveSyncItem {
    pub kind: i32,
    pub body: Vec<u8>,
    pub original: Vec<u8>,
    pub unknown_fields: Vec<Field>,
}

impl LiveSyncResponse {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let values = fields(input)?;
        let mut items = Vec::new();
        for value in values.iter().filter(|v| v.number == 1) {
            ensure!(items.len() < 256, "Too many call sync items");
            let raw = field_bytes(value)?;
            let nested = fields(raw)?;
            let kind = i32::try_from(required_u64(&nested, 1)? as i64)?;
            let body = required_bytes(&nested, 2)?.to_vec();
            items.push(LiveSyncItem {
                kind,
                body,
                original: raw.to_vec(),
                unknown_fields: signaling::unknown_fields(nested, 2),
            });
        }
        Ok(Self {
            items,
            cookies: p::optional_data(&values, 2)?.map(Vec::from),
            original: input.to_vec(),
            unknown_fields: signaling::unknown_fields(values, 2),
        })
    }
}

/// Caller client context retains original Strategy separately from later current
/// capability/AV snapshots. Retry seconds are not IM mode or a sync deadline.
pub struct JoinClientResponse {
    pub strategy: Vec<u8>,
    pub members: Vec<u8>,
    pub av_status: Option<Vec<u8>>,
    pub live_id: u64,
    pub cookies: Vec<u8>,
    pub retry_seconds: Option<u32>,
    pub original: Vec<u8>,
}

impl JoinClientResponse {
    pub fn decode(input: &[u8]) -> Result<Self> {
        let values = fields(input)?;
        let strategy = required_bytes(&values, 1)?;
        let members = required_bytes(&values, 2)?;
        ensure!(
            !strategy.is_empty() && !members.is_empty(),
            "Missing caller room negotiation"
        );
        fields(strategy)?;
        fields(members)?;
        for n in [3, 4] {
            if let Some(raw) = p::optional_data(&values, n)? {
                fields(raw)?;
            }
        }
        for n in [7, 8, 10] {
            signaling::optional_u32(&values, n)?;
        }
        for value in values.iter().filter(|value| value.number == 9) {
            fields(field_bytes(value)?)?;
        }
        let live_id = required_u64(&values, 5)?;
        ensure!(live_id != 0, "Missing caller live identity");
        Ok(Self {
            strategy: strategy.to_vec(),
            members: members.to_vec(),
            av_status: p::optional_data(&values, 3)?.map(Vec::from),
            live_id,
            cookies: required_bytes(&values, 6)?.to_vec(),
            retry_seconds: signaling::optional_u32(&values, 7)?,
            original: input.to_vec(),
        })
    }
}

fn message(parts: &[Vec<u8>]) -> Result<Vec<u8>> {
    let value = p::message(parts)?;
    ensure!(value.len() <= MAX_BODY, "Personal call body too large");
    Ok(value)
}
fn fields(input: &[u8]) -> Result<Vec<Field>> {
    ensure!(input.len() <= MAX_BODY, "Personal call body too large");
    signaling::decode_fields(input)
}
fn required_bytes(values: &[Field], number: u32) -> Result<&[u8]> {
    p::optional_data(values, number)?
        .ok_or_else(|| anyhow::anyhow!("Missing call bytes field {number}"))
}
fn required_u64(values: &[Field], number: u32) -> Result<u64> {
    match p::optional(values, number)? {
        Some(Value::Varint(v)) => Ok(*v),
        _ => bail!("Missing or invalid call integer field {number}"),
    }
}
fn required_u32(values: &[Field], number: u32) -> Result<u32> {
    Ok(u32::try_from(required_u64(values, number)?)?)
}
fn field_bytes(field: &Field) -> Result<&[u8]> {
    match &field.value {
        Value::Bytes(value) => Ok(value),
        _ => bail!("Invalid call bytes field {}", field.number),
    }
}
fn text(value: &[u8], maximum: usize, nonempty: bool) -> Result<()> {
    ensure!(
        value.len() <= maximum && (!nonempty || !value.is_empty()) && !value.contains(&0),
        "Invalid call text"
    );
    std::str::from_utf8(value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(sdk: &'a [u8], group: &'a [u8]) -> VoiceInputs<'a> {
        VoiceInputs {
            sdk,
            group,
            device: &[],
            engine_capability: &[1],
            channel_capability: &[2],
            network_type: 1,
            sim_type: 0,
            video_length: 0,
            video_ratio: 0,
            nics: &[],
        }
    }
    fn metadata() -> (Vec<u8>, Vec<u8>) {
        (
            vec![8, 1, 18, 1, b'x'],
            vec![
                10, 6, b'w', b'e', b'c', b'h', b'a', b't', 16, 0, 26, 7, b'v', b'o', b'i', b'p',
                b'-', b'2', b'p', 34, 0, 40, 1, 50, 0, 56, 0,
            ],
        )
    }
    #[test]
    fn literal_live_sync_has_distinct_inner_and_host_timeouts() {
        let request = live_sync(7, &[0xab], 6, 9).unwrap();
        assert_eq!(
            request.body(),
            &[8, 7, 18, 1, 0xab, 34, 7, b'v', b'o', b'i', b'p', b'-', b'2', b'p', 40, 6]
        );
        assert_eq!(request.longpoll_seconds, Some(11));
        assert_eq!(request.operation.command(), 9262);
    }
    #[test]
    fn invitation_preserves_absent_custom_auth_and_independent_tasks() {
        let (sdk, group) = metadata();
        let request = invite(&inputs(&sdk, &group), "peer", u64::MAX, 17, None).unwrap();
        let body = fields(request.body()).unwrap();
        assert_eq!(required_u64(&body, 5).unwrap(), u64::MAX);
        assert!(p::optional(&body, 6).unwrap().is_none());
        assert!(invite(&inputs(&sdk, &group), "peer", 1, 17, Some(&[])).is_err());
        let base = vec![48, 99];
        let common = CommonRequest::decode(&request.common(base.clone()).unwrap()).unwrap();
        assert_eq!(common.base_request, Some(base));
        assert_eq!(request.sdk_task, 17);
    }
    #[test]
    fn cancel_preserves_duplicate_pending_names_and_zero_pre_room() {
        let (sdk, group) = metadata();
        let names = vec!["peer".to_owned(), "peer".to_owned()];
        let request = cancel(
            &inputs(&sdk, &group),
            0,
            0,
            99,
            &names,
            CancelReason::Local,
            2,
        )
        .unwrap();
        let body = fields(request.body()).unwrap();
        assert_eq!(body.iter().filter(|v| v.number == 4).count(), 2);
        assert_eq!(required_u64(&body, 3).unwrap(), 0);
        assert!(cancel(
            &inputs(&sdk, &group),
            1,
            0,
            99,
            &names,
            CancelReason::Local,
            2
        )
        .is_err());
    }
    #[test]
    fn invalid_scalar_duplicate_and_utf8_never_coerce() {
        let (sdk, mut group) = metadata();
        group.extend_from_slice(&[40, 1]);
        assert!(invite(&inputs(&sdk, &group), "peer", 1, 1, None).is_err());
        assert!(live_sync(1, &[], 2, 1).is_err());
        let (mut sdk, group) = metadata();
        sdk[4] = 0xff;
        assert!(invite(&inputs(&sdk, &group), "peer", 1, 1, None).is_err());
    }
    #[test]
    fn cloud_join_and_exit_use_distinct_app_envelope_and_source_reason_mapping() {
        let (sdk, group) = metadata();
        let inputs = inputs(&sdk, &group);
        let room = VoiceRoom {
            room: 7,
            room_key: 9,
            own_member: 0,
        };
        let request = join(&inputs, room, 0, 3).unwrap();
        assert_eq!(request.command_id, Some(3361));
        let body = fields(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(required_u64(&body, 5).unwrap(), 2);
        assert_eq!(required_u64(&body, 11).unwrap(), 7);
        assert_eq!(required_u64(&body, 12).unwrap(), 9);
        let cloud_base = [8, 9, 18, 1, b'd', 24, 1];
        let info = fields(&cloud_request_info(&request, &cloud_base).unwrap()).unwrap();
        assert_eq!(required_u64(&info, 1).unwrap(), 3361);
        let request = exit(&inputs, room, 6, false, 4).unwrap();
        assert_eq!(request.command_id, Some(3380));
        let body = fields(request.body.as_deref().unwrap()).unwrap();
        assert_eq!(required_u64(&body, 5).unwrap(), 0);
        assert_eq!(required_u64(&body, 6).unwrap(), 8);
        assert_eq!(required_u64(&body, 8).unwrap(), 1);
    }

    #[test]
    fn full_sync_preserves_later_unknown_item_and_cookie_presence() {
        let first = message(&[p::uint(1, 1), p::bytes(2, b"a")]).unwrap();
        let later = message(&[p::int32(1, -1), p::bytes(2, b"b"), p::uint(99, 7)]).unwrap();
        let wire = message(&[p::bytes(1, &first), p::bytes(1, &later), p::bytes(2, &[])]).unwrap();
        let result = LiveSyncResponse::decode(&wire).unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[1].kind, -1);
        assert_eq!(result.items[1].unknown_fields.len(), 1);
        assert_eq!(result.cookies, Some(vec![]));
        assert!(LiveSyncResponse::decode(&[]).unwrap().cookies.is_none());
        let bad = message(&[p::bytes(1, &[8, 1, 8, 2, 18, 0])]).unwrap();
        assert!(LiveSyncResponse::decode(&bad).is_err());
    }

    #[test]
    fn caller_response_keeps_retry_separate_and_requires_full_widths() {
        let client = message(&[
            p::bytes(1, &[8, 1]),
            p::bytes(2, &[8, 0]),
            p::uint(5, 9),
            p::bytes(6, &[]),
            p::uint(7, 3),
        ])
        .unwrap();
        let result = JoinClientResponse::decode(&client).unwrap();
        assert_eq!(result.live_id, 9);
        assert_eq!(result.retry_seconds, Some(3));
        let mut bad = client.clone();
        bad.extend(p::uint(7, 4));
        assert!(JoinClientResponse::decode(&bad).is_err());
        let mut response = message(&[
            p::uint(1, 10),
            p::uint(2, u64::MAX),
            p::uint(3, 0),
            p::bytes(4, b"g"),
            p::bytes(5, &client),
            p::bytes(7, b"auth"),
            p::uint(100, 1),
        ])
        .unwrap();
        let result = InviteResponse::decode(&response).unwrap();
        assert_eq!(result.own_member, 0);
        assert_eq!(result.room_key, u64::MAX);
        assert_eq!(result.unknown_fields.len(), 1);
        assert_eq!(result.original, response);
        response.extend(p::uint(3, u64::MAX));
        assert!(InviteResponse::decode(&response).is_err());
    }
}
